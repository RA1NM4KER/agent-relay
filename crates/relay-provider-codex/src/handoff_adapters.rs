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
        // Active if EITHER the recorded process is still the same live process OR any process
        // runs under this profile's isolated CODEX_HOME. The recorded pid is only the short-lived
        // `codex exec` that created the thread; the interactive `codex resume` a supervised
        // terminal runs afterwards is a different process, visible only through its environment.
        let recorded_alive = recorded_owner
            .and_then(ProcessIdentity::is_still_the_same_process)
            .unwrap_or(false);
        Ok(LivenessVerdict {
            active: recorded_alive || codex_process_running_for(source_config_dir)?,
            // Codex has no structured cross-check for other untracked *sessions* under this
            // profile (see module doc); this is a known gap, not a claim of certainty.
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
        // Two independent views of the same question, exactly as `CodexSourceLiveness::check`
        // already combines them for liveness: the recorded pid is only the `codex exec` that
        // created the thread, while the interactive `codex resume` the user is typing into is a
        // different process, visible only through its environment (see module doc). An ambiguous
        // or failed recorded-pid attempt must not short-circuit the CODEX_HOME-token scan below —
        // that scan is the more authoritative signal and, on its own, is enough to establish
        // quiescence. Only report `StopNotVerified` when *neither* view can confirm it.
        let recorded_result =
            recorded_owner.map(|owner| terminate_verified_process(owner, ORPHAN_TERM_GRACE));
        // Also stop every process whose environment carries this exact profile's CODEX_HOME
        // token. Acceptable because this CODEX_HOME is Relay-owned (see docs/security.md) —
        // nothing else is expected to run under it.
        let scan_result = stop_codex_home_scan(source_config_dir);
        match (recorded_result, scan_result) {
            (Some(Ok(())), _) | (_, Ok(())) => Ok(()),
            (Some(Err(error)), Err(_)) | (None, Err(error)) => Err(error),
        }
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
        stop_codex_home_scan(target_config_dir)
    }
}

/// Terminates (verified) every process whose environment carries `config_dir`'s CODEX_HOME
/// token. `Err` only when the scan itself could not run, or a matched process could not be
/// confirmed stopped.
fn stop_codex_home_scan(config_dir: &Path) -> Result<()> {
    for pid in codex_pids_for(config_dir)? {
        terminate_verified_process(&ProcessIdentity::query(pid), ORPHAN_TERM_GRACE)?;
    }
    Ok(())
}

fn codex_process_running_for(config_dir: &Path) -> Result<bool> {
    Ok(!codex_pids_for(config_dir)?.is_empty())
}

/// Pure-ish (one `ps` shellout): pids of any process whose environment carries
/// `CODEX_HOME=<config_dir>`, from `ps -Eww -axo pid=,command=` text. Mirrors
/// `relay_provider_claude::handoff_adapters::matching_target_pids`'s whole-token matching so a
/// path that merely shares a prefix can never match.
/// `ps` itself can transiently fail to run (or report a non-zero exit) under heavy concurrent
/// process load — observed on GitHub's resource-constrained `macos-latest` CI runner during a
/// full parallel `cargo test --workspace`. Retried a few times before this scan (the more
/// authoritative of the two liveness signals — see module doc) gives up and reports a hard
/// failure; a `ps` that ran fine and simply listed nothing is not a failure at all, just an
/// empty result, and is never retried.
const PS_SPAWN_RETRY_ATTEMPTS: u32 = 3;
const PS_SPAWN_RETRY_DELAY: Duration = Duration::from_millis(20);

fn codex_pids_for(config_dir: &Path) -> Result<Vec<u32>> {
    let mut succeeded = None;
    for attempt in 0..PS_SPAWN_RETRY_ATTEMPTS {
        match Command::new("ps")
            .args(["-Eww", "-axo", "pid=,command="])
            .output()
        {
            Ok(output) if output.status.success() => {
                succeeded = Some(output);
                break;
            }
            _ if attempt + 1 < PS_SPAWN_RETRY_ATTEMPTS => thread::sleep(PS_SPAWN_RETRY_DELAY),
            _ => {}
        }
    }
    let output = succeeded.ok_or(Error::ProviderCommandFailed)?;
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

/// A fresh Codex thread created by Relay itself (`relay codex`).
#[derive(Clone, Debug)]
pub struct LaunchedThread {
    pub thread_id: String,
    pub process: Option<ProcessIdentity>,
}

/// Fixed, content-free bootstrap prompt: the thread has to exist before Relay can record a writer
/// lease for it, and no user text or project state is needed for that. The user's own first message
/// and provider arguments only ever reach the *interactive* session that continues this thread.
const NEW_THREAD_PROMPT: &str = "Agent Relay is starting a managed session. Reply with the single word READY and do not use any tools.";

/// Creates a new Codex thread under `config_dir` with `codex exec --json`, using only Relay's own
/// arguments (never user passthrough arguments), and returns its thread id. Fails closed unless the
/// turn completed cleanly and reported a thread id.
pub fn launch_new_thread(
    config_dir: &Path,
    project_dir: &Path,
    codex_executable: Option<&Path>,
) -> Result<LaunchedThread> {
    let inspector = CodexInspector::discover(codex_executable)?;
    let mut command = Command::new(inspector.executable());
    command
        .current_dir(project_dir)
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .env("CODEX_HOME", config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let mut process = None;
    let stdout = run_with_timeout(
        command,
        LAUNCH_TIMEOUT,
        LAUNCH_OUTPUT_LIMIT,
        NEW_THREAD_PROMPT.as_bytes(),
        &mut |identity| {
            process = identity;
            Ok(())
        },
    )?;
    let verification = parse_exec_json_stream(&stdout)?;
    if !verification.started_successfully {
        return Err(Error::ProviderCommandFailed);
    }
    Ok(LaunchedThread {
        thread_id: verification.target_session_id,
        process,
    })
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
    use super::*;

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

    /// `codex_pids_for`'s CODEX_HOME scan shells out to `ps -Eww` — BSD/macOS environment-listing
    /// syntax, not portable to every `ps` (observed: Ubuntu's procps rejects it outright, so even
    /// an *empty* scan returns `Err`, not `Ok(vec![])`). Even where the syntax is valid, a
    /// just-spawned child's CODEX_HOME token has shown real lag before appearing in it on
    /// GitHub's macos-latest CI runner. Skip cleanly wherever this exact mechanism — the one the
    /// code under test actually uses, not a merely-correlated one like the portable `ps -p <pid>`
    /// query `ProcessIdentity` uses elsewhere — cannot be relied on here, rather than asserting on
    /// something the platform/runner never promised (same policy as the `relay-cli` integration
    /// tests' `skip_without_process_env_scan!`).
    const CODEX_HOME_SCAN_RETRY_ATTEMPTS: u32 = 100;
    const CODEX_HOME_SCAN_RETRY_DELAY: Duration = Duration::from_millis(100);

    fn wait_for_codex_home_scan_to_find(config_dir: &Path, pid: u32) -> bool {
        (0..CODEX_HOME_SCAN_RETRY_ATTEMPTS).any(|attempt| {
            if attempt > 0 {
                thread::sleep(CODEX_HOME_SCAN_RETRY_DELAY);
            }
            codex_pids_for(config_dir).is_ok_and(|pids| pids.contains(&pid))
        })
    }

    /// Regression for the incident where `stop_and_verify` aborted on an ambiguous *recorded*
    /// pid (no captured start-time fingerprint — exactly what `is_still_the_same_process` reports
    /// `None` for) without ever reaching the CODEX_HOME scan below it, leaving the real process
    /// running and the handoff `StopNotVerified`. The scan must get its chance regardless.
    #[test]
    fn an_ambiguous_recorded_pid_still_stops_via_the_codex_home_scan() {
        let config_dir = tempfile::tempdir().expect("config dir");
        let project_dir = tempfile::tempdir().expect("project dir");
        let mut child = Command::new("sleep")
            .arg("30")
            .env("CODEX_HOME", config_dir.path())
            .spawn()
            .expect("spawn a stand-in codex process");
        if !wait_for_codex_home_scan_to_find(config_dir.path(), child.id()) {
            eprintln!(
                "skipping: this environment's CODEX_HOME scan cannot find a freshly spawned child"
            );
            let _ignored = child.kill();
            let _ignored = child.wait();
            return;
        }
        // The exact shape record_writer_process would have persisted had the pid already been
        // gone (or unreadable) at the moment it queried `ps` — an identity `stop_and_verify` can
        // never confirm on its own, by construction.
        let ambiguous = ProcessIdentity {
            pid: child.id(),
            start_time_fingerprint: None,
        };
        CodexSessionStopper
            .stop_and_verify(
                config_dir.path(),
                project_dir.path(),
                "session",
                Some(&ambiguous),
            )
            .expect("the CODEX_HOME scan alone must be enough to establish quiescence");
        for _ in 0..50 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("the real process, found only via the CODEX_HOME scan, was never stopped");
    }

    /// No recorded owner at all (the `stop_unrecorded_targets` case in miniature): the CODEX_HOME
    /// scan is the only signal, and an empty scan is itself a clean pass — but only where the
    /// scan mechanism runs at all; see `wait_for_codex_home_scan_to_find`'s doc comment.
    #[test]
    fn no_recorded_owner_and_an_empty_scan_is_a_clean_stop() {
        let config_dir = tempfile::tempdir().expect("config dir");
        let project_dir = tempfile::tempdir().expect("project dir");
        if codex_pids_for(config_dir.path()).is_err() {
            eprintln!("skipping: the CODEX_HOME scan does not run in this environment");
            return;
        }
        CodexSessionStopper
            .stop_and_verify(config_dir.path(), project_dir.path(), "session", None)
            .expect("nothing to stop is itself quiescent");
    }
}
