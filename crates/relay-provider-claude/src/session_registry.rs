//! M2B.5: queries Claude Code's own session bookkeeping (`claude agents --json`) as one signal
//! toward writer liveness. This is official, structured provider output — matching
//! docs/security.md's "prefer authenticated process-associated hooks and official JSON output"
//! — but it is not itself sufficient proof: live testing during this milestone showed it can
//! still report a session as `"state": "working"` after the underlying process was confirmed
//! killed (`kill -9`). It must always be corroborated with a direct pid+fingerprint check
//! (see `crate::handoff_adapters::ClaudeSourceLiveness`), never trusted alone.

use std::{
    io::Read,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use relay_core::{ClaudeConfigMode, Error, Result, handoff::ProcessIdentity};
use serde::Deserialize;

use crate::{AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector};

const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
const QUERY_OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct AgentSessionRecord {
    /// Present only for background sessions (the handle `claude stop` accepts). An interactive
    /// session — e.g. a person actively using the profile — has no `id`; it must still parse,
    /// because one such session would otherwise make every liveness/stop check on the whole
    /// profile fail with `malformed_provider_output`.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

/// Lists currently-active sessions (interactive and background) for one profile's config dir, as
/// Claude Code itself understands "active" — not a filesystem/process scan. Empty on a profile
/// with nothing running.
pub fn query_active_sessions(
    config_dir: &Path,
    mode: ClaudeConfigMode,
    claude_executable: Option<&Path>,
) -> Result<Vec<AgentSessionRecord>> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let executable = inspector.executable().to_path_buf();

    let mut command = std::process::Command::new(&executable);
    command
        .arg("agents")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::apply_config_mode(&mut command, mode, config_dir);
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }

    let stdout = run_bounded(command, QUERY_TIMEOUT, QUERY_OUTPUT_LIMIT)?;
    serde_json::from_slice(&stdout).map_err(|_| Error::MalformedProviderOutput)
}

/// Whether `session_id` is CURRENTLY running under some pid, straight from Claude's own
/// structured session listing (which lists interactive sessions too, not only background jobs —
/// see this module's doc comment) — never a filesystem/newest-file guess.
///
/// Used only to correct a lease whose previously recorded process is confirmed gone while the
/// exact same native conversation is genuinely still (or again) running under a new pid: for
/// example a session with no supervising Relay parent process, or one an operator resumed outside
/// Relay. It never decides *which* conversation is meant — only whether this exact, already-known
/// session id currently has a live process, and if so, which one. `None` on any doubt (the
/// listing failed, no match, or the matched pid cannot itself be confirmed alive) — never guessed.
#[must_use]
pub fn find_live_pid_for_session(
    config_dir: &Path,
    mode: ClaudeConfigMode,
    claude_executable: Option<&Path>,
    session_id: &str,
) -> Option<ProcessIdentity> {
    let sessions = query_active_sessions(config_dir, mode, claude_executable).ok()?;
    let pid = sessions
        .into_iter()
        .find(|record| record.session_id == session_id)?
        .pid?;
    let identity = ProcessIdentity::query(pid);
    identity
        .start_time_fingerprint
        .is_some()
        .then_some(identity)
}

fn run_bounded(
    mut command: std::process::Command,
    timeout: Duration,
    output_limit: usize,
) -> Result<Vec<u8>> {
    let mut child = command.spawn().map_err(|_| Error::ProviderCommandFailed)?;
    let stdout = child.stdout.take().ok_or(Error::ProviderCommandFailed)?;
    let stderr = child.stderr.take().ok_or(Error::ProviderCommandFailed)?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, output_limit));
    let stderr_reader = thread::spawn(move || read_limited(stderr, output_limit));

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| Error::ProviderCommandFailed)? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ignored = child.kill();
            let _ignored = child.wait();
            let _ignored = stdout_reader.join();
            let _ignored = stderr_reader.join();
            return Err(Error::ProviderCommandTimeout);
        }
        thread::sleep(Duration::from_millis(30));
    };

    let stdout_bytes = stdout_reader
        .join()
        .map_err(|_| Error::ProviderCommandFailed)??;
    let _discarded_stderr = stderr_reader
        .join()
        .map_err(|_| Error::ProviderCommandFailed)??;
    if !status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    Ok(stdout_bytes)
}

fn read_limited(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let take_limit = u64::try_from(limit)
        .map_err(|_| Error::MalformedProviderOutput)?
        .saturating_add(1);
    let mut bytes = Vec::new();
    reader
        .take(take_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::ProviderCommandFailed)?;
    if bytes.len() > limit {
        return Err(Error::MalformedProviderOutput);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::AgentSessionRecord;
    use relay_core::ClaudeConfigMode;

    #[test]
    fn parses_the_observed_schema_including_optional_fields() {
        let records: Vec<AgentSessionRecord> = serde_json::from_str(
            r#"[{"pid":123,"id":"abc12345","cwd":"/tmp/proj","kind":"background","startedAt":1,"sessionId":"11111111-2222-3333-4444-555555555555","name":"x","status":"idle","state":"working"}]"#,
        )
        .expect("parse");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pid, Some(123));
        assert_eq!(
            records[0].session_id,
            "11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn parses_an_interactive_session_that_has_no_background_id() {
        let records: Vec<AgentSessionRecord> = serde_json::from_str(
            r#"[{"pid":55247,"cwd":"/tmp/proj","kind":"interactive","startedAt":1,"sessionId":"11111111-2222-3333-4444-555555555555","name":"x","status":"busy"}]"#,
        )
        .expect("an interactive session record must parse");
        assert_eq!(records[0].id, None);
        assert_eq!(records[0].pid, Some(55247));
    }

    #[test]
    fn parses_an_empty_list() {
        let records: Vec<AgentSessionRecord> = serde_json::from_str("[]").expect("parse");
        assert!(records.is_empty());
    }

    #[test]
    fn tolerates_a_record_missing_the_optional_pid_field() {
        let records: Vec<AgentSessionRecord> = serde_json::from_str(
            r#"[{"id":"abc12345","cwd":"/tmp/proj","kind":"background","startedAt":1,"sessionId":"11111111-2222-3333-4444-555555555555","name":"x","state":"working"}]"#,
        )
        .expect("parse");
        assert_eq!(records[0].pid, None);
    }

    fn fake_claude(dir: &std::path::Path, listing: &str) -> std::path::PathBuf {
        let executable = dir.join("claude");
        std::fs::write(&executable, format!("#!/bin/sh\nprintf '%s' '{listing}'\n"))
            .expect("write fake claude");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
                .expect("permissions");
        }
        executable
    }

    #[test]
    fn finds_the_live_pid_for_an_exact_session_id_and_nothing_else() {
        let root = tempfile::tempdir().expect("tempdir");
        let this_pid = std::process::id();
        let listing = format!(
            r#"[{{"pid":{this_pid},"cwd":"/tmp/proj","kind":"interactive","startedAt":1,"sessionId":"11111111-2222-3333-4444-555555555555","name":"x","status":"busy"}}]"#
        );
        let claude = fake_claude(root.path(), &listing);
        let found = super::find_live_pid_for_session(
            root.path(),
            ClaudeConfigMode::Explicit,
            Some(&claude),
            "11111111-2222-3333-4444-555555555555",
        )
        .expect("a live match");
        assert_eq!(found.pid, this_pid);
        // A different session id in the same listing is never matched.
        assert!(
            super::find_live_pid_for_session(
                root.path(),
                ClaudeConfigMode::Explicit,
                Some(&claude),
                "no-such-session"
            )
            .is_none()
        );
    }

    #[test]
    fn a_listed_pid_that_is_not_actually_running_is_never_returned() {
        let root = tempfile::tempdir().expect("tempdir");
        // A pid essentially guaranteed not to exist.
        let listing = r#"[{"pid":999999,"cwd":"/tmp/proj","kind":"interactive","startedAt":1,"sessionId":"11111111-2222-3333-4444-555555555555","name":"x","status":"busy"}]"#;
        let claude = fake_claude(root.path(), listing);
        assert!(
            super::find_live_pid_for_session(
                root.path(),
                ClaudeConfigMode::Explicit,
                Some(&claude),
                "11111111-2222-3333-4444-555555555555"
            )
            .is_none()
        );
    }
}
