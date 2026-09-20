//! Read-only Codex CLI discovery/version/auth inspection, mirroring
//! `relay-provider-claude::inspection` for the pieces Codex actually has an equivalent of.
//!
//! Codex has no `--json` auth-status command (confirmed against the installed 0.155.0: `codex
//! login status --help` lists no such flag). The only structured, redacted machine-readable
//! status Relay found is `codex doctor --json`, whose `checks."auth.credentials".status` field
//! is `"ok"`/`"fail"` — used here as the auth signal. It never includes account identity (email,
//! account id): Codex simply does not expose one in redacted output the way Claude's `auth
//! status --json` does. Relay therefore cannot pin a Codex profile to a real account identity the
//! way it pins Claude profiles; profile isolation instead relies structurally on each profile
//! having its own private `CODEX_HOME` (see docs/security.md).

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use relay_core::{Error, Result};
use serde_json::Value;

const VERSION_OUTPUT_LIMIT: usize = 4 * 1024;
const DOCTOR_OUTPUT_LIMIT: usize = 256 * 1024;
const LOGIN_STATUS_OUTPUT_LIMIT: usize = 4 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Codex versions this crate was actually exercised against. See
/// `relay-provider-claude::capabilities::VERIFIED_VERSIONS` for the same philosophy: newer
/// patches within the same line are treated as unverified rather than rejected outright.
pub const VERIFIED_VERSIONS: &[&str] = &["0.155.0"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionStatus {
    Verified,
    /// Same CLI shape presumed compatible, but not itself live-tested.
    Unverified,
}

#[must_use]
pub fn assess_version(version: &str) -> VersionStatus {
    if VERIFIED_VERSIONS.contains(&version) {
        VersionStatus::Verified
    } else {
        VersionStatus::Unverified
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexAuthStatus {
    pub authenticated: bool,
    /// A short, non-secret human summary Codex itself prints (e.g. "no Codex credentials were
    /// found" / the doctor check's own summary text) — never a credential value.
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct CodexInspector {
    executable: PathBuf,
}

impl CodexInspector {
    pub fn discover(requested: Option<&Path>) -> Result<Self> {
        Ok(Self {
            executable: resolve_executable(requested)?,
        })
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn inspect_version(&self) -> Result<String> {
        let output = run(
            &self.executable,
            &["--version"],
            &[],
            None,
            VERSION_OUTPUT_LIMIT,
        )?;
        if !output.success {
            return Err(Error::ProviderCommandFailed);
        }
        parse_version(&output.stdout)
    }

    /// `CODEX_HOME=config_dir codex doctor --json`. Never inspects `auth.json` directly — only
    /// this redacted, provider-owned report.
    pub fn inspect_auth_status(&self, config_dir: &Path) -> Result<CodexAuthStatus> {
        let output = run(
            &self.executable,
            &["doctor", "--json"],
            &[("CODEX_HOME", config_dir)],
            None,
            DOCTOR_OUTPUT_LIMIT,
        )?;
        parse_doctor_auth(&output.stdout)
    }

    /// `CODEX_HOME=config_dir codex login status`. Human text only (no `--json` variant exists
    /// on the installed version) — parsed defensively: only ever used to corroborate
    /// [`Self::inspect_auth_status`], never as the sole source of truth.
    pub fn login_status_text(&self, config_dir: &Path) -> Result<String> {
        let output = run(
            &self.executable,
            &["login", "status"],
            &[("CODEX_HOME", config_dir)],
            None,
            LOGIN_STATUS_OUTPUT_LIMIT,
        )?;
        String::from_utf8(output.stdout).map_err(|_| Error::MalformedProviderOutput)
    }
}

pub(crate) struct CommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
}

/// `stdin_payload`, when set, is written on its own thread and then the handle is dropped
/// (closing stdin) — see `relay-provider-claude::handoff_adapters::run_with_timeout` for why
/// this must never happen on the thread also draining stdout.
pub(crate) fn run(
    executable: &Path,
    args: &[&str],
    env: &[(&str, &Path)],
    stdin_payload: Option<&[u8]>,
    output_limit: usize,
) -> Result<CommandOutput> {
    let mut command = Command::new(executable);
    command.args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    for variable in crate::AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    command
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|_| Error::ProviderCommandFailed)?;
    let stdin_writer = stdin_payload.map(|payload| {
        let mut stdin = child.stdin.take().expect("stdin was configured as piped");
        let payload = payload.to_vec();
        thread::spawn(move || {
            use std::io::Write;
            let _ignored = stdin.write_all(&payload);
        })
    });
    let stdout = child.stdout.take().ok_or(Error::ProviderCommandFailed)?;
    let stderr = child.stderr.take().ok_or(Error::ProviderCommandFailed)?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, output_limit));
    let stderr_reader = thread::spawn(move || read_limited(stderr, output_limit));

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| Error::ProviderCommandFailed)? {
            break status;
        }
        if started.elapsed() >= COMMAND_TIMEOUT {
            let _ignored = child.kill();
            let _ignored = child.wait();
            let _ignored = stdout_reader.join();
            let _ignored = stderr_reader.join();
            if let Some(writer) = stdin_writer {
                let _ignored = writer.join();
            }
            return Err(Error::ProviderCommandTimeout);
        }
        thread::sleep(Duration::from_millis(20));
    };
    if let Some(writer) = stdin_writer {
        let _ignored = writer.join();
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| Error::ProviderCommandFailed)??;
    let _discarded_stderr = stderr_reader
        .join()
        .map_err(|_| Error::ProviderCommandFailed)??;
    Ok(CommandOutput {
        success: status.success(),
        stdout,
    })
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

/// Parses `codex --version` output, observed as `codex-cli 0.155.0`. Strict: only a bare
/// `MAJOR.MINOR.PATCH` is accepted as the version, matching the Claude parser's philosophy of
/// never guessing at an unfamiliar shape.
fn parse_version(stdout: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(stdout);
    for token in text.split_whitespace() {
        if token.split('.').count() == 3
            && token
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Ok(token.to_owned());
        }
    }
    Err(Error::MalformedProviderOutput)
}

fn parse_doctor_auth(stdout: &[u8]) -> Result<CodexAuthStatus> {
    let value: Value =
        serde_json::from_slice(stdout).map_err(|_| Error::MalformedProviderOutput)?;
    let check = value
        .get("checks")
        .and_then(|checks| checks.get("auth.credentials"))
        .ok_or(Error::MalformedProviderOutput)?;
    let status = check
        .get("status")
        .and_then(Value::as_str)
        .ok_or(Error::MalformedProviderOutput)?;
    let summary = check
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(CodexAuthStatus {
        authenticated: status == "ok",
        summary,
    })
}

fn resolve_executable(requested: Option<&Path>) -> Result<PathBuf> {
    if let Some(requested) = requested {
        return validate_executable(requested);
    }
    let path = std::env::var_os("PATH").ok_or(Error::ProviderExecutableMissing)?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("codex");
        if candidate.is_file() {
            return validate_executable(&candidate);
        }
    }
    Err(Error::ProviderExecutableMissing)
}

fn validate_executable(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|_| Error::ProviderExecutableMissing)?;
    let metadata = fs::metadata(&canonical).map_err(|_| Error::ProviderExecutableMissing)?;
    if !metadata.is_file() {
        return Err(Error::UnsafeProviderExecutable);
    }
    validate_executable_platform(&metadata)?;
    Ok(canonical)
}

#[cfg(unix)]
fn validate_executable_platform(metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 || mode & 0o022 != 0 {
        return Err(Error::UnsafeProviderExecutable);
    }
    let home = std::env::var_os("HOME").ok_or(Error::MissingEnvironment("HOME"))?;
    let home_owner = fs::metadata(home)
        .map_err(|_| Error::UnsafeProviderExecutable)?
        .uid();
    if metadata.uid() != 0 && metadata.uid() != home_owner {
        return Err(Error::UnsafeProviderExecutable);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_executable_platform(_metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{VersionStatus, assess_version, parse_doctor_auth, parse_version};
    use relay_core::Error;

    #[test]
    fn version_parser_accepts_the_observed_shape() {
        assert_eq!(
            parse_version(b"codex-cli 0.155.0\n").expect("version"),
            "0.155.0"
        );
        assert!(matches!(
            parse_version(b"not a version"),
            Err(Error::MalformedProviderOutput)
        ));
    }

    #[test]
    fn known_version_is_verified_and_others_are_unverified_not_rejected() {
        assert_eq!(assess_version("0.155.0"), VersionStatus::Verified);
        assert_eq!(assess_version("0.156.0"), VersionStatus::Unverified);
    }

    #[test]
    fn doctor_json_reports_authenticated_from_the_ok_status() {
        let json = br#"{"checks":{"auth.credentials":{"status":"ok","summary":"logged in"}}}"#;
        let status = parse_doctor_auth(json).expect("parse");
        assert!(status.authenticated);
        assert_eq!(status.summary, "logged in");
    }

    #[test]
    fn doctor_json_reports_unauthenticated_from_the_fail_status() {
        let json =
            br#"{"checks":{"auth.credentials":{"status":"fail","summary":"no Codex credentials were found"}}}"#;
        let status = parse_doctor_auth(json).expect("parse");
        assert!(!status.authenticated);
        assert_eq!(status.summary, "no Codex credentials were found");
    }

    #[test]
    fn malformed_doctor_output_fails_closed() {
        let error = parse_doctor_auth(b"not json").expect_err("must fail closed");
        assert_eq!(error.code(), "malformed_provider_output");
    }

    #[test]
    fn doctor_output_missing_the_auth_check_fails_closed() {
        let error = parse_doctor_auth(br#"{"checks":{}}"#).expect_err("must fail closed");
        assert_eq!(error.code(), "malformed_provider_output");
    }
}
