//! M2C: tiered Claude usage/rate-limit detection.
//!
//! Detection priority, matching the spec exactly:
//! 1. Structured session state from Claude Code's own `agents --json` bookkeeping. Currently
//!    inert against real Claude Code output (no exhaustion-shaped `state` value has been
//!    confirmed live) but real, forward-compatible, and free — never spends API usage.
//!
//! 2/3. A real, explicit, content-free probe invocation (`claude -p`, no `--resume`, never
//!    touching the actual session or its transcript), classified first as the same structured
//!    `is_error`/`result` JSON schema the handoff verification canary already relies on, then, if
//!    that does not parse, as a conservative allowlisted-phrase match against its raw output.
//!    This tier costs a small amount of real API usage each time it runs, so it is opt-in
//!    (`probe_enabled`) rather than something `detect()` does unconditionally on every call.
//!
//! Never returns `Exhausted` from a vague or ambiguous signal: anything that does not clear one
//! of these three tiers resolves to [`UsageState::Unknown`], which the automation layer treats as
//! "do nothing" rather than a trigger.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use relay_core::{
    Error, Result,
    usage::{UsageEvidence, UsageObservation, UsageSignal, UsageState},
};
use serde_json::Value;

use crate::{AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector, session_registry};

/// Session-record `state` values that would indicate exhaustion, if Claude Code ever emits one.
/// Not confirmed against real live output — kept narrow and explicit so a real but differently-
/// spelled state can never be silently misread as something else.
const EXHAUSTED_SESSION_STATES: &[&str] = &["rate_limited", "usage_limited", "exhausted"];

/// Conservative, exact (case-insensitive) phrase allowlist for the last-resort output-pattern
/// tier. Deliberately narrow: a false positive here triggers a real, disruptive handoff, so
/// nothing vaguely "error-shaped" is matched — only phrasing that unambiguously means a usage or
/// rate limit was hit.
const RATE_LIMIT_PHRASES: &[&str] = &[
    "usage limit reached",
    "5-hour limit reached",
    "weekly limit reached",
    "rate limit exceeded",
];

const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const PROBE_PROMPT: &str = "Reply with exactly this text and nothing else: RELAY_USAGE_PROBE_OK";

#[derive(Clone, Debug, Default)]
pub struct ClaudeUsageSignal {
    claude_executable: Option<PathBuf>,
    /// Tier 2/3 spends a small amount of real API usage each call, so it only runs when the
    /// caller explicitly opts in (`relay watch run --probe`) rather than on every free structured
    /// check.
    probe_enabled: bool,
}

impl ClaudeUsageSignal {
    #[must_use]
    pub const fn new(claude_executable: Option<PathBuf>, probe_enabled: bool) -> Self {
        Self {
            claude_executable,
            probe_enabled,
        }
    }
}

impl UsageSignal for ClaudeUsageSignal {
    fn detect(
        &self,
        config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<UsageObservation> {
        let now = now_unix_ms();
        if let Ok(sessions) =
            session_registry::query_active_sessions(config_dir, self.claude_executable.as_deref())
            && let Some(record) = sessions
                .iter()
                .find(|record| record.session_id == session_id)
            && let Some(state) = record.state.as_deref()
            && EXHAUSTED_SESSION_STATES.contains(&state)
        {
            return Ok(UsageObservation {
                state: UsageState::Exhausted,
                evidence: UsageEvidence::StructuredSessionState,
                detected_via: format!("agents --json state={state}"),
                observed_unix_ms: now,
                reset_unix_ms: None,
            });
        }

        if !self.probe_enabled {
            return Ok(UsageObservation::unknown(
                now,
                "no structured exhaustion signal and the real-usage probe is disabled",
            ));
        }

        let inspector = ClaudeInspector::discover(self.claude_executable.as_deref())?;
        let executable = inspector.executable().to_path_buf();
        let mut command = std::process::Command::new(&executable);
        command
            .current_dir(project_dir)
            .arg("-p")
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("--output-format")
            .arg("json")
            .arg(PROBE_PROMPT)
            .env("CLAUDE_CONFIG_DIR", config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
            command.env_remove(variable);
        }
        let (stdout, stderr) =
            run_capturing_regardless_of_exit(command, PROBE_TIMEOUT, PROBE_OUTPUT_LIMIT)?;
        Ok(classify_probe_output(&stdout, &stderr, now))
    }
}

/// Never errors on a non-zero exit — a probe that fails *is* the signal being classified, not a
/// tool failure. Only spawn/timeout/output-limit problems are reported as `Err`.
fn run_capturing_regardless_of_exit(
    mut command: std::process::Command,
    timeout: Duration,
    output_limit: usize,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut child = command.spawn().map_err(|_| Error::ProviderCommandFailed)?;
    let stdout = child.stdout.take().ok_or(Error::ProviderCommandFailed)?;
    let stderr = child.stderr.take().ok_or(Error::ProviderCommandFailed)?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, output_limit));
    let stderr_reader = thread::spawn(move || read_limited(stderr, output_limit));

    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(|_| Error::ProviderCommandFailed)?
            .is_some()
        {
            break;
        }
        if started.elapsed() >= timeout {
            let _ignored = child.kill();
            let _ignored = child.wait();
            let _ignored = stdout_reader.join();
            let _ignored = stderr_reader.join();
            return Err(Error::ProviderCommandTimeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
    let stdout_bytes = stdout_reader
        .join()
        .map_err(|_| Error::ProviderCommandFailed)??;
    let stderr_bytes = stderr_reader
        .join()
        .map_err(|_| Error::ProviderCommandFailed)??;
    Ok((stdout_bytes, stderr_bytes))
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
        bytes.truncate(limit);
    }
    Ok(bytes)
}

/// Pure and directly unit-testable: tier 2 (structured JSON) first, then tier 3 (allowlisted
/// phrase match against raw output), then a fail-closed [`UsageState::Unknown`].
#[must_use]
pub fn classify_probe_output(
    stdout: &[u8],
    stderr: &[u8],
    observed_unix_ms: u64,
) -> UsageObservation {
    if let Ok(value) = serde_json::from_slice::<Value>(stdout)
        && let Some(object) = value.as_object()
        && object.get("is_error").and_then(Value::as_bool) == Some(true)
        && let Some(result_text) = object.get("result").and_then(Value::as_str)
        && let Some(phrase) = matching_phrase(result_text)
    {
        return UsageObservation {
            state: UsageState::Exhausted,
            evidence: UsageEvidence::StructuredProbeResult,
            detected_via: format!("probe result matched phrase: {phrase}"),
            observed_unix_ms,
            reset_unix_ms: None,
        };
    }

    let combined_stdout = String::from_utf8_lossy(stdout);
    let combined_stderr = String::from_utf8_lossy(stderr);
    if let Some(phrase) =
        matching_phrase(&combined_stdout).or_else(|| matching_phrase(&combined_stderr))
    {
        return UsageObservation {
            state: UsageState::Exhausted,
            evidence: UsageEvidence::OutputPatternMatch,
            detected_via: format!("raw probe output matched phrase: {phrase}"),
            observed_unix_ms,
            reset_unix_ms: None,
        };
    }

    UsageObservation::unknown(
        observed_unix_ms,
        "probe completed but matched no known rate-limit signal",
    )
}

fn matching_phrase(text: &str) -> Option<&'static str> {
    let lowercase = text.to_lowercase();
    RATE_LIMIT_PHRASES
        .iter()
        .find(|phrase| lowercase.contains(*phrase))
        .copied()
}

/// Always returns the injected state; never touches a real provider. Used by `relay watch run
/// --simulate-usage` for controlled, quota-free validation of the real handoff machinery, and by
/// unit/integration tests.
#[derive(Clone, Debug)]
pub struct SimulatedUsageSignal {
    pub state: UsageState,
    pub reset_unix_ms: Option<u64>,
}

impl UsageSignal for SimulatedUsageSignal {
    fn detect(
        &self,
        _config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> Result<UsageObservation> {
        Ok(UsageObservation {
            state: self.state,
            evidence: UsageEvidence::Simulated,
            detected_via: "--simulate-usage".to_owned(),
            observed_unix_ms: now_unix_ms(),
            reset_unix_ms: self.reset_unix_ms,
        })
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::classify_probe_output;
    use relay_core::usage::{UsageEvidence, UsageState};

    #[test]
    fn structured_json_error_with_a_known_phrase_is_classified_exhausted() {
        let stdout =
            br#"{"is_error":true,"result":"You have hit your usage limit reached for today."}"#;
        let observation = classify_probe_output(stdout, b"", 1_000);
        assert_eq!(observation.state, UsageState::Exhausted);
        assert_eq!(observation.evidence, UsageEvidence::StructuredProbeResult);
    }

    #[test]
    fn structured_json_success_is_never_exhausted() {
        let stdout = br#"{"is_error":false,"result":"RELAY_USAGE_PROBE_OK"}"#;
        let observation = classify_probe_output(stdout, b"", 1_000);
        assert_eq!(observation.state, UsageState::Unknown);
    }

    #[test]
    fn structured_json_error_with_an_unrelated_message_stays_unknown_not_exhausted() {
        // A vague/unrelated error must never be treated as usage exhaustion.
        let stdout = br#"{"is_error":true,"result":"network connection reset"}"#;
        let observation = classify_probe_output(stdout, b"", 1_000);
        assert_eq!(
            observation.state,
            UsageState::Unknown,
            "a vague error must never trigger a false-positive Exhausted reading"
        );
    }

    #[test]
    fn raw_stderr_falls_back_to_pattern_match_when_stdout_is_not_json() {
        let stderr = b"error: weekly limit reached, try again later";
        let observation = classify_probe_output(b"not json", stderr, 1_000);
        assert_eq!(observation.state, UsageState::Exhausted);
        assert_eq!(observation.evidence, UsageEvidence::OutputPatternMatch);
    }

    #[test]
    fn phrase_matching_is_case_insensitive() {
        let stderr = b"RATE LIMIT EXCEEDED";
        let observation = classify_probe_output(b"", stderr, 1_000);
        assert_eq!(observation.state, UsageState::Exhausted);
    }

    #[test]
    fn completely_unrelated_output_fails_closed_to_unknown() {
        let observation = classify_probe_output(b"hello world", b"", 1_000);
        assert_eq!(observation.state, UsageState::Unknown);
    }

    #[test]
    fn empty_output_fails_closed_to_unknown() {
        let observation = classify_probe_output(b"", b"", 1_000);
        assert_eq!(observation.state, UsageState::Unknown);
    }
}
