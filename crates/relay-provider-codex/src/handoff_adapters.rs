//! M6: Codex-aware implementations of `relay_core::handoff`'s provider-neutral ports.
//!
//! Two structural differences from `relay-provider-claude::handoff_adapters` that shape
//! everything here:
//!
//! 1. Codex exposes no non-interactive, structured "list active sessions for this profile"
//!    command (`codex agents` is TUI-only; confirmed against 0.155.0's `--help`). Liveness and
//!    stop therefore lean primarily on the pid + start-time fingerprint Relay itself recorded at
//!    spawn time (`ProcessIdentity`, the port docs' "strongest available signal"), falling back
//!    to a `ps`-scan keyed on the `CODEX_HOME` environment token only when no recorded identity
//!    exists. This is a real, documented limitation relative to Claude's daemon-backed
//!    bookkeeping — see M6_FINAL_REPORT.md.
//! 2. Codex is never a `SESSION_CONTINUATION` target (no documented cross-`CODEX_HOME` thread
//!    transfer) — [`CodexTargetLauncher`] only implements
//!    [`relay_core::handoff::LaunchDirective::Bootstrap`].

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use relay_core::{
    Error, Result,
    handoff::{
        LaunchDirective, LivenessVerdict, ProcessIdentity, SessionStopper, SourceLiveness,
        TargetLauncher, TargetVerification, render_bootstrap_prompt,
    },
};
use serde_json::Value;

use crate::{AUTHENTICATION_OVERRIDE_VARIABLES, CodexInspector};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(300);
const LAUNCH_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const ORPHAN_TERM_GRACE: Duration = Duration::from_secs(5);
const ORPHAN_POLL_DELAY: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Default)]
pub struct CodexSourceLiveness;

impl SourceLiveness for CodexSourceLiveness {
    fn check(
        &self,
        source_config_dir: &Path,
        _project_dir: &Path,
        _expected_session_id: &str,
        recorded_owner: Option<&ProcessIdentity>,
    ) -> Result<LivenessVerdict> {
        if let Some(owner) = recorded_owner
            && let Some(definite) = owner.is_still_the_same_process()
        {
            return Ok(LivenessVerdict {
                active: definite,
                // Codex has no structured cross-check for OTHER untracked processes under this
                // profile (see module doc); this is a known gap, not a claim of certainty.
                untracked_session_ids: Vec::new(),
            });
        }
        Ok(LivenessVerdict {
            active: codex_process_running_for(source_config_dir)?,
            untracked_session_ids: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct CodexSessionStopper;

impl SessionStopper for CodexSessionStopper {
    fn stop_and_verify(
        &self,
        source_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
        recorded_owner: Option<&ProcessIdentity>,
    ) -> Result<()> {
        if let Some(owner) = recorded_owner {
            return terminate_verified_process(owner, ORPHAN_TERM_GRACE);
        }
        // No recorded identity: the coarsest available fallback is stopping every process whose
        // environment carries this exact profile's CODEX_HOME token. Acceptable because this
        // CODEX_HOME is Relay-owned (see docs/security.md) — nothing else is expected to run
        // under it.
        for pid in codex_pids_for(source_config_dir)? {
            terminate_verified_process(&ProcessIdentity::query(pid), ORPHAN_TERM_GRACE)?;
        }
        Ok(())
    }

    fn stop_orphan_target(
        &self,
        _target_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
        orphan: &ProcessIdentity,
    ) -> Result<()> {
        terminate_verified_process(orphan, ORPHAN_TERM_GRACE)
    }

    fn stop_unrecorded_targets(
        &self,
        target_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> Result<()> {
        for pid in codex_pids_for(target_config_dir)? {
            terminate_verified_process(&ProcessIdentity::query(pid), ORPHAN_TERM_GRACE)?;
        }
        Ok(())
    }
}

fn codex_process_running_for(config_dir: &Path) -> Result<bool> {
    Ok(!codex_pids_for(config_dir)?.is_empty())
}

/// Pure-ish (one `ps` shellout): pids of any process whose environment carries
/// `CODEX_HOME=<config_dir>`, from `ps -Eww -axo pid=,command=` text. Mirrors
/// `relay_provider_claude::handoff_adapters::matching_target_pids`'s whole-token matching so a
/// path that merely shares a prefix can never match.
fn codex_pids_for(config_dir: &Path) -> Result<Vec<u32>> {
    let output = Command::new("ps")
        .args(["-Eww", "-axo", "pid=,command="])
        .output()
        .map_err(|_| Error::ProviderCommandFailed)?;
    if !output.status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let needle = format!("CODEX_HOME={}", config_dir.display());
    let own_pid = std::process::id();
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let pid: u32 = tokens.next()?.parse().ok()?;
            let rest: Vec<&str> = tokens.collect();
            (pid != own_pid && rest.iter().any(|token| *token == needle)).then_some(pid)
        })
        .collect())
}

/// Identical safety contract to
/// `relay_provider_claude::handoff_adapters::terminate_verified_process`: the pid + start-time
/// fingerprint is re-confirmed immediately before every signal, so a reused pid is never
/// signalled and an unverifiable identity fails closed rather than guessing.
fn terminate_verified_process(process: &ProcessIdentity, grace: Duration) -> Result<()> {
    match process.is_still_the_same_process() {
        Some(false) => return Ok(()),
        None => {
            return Err(Error::StopNotVerified(format!(
                "cannot confirm the identity of recorded pid {}",
                process.pid
            )));
        }
        Some(true) => {}
    }
    for signal in ["-TERM", "-KILL"] {
        if process.is_still_the_same_process() != Some(true) {
            break;
        }
        let _ignored = Command::new("kill")
            .arg(signal)
            .arg(process.pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if process.is_still_the_same_process() == Some(false) {
                return Ok(());
            }
            thread::sleep(ORPHAN_POLL_DELAY);
        }
    }
    if process.is_still_the_same_process() == Some(false) {
        Ok(())
    } else {
        Err(Error::StopNotVerified(format!(
            "recorded pid {} survived SIGTERM and SIGKILL",
            process.pid
        )))
    }
}

/// `STATE_CONTINUATION` only: launches `codex exec --json` under the target profile's
/// `CODEX_HOME`, in the project directory, with the rendered bootstrap prompt on stdin (never
/// argv — see [`render_bootstrap_prompt`] and the M6 spec's prompt-transport section). Parses
/// **stdout only** as NDJSON; stderr (tracing logs, "Reading additional input from stdin…") is
/// collected separately and never merged into the parser, matching the spec's explicit warning.
///
/// The exact terminal event names below (`turn.completed` / `turn.failed`) are Relay's best
/// understanding from Codex 0.155.0's observed `thread.started`/`turn.started`/`item.completed`/
/// `error`/`turn.failed` events during an UNAUTHENTICATED probe (real network calls were never
/// made — see M6_FINAL_REPORT.md's Codex CLI research notes); a real authenticated run has not
/// yet been observed at the time this was written, so this parsing is marked unvalidated in the
/// final report pending live confirmation.
#[derive(Clone, Debug, Default)]
pub struct CodexTargetLauncher {
    codex_executable: Option<PathBuf>,
}

impl CodexTargetLauncher {
    #[must_use]
    pub const fn new(codex_executable: Option<PathBuf>) -> Self {
        Self { codex_executable }
    }
}

impl TargetLauncher for CodexTargetLauncher {
    fn launch_and_verify(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        directive: &LaunchDirective<'_>,
        on_started: &mut dyn FnMut(Option<ProcessIdentity>) -> Result<()>,
    ) -> Result<TargetVerification> {
        let LaunchDirective::Bootstrap { bundle } = directive else {
            // Codex is never a SESSION_CONTINUATION target — see module doc and
            // PROVIDER_CAPABILITIES.native_session_transfer.
            return Err(Error::ProviderUnsupported);
        };
        let inspector = CodexInspector::discover(self.codex_executable.as_deref())?;
        let executable = inspector.executable().to_path_buf();
        let prompt = render_bootstrap_prompt(bundle);

        let mut command = Command::new(&executable);
        command
            .current_dir(project_dir)
            .arg("exec")
            .arg("--json")
            .arg("--skip-git-repo-check")
            .env("CODEX_HOME", target_config_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
            command.env_remove(variable);
        }

        let stdout = run_with_timeout(
            command,
            LAUNCH_TIMEOUT,
            LAUNCH_OUTPUT_LIMIT,
            prompt.as_bytes(),
            on_started,
        )?;
        parse_exec_json_stream(&stdout)
    }
}

fn run_with_timeout(
    mut command: Command,
    timeout: Duration,
    output_limit: usize,
    stdin_payload: &[u8],
    on_spawned: &mut dyn FnMut(Option<ProcessIdentity>) -> Result<()>,
) -> Result<Vec<u8>> {
    let mut child = command.spawn().map_err(|_| Error::ProviderCommandFailed)?;
    on_spawned(Some(ProcessIdentity::query(child.id())))?;
    let mut stdin = child.stdin.take().expect("stdin was configured as piped");
    let payload = stdin_payload.to_vec();
    let stdin_writer = thread::spawn(move || {
        use std::io::Write;
        let _ignored = stdin.write_all(&payload);
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
        if started.elapsed() >= timeout {
            let _ignored = child.kill();
            let _ignored = child.wait();
            let _ignored = stdout_reader.join();
            let _ignored = stderr_reader.join();
            let _ignored = stdin_writer.join();
            return Err(Error::ProviderCommandTimeout);
        }
        thread::sleep(Duration::from_millis(50));
    };
    let _ignored = stdin_writer.join();

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

/// Parses `codex exec --json`'s NDJSON stdout ONLY. A malformed line is skipped (Codex's NDJSON
/// stream may include event shapes Relay does not otherwise care about); a stream that never
/// reports a `thread.started` id, or that reports `turn.failed`/a terminal `error`, fails closed.
fn parse_exec_json_stream(stdout: &[u8]) -> Result<TargetVerification> {
    let text = String::from_utf8_lossy(stdout);
    let mut thread_id: Option<String> = None;
    let mut turn_completed = false;
    let mut turn_failed = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                if let Some(id) = value.get("thread_id").and_then(Value::as_str) {
                    thread_id = Some(id.to_owned());
                }
            }
            Some("turn.completed") => turn_completed = true,
            Some("turn.failed") => turn_failed = true,
            _ => {}
        }
    }
    match thread_id {
        Some(thread_id) if turn_completed && !turn_failed => Ok(TargetVerification {
            target_session_id: thread_id,
            started_successfully: true,
        }),
        Some(thread_id) => Ok(TargetVerification {
            target_session_id: thread_id,
            started_successfully: false,
        }),
        None => Err(Error::MalformedProviderOutput),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_exec_json_stream;

    #[test]
    fn a_completed_turn_with_a_thread_id_verifies_successfully() {
        let stream = "{\"type\":\"thread.started\",\"thread_id\":\"01a-thread\"}\n\
                       {\"type\":\"turn.started\"}\n\
                       {\"type\":\"item.completed\",\"item\":{\"type\":\"assistant_message\"}}\n\
                       {\"type\":\"turn.completed\"}\n";
        let verification = parse_exec_json_stream(stream.as_bytes()).expect("parse");
        assert_eq!(verification.target_session_id, "01a-thread");
        assert!(verification.started_successfully);
    }

    #[test]
    fn a_failed_turn_is_reported_as_not_started_successfully_but_keeps_the_thread_id() {
        let stream = "{\"type\":\"thread.started\",\"thread_id\":\"01a-thread\"}\n\
                       {\"type\":\"turn.failed\",\"error\":{\"message\":\"boom\"}}\n";
        let verification = parse_exec_json_stream(stream.as_bytes()).expect("parse");
        assert_eq!(verification.target_session_id, "01a-thread");
        assert!(!verification.started_successfully);
    }

    #[test]
    fn no_thread_started_event_fails_closed() {
        let error = parse_exec_json_stream(b"{\"type\":\"error\",\"message\":\"boom\"}\n")
            .expect_err("must fail closed without a thread id");
        assert_eq!(error.code(), "malformed_provider_output");
    }

    #[test]
    fn stray_non_json_lines_are_skipped_rather_than_failing_the_parse() {
        let stream = "not json at all\n\
                       {\"type\":\"thread.started\",\"thread_id\":\"01a-thread\"}\n\
                       also not json\n\
                       {\"type\":\"turn.completed\"}\n";
        let verification = parse_exec_json_stream(stream.as_bytes()).expect("parse");
        assert!(verification.started_successfully);
    }
}
