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

use relay_core::{Error, Result};
use serde::Deserialize;

use crate::{AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector};

const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
const QUERY_OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct AgentSessionRecord {
    pub id: String,
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
    claude_executable: Option<&Path>,
) -> Result<Vec<AgentSessionRecord>> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let executable = inspector.executable().to_path_buf();

    let mut command = std::process::Command::new(&executable);
    command
        .arg("agents")
        .arg("--json")
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }

    let stdout = run_bounded(command, QUERY_TIMEOUT, QUERY_OUTPUT_LIMIT)?;
    serde_json::from_slice(&stdout).map_err(|_| Error::MalformedProviderOutput)
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
}
