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
    process::{Command, Stdio},
};

use relay_core::{
    ProfileName, ProfileService, ProviderKind, RelayPaths,
    handoff::{LeaseStore, ProjectId},
};
use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES;
use serde_json::Value;

use crate::preferences::Preferences;

/// Defaults for the bounded retry: about two minutes of re-evaluation, each a purely local read.
pub const DEFAULT_ATTEMPTS: u32 = 7;
pub const DEFAULT_INTERVAL_MS: u64 = 20_000;

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
    let lease = LeaseStore::at_path(project_state_dir.join("lease.json"))
        .load()
        .ok()??;
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
        (
            env_number(ATTEMPTS_ENV, u64::from(DEFAULT_ATTEMPTS)),
            env_number(INTERVAL_MS_ENV, DEFAULT_INTERVAL_MS),
        ),
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
        (1, 0),
        paths.project_state_dir(&project_id).join(LOG_FILE_NAME),
    ))
}

fn assemble(
    paths: &RelayPaths,
    profile: &ProfileName,
    fallbacks: &[&ProfileName],
    project: PathBuf,
    session_id: &str,
    (attempts, interval_ms): (u64, u64),
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
        attempts.to_string().into(),
        "--interval-ms".into(),
        interval_ms.to_string().into(),
    ]);
    AutoWatchPlan { args, log_path }
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
pub fn spawn_detached(plan: &AutoWatchPlan) {
    let Ok(program) = std::env::current_exe() else {
        return;
    };
    let Some(log) = open_log(&plan.log_path) else {
        return;
    };
    let Ok(log_err) = log.try_clone() else {
        return;
    };
    let mut command = Command::new(program);
    command
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let _ignored = command.spawn();
}

fn open_log(path: &Path) -> Option<std::fs::File> {
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
}
