//! Supervising an interactive Claude/Codex terminal on behalf of a Relay Session: which real
//! provider command safely continues a lease ([`plan_terminal_for_lease`],
//! [`resolve_claude_resume_action`]), running it as a child of Relay so a mid-session handoff can
//! move the user to the new owner ([`run_managed_terminal`]), and the private per-project control
//! channel an in-agent `/relay switch` uses to ask this terminal to do that
//! ([`serve_control_request`]). Shared by `relay claude`, `relay codex`, `relay resume` and
//! `relay switch`'s post-handoff attach — one place decides "what does continuing this
//! conversation actually run".

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use relay_core::{
    ClaudeConfigMode, Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
    handoff::{LeaseStore, OrchestrationLock},
};
use relay_provider_claude::{ClaudeInspector, query_active_sessions};
use serde_json::Value;

use crate::{
    auto_handoff,
    control::{self},
    launch::record_writer_process,
    output::CommandOutput,
    preferences, provider_args, providers, sessions, target, terminal,
    util::{bind_herdr_pane, current_unix_ms},
};

/// How often a supervised *Codex* session asks Codex's structured usage interface whether it is
/// exhausted (Codex has no limit event to hook). Seconds; `RELAY_CODEX_POLL_SECS=0` disables.
const CODEX_POLL_DEFAULT_SECS: u64 = 120;
const CODEX_POLL_ENV: &str = "RELAY_CODEX_POLL_SECS";

/// Everything [`run_managed_terminal`] needs to continue a conversation on whichever profile
/// owns the project's lease *now*, without re-deriving anything from the configured primary.
pub(crate) struct ContinuationContext<'a> {
    service: &'a ProfileService,
    /// The Relay Session this terminal supervises. Interior mutability because `relay claude
    /// --resume` learns which session it is only when Claude reports the conversation it resumed.
    session: std::cell::RefCell<sessions::SessionCtx>,
    canonical_project: PathBuf,
    claude_executable: Option<PathBuf>,
    codex_executable: Option<PathBuf>,
    json_mode: bool,
    paths: RelayPaths,
    preferences: preferences::Preferences,
    /// `relay claude --resume`: where the `SessionStart` hook records whether the resumed
    /// conversation was adopted, so a failure can be explained when the session ends.
    pub(crate) adopt_result: Option<PathBuf>,
}

impl<'a> ContinuationContext<'a> {
    pub(crate) fn new(
        service: &'a ProfileService,
        paths: &RelayPaths,
        canonical_project: &Path,
        session: &sessions::SessionCtx,
        claude_executable: Option<PathBuf>,
        codex_executable: Option<PathBuf>,
        json_mode: bool,
    ) -> Result<Self, Error> {
        Ok(Self {
            service,
            session: std::cell::RefCell::new(session.clone()),
            canonical_project: canonical_project.to_path_buf(),
            claude_executable,
            codex_executable,
            json_mode,
            paths: paths.clone(),
            preferences: preferences::Preferences::load(paths.config_root())?.unwrap_or_default(),
            adopt_result: None,
        })
    }
}

impl ContinuationContext<'_> {
    fn session(&self) -> sessions::SessionCtx {
        self.session.borrow().clone()
    }
    /// The Relay Session's own state directory (lease, journals, provider args, control).
    fn state_dir(&self) -> PathBuf {
        self.session.borrow().dir.clone()
    }
    fn lease_store(&self) -> LeaseStore {
        self.session.borrow().lease_store()
    }
    fn lock(&self) -> OrchestrationLock {
        self.session.borrow().lock()
    }
    fn control(&self) -> control::ControlDir {
        self.session.borrow().control()
    }

    /// `relay claude --resume`: once the `SessionStart` hook has reported which Relay session the
    /// resumed conversation belongs to (a brand-new one, or an existing dormant one), follow it.
    fn rebind_from_adoption(&self) {
        let Some(result) = &self.adopt_result else {
            return;
        };
        let Some(id) = std::fs::read(result)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|value| value["ok"] == true)
            .and_then(|value| value["relay_session_id"].as_str().map(str::to_owned))
            .and_then(|id| relay_core::handoff::RelaySessionId::parse(&id).ok())
        else {
            return;
        };
        if self.session.borrow().id != id {
            let dir = self
                .paths
                .project_state_dir(&self.session.borrow().project_id)
                .join("sessions")
                .join(id.as_str());
            let project_id = self.session.borrow().project_id.clone();
            *self.session.borrow_mut() = sessions::SessionCtx {
                id,
                dir,
                project_id,
            };
        }
    }
}

/// How many times one terminal invocation will follow the conversation across handoffs before it
/// stops and leaves the rest to an explicit `relay resume` (main -> fallback1 -> fallback2 ->
/// fallback3 is already more than a realistic priority list).
const MAX_CONTINUATIONS: usize = 4;

/// Runs an interactive provider session in the user's terminal as a child of Relay and, if the
/// project's writer lease moves to a different profile while (or right after) it runs — i.e. an
/// automatic or manual handoff completed — continues the *same conversation* on the new owner
/// with no command for the user to discover.
///
/// This is deliberately not a daemon and not a usage poller: it lives only as long as the user's
/// own interactive session, only re-reads its own project's lease, never starts or stops any
/// writer itself (every ownership change still goes through the `HandoffCoordinator` under the
/// orchestration lock), and treats anything unexpected — a missing/unreadable lease, a handoff
/// that never settles, an ambiguous-liveness lease — as a reason to stop and hand control back,
/// never to guess.
pub(crate) fn run_managed_terminal(
    context: &ContinuationContext<'_>,
    first: terminal::TerminalCommand,
    first_owner: ProfileName,
    on_first_spawn: Option<&dyn Fn(u32)>,
) -> Result<CommandOutput, Error> {
    let code = run_managed_terminal_inner(context, first, first_owner, on_first_spawn);
    context.rebind_from_adoption();
    let control = context.control();
    control.clear_supervisor();
    // The provider process is gone: this Relay Session no longer has an active owner. (Skipped
    // when a transaction still holds the session, and for a process that is not provably gone.)
    if let Ok(registered) = context.service.list() {
        sessions::release_after_exit(
            &context.paths,
            &context.canonical_project,
            &context.session().id,
            &registered,
        );
    }
    // An in-agent switch that failed after the session was stopped: say why, right here.
    if !context.json_mode
        && let Some(last) = control.last_result()
        && !last.ok
        && current_unix_ms().saturating_sub(last.unix_ms) < 120_000
    {
        eprintln!(
            "\nAgent Relay: {} — run `relay resume` to continue.",
            last.message
        );
    }
    if let Some(result) = &context.adopt_result {
        report_adoption_outcome(context, result);
    }
    std::process::exit(code?)
}

/// Keeps `supervisor.json` describing the conversation this terminal currently supervises (it
/// only appears once a lease for the launched profile exists, e.g. after an adoption).
fn publish_supervisor_record(
    control: &control::ControlDir,
    lease_store: &LeaseStore,
    owner: &ProfileName,
) {
    if let Ok(Some(lease)) = lease_store.load()
        && &lease.owner_profile == owner
    {
        let current = control.live_supervisor();
        // Republish whenever the conversation or its owner changed (a Claude → Claude switch keeps
        // the native session but changes the profile), so the record always names the current owner.
        if current.as_ref().is_none_or(|record| {
            record.session_id != lease.session_id || record.owner_profile != owner.as_str()
        }) {
            control.publish_supervisor(owner.as_str(), &lease.session_id);
        }
    }
}

/// Answers one in-agent `/relay switch` request. The request must name the current lease's
/// session and owner and come from the very process this terminal runs; the target is vetted
/// (enabled, usage, login, identity) *before* anything is promised. The switch itself then runs as
/// the ordinary `relay switch <target> --no-attach` on a helper thread (the transaction stops the
/// running session, which this supervisor must keep reaping), and this terminal follows the new
/// owner exactly as it follows an automatic handoff.
fn serve_control_request(
    context: &ContinuationContext<'_>,
    control: &control::ControlDir,
    lease_store: &LeaseStore,
    child_pid: u32,
) -> Option<std::thread::JoinHandle<()>> {
    let request = control.take_request()?;
    let refuse = |message: &str| {
        control.respond(&control::Response {
            id: request.id.clone(),
            ok: false,
            message: format!("Agent Relay refused: {message}"),
        });
        None
    };
    let control::RequestKind::Switch { target } = &request.request;
    let Ok(Some(lease)) = lease_store.load() else {
        return refuse("there is no active Relay conversation for this project");
    };
    if lease.session_id != request.session_id
        || lease.owner_profile.as_str() != request.owner_profile
    {
        return refuse("that request is stale — the conversation has moved on");
    }
    if child_pid == 0 || request.caller_pid != child_pid {
        return refuse("the request did not come from the session this terminal is running");
    }
    let Ok(registered) = context.service.list() else {
        return refuse("Relay could not read its profiles");
    };
    let Some(profile) = registered
        .iter()
        .find(|profile| profile.name.as_str() == target)
    else {
        return refuse(&format!("'{target}' is not a registered profile"));
    };
    let executables = providers::ExecutableOverrides {
        claude: context.claude_executable.clone(),
        codex: context.codex_executable.clone(),
    };
    let row = target::row_for(
        profile,
        &lease.owner_profile,
        0,
        &executables,
        &context.canonical_project,
        true,
    );
    if row.current {
        return refuse(&format!("'{target}' already holds this conversation"));
    }
    if let Some(reason) = row
        .unavailable
        .or_else(|| target::verification_reason(context.service, profile, &executables))
    {
        return refuse(&format!("'{target}' is unavailable ({reason})"));
    }
    let source = registered
        .iter()
        .find(|candidate| candidate.name == lease.owner_profile);
    let source_provider = source.map_or(ProviderKind::Claude, |candidate| candidate.provider);
    // The same preflight the transaction runs before it stops anything: a foreseeable refusal
    // (another writer in this project, a missing session) is answered now, with the session
    // untouched, instead of after the agent has been closed.
    if let Some(source) = source
        && source_provider == ProviderKind::Claude
        && profile.provider == ProviderKind::Claude
        && let Some(stager) = providers::ports_for(
            ProviderKind::Claude,
            &executables,
            source.effective_claude_config_mode(),
        )
        .stager
        && let Err(error) = stager.preflight(
            &source.config_dir,
            &context.canonical_project,
            &lease.session_id,
            Some(&lease.owner_process),
            source.effective_claude_config_mode(),
        )
    {
        return refuse(&format!(
            "the switch cannot start safely ({error}); the session was left running"
        ));
    }
    control.respond(&control::Response {
        id: request.id.clone(),
        ok: true,
        message: format!(
            "Agent Relay: switching to '{target}' ({}). This terminal reopens the conversation there in a moment.",
            if source_provider == ProviderKind::Claude && profile.provider == ProviderKind::Claude {
                "the same Claude conversation continues"
            } else {
                "continues from Relay's state bundle in a new session"
            }
        ),
    });
    // Let the acknowledgement render in the agent before its session is stopped.
    std::thread::sleep(std::time::Duration::from_millis(400));

    let program = std::env::current_exe().ok()?;
    let mut command = std::process::Command::new(program);
    command
        .arg("--json")
        .arg("--config-root")
        .arg(context.paths.config_root())
        .arg("--state-root")
        .arg(context.paths.state_root())
        .args(["switch", target.as_str(), "--no-attach", "--session"])
        .arg(context.session().id.as_str())
        .arg("--project-dir")
        .arg(&context.canonical_project)
        .stdin(std::process::Stdio::null());
    if let Some(claude) = &context.claude_executable {
        command.arg("--claude-executable").arg(claude);
    }
    if let Some(codex) = &context.codex_executable {
        command.arg("--codex-executable").arg(codex);
    }
    let control_dir = control::ControlDir::for_project(&context.state_dir());
    let target = target.clone();
    let lease_path = context.state_dir().join("lease.json");
    let preferences = context.preferences.clone();
    let profile_name = profile.name.clone();
    Some(std::thread::spawn(move || {
        let output = command.output();
        let succeeded = output.as_ref().is_ok_and(|output| output.status.success());
        let message = if succeeded {
            format!("switched to '{target}'")
        } else {
            let detail = output
                .as_ref()
                .ok()
                .and_then(|output| serde_json::from_slice::<Value>(&output.stderr).ok())
                .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "the switch did not complete".to_owned());
            format!("the switch to '{target}' failed: {detail}")
        };
        control_dir.record_last(succeeded, &message);
        if succeeded && let Ok(Some(lease)) = LeaseStore::at_path(lease_path).load() {
            let fallback: Vec<ProfileName> =
                auto_handoff::hierarchy_without(&preferences, &profile_name, |_| true)
                    .into_iter()
                    .cloned()
                    .collect();
            bind_herdr_pane(&profile_name, &fallback, &lease.session_id);
        }
    }))
}

/// `relay claude --resume` ended: say why the conversation was not adopted, if it was not.
fn report_adoption_outcome(context: &ContinuationContext<'_>, result: &Path) {
    let recorded: Option<Value> = std::fs::read(result)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let _ignored = std::fs::remove_file(result);
    if context.json_mode {
        return;
    }
    match recorded {
        Some(value) if value["ok"] == true => {}
        Some(value) => eprintln!(
            "\n{}",
            value["message"]
                .as_str()
                .unwrap_or("Agent Relay did not adopt this conversation.")
        ),
        None => eprintln!(
            "\nAgent Relay did not adopt a conversation (none was resumed), so nothing is managed."
        ),
    }
}

fn run_managed_terminal_inner(
    context: &ContinuationContext<'_>,
    first: terminal::TerminalCommand,
    first_owner: ProfileName,
    on_first_spawn: Option<&dyn Fn(u32)>,
) -> Result<i32, Error> {
    use std::io::Write as _;
    let timing = terminal::Timing::default();
    let mut command = first;
    let mut owner = terminal::LeaseOwner(first_owner);
    // Failed in-agent switches whose source session has already been reopened (each once).
    let mut restored_after: Vec<u64> = Vec::new();

    for continuation in 0..=MAX_CONTINUATIONS {
        let _ = std::io::stdout().flush();
        // A Codex session has no "limit reached" event to hang a trigger on, so while the user's
        // own terminal session is running, periodically start the same one-shot evaluation the
        // Claude hook starts. It only *evaluates*: any handoff goes through the unchanged
        // coordinator, lock, cooldown and ledger.
        let poll_secs = std::env::var(CODEX_POLL_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(CODEX_POLL_DEFAULT_SECS);
        let poll_action = || {
            let Ok(Some(lease)) = context.lease_store().load() else {
                return;
            };
            let Ok(registered) = context.service.list() else {
                return;
            };
            let Some(profile) = registered
                .iter()
                .find(|candidate| candidate.name == lease.owner_profile)
            else {
                return;
            };
            if let Some(plan) = auto_handoff::plan_poll(
                &context.paths,
                &context.preferences,
                &registered,
                profile,
                &lease,
                &context.canonical_project,
            ) {
                auto_handoff::spawn_detached(&plan);
            }
        };
        let owner_is_codex = context.service.list().is_ok_and(|registered| {
            registered
                .iter()
                .any(|profile| profile.name == owner.0 && profile.provider == ProviderKind::Codex)
        });
        if owner_is_codex {
            let profile = context
                .service
                .list()?
                .into_iter()
                .find(|profile| profile.name == owner.0)
                .ok_or_else(|| Error::ProfileNotFound(owner.0.to_string()))?;
            let skill = crate::codex_integration::install(&profile.config_dir);
            // Scope every skill invocation to this supervisor's roots and conversation. Replaced
            // on every attach, including a Claude -> Codex handoff and same-profile resume.
            command.envs.extend([
                (
                    "RELAY_EXECUTABLE".into(),
                    std::env::current_exe()
                        .map_err(|source| Error::Io {
                            path: "relay".into(),
                            source,
                        })?
                        .into_os_string(),
                ),
                (
                    "RELAY_CONFIG_ROOT".into(),
                    context.paths.config_root().as_os_str().to_owned(),
                ),
                (
                    "RELAY_STATE_ROOT".into(),
                    context.paths.state_root().as_os_str().to_owned(),
                ),
                (
                    "RELAY_PROJECT_DIR".into(),
                    context.canonical_project.as_os_str().to_owned(),
                ),
                (
                    "RELAY_SESSION_ID".into(),
                    context.session().id.as_str().into(),
                ),
            ]);
            if !context.json_mode {
                eprintln!(
                    "{} Codex · session {}",
                    crate::badge::styled(&format!("[Relay · {}]", owner.0)),
                    context.session().id.short()
                );
                match skill {
                    Ok(()) => eprintln!(
                        "Relay commands: $relay status · $relay doctor · $relay why · $relay history"
                    ),
                    Err(error) => eprintln!(
                        "Relay skill unavailable: {error}. Run `relay doctor` in another terminal."
                    ),
                }
            }
        }
        // One 300ms tick serves both duties, each on its own cadence: the in-agent control
        // channel (a request is answered within a fraction of a second) and, for Codex only, the
        // periodic usage evaluation.
        let child_pid = std::cell::Cell::new(0_u32);
        let mut last_codex_poll = std::time::Instant::now();
        let mut pending_switch: Option<std::thread::JoinHandle<()>> = None;
        let mut tick_action = || {
            context.rebind_from_adoption();
            let control = context.control();
            publish_supervisor_record(&control, &context.lease_store(), &owner.0);
            if pending_switch
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            {
                pending_switch = None;
            }
            if pending_switch.is_none() {
                pending_switch = serve_control_request(
                    context,
                    &control,
                    &context.lease_store(),
                    child_pid.get(),
                );
            }
            if owner_is_codex
                && poll_secs > 0
                && last_codex_poll.elapsed() >= std::time::Duration::from_secs(poll_secs)
            {
                last_codex_poll = std::time::Instant::now();
                poll_action();
            }
        };
        let tick = Some(terminal::Tick {
            every: std::time::Duration::from_millis(300),
            action: &mut tick_action,
        });
        // The interactive process of a *continuation* is recorded in the lease too, so the next
        // stop-and-verify (and in-agent switch) can identify it exactly.
        let continuation_session = context
            .lease_store()
            .load()
            .ok()
            .flatten()
            .map(|lease| lease.session_id);
        let record_spawn = |pid: u32| {
            child_pid.set(pid);
            match (continuation, on_first_spawn, &continuation_session) {
                (0, Some(first), _) => first(pid),
                (1.., _, Some(session)) => {
                    record_writer_process(&context.lease_store(), &context.lock(), session, pid);
                }
                _ => {}
            }
        };
        let end = terminal::run_watching_lease(
            &command,
            &|| context.lease_store(),
            &owner,
            &timing,
            tick,
            Some(&record_spawn),
        )
        .map_err(|source| Error::Io {
            path: command.program.clone(),
            source,
        })?;
        // The switch helper (if any) finishes recording its outcome before the decision below.
        if let Some(helper) = pending_switch.take() {
            let _ignored = helper.join();
        }
        let code = match end {
            terminal::TerminalEnd::Exited(code) => code,
            terminal::TerminalEnd::OwnerMoved => 0,
        };

        // Codex has no "the limit was hit" event: the only honest way to notice real exhaustion
        // is the periodic structured-usage tick above, while the child runs. But the child can
        // also exit *because* it just hit the limit — possibly between two ticks, or before the
        // first one ever fired — and nothing has evaluated that yet. A detached, fire-and-forget
        // poll (as the periodic tick uses) would race `release_after_exit` in the caller and could
        // easily lose: this session must not be released before a final evaluation has had its
        // chance to run and, if the writer really is exhausted, complete the handoff — so it runs
        // synchronously, in-process, right here.
        if owner_is_codex
            && matches!(end, terminal::TerminalEnd::Exited(_))
            && let Ok(Some(lease)) = context.lease_store().load()
            && let Ok(registered) = context.service.list()
            && let Some(profile) = registered
                .iter()
                .find(|candidate| candidate.name == lease.owner_profile)
        {
            let _ignored = auto_handoff::evaluate_now(
                &context.paths,
                &context.preferences,
                &registered,
                profile,
                &lease,
                &context.canonical_project,
                context.claude_executable.as_deref(),
            );
        }

        // A handoff stops the source session itself, so the session can end *before* the lease
        // has moved: wait for any in-flight transaction to settle before deciding.
        if !terminal::wait_until_settled(
            &context.lock(),
            timing.settle_timeout,
            std::time::Duration::from_millis(500),
        ) {
            if !context.json_mode {
                eprintln!(
                    "\nAgent Relay: a handoff is still in progress; run `relay resume` once it completes."
                );
            }
            return Ok(code);
        }
        let Ok(Some(lease)) = context.lease_store().load() else {
            return Ok(code);
        };
        // A switch that failed AFTER the session was deliberately stopped, but before ownership
        // moved, leaves the lease exactly as it was: the same owner, the same session, no process.
        // Reopening that very session natively is safe and deterministic (one writer: nobody else
        // holds the project), so the terminal does that instead of leaving the user stranded.
        if terminal::LeaseOwner::of(&lease) == owner
            && continuation < MAX_CONTINUATIONS
            && context.control().last_result().is_some_and(|last| {
                !last.ok
                    && current_unix_ms().saturating_sub(last.unix_ms) < 120_000
                    && !restored_after.contains(&last.unix_ms)
            })
            && let Some(last) = context.control().last_result()
        {
            restored_after.push(last.unix_ms);
            let registered = context.service.list()?;
            if let Some(profile) = registered
                .iter()
                .find(|candidate| candidate.name == lease.owner_profile)
                && let Ok(reopened) = provider_args::ProviderArgs::load(&context.state_dir())
                    .and_then(|stored| {
                        plan_terminal_for_lease(
                            profile,
                            &lease,
                            &context.canonical_project,
                            context.claude_executable.as_deref(),
                            context.codex_executable.as_deref(),
                            &stored,
                        )
                    })
            {
                if !context.json_mode {
                    println!(
                        "\nAgent Relay: {} — reopening the same conversation on '{}' (nothing moved).",
                        last.message, lease.owner_profile
                    );
                }
                command = reopened;
                continue;
            }
        }
        if terminal::LeaseOwner::of(&lease) == owner || continuation == MAX_CONTINUATIONS {
            if continuation == MAX_CONTINUATIONS
                && terminal::LeaseOwner::of(&lease) != owner
                && !context.json_mode
            {
                eprintln!(
                    "\nAgent Relay: the conversation moved to '{}'; run `relay resume` to continue it.",
                    lease.owner_profile
                );
            }
            return Ok(code);
        }

        // Continue the same conversation on whoever owns it now — resolved from the lease, never
        // from the configured primary.
        let registered = context.service.list()?;
        let Some(profile) = registered
            .iter()
            .find(|candidate| candidate.name == lease.owner_profile)
        else {
            return Ok(code);
        };
        let next = match provider_args::ProviderArgs::load(&context.state_dir()).and_then(
            |stored| {
                plan_terminal_for_lease(
                    profile,
                    &lease,
                    &context.canonical_project,
                    context.claude_executable.as_deref(),
                    context.codex_executable.as_deref(),
                    &stored,
                )
            },
        ) {
            Ok(next) => next,
            Err(error) => {
                if !context.json_mode {
                    eprintln!(
                        "\nAgent Relay: the conversation moved to '{}' but could not be continued automatically ({error}); run `relay resume`.",
                        lease.owner_profile
                    );
                }
                return Ok(code);
            }
        };
        if !context.json_mode {
            println!(
                "\nAgent Relay: the conversation moved from '{}' to '{}' — continuing on '{}'...",
                owner.0, lease.owner_profile, lease.owner_profile
            );
        }
        owner = terminal::LeaseOwner::of(&lease);
        command = next;
    }
    unreachable!("the loop above always returns")
}

/// Which real Claude command safely continues this lease. Dogfood-found (M6): `relay resume`
/// used to always run `claude --resume <session_id>` — but a session with a still-live
/// `claude --bg` background job rejects `--resume` outright ("running as a background session
/// ... run `claude attach <id>`"); only Claude's own `attach` works for a live job. Conversely,
/// once the background job is confirmed gone, `attach` would find nothing — `--resume` (native
/// session/thread resumption) is what's actually safe there.
///
/// - The lease's `provider_handle` (present for a direct launch, absent for a lease a cross-
///   profile handoff produced — see `WriterLease::provider_handle`'s own doc comment) is checked
///   against `claude agents --json`'s live listing: found → definitely live → `Attach`.
/// - Not found, but present: fall back to the recorded owner process's own pid+start-time
///   fingerprint (the same authoritative signal `ClaudeSourceLiveness`/`SessionStopper` already
///   use elsewhere). Confirmed gone (`Some(false)`) → `NativeResume`. Anything else — genuinely
///   indeterminate (`None`), *or* the recorded process improbably still matches yet Claude's own
///   listing disagrees with it (`Some(true)`) — is an inconsistent state this must never guess
///   through, so it fails closed with `AmbiguousSessionLiveness` rather than risking either a
///   failed attach or, worse, racing a background job that is in fact still there.
/// - No `provider_handle` at all: no background job was ever recorded for this lease (a
///   handoff's target launch is a foreground verification turn that has already exited by the
///   time anyone resumes later, never a persistent `--bg` job) — nothing to check liveness
///   against, so this is unconditionally `NativeResume`, not ambiguous.
fn resolve_claude_resume_action(
    config_dir: &Path,
    mode: ClaudeConfigMode,
    claude_executable: Option<&Path>,
    lease: &relay_core::handoff::WriterLease,
) -> Result<ClaudeResumeAction, Error> {
    let Some(handle) = &lease.provider_handle else {
        return Ok(ClaudeResumeAction::NativeResume);
    };
    let sessions = query_active_sessions(config_dir, mode, claude_executable)?;
    let listed = sessions
        .iter()
        .any(|record| record.id.as_deref() == Some(handle.as_str()));
    if listed {
        return Ok(ClaudeResumeAction::Attach(handle.clone()));
    }
    match lease.owner_process.is_still_the_same_process() {
        Some(false) => Ok(ClaudeResumeAction::NativeResume),
        Some(true) | None => Err(Error::AmbiguousSessionLiveness(
            lease.owner_profile.to_string(),
        )),
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ClaudeResumeAction {
    /// `claude attach <id>` — the recorded background job is confirmed live.
    Attach(String),
    /// `claude --resume <session_id>` — no live background job; the native session itself is
    /// what gets resumed.
    NativeResume,
}

/// Builds (does not run) `codex resume <thread-id>` under the profile's own `CODEX_HOME`.
/// Codex has no background-job/attach concept, so this is unconditional (`NATIVE_RESUME`).
fn verify_codex_thread(
    codex_executable: &Path,
    config_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
) -> Result<(), Error> {
    let not_verified = || Error::CodexThreadNotVerified(thread_id.to_owned());
    let identity =
        relay_provider_codex::app_server::read_thread(codex_executable, config_dir, thread_id)
            .map_err(|_| not_verified())?;
    if identity.id != thread_id {
        return Err(not_verified());
    }
    if let Some(cwd) = identity.cwd {
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if canonical(&cwd) != canonical(project_dir) {
            return Err(not_verified());
        }
    }
    Ok(())
}

pub(crate) fn plan_codex_resume(
    codex_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    extra_args: &[String],
    initial_message: Option<&str>,
) -> Result<terminal::TerminalCommand, Error> {
    let inspector = relay_provider_codex::CodexInspector::discover(codex_executable)?;
    // NATIVE_RESUME is only claimed for a thread Codex itself confirms: the id must exist in this
    // profile's own CODEX_HOME and belong to this project. (An interactive `codex resume` with a
    // missing or stale id can otherwise start a different thread without any clear failure.)
    verify_codex_thread(inspector.executable(), config_dir, project_dir, thread_id)?;
    let mut args: Vec<OsString> = vec!["resume".into(), thread_id.into()];
    args.extend(extra_args.iter().map(OsString::from));
    if let Some(message) = initial_message {
        // After `--` so a variadic option (`-i FILE…`) or a leading `-` can never swallow it.
        args.push("--".into());
        args.push(message.into());
    }
    Ok(terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args,
        envs: vec![("CODEX_HOME".into(), config_dir.into())],
        env_removals: Vec::new(),
        current_dir: Some(project_dir.to_path_buf()),
    })
}

/// Builds (does not run) an interactive `claude --resume <session-id>` under the given profile's
/// own `CLAUDE_CONFIG_DIR`.
fn plan_claude_resume(
    claude_executable: Option<&Path>,
    config_dir: &Path,
    mode: ClaudeConfigMode,
    project_dir: &Path,
    session_id: &str,
    extra_args: &[String],
) -> Result<terminal::TerminalCommand, Error> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let mut args: Vec<OsString> = vec!["--resume".into(), session_id.into()];
    // The user's own arguments follow Relay's `--resume <id>` verbatim; the session is always the
    // one Relay chose (a conflicting flag was rejected up front).
    args.extend(extra_args.iter().map(OsString::from));
    let (envs, env_removals) = claude_terminal_env(config_dir, mode);
    Ok(terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args,
        envs,
        env_removals,
        current_dir: Some(project_dir.to_path_buf()),
    })
}

/// The `CLAUDE_CONFIG_DIR` environment a Claude child launched into the user's own terminal must
/// get, by mode — mirrors `relay_provider_claude::apply_config_mode` (which operates on a
/// `std::process::Command` we don't build directly here, since `TerminalCommand` also covers
/// Codex).
pub(crate) fn claude_terminal_env(
    config_dir: &Path,
    mode: ClaudeConfigMode,
) -> (Vec<(OsString, OsString)>, Vec<OsString>) {
    match mode {
        ClaudeConfigMode::Explicit => (
            vec![("CLAUDE_CONFIG_DIR".into(), config_dir.into())],
            Vec::new(),
        ),
        ClaudeConfigMode::NativeDefault => (Vec::new(), vec!["CLAUDE_CONFIG_DIR".into()]),
    }
}

/// The interactive command that safely continues `lease` under its *owner's* provider and
/// isolated config directory: Codex is unconditionally `NATIVE_RESUME`; Claude is decided by
/// [`resolve_claude_resume_action`] (attach to a live background job, native resume otherwise, or
/// fail closed on ambiguous liveness). Shared by `relay resume` and by the automatic continuation
/// after a handoff, so both always pick the identical command for the identical lease.
pub(crate) fn plan_terminal_for_lease(
    profile: &Profile,
    lease: &relay_core::handoff::WriterLease,
    canonical_project: &Path,
    claude_executable: Option<&Path>,
    codex_executable: Option<&Path>,
    stored_args: &provider_args::ProviderArgs,
) -> Result<terminal::TerminalCommand, Error> {
    // Only the OWNER's provider's arguments are ever used here; the other provider's stay unread.
    let provider_args = stored_args.for_provider(profile.provider);
    match profile.provider {
        ProviderKind::Codex => plan_codex_resume(
            codex_executable,
            &profile.config_dir,
            canonical_project,
            &lease.session_id,
            provider_args,
            None,
        ),
        ProviderKind::Claude | ProviderKind::Fake => {
            match resolve_claude_resume_action(
                &profile.config_dir,
                profile.effective_claude_config_mode(),
                claude_executable,
                lease,
            )? {
                ClaudeResumeAction::Attach(short_id) => {
                    let inspector = ClaudeInspector::discover(claude_executable)?;
                    Ok(plan_claude_attach(
                        inspector.executable(),
                        &profile.config_dir,
                        profile.effective_claude_config_mode(),
                        &short_id,
                    ))
                }
                ClaudeResumeAction::NativeResume => plan_claude_resume(
                    claude_executable,
                    &profile.config_dir,
                    profile.effective_claude_config_mode(),
                    canonical_project,
                    &lease.session_id,
                    provider_args,
                ),
            }
        }
    }
}

/// M4.4: the officially supported way to give the user a real, live, interactive terminal on a
/// session Relay itself launched with `claude --bg` — `claude attach <short-id>` ("Open the
/// background session in this terminal ... The session keeps running either way", per `claude
/// attach --help`, live-checked against Claude Code 2.1.278). This is not a Relay-invented
/// workaround: it is Claude's own documented attach mechanism for exactly this session kind, so
/// it preserves the process Relay's `WriterLease`/liveness checks already track — no new pid is
/// spawned independently of the one Relay recorded.
///
/// Builds (does not run) that command. It is run through [`run_managed_terminal`] as a child in
/// the user's own terminal (full TTY, identical to typing `claude attach <id>` yourself) rather
/// than `exec`'d, so Relay can continue the conversation on a fallback profile if a handoff
/// happens while the user is attached.
///
/// `config_dir` must be the *lease owner's* registered `CLAUDE_CONFIG_DIR` (looked up by
/// `lease.owner_profile`, never assumed to be the configured primary — a handoff can leave a
/// fallback profile holding the lease). Without it, `claude attach` falls back to whatever
/// `CLAUDE_CONFIG_DIR` this process inherited from its parent shell — typically the default
/// account, not the isolated profile that actually owns the background job registry entry — so
/// attach fails with "No job matching '<id>'" even though the session is live under the correct
/// profile (dogfood-found: M6). Set only on the child's environment (`Command::env`), never on
/// this process's own, so credential isolation between profiles is preserved and no global state
/// is mutated.
pub(crate) fn plan_claude_attach(
    executable: &Path,
    config_dir: &Path,
    mode: ClaudeConfigMode,
    short_id: &str,
) -> terminal::TerminalCommand {
    let (envs, env_removals) = claude_terminal_env(config_dir, mode);
    terminal::TerminalCommand {
        program: executable.to_path_buf(),
        args: vec!["attach".into(), short_id.into()],
        envs,
        env_removals,
        current_dir: None,
    }
}

#[cfg(test)]
mod tests {
    use relay_core::{
        ProfileName,
        handoff::{LeaseStore, ProcessIdentity, ProjectId, TransactionId, WriterLease},
    };

    use super::publish_supervisor_record;

    #[test]
    fn the_supervisor_record_follows_a_conversation_across_a_same_session_owner_change() {
        let dir = tempfile::tempdir().expect("dir");
        let control = crate::control::ControlDir::for_project(dir.path());
        let store = LeaseStore::at_path(dir.path().join("lease.json"));
        let project = ProjectId::for_canonical_path(std::path::Path::new("/work/x")).expect("id");
        let lease_for = |owner: &str| {
            WriterLease::new(
                project.clone(),
                ProfileName::new(owner).expect("name"),
                ProcessIdentity::current(),
                "native-1".to_owned(),
                TransactionId::generate(),
                1,
            )
        };
        let megan = ProfileName::new("megan").expect("name");
        let erika = ProfileName::new("erika").expect("name");
        store.save(&lease_for("megan")).expect("lease");
        publish_supervisor_record(&control, &store, &megan);
        assert_eq!(
            control.live_supervisor().expect("record").owner_profile,
            "megan"
        );
        // A Claude → Claude switch keeps the native session and changes the owner.
        store.save(&lease_for("erika")).expect("lease");
        publish_supervisor_record(&control, &store, &erika);
        assert_eq!(
            control.live_supervisor().expect("record").owner_profile,
            "erika"
        );
    }
}
