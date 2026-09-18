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
        SessionStager, SourceLiveness, TargetLauncher, TargetVerification, TransferOutcome,
        TransferredArtifact,
    },
};
use serde_json::Value;

use crate::{
    AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector, ProcessLister, SystemProcessLister,
    session_transfer,
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

/// Wraps M2A's [`SystemProcessLister`]: is the source profile's Claude process currently live?
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeSourceLiveness;

impl SourceLiveness for ClaudeSourceLiveness {
    fn is_active(&self, source_config_dir: &Path) -> Result<bool> {
        SystemProcessLister.claude_process_running_for(source_config_dir)
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
    use super::parse_verification;

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
