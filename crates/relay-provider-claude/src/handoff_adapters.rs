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
        LivenessVerdict, ProcessIdentity, SessionStager, SessionStopper, SourceLiveness,
        TargetLauncher, TargetVerification, TransferOutcome, TransferredArtifact,
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

/// The writer-liveness check. Primarily trusts Claude Code's own session bookkeeping (`claude
/// agents --json`): a session it still lists is treated as active outright, never text-matched.
/// Live testing across M2B.5/M2B.75 established *why* this must be the primary signal rather
/// than a pid+fingerprint check: Claude's background daemon keeps a session listed (state
/// "working"/"blocked") indefinitely after its worker process dies from a raw `kill` — it is
/// dormant and resurrectable, not gone — until the session is explicitly `claude stop`ped. A
/// confirmed-dead worker pid must never override a still-listed session to "not active": an
/// earlier version of this check did exactly that and let a live-tested competing `relay launch`
/// through while the original session could still have been resurrected. The pid+fingerprint
/// check only adds corroborating evidence for the case where the session is *not* listed at all
/// (never as a way to contradict a listing), and unestablishable identity always fails closed.
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

        // M2B.75 live finding: Claude's background daemon keeps a session listed (state
        // "working"/"blocked") indefinitely after its worker process dies, until the session is
        // explicitly `claude stop`ped — it is dormant and resurrectable, not gone. A confirmed-
        // dead worker pid must NOT be allowed to override that listing to "not active": doing so
        // let a real live-test competing `relay launch` through while the daemon could still have
        // resurrected the original session at any time. Presence in the listing is therefore
        // trusted outright; the pid+fingerprint check only adds corroborating evidence for the
        // case where the session is *not* listed at all.
        let active = match matching {
            Some(_) => true,
            None => match recorded_owner {
                None => false,
                // Unestablishable identity fails closed: assume active rather than guess.
                Some(owner) => owner.is_still_the_same_process().unwrap_or(true),
            },
        };
        Ok(LivenessVerdict {
            active,
            untracked_session_ids: untracked,
        })
    }
}

const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_OUTPUT_LIMIT: usize = 16 * 1024;
const QUIESCENCE_POLL_ATTEMPTS: u32 = 10;
const QUIESCENCE_POLL_DELAY: Duration = Duration::from_millis(400);
/// M2B.75's core safety property: one quiet observation is never enough (matches
/// docs/architecture.md's "never trigger a destructive transition from display text alone").
const REQUIRED_CONSECUTIVE_QUIET: u32 = 3;

/// M2B.75's authoritative shutdown: issues Claude Code's own documented `claude stop <id>`
/// (never a raw `kill`), then verifies quiescence with [`REQUIRED_CONSECUTIVE_QUIET`] consecutive
/// observations before returning `Ok`. Each observation combines two independent signals —
/// Claude's own session bookkeeping (`agents --json`) and, when a prior pid is known, a direct
/// pid+fingerprint check — because M2B.5 proved live that a hard `kill -9` of a `--bg` session's
/// reported pid can be silently reassigned to a new process by Claude's own background daemon;
/// only `claude stop` reliably and permanently ends one from outside that daemon.
#[derive(Clone, Debug, Default)]
pub struct ClaudeSessionStopper {
    claude_executable: Option<PathBuf>,
}

impl ClaudeSessionStopper {
    #[must_use]
    pub const fn new(claude_executable: Option<PathBuf>) -> Self {
        Self { claude_executable }
    }
}

impl SessionStopper for ClaudeSessionStopper {
    fn stop_and_verify(
        &self,
        source_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
        recorded_owner: Option<&ProcessIdentity>,
    ) -> Result<()> {
        let project_dir_text = project_dir.to_string_lossy();
        let matches_this_session = |record: &session_registry::AgentSessionRecord| {
            record.session_id == session_id
                && record
                    .cwd
                    .as_deref()
                    .is_none_or(|cwd| cwd == project_dir_text)
        };

        // Issue the authoritative stop only if the provider currently lists this exact session
        // for this exact profile+project — never a different session, never another profile's
        // (query_active_sessions is itself scoped to source_config_dir).
        let sessions = session_registry::query_active_sessions(
            source_config_dir,
            self.claude_executable.as_deref(),
        )?;
        // An interactive session has no background id and cannot be stopped this way; it simply
        // stays listed, so quiescence below is never reached and the stop fails closed.
        if let Some(handle) = sessions
            .iter()
            .find(|record| matches_this_session(record))
            .and_then(|record| record.id.as_deref())
        {
            issue_stop(source_config_dir, handle, self.claude_executable.as_deref())?;
        }

        let mut consecutive_quiet = 0u32;
        for attempt in 0..QUIESCENCE_POLL_ATTEMPTS {
            let sessions = session_registry::query_active_sessions(
                source_config_dir,
                self.claude_executable.as_deref(),
            )?;
            let still_listed = sessions.iter().any(matches_this_session);
            // A prior recorded pid that is still confirmed the same process means "not quiet"
            // even if the provider's own listing has already dropped the session (belt and
            // suspenders against the provider-bookkeeping staleness M2B.5 found live).
            let pid_quiet = match recorded_owner {
                None => true,
                Some(owner) => owner.is_still_the_same_process() != Some(true),
            };
            let (next, reached) = quiescence_step(consecutive_quiet, still_listed, pid_quiet);
            consecutive_quiet = next;
            if reached {
                return Ok(());
            }
            if attempt + 1 < QUIESCENCE_POLL_ATTEMPTS {
                thread::sleep(QUIESCENCE_POLL_DELAY);
            }
        }
        Err(Error::StopNotVerified(format!(
            "session {session_id} did not go quiet within the bounded window"
        )))
    }

    fn stop_orphan_target(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
        orphan: &ProcessIdentity,
    ) -> Result<()> {
        terminate_verified_process(orphan, ORPHAN_TERM_GRACE)?;
        self.stop_and_verify(target_config_dir, project_dir, session_id, Some(orphan))
    }

    fn stop_unrecorded_targets(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<()> {
        let output = std::process::Command::new("ps")
            .args(["-Eww", "-axo", "pid=,command="])
            .output()
            .map_err(|_| Error::ProviderCommandFailed)?;
        if !output.status.success() {
            return Err(Error::ProviderCommandFailed);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        for pid in matching_target_pids(&text, target_config_dir, session_id, std::process::id()) {
            terminate_verified_process(&ProcessIdentity::query(pid), ORPHAN_TERM_GRACE)?;
        }
        self.stop_and_verify(target_config_dir, project_dir, session_id, None)
    }
}

/// Pure: pids of `claude -p --resume <session_id>` processes running under exactly this
/// `CLAUDE_CONFIG_DIR`, from `ps -Eww -axo pid=,command=` text. Every condition is matched as a
/// whole token, so a different session, a different profile, or a longer path that merely shares
/// a prefix can never match.
fn matching_target_pids(
    ps_text: &str,
    config_dir: &Path,
    session_id: &str,
    own_pid: u32,
) -> Vec<u32> {
    let config_token = format!("CLAUDE_CONFIG_DIR={}", config_dir.display());
    ps_text
        .lines()
        .filter_map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let pid: u32 = tokens.first()?.parse().ok()?;
            let resumes_session = tokens
                .windows(2)
                .any(|pair| pair[0] == "--resume" && pair[1] == session_id);
            let is_print_mode = tokens
                .iter()
                .any(|token| *token == "-p" || *token == "--print");
            let same_profile = tokens.iter().any(|token| *token == config_token);
            (pid != own_pid && resumes_session && is_print_mode && same_profile).then_some(pid)
        })
        .collect()
}

const ORPHAN_TERM_GRACE: Duration = Duration::from_secs(5);
const ORPHAN_POLL_DELAY: Duration = Duration::from_millis(100);

/// Terminates the exact process Relay itself spawned as a `claude -p --resume` target. The
/// pid + start-time fingerprint is re-confirmed immediately before every signal, so a pid that
/// was reassigned to an unrelated process is never signalled, and an unverifiable identity fails
/// closed instead of guessing. `SIGTERM` first, `SIGKILL` only if it survives the grace period.
fn terminate_verified_process(orphan: &ProcessIdentity, grace: Duration) -> Result<()> {
    match orphan.is_still_the_same_process() {
        Some(false) => return Ok(()),
        None => {
            return Err(Error::StopNotVerified(format!(
                "cannot confirm the identity of recorded target pid {}",
                orphan.pid
            )));
        }
        Some(true) => {}
    }
    for signal in ["-TERM", "-KILL"] {
        if orphan.is_still_the_same_process() != Some(true) {
            break;
        }
        let status = std::process::Command::new("kill")
            .arg(signal)
            .arg(orphan.pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| Error::ProviderCommandFailed)?;
        let _ignored = status;
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if orphan.is_still_the_same_process() == Some(false) {
                return Ok(());
            }
            thread::sleep(ORPHAN_POLL_DELAY);
        }
    }
    if orphan.is_still_the_same_process() == Some(false) {
        Ok(())
    } else {
        Err(Error::StopNotVerified(format!(
            "recorded target pid {} survived SIGTERM and SIGKILL",
            orphan.pid
        )))
    }
}

/// Never returns or logs the stop command's raw output — only success/failure.
/// Pure quiescence decision, kept standalone so the consecutive-observation state machine is
/// unit-testable without a real Claude subprocess. Any non-quiet observation (still listed, or a
/// prior pid confirmed still the same process) resets the streak to zero — a single quiet
/// reading is never enough, and reappearance/PID-reassignment must be caught, not averaged away.
fn quiescence_step(consecutive_quiet: u32, still_listed: bool, pid_quiet: bool) -> (u32, bool) {
    if still_listed || !pid_quiet {
        (0, false)
    } else {
        let next = consecutive_quiet + 1;
        (next, next >= REQUIRED_CONSECUTIVE_QUIET)
    }
}

fn issue_stop(
    config_dir: &Path,
    provider_handle: &str,
    claude_executable: Option<&Path>,
) -> Result<()> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let executable = inspector.executable().to_path_buf();
    let mut command = std::process::Command::new(&executable);
    command
        .arg("stop")
        .arg(provider_handle)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let _discarded = run_with_timeout(command, STOP_TIMEOUT, STOP_OUTPUT_LIMIT, &mut |_| Ok(()))?;
    Ok(())
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
        on_started: &mut dyn FnMut(Option<ProcessIdentity>) -> Result<()>,
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

        let stdout = run_with_timeout(command, LAUNCH_TIMEOUT, LAUNCH_OUTPUT_LIMIT, on_started)?;
        parse_verification(&stdout)
    }
}

/// `on_spawned` is invoked exactly once, immediately after `spawn()` succeeds and before the
/// (potentially long) wait for the child to finish — callers that need durable evidence of a
/// live child (M2C's orphan-target supervision) rely on this ordering.
fn run_with_timeout(
    mut command: std::process::Command,
    timeout: Duration,
    output_limit: usize,
    on_spawned: &mut dyn FnMut(Option<ProcessIdentity>) -> Result<()>,
) -> Result<Vec<u8>> {
    let mut child = command.spawn().map_err(|_| Error::ProviderCommandFailed)?;
    on_spawned(Some(ProcessIdentity::query(child.id())))?;
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
    let stdout = run_with_timeout(
        command,
        LAUNCH_BG_TIMEOUT,
        LAUNCH_BG_OUTPUT_LIMIT,
        &mut |_| Ok(()),
    )?;
    let text = String::from_utf8_lossy(&stdout);
    let provider_handle = parse_background_job_id(&text)?;

    let mut found_session_id: Option<String> = None;
    for attempt in 0..AGENTS_JSON_POLL_ATTEMPTS {
        let sessions = session_registry::query_active_sessions(config_dir, claude_executable)?;
        if let Some(record) = sessions
            .iter()
            .find(|record| record.id.as_deref() == Some(provider_handle.as_str()))
        {
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
            .find(|record| record.id.as_deref() == Some(provider_handle.as_str()))
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
    use super::{
        REQUIRED_CONSECUTIVE_QUIET, matching_target_pids, parse_background_job_id,
        parse_verification, quiescence_step, terminate_verified_process,
    };
    use relay_core::handoff::ProcessIdentity;
    use std::time::Duration;

    /// Spawns a real long-lived child and reaps it on a helper thread, so a terminated child
    /// disappears from the process table the way a reparented orphan does in production.
    fn spawn_reaped_sleeper() -> ProcessIdentity {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::query(child.id());
        std::thread::spawn(move || {
            let _ignored = child.wait();
        });
        identity
    }

    #[test]
    fn target_pid_scan_matches_only_the_exact_session_profile_and_print_mode() {
        let dir = std::path::Path::new("/p/megan/claude");
        let session = "99370db4-a0d6-44c0-bfba-6c15b5bcfab4";
        let ps = [
            format!("100 claude -p --resume {session} --permission-mode acceptEdits CLAUDE_CONFIG_DIR=/p/megan/claude HOME=/h"),
            format!("101 claude -p --resume {session} CLAUDE_CONFIG_DIR=/p/erika/claude"),
            format!("102 claude -p --resume {session} CLAUDE_CONFIG_DIR=/p/megan/claude2"),
            "103 claude -p --resume 11111111-2222-3333-4444-555555555555 CLAUDE_CONFIG_DIR=/p/megan/claude".to_owned(),
            format!("104 claude --resume {session} CLAUDE_CONFIG_DIR=/p/megan/claude"),
            format!("105 claude --print --resume {session} CLAUDE_CONFIG_DIR=/p/megan/claude"),
            format!("106 claude -p --resume {session} CLAUDE_CONFIG_DIR=/p/megan/claude"),
        ]
        .join("\n");
        assert_eq!(matching_target_pids(&ps, dir, session, 106), vec![100, 105]);
    }

    #[test]
    fn a_verified_live_orphan_is_terminated() {
        let orphan = spawn_reaped_sleeper();
        assert_eq!(orphan.is_still_the_same_process(), Some(true));
        terminate_verified_process(&orphan, Duration::from_secs(5)).expect("terminate");
        assert_eq!(orphan.is_still_the_same_process(), Some(false));
    }

    #[test]
    fn a_process_with_a_mismatched_fingerprint_is_never_signalled() {
        let real = spawn_reaped_sleeper();
        let impostor = ProcessIdentity {
            pid: real.pid,
            start_time_fingerprint: Some("Mon Jan  1 00:00:00 1990".to_owned()),
        };
        terminate_verified_process(&impostor, Duration::from_millis(200))
            .expect("a different process is treated as already gone");
        assert_eq!(
            real.is_still_the_same_process(),
            Some(true),
            "the unrelated process that reused the pid must be left running"
        );
        let _cleanup = std::process::Command::new("kill")
            .arg(real.pid.to_string())
            .status();
    }

    #[test]
    fn an_orphan_without_a_fingerprint_fails_closed() {
        let unknown = ProcessIdentity {
            pid: std::process::id(),
            start_time_fingerprint: None,
        };
        assert!(terminate_verified_process(&unknown, Duration::from_millis(100)).is_err());
    }

    #[test]
    fn an_already_gone_orphan_is_a_clean_success() {
        let orphan = spawn_reaped_sleeper();
        terminate_verified_process(&orphan, Duration::from_secs(5)).expect("first");
        terminate_verified_process(&orphan, Duration::from_secs(5)).expect("idempotent");
    }

    #[test]
    fn requires_more_than_one_quiet_observation() {
        let (count, reached) = quiescence_step(0, false, true);
        assert_eq!(count, 1);
        assert!(
            !reached,
            "a single quiet observation must not be sufficient"
        );
    }

    #[test]
    fn reaches_quiescence_only_after_the_required_consecutive_count() {
        let mut count = 0;
        let mut reached = false;
        for _ in 0..REQUIRED_CONSECUTIVE_QUIET {
            assert!(!reached, "must not report reached before the count is hit");
            (count, reached) = quiescence_step(count, false, true);
        }
        assert!(reached, "must report reached at exactly the required count");
    }

    #[test]
    fn a_session_that_disappears_then_reappears_resets_the_streak() {
        let (count, _) = quiescence_step(0, false, true); // quiet
        assert_eq!(count, 1);
        let (count, reached) = quiescence_step(count, true, true); // reappeared: still listed
        assert_eq!(
            count, 0,
            "reappearance must reset the streak, not just pause it"
        );
        assert!(!reached);
    }

    #[test]
    fn a_pid_change_during_stop_resets_the_streak_even_if_no_longer_listed() {
        // Simulates the exact live-observed case: agents --json stops listing the session, but
        // the recorded pid is confirmed to still be a live, different-fingerprint process (the
        // daemon reassigned a new pid) — must not be treated as quiet.
        let (count, _) = quiescence_step(0, false, true);
        assert_eq!(count, 1);
        let (count, reached) = quiescence_step(count, false, false); // pid_quiet=false
        assert_eq!(
            count, 0,
            "a live pid must reset the streak even if unlisted"
        );
        assert!(!reached);
    }

    #[test]
    fn no_recorded_pid_means_agents_json_alone_can_reach_quiescence() {
        // No prior WriterLease (e.g. a session never launched via `relay launch`): pid_quiet is
        // unconditionally true, so agents --json no longer listing the session is sufficient on
        // its own — there is nothing else to corroborate against.
        let mut count = 0;
        let mut reached = false;
        for _ in 0..REQUIRED_CONSECUTIVE_QUIET {
            (count, reached) = quiescence_step(count, false, true);
        }
        assert!(reached);
    }

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
