//! M2B: real Claude-aware implementations of `relay_core::handoff`'s provider-neutral ports.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use relay_core::{
    Error, Result,
    handoff::{
        LivenessVerdict, ProcessIdentity, SessionStager, SourceLiveness, TargetLauncher,
        TargetVerification, TransferOutcome, TransferredArtifact,
    },
};
use serde_json::Value;

use crate::{
    AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector, ProcessLister, SystemProcessLister,
    session_registry, session_transfer,
};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(180);
const LAUNCH_OUTPUT_LIMIT: usize = 1024 * 1024;

/// A fixed, harmless canary prompt: verification only confirms the target process started and
/// resumed the right session, it never performs the operator's own next task. Keeping the
/// verification turn's content fixed and content-free also keeps it out of docs/security.md's
/// forbidden-in-diagnostics transcript-content category — nothing operator- or project-specific
/// is ever sent here.
const VERIFICATION_PROMPT: &str =
    "Reply with exactly this text and nothing else: RELAY_HANDOFF_VERIFIED";

/// The M2B.5 writer-liveness check: primarily an exact pid + start-time fingerprint check (never
/// text-matching a process list), corroborated by Claude Code's own session bookkeeping
/// (`claude agents --json`) to discover the pid in the first place and to detect sessions Relay
/// never launched. Live testing during this milestone proved `agents --json` alone is
/// insufficient — it kept reporting `"state": "working"` for a session whose process had already
/// been `kill -9`'d — so the pid+fingerprint check is always the final arbiter when a pid is
/// available; `agents --json`'s claim is only trusted outright when no pid can be obtained at
/// all (nothing to verify against), which fails closed (treated as active) rather than assumed
/// safe.
#[derive(Clone, Debug, Default)]
pub struct ClaudeSourceLiveness {
    claude_executable: Option<PathBuf>,
}

impl ClaudeSourceLiveness {
    #[must_use]
    pub const fn new(claude_executable: Option<PathBuf>) -> Self {
        Self { claude_executable }
    }
}

impl SourceLiveness for ClaudeSourceLiveness {
    fn check(
        &self,
        source_config_dir: &Path,
        project_dir: &Path,
        expected_session_id: &str,
        recorded_owner: Option<&ProcessIdentity>,
    ) -> Result<LivenessVerdict> {
        let sessions = match session_registry::query_active_sessions(
            source_config_dir,
            self.claude_executable.as_deref(),
        ) {
            Ok(sessions) => sessions,
            Err(_) => {
                // `claude agents --json` itself unavailable (older Claude build, or the
                // command failed): fall back to the M2A ps-scan alone as a diagnostic-only
                // signal, rather than failing the whole check outright.
                let ps_active =
                    SystemProcessLister.claude_process_running_for(source_config_dir)?;
                return Ok(LivenessVerdict {
                    active: ps_active,
                    untracked_session_ids: Vec::new(),
                });
            }
        };

        let project_dir_text = project_dir.to_string_lossy();
        let in_this_project = |record: &&session_registry::AgentSessionRecord| {
            record
                .cwd
                .as_deref()
                .is_none_or(|cwd| cwd == project_dir_text)
        };

        let matching = sessions
            .iter()
            .filter(in_this_project)
            .find(|record| record.session_id == expected_session_id);
        let untracked: Vec<String> = sessions
            .iter()
            .filter(in_this_project)
            .filter(|record| record.session_id != expected_session_id)
            .map(|record| record.session_id.clone())
            .collect();

        let active = match matching {
            None => false,
            Some(record) => {
                let pid = record.pid.or_else(|| recorded_owner.map(|owner| owner.pid));
                match pid {
                    None => true, // nothing to verify against: fail closed
                    Some(pid) => {
                        let identity = match recorded_owner {
                            Some(owner) if owner.pid == pid => owner.clone(),
                            _ => ProcessIdentity::query(pid),
                        };
                        // None (identity unestablishable) also fails closed: assume active.
                        identity.is_still_the_same_process().unwrap_or(true)
                    }
                }
            }
        };
        Ok(LivenessVerdict {
            active,
            untracked_session_ids: untracked,
        })
    }
}

/// Wraps M2A's [`session_transfer::stage_transfer`] behind the core-level [`SessionStager`] port.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeSessionStager;

impl SessionStager for ClaudeSessionStager {
    fn stage(
        &self,
        source_config_dir: &Path,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<TransferOutcome> {
        let report = session_transfer::stage_transfer(
            &SystemProcessLister,
            source_config_dir,
            target_config_dir,
            project_dir,
            session_id,
        )?;
        Ok(TransferOutcome {
            artifacts: report
                .artifacts
                .into_iter()
                .map(|artifact| TransferredArtifact {
                    relative_path: artifact.relative_path,
                    sha256: artifact.sha256,
                    size_bytes: artifact.size_bytes,
                })
                .collect(),
        })
    }
}

/// Launches `claude -p --resume <session-id>` under the target profile's `CLAUDE_CONFIG_DIR`,
/// in the project directory, and verifies it actually resumed the expected session rather than
/// starting a fresh one. This is the only place in M2B that spends real API usage; it sends a
/// fixed, content-free canary prompt (see [`VERIFICATION_PROMPT`]) — never the operator's own
/// task, which stays a separate, explicit step after the transaction completes.
#[derive(Clone, Debug, Default)]
pub struct ClaudeTargetLauncher {
    claude_executable: Option<PathBuf>,
}

impl ClaudeTargetLauncher {
    #[must_use]
    pub const fn new(claude_executable: Option<PathBuf>) -> Self {
        Self { claude_executable }
    }
}

impl TargetLauncher for ClaudeTargetLauncher {
    fn launch_and_verify(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<TargetVerification> {
        session_transfer::validate_session_id(session_id)?;
        let inspector = ClaudeInspector::discover(self.claude_executable.as_deref())?;
        let executable = inspector.executable().to_path_buf();

        let mut command = std::process::Command::new(&executable);
        command
            .current_dir(project_dir)
            .arg("-p")
            .arg("--resume")
            .arg(session_id)
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("--output-format")
            .arg("json")
            .arg(VERIFICATION_PROMPT)
            .env("CLAUDE_CONFIG_DIR", target_config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
            command.env_remove(variable);
        }

        let stdout = run_with_timeout(command, LAUNCH_TIMEOUT, LAUNCH_OUTPUT_LIMIT)?;
        parse_verification(&stdout)
    }
}

fn run_with_timeout(
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
        thread::sleep(Duration::from_millis(50));
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

const LAUNCH_BG_TIMEOUT: Duration = Duration::from_secs(30);
const LAUNCH_BG_OUTPUT_LIMIT: usize = 64 * 1024;
const AGENTS_JSON_POLL_ATTEMPTS: u32 = 10;
const AGENTS_JSON_POLL_DELAY: Duration = Duration::from_millis(300);
/// The session record itself appears quickly, but `agents --json` populates its `pid` field
/// slightly later still (observed live during M2B.5 development) — poll longer specifically for
/// the pid, since a lease without one cannot be verified against a real process later.
const PID_POLL_ATTEMPTS: u32 = 20;
const PID_POLL_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchedWriter {
    pub session_id: String,
    pub pid: Option<u32>,
    pub provider_handle: String,
}

/// Actually launches Claude as a Relay-managed writer: spawns `claude --bg`, extracts the short
/// job id Claude prints (validated against a strict pattern — this is Relay's own direct child's
/// stdout, not untrusted external text, but it is still only ever used as a lookup key into the
/// authoritative `claude agents --json` record, never trusted as safety-relevant data itself),
/// then polls that structured listing for the real session id and pid.
pub fn launch_background(
    config_dir: &Path,
    project_dir: &Path,
    prompt: &str,
    claude_executable: Option<&Path>,
) -> Result<LaunchedWriter> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let executable = inspector.executable().to_path_buf();

    let mut command = std::process::Command::new(&executable);
    command
        .current_dir(project_dir)
        .arg("--bg")
        .arg("--permission-mode")
        .arg("acceptEdits")
        .arg(prompt)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let stdout = run_with_timeout(command, LAUNCH_BG_TIMEOUT, LAUNCH_BG_OUTPUT_LIMIT)?;
    let text = String::from_utf8_lossy(&stdout);
    let provider_handle = parse_background_job_id(&text)?;

    let mut found_session_id: Option<String> = None;
    for attempt in 0..AGENTS_JSON_POLL_ATTEMPTS {
        let sessions = session_registry::query_active_sessions(config_dir, claude_executable)?;
        if let Some(record) = sessions.iter().find(|record| record.id == provider_handle) {
            found_session_id = Some(record.session_id.clone());
            if let Some(pid) = record.pid {
                return Ok(LaunchedWriter {
                    session_id: record.session_id.clone(),
                    pid: Some(pid),
                    provider_handle,
                });
            }
            break;
        }
        if attempt + 1 < AGENTS_JSON_POLL_ATTEMPTS {
            thread::sleep(AGENTS_JSON_POLL_DELAY);
        }
    }
    let Some(session_id) = found_session_id else {
        return Err(Error::MalformedProviderOutput);
    };

    // The record exists but its pid had not populated yet: poll specifically for that, longer.
    for attempt in 0..PID_POLL_ATTEMPTS {
        let sessions = session_registry::query_active_sessions(config_dir, claude_executable)?;
        if let Some(pid) = sessions
            .iter()
            .find(|record| record.id == provider_handle)
            .and_then(|record| record.pid)
        {
            return Ok(LaunchedWriter {
                session_id,
                pid: Some(pid),
                provider_handle,
            });
        }
        if attempt + 1 < PID_POLL_ATTEMPTS {
            thread::sleep(PID_POLL_DELAY);
        }
    }
    // Session is real and tracked even though a pid never appeared in time; the caller can
    // still record the lease (liveness checks fail closed without a pid, which is the correct,
    // conservative behavior rather than losing the launch outright).
    Ok(LaunchedWriter {
        session_id,
        pid: None,
        provider_handle,
    })
}

/// Parses Claude's own `backgrounded · <id>` line. Strict: only ASCII lowercase-hex ids of a
/// bounded length are accepted, so this can never become a path/argument-injection vector even
/// though it is Relay's own child's output.
fn parse_background_job_id(text: &str) -> Result<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(id) = trimmed.strip_prefix("backgrounded").map(str::trim) {
            let id = id.trim_start_matches('·').trim();
            let valid =
                !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
            if valid {
                return Ok(id.to_owned());
            }
        }
    }
    Err(Error::MalformedProviderOutput)
}

fn parse_verification(stdout: &[u8]) -> Result<TargetVerification> {
    let value: Value =
        serde_json::from_slice(stdout).map_err(|_| Error::MalformedProviderOutput)?;
    let object = value.as_object().ok_or(Error::MalformedProviderOutput)?;
    let session_id = object
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or(Error::MalformedProviderOutput)?
        .to_owned();
    let is_error = object
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(TargetVerification {
        target_session_id: session_id,
        started_successfully: !is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_background_job_id, parse_verification};

    #[test]
    fn parses_the_observed_backgrounded_line() {
        let text = "Starting background service…\nbackgrounded · ce92abd4\n  claude agents             list sessions\n";
        assert_eq!(parse_background_job_id(text).expect("parse"), "ce92abd4");
    }

    #[test]
    fn rejects_output_without_a_backgrounded_line() {
        let error = parse_background_job_id("some unrelated output\n")
            .expect_err("must fail closed without the expected line");
        assert_eq!(error.code(), "malformed_provider_output");
    }

    #[test]
    fn rejects_a_suspicious_id_that_is_not_plain_hex() {
        let error = parse_background_job_id("backgrounded · ../../etc/passwd\n")
            .expect_err("must reject a non-hex id");
        assert_eq!(error.code(), "malformed_provider_output");
    }

    #[test]
    fn successful_output_is_parsed_as_verified() {
        let verification = parse_verification(
            br#"{"session_id":"8586fe71-395b-4449-b973-78011d561fed","is_error":false,"subtype":"success"}"#,
        )
        .expect("parse");
        assert_eq!(
            verification.target_session_id,
            "8586fe71-395b-4449-b973-78011d561fed"
        );
        assert!(verification.started_successfully);
    }

    #[test]
    fn an_error_result_is_reported_as_not_started_successfully() {
        let verification = parse_verification(
            br#"{"session_id":"8586fe71-395b-4449-b973-78011d561fed","is_error":true}"#,
        )
        .expect("parse");
        assert!(!verification.started_successfully);
    }

    #[test]
    fn malformed_output_fails_closed() {
        let error = parse_verification(b"not json").expect_err("must fail closed");
        assert_eq!(error.code(), "malformed_provider_output");
    }

    #[test]
    fn missing_session_id_fails_closed() {
        let error = parse_verification(br#"{"is_error":false}"#).expect_err("must fail closed");
        assert_eq!(error.code(), "malformed_provider_output");
    }
}
