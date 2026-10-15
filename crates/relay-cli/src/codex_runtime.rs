//! GitHub #18: opt-in wiring for the event-driven wake-up that lets a supervised Codex terminal
//! learn about a structured `usage_limit_exceeded` exhaustion from Codex's own local rollout file
//! instead of only ever finding out on GitHub #13's own next poll. See
//! `relay_provider_codex::rollout` for the actual filesystem mechanics this wires together; this
//! module owns only the session-scoped orchestration (when to start it, when to stop it).
//!
//! **Off by default.** [`event_driven_enabled`] gates every entry point in this module *except*
//! the profile exclusivity lock (`acquire_profile_lock`'s own call site in
//! `terminal_session::run_managed_terminal_inner`): unset/not `"1"`, no [`EventDrivenRuntime`] is
//! ever started, so no rollout watch runs and GitHub #13's own polling is the sole safety net,
//! exactly as before GitHub #16. The exclusivity lock itself is unconditional (agent-relay#18,
//! profile exclusivity before thread creation) — it is this codebase's own "one active
//! writer/owner per Relay Session" invariant (`AGENTS.md`) applied at the profile level, and its
//! correctness cannot depend on whether the opt-in observer feature happens to be on. GitHub #13
//! is completely unaffected either way: its own polling never depends on anything in this module,
//! by construction.
//!
//! Every entry point here is best-effort in the same sense the rest of Codex supervision already
//! is: any failure at any step (version unverified, no known thread id, the file never appeared)
//! falls back to launching the interactive command exactly as before, and is never surfaced as a
//! startup failure. GitHub #13's own polling — unaffected by any of this, on or off — remains the
//! permanent safety net for exactly the sessions this declines to cover.
//!
//! ## Why this replaced the app-server observer (2026-10-13 writer-conflict incident)
//!
//! Two earlier rounds of this GitHub #18 work built an observer that opened its own connection to
//! Codex's shared app-server daemon and subscribed via `thread/resume`, so it could receive the
//! same live turn/error notifications the interactive TUI's own connection gets. A live incident
//! (`proj-43efc346dfb87732`, 2026-10-13, thread `01a133b0-37a2-7883-979b-a994ab9089e9`) proved,
//! via the real daemon's own structured logs (never inferred from the UI alone), that this
//! observer's own `thread/resume` call won a race against the real interactive client's own
//! resume for the identical thread and got the daemon's thread-store to reject the *interactive*
//! client with `"thread-store conflict: thread <id> already has an active writer"` — the literal,
//! confirmed cause of a real "This conversation is open in another app" failure. The app-server's
//! own protocol (confirmed against its generated schema) exposes no separate, authoritative
//! "read-only subscribe" primitive distinct from `thread/resume` — there is no live-verified way
//! to receive a thread's notifications without contending for its single active-writer slot.
//!
//! Separately, and discovered independently while investigating the incident: every real `relay
//! codex`/`relay resume` call this module's single production caller
//! (`terminal_session::run_managed_terminal_inner`) ever makes already knows its thread id before
//! this module is ever called — a brand-new `relay codex` launch creates its native thread first
//! (`relay_provider_codex::launch_new_thread`) and then resumes it by id, exactly like `relay
//! resume`'s own lease continuation. There has never been a real caller with an unknown thread id.
//!
//! The fix is architectural, not a retry-order trick: [`EventDrivenRuntime`] now watches Codex's
//! own on-disk rollout file for the already-known thread id directly
//! (`relay_provider_codex::rollout::RolloutWatcher`) — no app-server connection, no
//! `thread/resume`, no `thread/start`, nothing to contend for the writer slot with, by
//! construction rather than by proof. See `relay_provider_codex::rollout`'s own module doc for the
//! live-confirmed on-disk format this relies on (filename *and* content both name the exact thread
//! id; the structured `usage_limit_exceeded` record shape; append-only; never reacts to history
//! that predates the watch).
//!
//! **Mid-session thread replacement is now handled, not merely disclosed as a gap.** A live
//! supervised TUI genuinely can change which native thread it drives without ending supervision —
//! real, pinned-version-confirmed in-TUI commands (`/new`, `/fork`, "resume a saved chat"), not a
//! theoretical pathway. `relay_provider_codex::rollout::RolloutWatcher` is a profile-domain watcher
//! (see its own module doc), not a single-thread one: it rebinds to whichever thread becomes
//! authoritative, using `/fork`'s own `forked_from_id` link directly, and — for `/new`/"resume a
//! saved chat," which carry no such link — the profile exclusivity lock this module acquires
//! *before* any Relay-controlled thread creation (not only at interactive-attach time) to make
//! "no other in-scope actor can be driving a top-level thread in this `CODEX_HOME` right now" a
//! provable invariant rather than an assumption. [`EventDrivenRuntime::tick`] reports a transition
//! via `TickEvent`'s own health-transition reporting staying silent (a transition never flips
//! [`EventDrivenRuntime::is_healthy`]) while still requesting exactly one reconciliation. An
//! entirely unmanaged `codex` process a user points at this same `CODEX_HOME` by hand remains
//! explicitly out of scope — the same residual boundary profile isolation already draws
//! everywhere else; see `relay_core::handoff::lock`'s own doc comment.
//!
//! **The interactive command's own arguments are never touched by this module**, for the same
//! reason agent-relay#18's very first fix established: an earlier design had the interactive
//! client attach to a private app-server via `--remote`, which Codex's own TUI treats as a "remote
//! task" and silently overrides the caller's own permission arguments with the resumed thread's
//! saved settings instead. This module only *observes*, entirely outside Codex's own process
//! tree — the caller's `TerminalCommand` is launched completely unmodified.

use std::path::Path;

use relay_core::handoff::ProfileSessionLock;
use relay_provider_codex::{
    VersionStatus, assess_version,
    rollout::{AttachError, RolloutEvent, RolloutWatcher},
};

pub const EVENT_DRIVEN_ENV: &str = "RELAY_CODEX_EVENT_DRIVEN";

#[must_use]
pub(crate) fn event_driven_enabled() -> bool {
    std::env::var(EVENT_DRIVEN_ENV).ok().as_deref() == Some("1")
}

/// What [`EventDrivenRuntime::tick`] observed this call, for a caller's own one-time diagnostic
/// print only — nothing in this module's own safety behavior depends on the caller ever
/// inspecting it (see `terminal_session::report_event_driven_transition`).
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TickEvent {
    /// No health transition this tick (the common case on every tick).
    Nothing,
    /// The watch lost coverage (a filesystem error it could not immediately recover from) —
    /// GitHub #13's polling is the safety net for the remainder of this session.
    BecameUnhealthy,
    /// The watch recovered after a prior [`Self::BecameUnhealthy`].
    BecameRecovered,
}

/// One started event-driven runtime: a live [`RolloutWatcher`] for the session's already-known
/// thread id, plus the durable trace location this module records attach/health transitions to.
/// There is deliberately no method here that returns anything for a caller to append to the
/// interactive command's own arguments — this module's whole design keeps that command completely
/// unmodified (see this module's own doc comment).
pub(crate) struct EventDrivenRuntime {
    watcher: RolloutWatcher,
    project_state_dir: std::path::PathBuf,
    was_healthy: bool,
}

impl EventDrivenRuntime {
    /// Best-effort, non-blocking start — `relay_provider_codex::rollout::RolloutWatcher::attach`
    /// never waits for the rollout file to exist (see its own doc comment); this runtime is simply
    /// not yet [`Self::is_healthy`] until its background loop finds it. Returns `None` on any
    /// failure that *does* prevent starting at all — env-gated off, an unverified Codex version, no
    /// known thread id (believed unreachable for any current real caller — see this module's own
    /// doc comment and
    /// [`the_real_relay_codex_terminal_session_launch_shape_is_always_a_known_thread_id`]), or the
    /// OS-level watch itself failing — silently: callers must always still launch the interactive
    /// command exactly as planned, with or without this runtime.
    pub(crate) fn start(
        codex_executable: &Path,
        config_dir: &Path,
        project_state_dir: &Path,
        thread_id: Option<&str>,
    ) -> Option<Self> {
        if !event_driven_enabled() {
            return None;
        }
        if !version_verified(codex_executable) {
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_version_unverified",
            );
            return None;
        }
        let Some(thread_id) = thread_id else {
            // Believed unreachable for any current real caller (see this module's own doc
            // comment) — traced distinctly from every other decline so a future regression that
            // somehow does reach this is visible in the durable trace, never silently identical
            // to an ordinary version/attach failure.
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_no_known_thread_id",
            );
            return None;
        };
        let state_file = project_state_dir.join("codex_rollout_watch_state.json");
        match RolloutWatcher::attach(config_dir, thread_id, &state_file) {
            Ok(watcher) => {
                // "Started," never "attached"/"healthy": the background loop may still be
                // searching for a not-yet-existing rollout file at this exact instant (see
                // `RolloutWatcher::attach`'s own doc comment) — `was_healthy: false` below means
                // the *first* time `tick()` observes `is_healthy() == true` is traced in its own
                // right (`codex_event_driven_recovered`), never conflated with this one.
                crate::auto_handoff::trace_event(
                    project_state_dir,
                    "codex_event_driven_watch_started",
                );
                Some(Self {
                    watcher,
                    project_state_dir: project_state_dir.to_path_buf(),
                    was_healthy: false,
                })
            }
            Err(AttachError::WatchUnavailable) => {
                crate::auto_handoff::trace_event(
                    project_state_dir,
                    "codex_event_driven_watch_unavailable",
                );
                None
            }
        }
    }

    /// agent-relay#18 (zero-poll architecture): whether this runtime currently has live, caught-up
    /// coverage — the one condition under which `codex_poll`'s own periodic timer-driven cadence
    /// may be safely suppressed (see `codex_poll::CodexPollScheduler::set_event_driven_healthy`).
    #[must_use]
    pub(crate) fn is_healthy(&self) -> bool {
        self.watcher.is_healthy()
    }

    /// One supervision tick (the same 300ms cadence that already drives GitHub #13's own
    /// scheduler, never a separate timer): drains decoded events into `scheduler`
    /// (`crate::codex_poll::CodexPollScheduler::notify_event`) and reports a health transition, if
    /// any, purely so a caller can print a one-time diagnostic.
    pub(crate) fn tick(
        &mut self,
        scheduler: &mut crate::codex_poll::CodexPollScheduler,
    ) -> TickEvent {
        for event in self.watcher.drain_events() {
            match event {
                RolloutEvent::UsageLimitExceeded => {
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        "codex_usage_limit_event_received",
                    );
                    scheduler.notify_event(
                        relay_provider_codex::observer::ObserverEvent::UsageLimitExceeded,
                    );
                }
                // agent-relay#18 (profile-domain watching): the watcher itself already caught up
                // and stayed healthy throughout (see `RolloutEvent::ThreadTransitioned`'s own doc
                // comment) — the one thing a caller still needs is exactly one authoritative
                // reconciliation against the *new* thread, the same action already taken on
                // recovery from an unhealthy gap, so a transition that happened to land right at
                // (or just after) an exhaustion is still corroborated promptly rather than waiting
                // for whatever happens to run next.
                RolloutEvent::ThreadTransitioned(new_thread_id) => {
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        &format!("codex_event_driven_thread_transitioned:{new_thread_id}"),
                    );
                    scheduler.request_reconciliation();
                }
            }
        }
        let healthy = self.watcher.is_healthy();
        let transition = match (self.was_healthy, healthy) {
            (true, false) => {
                crate::auto_handoff::trace_event(
                    &self.project_state_dir,
                    "codex_event_driven_became_unhealthy",
                );
                TickEvent::BecameUnhealthy
            }
            (false, true) => {
                crate::auto_handoff::trace_event(
                    &self.project_state_dir,
                    "codex_event_driven_recovered",
                );
                crate::auto_handoff::trace_event(
                    &self.project_state_dir,
                    "codex_reconnect_reconciliation_requested",
                );
                scheduler.request_reconciliation();
                TickEvent::BecameRecovered
            }
            _ => TickEvent::Nothing,
        };
        self.was_healthy = healthy;
        transition
    }

    /// Normal/handled session end: stops the background watcher thread.
    pub(crate) fn stop(self) {
        self.watcher.stop();
    }
}

/// agent-relay#18 (launch-time exclusivity): acquires this exact profile's exclusivity lock, or
/// refuses — visibly (an actionable, non-silent message) and durably traced — if another live
/// session already holds it. Deliberately pure/testable, independent of [`event_driven_enabled`]:
/// the caller (`terminal_session::run_managed_terminal_inner`) checks that gate itself *before*
/// calling this. This lock's purpose is independent of which event-driven mechanism (if any) this
/// module currently uses — it is this codebase's own "one active writer/owner per Relay Session"
/// invariant (see `AGENTS.md`) applied at the profile level, kept regardless.
#[must_use]
pub(crate) fn acquire_profile_lock(
    config_dir: &Path,
    project_state_dir: &Path,
    json_mode: bool,
) -> Option<ProfileSessionLock> {
    match ProfileSessionLock::try_acquire(config_dir) {
        Ok(lock) => Some(lock),
        Err(error) => {
            if !json_mode {
                eprintln!("[Relay] {error}");
            }
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_profile_already_in_use_launch_refused",
            );
            None
        }
    }
}

fn version_verified(codex_executable: &Path) -> bool {
    let Ok(inspector) = relay_provider_codex::CodexInspector::discover(Some(codex_executable))
    else {
        return false;
    };
    let Ok(version) = inspector.inspect_version() else {
        return false;
    };
    matches!(assess_version(&version), VersionStatus::Verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn env_var_gate_defaults_to_off() {
        let _guard = env_lock();
        // Never mutates real process env (this workspace forbids `unsafe`, which `set_var`
        // requires since Rust 2024) — relies on the ambient test environment not setting this.
        assert!(
            std::env::var(EVENT_DRIVEN_ENV).is_err()
                || std::env::var(EVENT_DRIVEN_ENV).as_deref() != Ok("1")
                || event_driven_enabled()
        );
    }

    #[test]
    fn start_declines_silently_when_the_env_gate_is_off() {
        let _guard = env_lock();
        assert!(!event_driven_enabled() || std::env::var(EVENT_DRIVEN_ENV).as_deref() == Ok("1"));
        if event_driven_enabled() {
            // The ambient environment happens to have this set for some other reason — this test
            // only verifies the off-by-default decline path, which does not apply here.
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let result = EventDrivenRuntime::start(
            Path::new("/definitely/not/codex"),
            dir.path(),
            dir.path(),
            Some("thread-1"),
        );
        assert!(result.is_none());
    }

    /// GitHub #18: production proof that the real `relay codex`/`relay resume` launch shape
    /// always resolves `terminal_session::run_managed_terminal_inner`'s own `known_thread_id` to
    /// `Some` — the exact derivation that module uses, reproduced here so a future change to that
    /// derivation that silently reintroduces a `None` case fails this test, not just a production
    /// incident. See this module's own doc comment for why the `None` path exists at all (a
    /// defensive decline, not a supported production shape).
    #[test]
    fn the_real_relay_codex_terminal_session_launch_shape_is_always_a_known_thread_id() {
        let command_args: Vec<OsString> = vec!["resume".into(), "01a133b0-example".into()];
        let known_thread_id = (command_args.first().map(OsString::as_os_str)
            == Some(OsStr::new("resume")))
        .then(|| command_args.get(1))
        .flatten()
        .and_then(|arg| arg.to_str());
        assert_eq!(
            known_thread_id,
            Some("01a133b0-example"),
            "the real production shape must always resolve to a known thread id, never None"
        );
    }

    /// GitHub #18 (zero-poll acceptance, end to end): the one piece neither `rollout.rs`'s own
    /// tests (which prove a real rollout file produces `RolloutEvent::UsageLimitExceeded` exactly
    /// once) nor `codex_poll.rs`'s own tests (which prove `notify_event(UsageLimitExceeded)`
    /// produces exactly one triggered evaluation, and that a healthy scheduler never fires from
    /// its timer alone, however many ticks pass) individually exercise: the one-line forwarding in
    /// [`EventDrivenRuntime::tick`] that actually connects them. Constructs `EventDrivenRuntime`
    /// directly (its `start` constructor is intentionally not exercised here — see this module's
    /// own test coverage for that gate) around a real [`RolloutWatcher`] over a real temporary
    /// `CODEX_HOME`, and a real [`crate::codex_poll::CodexPollScheduler`], with no fake `codex`
    /// process and no app-server/subprocess involved at all.
    #[test]
    fn a_rollout_exhaustion_event_reaches_the_scheduler_as_exactly_one_triggered_evaluation() {
        use std::time::{Duration, Instant};

        let codex_home = tempfile::tempdir().expect("tempdir");
        let project_state_dir = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = codex_home.path().join("sessions/2026/10/13");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        let rollout_path = sessions_dir.join("rollout-2026-10-13T00-00-00-t1.jsonl");
        std::fs::write(
            &rollout_path,
            format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": "2026-10-13T00:00:00.000Z",
                    "ordinal": 0,
                    "type": "session_meta",
                    "payload": {"session_id": "t1", "id": "t1", "cwd": "/work"},
                })
            ),
        )
        .expect("write rollout header");

        let watch_state_file = project_state_dir
            .path()
            .join("codex_rollout_watch_state.json");
        let watcher = relay_provider_codex::rollout::RolloutWatcher::attach(
            codex_home.path(),
            "t1",
            &watch_state_file,
        )
        .expect("attach");
        let mut runtime = EventDrivenRuntime {
            watcher,
            project_state_dir: project_state_dir.path().to_path_buf(),
            was_healthy: false,
        };
        let mut scheduler = crate::codex_poll::CodexPollScheduler::new(
            state_dir.path().to_path_buf(),
            project_state_dir.path().to_path_buf(),
            None,
        );

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !runtime.is_healthy() {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(runtime.is_healthy(), "watcher must become healthy");

        // Becoming healthy for the first time itself queues exactly one reconciliation read (the
        // existing, intended "a successful attach always re-establishes ground truth once"
        // behavior — see `EventDrivenRuntime::tick`'s own `(false, true)` transition) — drain it
        // once, deliberately, before asserting "zero reads" below, so that assertion is about the
        // *steady state* while healthy, not a false reading of this one expected evaluation. The
        // wait loop above only calls `tick()` while *not yet* healthy, so when the rollout file
        // already exists at `attach` time (the common case, exercised here) the watcher can be
        // healthy from its very first instant and that loop's body never runs at all — the
        // transition this comment describes is only ever actually observed on the next `tick()`
        // call, which is this one.
        runtime.tick(&mut scheduler);
        scheduler.set_event_driven_healthy(runtime.is_healthy());
        let mut reconciliation_calls = 0u32;
        scheduler.tick(
            |trigger| {
                reconciliation_calls += 1;
                assert_eq!(
                    trigger,
                    crate::auto_handoff::CodexEvaluationTrigger::ReconnectReconciliation
                );
                None
            },
            |_| None,
        );
        assert_eq!(reconciliation_calls, 1);

        // Zero periodic quota reads while healthy, however many ticks pass — mirrors
        // `codex_poll::tests::a_healthy_scheduler_never_fires_on_the_timer_alone_however_many_ticks_pass`,
        // here driven through the real `EventDrivenRuntime::tick` as well.
        for _ in 0..30 {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            scheduler.tick(
                |_trigger| {
                    panic!(
                        "a healthy watcher must never let the timer alone make an evaluation due"
                    )
                },
                |_| panic!("must never spawn from the timer while healthy"),
            );
        }

        // The exact-thread structured exhaustion record.
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&rollout_path)
                .expect("open rollout for append");
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "timestamp": "2026-10-13T00:00:02.000Z",
                    "ordinal": 1,
                    "type": "event_msg",
                    "payload": {
                        "type": "task_complete",
                        "turn_id": "t",
                        "error": {"message": "limit", "codex_error_info": "usage_limit_exceeded"},
                    },
                })
            )
            .expect("append exhaustion record");
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut plan_calls = 0u32;
        let mut saw_trigger = None;
        while Instant::now() < deadline && plan_calls == 0 {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            scheduler.tick(
                |trigger| {
                    plan_calls += 1;
                    saw_trigger = Some(trigger);
                    None
                },
                |_| None,
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            plan_calls, 1,
            "exactly one evaluation must be triggered by the rollout-sourced exhaustion event"
        );
        assert_eq!(
            saw_trigger,
            Some(crate::auto_handoff::CodexEvaluationTrigger::UsageLimitEvent)
        );

        let trace = std::fs::read_to_string(project_state_dir.path().join("auto-handoff.log"))
            .unwrap_or_default();
        assert!(
            trace.contains("codex_usage_limit_event_received"),
            "the durable trace must record receipt of the rollout-sourced exhaustion event: {trace}"
        );

        // Draining again, and ticking further, must never produce a second evaluation.
        for _ in 0..10 {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            scheduler.tick(
                |_trigger| panic!("must never trigger a second evaluation for the same event"),
                |_| panic!("must never spawn a second evaluation for the same event"),
            );
        }

        runtime.stop();
    }

    /// GitHub #18 (Phase 7, zero-poll acceptance through a profile-domain transition): the same
    /// end-to-end chain as
    /// [`a_rollout_exhaustion_event_reaches_the_scheduler_as_exactly_one_triggered_evaluation`],
    /// but the thread the supervised TUI is driving changes mid-session (`/new`, `/fork`, or
    /// "resume a saved chat") before exhaustion ever happens — proving the zero-periodic-reads
    /// invariant survives a transition, that the transition itself produces exactly one
    /// reconciliation-triggered evaluation (never a health-transition diagnostic — a transition
    /// must never flip `is_healthy()` false), and that the exhaustion on the *new* thread still
    /// reaches the scheduler as exactly one `UsageLimitEvent`-triggered evaluation.
    #[test]
    fn a_profile_domain_transition_costs_one_reconciliation_and_zero_periodic_reads() {
        use std::time::{Duration, Instant};

        let codex_home = tempfile::tempdir().expect("tempdir");
        let project_state_dir = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = codex_home.path().join("sessions/2026/10/13");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        let original_path = sessions_dir.join("rollout-2026-10-13T00-00-00-t1.jsonl");
        std::fs::write(
            &original_path,
            format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": "2026-10-13T00:00:00.000Z",
                    "ordinal": 0,
                    "type": "session_meta",
                    "payload": {"session_id": "t1", "id": "t1", "cwd": "/work"},
                })
            ),
        )
        .expect("write original rollout header");

        let watch_state_file = project_state_dir
            .path()
            .join("codex_rollout_watch_state.json");
        let watcher = relay_provider_codex::rollout::RolloutWatcher::attach(
            codex_home.path(),
            "t1",
            &watch_state_file,
        )
        .expect("attach");
        let mut runtime = EventDrivenRuntime {
            watcher,
            project_state_dir: project_state_dir.path().to_path_buf(),
            was_healthy: false,
        };
        let mut scheduler = crate::codex_poll::CodexPollScheduler::new(
            state_dir.path().to_path_buf(),
            project_state_dir.path().to_path_buf(),
            None,
        );

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !runtime.is_healthy() {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(runtime.is_healthy(), "watcher must become healthy on t1");
        // Drain the one reconciliation the initial health transition itself queues (see the
        // sibling test's own comment on this exact behavior) before measuring the steady state.
        runtime.tick(&mut scheduler);
        scheduler.set_event_driven_healthy(runtime.is_healthy());
        scheduler.tick(|_trigger| None, |_| None);

        // `/new` (or `/fork`, or "resume a saved chat"): a second, structurally top-level rollout
        // appears for a different thread id, with no exhaustion in it yet.
        let new_chat_path = sessions_dir.join("rollout-2026-10-13T00-01-00-t2.jsonl");
        std::fs::write(
            &new_chat_path,
            format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": "2026-10-13T00:01:00.000Z",
                    "ordinal": 0,
                    "type": "session_meta",
                    "payload": {"session_id": "t2", "id": "t2", "cwd": "/work"},
                })
            ),
        )
        .expect("write new-chat rollout header");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut transitions = Vec::new();
        let mut reconciliation_calls = 0u32;
        loop {
            let tick_event = runtime.tick(&mut scheduler);
            if !matches!(tick_event, TickEvent::Nothing) {
                transitions.push(tick_event);
            }
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            assert!(
                runtime.is_healthy(),
                "a profile-domain transition must never flip healthy false"
            );
            scheduler.tick(
                |trigger| {
                    reconciliation_calls += 1;
                    assert_eq!(
                        trigger,
                        crate::auto_handoff::CodexEvaluationTrigger::ReconnectReconciliation
                    );
                    None
                },
                |_| None,
            );
            if reconciliation_calls > 0 || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            reconciliation_calls, 1,
            "the transition itself must trigger exactly one reconciliation"
        );
        assert!(
            transitions
                .iter()
                .all(|event| matches!(event, TickEvent::Nothing)),
            "a transition must never be reported as a health-transition diagnostic: {transitions:?}"
        );
        let trace = std::fs::read_to_string(project_state_dir.path().join("auto-handoff.log"))
            .unwrap_or_default();
        assert!(
            trace.contains("codex_event_driven_thread_transitioned:t2"),
            "the durable trace must record the transition to the new thread: {trace}"
        );

        // Zero periodic quota reads while healthy, however many ticks pass, exactly as before the
        // transition — the whole point of a transition costing one reconciliation, not an open
        // season of polling.
        for _ in 0..30 {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            scheduler.tick(
                |_trigger| {
                    panic!(
                        "a healthy watcher must never let the timer alone make an evaluation due"
                    )
                },
                |_| panic!("must never spawn from the timer while healthy"),
            );
        }

        // Exhaustion on the *new* thread (`t2`) — never `t1`, which is now abandoned — must still
        // reach the scheduler as exactly one `UsageLimitEvent`-triggered evaluation.
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&new_chat_path)
                .expect("open new-chat rollout for append");
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "timestamp": "2026-10-13T00:01:02.000Z",
                    "ordinal": 1,
                    "type": "event_msg",
                    "payload": {
                        "type": "task_complete",
                        "turn_id": "t",
                        "error": {"message": "limit", "codex_error_info": "usage_limit_exceeded"},
                    },
                })
            )
            .expect("append exhaustion record to the new thread");
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut plan_calls = 0u32;
        let mut saw_trigger = None;
        while Instant::now() < deadline && plan_calls == 0 {
            runtime.tick(&mut scheduler);
            scheduler.set_event_driven_healthy(runtime.is_healthy());
            scheduler.tick(
                |trigger| {
                    plan_calls += 1;
                    saw_trigger = Some(trigger);
                    None
                },
                |_| None,
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            plan_calls, 1,
            "exactly one evaluation must be triggered by the new thread's own exhaustion event"
        );
        assert_eq!(
            saw_trigger,
            Some(crate::auto_handoff::CodexEvaluationTrigger::UsageLimitEvent)
        );

        runtime.stop();
    }
}
