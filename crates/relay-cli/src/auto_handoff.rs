//! Event-driven trigger for automatic exhaustion handoff.
//!
//! Found live (M6, the first genuine exhaustion test): a real usage limit was hit, Relay's
//! `StopFailure` hook correctly recorded the evidence — and then nothing ever asked
//! `WatchCoordinator` to look at it. `relay watch run` is a one-shot evaluator that needs a
//! caller, the only in-tree caller was the Herdr `pane.agent_status_changed` plugin event (a
//! best-effort signal from a third party's own status detection; its plugin log shows no
//! invocation at all across the real exhaustion), and outside Herdr nothing called it — despite
//! the docs saying `relay claude` would.
//!
//! The moment that matters is exactly the moment Claude's own `StopFailure` hook fires, so that is
//! the trigger: when the hook records a rate-limit failure for a session Relay itself manages, it
//! starts one short-lived, detached `relay watch auto` for that project. That is *not* a daemon
//! and *not* polling of provider state — it runs only because a real limit event just happened,
//! retries a bounded number of times (the statusline snapshot that corroborates a limit can land
//! a moment after the failure), and exits. Every decision is still made by the unchanged
//! `WatchCoordinator` under the orchestration lock, so its cooldown, per-window cap,
//! known-exhausted ledger and single-writer guarantees apply exactly as before; a second
//! concurrent trigger (for example the Herdr event) just sees the lock or the cooldown.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use clap::Parser as _;
use relay_core::{ProfileName, ProfileService, ProviderKind, RelayPaths, handoff::ProjectId};
use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES;
use serde_json::Value;

use crate::{cli::Cli, output::CommandOutput, preferences::Preferences};

/// Defaults for the bounded retry: a short corroboration burst, then about two minutes of
/// ordinary re-evaluation. The burst never changes the evidence policy.
pub const DEFAULT_ATTEMPTS: u32 = 10;
pub const DEFAULT_INTERVAL_MS: u64 = 20_000;
pub const INITIAL_RETRY_DELAYS_MS: &[u64] = &[300, 1_000, 2_000];

/// Diagnostic overrides (tests, or an operator who wants a longer/shorter window).
pub const ATTEMPTS_ENV: &str = "RELAY_AUTO_WATCH_ATTEMPTS";
pub const INTERVAL_MS_ENV: &str = "RELAY_AUTO_WATCH_INTERVAL_MS";

/// The log a triggered run writes, next to the project's other state, so "did it fire and what did
/// it decide" is always answerable after the fact.
pub const LOG_FILE_NAME: &str = "auto-handoff.log";
const LOG_TRUNCATE_ABOVE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub struct AutoWatchPlan {
    pub args: Vec<OsString>,
    pub log_path: PathBuf,
    /// Sanitized wall-clock correlation point recorded by the StopFailure hook itself.
    pub triggered_unix_ms: u64,
    pub trigger: &'static str,
}

#[derive(Clone, Copy)]
struct WatchSchedule {
    attempts: u64,
    interval_ms: u64,
    trigger: &'static str,
}

/// Decides whether a `StopFailure` hook event should start an automatic-handoff evaluation, and
/// if so, with what arguments. Returns `None` — never an error, hooks must not fail Claude — for
/// anything that is not unambiguously a rate-limit failure of the session Relay currently
/// manages for that project.
#[must_use]
pub fn plan(
    paths: &RelayPaths,
    service: &ProfileService,
    preferences: &Preferences,
    config_dir: &Path,
    stdin: &[u8],
) -> Option<AutoWatchPlan> {
    let value: Value = serde_json::from_slice(stdin).ok()?;
    let object = value.as_object()?;
    if object.get("hook_event_name").and_then(Value::as_str) != Some("StopFailure") {
        return None;
    }
    let session_id = object.get("session_id").and_then(Value::as_str)?;
    let cwd = object.get("cwd").and_then(Value::as_str)?;

    // Which registered Claude profile owns this hook's config directory.
    let registered = service.list().ok()?;
    let hook_dir = std::fs::canonicalize(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    let profile = registered.iter().find(|profile| {
        profile.provider == ProviderKind::Claude
            && std::fs::canonicalize(&profile.config_dir)
                .unwrap_or_else(|_| profile.config_dir.clone())
                == hook_dir
    })?;

    // Only ever the session Relay manages for this project: the lease must name this exact
    // profile and this exact session. Any other Claude session that happens to share the
    // directory (or the same profile) is none of Relay's business.
    let project = std::fs::canonicalize(cwd).ok()?;
    let project_id = ProjectId::for_canonical_path(&project).ok()?;
    let project_state_dir = paths.project_state_dir(&project_id);
    // Which Relay session? The one holding this exact native conversation — never "the project's
    // lease", since a project can have many active sessions (even under the same profile).
    let store = relay_core::handoff::SessionStore::new(paths, project_id.clone());
    let view = store.find_by_native(session_id).ok()??;
    let lease = view.lease?;
    if lease.owner_profile != profile.name || lease.session_id != session_id {
        return None;
    }

    let fallbacks = hierarchy_without(preferences, &profile.name, |name| {
        registered.iter().any(|candidate| &candidate.name == name)
    });
    if fallbacks.is_empty() {
        return None;
    }
    Some(assemble(
        paths,
        &profile.name,
        &fallbacks,
        project,
        session_id,
        WatchSchedule {
            attempts: env_number(ATTEMPTS_ENV, u64::from(DEFAULT_ATTEMPTS)),
            interval_ms: env_number(INTERVAL_MS_ENV, DEFAULT_INTERVAL_MS),
            trigger: "stop_failure_received",
        },
        project_state_dir.join(LOG_FILE_NAME),
    ))
}

/// The plan for one poll of a supervised session whose provider has no limit event to hang a
/// trigger on (Codex): the same bounded one-shot evaluation as the hook trigger, but a single
/// attempt with no retry window, for the session the lease says this profile owns right now.
/// `None` when the profile has nothing to fall back to.
#[must_use]
pub fn plan_poll(
    paths: &RelayPaths,
    preferences: &Preferences,
    registered: &[relay_core::Profile],
    profile: &relay_core::Profile,
    lease: &relay_core::handoff::WriterLease,
    project: &Path,
) -> Option<AutoWatchPlan> {
    if lease.owner_profile != profile.name {
        return None;
    }
    let fallbacks = hierarchy_without(preferences, &profile.name, |name| {
        registered.iter().any(|candidate| &candidate.name == name)
    });
    if fallbacks.is_empty() {
        return None;
    }
    let project_id = ProjectId::for_canonical_path(project).ok()?;
    Some(assemble(
        paths,
        &profile.name,
        &fallbacks,
        project.to_path_buf(),
        &lease.session_id,
        WatchSchedule {
            attempts: 1,
            interval_ms: 0,
            trigger: "codex_poll",
        },
        paths.project_state_dir(&project_id).join(LOG_FILE_NAME),
    ))
}

/// The same one-shot evaluation [`plan_poll`] hands to a detached `relay watch auto`, but run
/// synchronously, in-process, right here. For the periodic tick a detached child is the right
/// call — the supervised terminal is still running and must not stall waiting on it. But a Codex
/// child that has just *exited* is a different situation: Codex has no "the limit was hit" event,
/// so this may be the only chance to notice real exhaustion before the session is released, and a
/// fire-and-forget detached process could easily lose that race against `release_after_exit`
/// running moments later. Mirrors `relay resume`'s own immediate Codex preflight
/// (`commands::resume::evaluate_handoff_now`), which exists for exactly the same reason.
pub fn evaluate_now(
    paths: &RelayPaths,
    preferences: &Preferences,
    registered: &[relay_core::Profile],
    profile: &relay_core::Profile,
    lease: &relay_core::handoff::WriterLease,
    project: &Path,
    claude_executable: Option<&Path>,
) -> Option<CommandOutput> {
    if lease.owner_profile != profile.name {
        return None;
    }
    let fallback = hierarchy_without(preferences, &profile.name, |name| {
        registered.iter().any(|candidate| &candidate.name == name)
    });
    if fallback.is_empty() {
        return None;
    }
    let mut argv: Vec<OsString> = vec![
        "relay".into(),
        "--json".into(),
        "--config-root".into(),
        paths.config_root().into(),
        "--state-root".into(),
        paths.state_root().into(),
        "watch".into(),
        "run".into(),
        "--profile".into(),
        profile.name.as_str().into(),
    ];
    for name in &fallback {
        argv.extend(["--fallback".into(), name.as_str().into()]);
    }
    argv.extend([
        "--project".into(),
        project.as_os_str().to_owned(),
        "--session".into(),
        lease.session_id.clone().into(),
    ]);
    if let Some(claude) = claude_executable {
        argv.extend(["--claude-executable".into(), claude.as_os_str().to_owned()]);
    }
    let inner = Cli::try_parse_from(argv).ok()?;
    crate::commands::dispatch(&inner).ok()
}

fn assemble(
    paths: &RelayPaths,
    profile: &ProfileName,
    fallbacks: &[&ProfileName],
    project: PathBuf,
    session_id: &str,
    schedule: WatchSchedule,
    log_path: PathBuf,
) -> AutoWatchPlan {
    let mut args: Vec<OsString> = vec![
        "--config-root".into(),
        paths.config_root().into(),
        "--state-root".into(),
        paths.state_root().into(),
        "watch".into(),
        "auto".into(),
        "--profile".into(),
        profile.as_str().into(),
    ];
    for fallback in fallbacks {
        args.push("--fallback".into());
        args.push(fallback.as_str().into());
    }
    args.extend([
        "--project".into(),
        project.into_os_string(),
        "--session".into(),
        session_id.into(),
        "--attempts".into(),
        schedule.attempts.to_string().into(),
        "--interval-ms".into(),
        schedule.interval_ms.to_string().into(),
    ]);
    AutoWatchPlan {
        args,
        log_path,
        triggered_unix_ms: crate::util::current_unix_ms(),
        trigger: schedule.trigger,
    }
}

/// One global ordered hierarchy (`primary > fallbacks…`), reconsidered in full at every
/// exhaustion: the current writer is sticky but is the only thing skipped, so after
/// `erika -> megan` a later limit on `megan` still considers `erika` (once her window has reset)
/// ahead of the rest. Exhausted, unhealthy and same-identity candidates are filtered by
/// `WatchCoordinator`, not here.
pub fn hierarchy_without<'a>(
    preferences: &'a Preferences,
    current: &ProfileName,
    is_registered: impl Fn(&ProfileName) -> bool,
) -> Vec<&'a ProfileName> {
    let mut ordered: Vec<&ProfileName> = Vec::new();
    for name in preferences
        .primary_profile
        .iter()
        .chain(preferences.fallback_profiles.iter())
    {
        if name != current && is_registered(name) && !ordered.contains(&name) {
            ordered.push(name);
        }
    }
    ordered
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Starts the planned run fully detached from the calling hook (and from Claude's own process
/// group, so the handoff's `claude stop` of the very session this hook belongs to cannot take
/// its own orchestrator down with it). Best-effort by design: a hook must never fail or block the
/// Claude session it runs inside.
///
/// The child's environment is scrubbed of `CLAUDE_CONFIG_DIR` and every provider credential
/// override: this process was launched *by* Claude, whose environment carries the source
/// profile's `CLAUDE_CONFIG_DIR` (and, in current Claude Code, messaging/session variables that
/// Relay's own authentication checks treat as conflicting overrides) — inherited as-is, every
/// fallback profile would be judged unhealthy and the handoff would silently never happen.
///
/// Returns the spawned [`Child`] (GitHub #13): the Claude `StopFailure` hook trigger ignores it
/// (the hook process exits right after starting it regardless), but the supervised Codex
/// terminal's [`crate::codex_poll::CodexPollScheduler`] holds it to know exactly when this one-shot
/// evaluation finishes and to guarantee at most one is ever in flight — never a second provider
/// read merely to check that. `None` only on a spawn failure that never started anything.
pub fn spawn_detached(plan: &AutoWatchPlan) -> Option<Child> {
    let program = std::env::current_exe().ok()?;
    let log = open_log(&plan.log_path, plan.triggered_unix_ms, plan.trigger)?;
    let log_err = log.try_clone().ok()?;
    let mut trace = log.try_clone().ok()?;
    let mut command = Command::new(program);
    command
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .env("RELAY_AUTO_HANDOFF_TRACE", "1")
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let child = command.spawn().ok()?;
    use std::io::Write as _;
    let _ignored = writeln!(
        trace,
        "[trace unix_ms={}] auto_watch_spawned",
        crate::util::current_unix_ms()
    );
    Some(child)
}

/// Records when the foreground supervisor observes its Claude child exit. The detached watcher
/// and the supervisor are different processes, so this is intentionally a sanitized wall-clock
/// correlation point rather than a state-machine input. Never create a log for an ordinary exit:
/// only append if this session already has an automatic-handoff trace.
pub fn record_supervisor_child_exit(state_dir: &Path) {
    use std::io::Write as _;

    let path = state_dir.join(LOG_FILE_NAME);
    if !path.is_file() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
        return;
    };
    let _ignored = writeln!(
        file,
        "[trace unix_ms={}] claude_child_exit_observed_by_supervisor",
        crate::util::current_unix_ms()
    );
}

fn open_log(path: &Path, triggered_unix_ms: u64, trigger: &str) -> Option<std::fs::File> {
    use std::io::Write as _;
    let too_large = std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_TRUNCATE_ABOVE_BYTES);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(!too_large).write(true);
    if too_large {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).ok()?;
    let _ignored = writeln!(file, "--- automatic handoff evaluation triggered ---");
    let _ignored = writeln!(file, "[trace unix_ms={triggered_unix_ms}] {trigger}");
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> ProfileName {
        ProfileName::new(value).expect("profile name")
    }

    fn preferences() -> Preferences {
        Preferences {
            primary_profile: Some(name("erika")),
            fallback_profiles: vec![name("megan"), name("codex")],
            ..Preferences::default()
        }
    }

    fn order(current: &str) -> Vec<String> {
        hierarchy_without(&preferences(), &name(current), |_| true)
            .into_iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_primary_is_reconsidered_first_after_a_handoff_away_from_it() {
        assert_eq!(order("megan"), ["erika", "codex"]);
    }

    #[test]
    fn the_primary_as_current_writer_falls_back_in_order() {
        assert_eq!(order("erika"), ["megan", "codex"]);
    }

    #[test]
    fn only_the_current_writer_is_skipped_and_unregistered_names_are_dropped() {
        let preferences = preferences();
        let listed = hierarchy_without(&preferences, &name("codex"), |candidate| {
            candidate.as_str() != "megan"
        });
        assert_eq!(listed, [&name("erika")]);
    }

    #[test]
    fn supervisor_exit_trace_only_appends_to_an_existing_automatic_handoff_log() {
        let root = tempfile::tempdir().expect("temp dir");
        record_supervisor_child_exit(root.path());
        assert!(!root.path().join(LOG_FILE_NAME).exists());

        let log = root.path().join(LOG_FILE_NAME);
        std::fs::write(&log, "existing trace\n").expect("log");
        record_supervisor_child_exit(root.path());
        let contents = std::fs::read_to_string(log).expect("read log");
        assert!(contents.contains("claude_child_exit_observed_by_supervisor"));
    }
}
