//! A thin, testable subprocess client for the `relay` CLI's stable `--json` envelope.
//!
//! This module deliberately never links against `relay-core`'s handoff/automation internals.
//! Whatever Herdr's own plugin execution model turns out to be (native binary, scripting
//! runtime, wasm — see `docs/herdr-integration.md`), a compiled adapter that shells out to the
//! `relay` binary works underneath any of them, and it is the boundary the project's own CLI
//! design already treats as stable (`OUTPUT_SCHEMA_VERSION`, per-command JSON envelopes, and
//! stable machine-readable error codes in `relay_core::Error::code`). Parsing is intentionally
//! tolerant of unknown fields (Relay may add fields later) but fails closed on anything required
//! that is missing or malformed. Human-readable CLI text is never parsed.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::HerdrIntegrationError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub timeout: Duration,
    pub output_limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Abstracts process execution so tests never spawn a real `relay` binary.
pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessResult, HerdrIntegrationError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessResult, HerdrIntegrationError> {
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| HerdrIntegrationError::RelayCommandFailed)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(HerdrIntegrationError::RelayCommandFailed)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(HerdrIntegrationError::RelayCommandFailed)?;
        let limit = spec.output_limit;
        let stdout_reader = thread::spawn(move || read_limited(stdout, limit));
        let stderr_reader = thread::spawn(move || read_limited(stderr, limit));

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => return Err(HerdrIntegrationError::RelayCommandFailed),
            }
            if started.elapsed() >= spec.timeout {
                let _ignored = child.kill();
                let _ignored = child.wait();
                let _ignored = stdout_reader.join();
                let _ignored = stderr_reader.join();
                return Err(HerdrIntegrationError::RelayCommandTimeout);
            }
            thread::sleep(Duration::from_millis(20));
        };

        let stdout = stdout_reader
            .join()
            .map_err(|_| HerdrIntegrationError::RelayCommandFailed)??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| HerdrIntegrationError::RelayCommandFailed)??;
        Ok(ProcessResult {
            success: status.success(),
            stdout,
            stderr,
        })
    }
}

fn read_limited(reader: impl Read, limit: usize) -> Result<Vec<u8>, HerdrIntegrationError> {
    let take_limit = u64::try_from(limit)
        .map_err(|_| HerdrIntegrationError::RelayMalformedOutput)?
        .saturating_add(1);
    let mut bytes = Vec::new();
    reader
        .take(take_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| HerdrIntegrationError::RelayCommandFailed)?;
    if bytes.len() > limit {
        return Err(HerdrIntegrationError::RelayMalformedOutput);
    }
    Ok(bytes)
}

/// Parses a successful `relay --json` invocation's **stdout**: `{"ok": true, "data": ...}`.
fn parse_success_envelope(stdout: &[u8]) -> Result<Value, HerdrIntegrationError> {
    let value: Value =
        serde_json::from_slice(stdout).map_err(|_| HerdrIntegrationError::RelayMalformedOutput)?;
    let ok = value
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or(HerdrIntegrationError::RelayMalformedOutput)?;
    if !ok {
        return Err(HerdrIntegrationError::RelayMalformedOutput);
    }
    value
        .get("data")
        .cloned()
        .ok_or(HerdrIntegrationError::RelayMalformedOutput)
}

/// Parses a failed `relay --json` invocation's **stderr**: `{"ok": false, "error": {...}}`. See
/// `relay-cli`'s `main()` — the error envelope is deliberately written to stderr, never stdout,
/// so a failed invocation's stdout is not meaningful JSON at all and must not be parsed.
fn parse_error_envelope(stderr: &[u8]) -> Result<(String, String), HerdrIntegrationError> {
    let value: Value =
        serde_json::from_slice(stderr).map_err(|_| HerdrIntegrationError::RelayMalformedOutput)?;
    let ok = value.get("ok").and_then(Value::as_bool);
    if ok != Some(false) {
        return Err(HerdrIntegrationError::RelayMalformedOutput);
    }
    let error = value
        .get("error")
        .ok_or(HerdrIntegrationError::RelayMalformedOutput)?;
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .ok_or(HerdrIntegrationError::RelayMalformedOutput)?
        .to_owned();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok((code, message))
}

#[derive(Clone, Debug)]
pub struct RelayClient<R: CommandRunner = SystemCommandRunner> {
    executable: PathBuf,
    runner: R,
    timeout: Duration,
}

impl RelayClient<SystemCommandRunner> {
    /// Resolves the `relay` executable the same way `relay-provider-claude` resolves `claude`:
    /// an explicit path if given, otherwise the first `relay` found on `PATH`, canonicalized and
    /// checked for unsafe permissions/ownership before ever being invoked.
    pub fn discover(requested: Option<&Path>) -> Result<Self, HerdrIntegrationError> {
        let executable = resolve_executable(requested)?;
        Ok(Self {
            executable,
            runner: SystemCommandRunner,
            timeout: DEFAULT_TIMEOUT,
        })
    }
}

impl<R: CommandRunner> RelayClient<R> {
    pub fn with_runner(executable: PathBuf, runner: R) -> Result<Self, HerdrIntegrationError> {
        Ok(Self {
            executable: validate_executable(&executable)?,
            runner,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Runs `relay <args> --json` and returns the deserialized `data` payload of a successful
    /// envelope, or `HerdrIntegrationError::RelayRefused` carrying Relay's own stable error code
    /// verbatim for a failed one. Relay's CLI writes its JSON error envelope to **stderr** (never
    /// stdout) on a non-zero exit, so this follows the same split rather than assuming stdout is
    /// always where the answer is. Never inspects human-readable text either way.
    pub fn run_json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, HerdrIntegrationError> {
        let mut arguments: Vec<OsString> = args.iter().map(OsString::from).collect();
        arguments.push(OsString::from("--json"));
        let result = self.runner.run(&ProcessSpec {
            executable: self.executable.clone(),
            arguments,
            timeout: self.timeout,
            output_limit: OUTPUT_LIMIT,
        })?;
        if result.success {
            let data = parse_success_envelope(&result.stdout)?;
            serde_json::from_value(data).map_err(|_| HerdrIntegrationError::RelayMalformedOutput)
        } else {
            let (code, message) = parse_error_envelope(&result.stderr)?;
            Err(HerdrIntegrationError::RelayRefused { code, message })
        }
    }
}

fn resolve_executable(requested: Option<&Path>) -> Result<PathBuf, HerdrIntegrationError> {
    if let Some(requested) = requested {
        return validate_executable(requested);
    }
    let path = std::env::var_os("PATH").ok_or(HerdrIntegrationError::RelayExecutableMissing)?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("relay");
        if candidate.is_file() {
            return validate_executable(&candidate);
        }
    }
    Err(HerdrIntegrationError::RelayExecutableMissing)
}

fn validate_executable(path: &Path) -> Result<PathBuf, HerdrIntegrationError> {
    let canonical =
        fs::canonicalize(path).map_err(|_| HerdrIntegrationError::RelayExecutableMissing)?;
    let metadata =
        fs::metadata(&canonical).map_err(|_| HerdrIntegrationError::RelayExecutableMissing)?;
    if !metadata.is_file() {
        return Err(HerdrIntegrationError::RelayUnsafeExecutable);
    }
    validate_executable_platform(&metadata)?;
    Ok(canonical)
}

#[cfg(unix)]
fn validate_executable_platform(metadata: &fs::Metadata) -> Result<(), HerdrIntegrationError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 || mode & 0o022 != 0 {
        return Err(HerdrIntegrationError::RelayUnsafeExecutable);
    }
    let home = std::env::var_os("HOME");
    if let Some(home) = home
        && let Ok(home_metadata) = fs::metadata(home)
    {
        let home_owner = home_metadata.uid();
        if metadata.uid() != 0 && metadata.uid() != home_owner {
            return Err(HerdrIntegrationError::RelayUnsafeExecutable);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_executable_platform(_metadata: &fs::Metadata) -> Result<(), HerdrIntegrationError> {
    Ok(())
}

/// Fixed-response runner for tests: returns a scripted sequence of results, one per call, so a
/// test can assert exactly how many subprocess invocations an action performs without spawning a
/// real `relay` binary. Not behind `cfg(test)` (deliberately public, like the standalone
/// `relay-testkit` crate's `FakeProvider`) so both this crate's unit tests and its `tests/`
/// integration tests can share it.
#[derive(Clone, Debug, Default)]
pub struct ScriptedCommandRunner {
    responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<ScriptedResponse>>>,
}

#[derive(Clone, Debug)]
pub enum ScriptedResponse {
    /// A successful invocation: `value` is written to stdout as-is (normally a full
    /// `{"ok": true, "data": ...}` envelope, but tests may also supply a malformed body).
    Success(Value),
    /// A failed invocation that produced Relay's own JSON error envelope on stderr, exactly as
    /// `relay --json` does on a refusal (`{"ok": false, "error": {"code", "message"}}`).
    RelayError { code: String, message: String },
    /// A failed invocation whose stderr was not a JSON envelope at all (a panic, a shell error,
    /// truncated output, etc.) — written to stderr verbatim.
    MalformedFailure(Vec<u8>),
    /// The subprocess itself could not be run at all (mirrors a real spawn/timeout failure).
    RunnerFailure(HerdrIntegrationError),
}

impl ScriptedCommandRunner {
    #[must_use]
    pub fn new(responses: Vec<ScriptedResponse>) -> Self {
        Self {
            responses: std::sync::Arc::new(std::sync::Mutex::new(responses.into())),
        }
    }
}

impl CommandRunner for ScriptedCommandRunner {
    fn run(&self, _spec: &ProcessSpec) -> Result<ProcessResult, HerdrIntegrationError> {
        let mut queue = self.responses.lock().expect("scripted runner lock");
        match queue.pop_front() {
            Some(ScriptedResponse::Success(value)) => Ok(ProcessResult {
                success: true,
                stdout: serde_json::to_vec(&value).expect("serialize scripted response"),
                stderr: Vec::new(),
            }),
            Some(ScriptedResponse::RelayError { code, message }) => Ok(ProcessResult {
                success: false,
                stdout: Vec::new(),
                stderr: serde_json::to_vec(&serde_json::json!({
                    "schema_version": 1,
                    "ok": false,
                    "error": { "code": code, "message": message },
                }))
                .expect("serialize scripted error"),
            }),
            Some(ScriptedResponse::MalformedFailure(bytes)) => Ok(ProcessResult {
                success: false,
                stdout: Vec::new(),
                stderr: bytes,
            }),
            Some(ScriptedResponse::RunnerFailure(error)) => Err(error),
            None => panic!("ScriptedCommandRunner ran out of scripted responses"),
        }
    }
}
