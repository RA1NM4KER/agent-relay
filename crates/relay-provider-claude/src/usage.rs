//! Claude usage/rate-limit detection (M2C, reworked in M2C.1).
//!
//! Detection hierarchy (see `usage_policy` for the exact decision rules):
//! 1. Free structured signals Relay recorded from Claude Code itself — `rate_limit_event`s,
//!    the statusline's `rate_limits`, and `StopFailure(rate_limit)` hook records — combined by the
//!    single pure policy in [`crate::usage_policy`]. No API request is made.
//! 2. `claude agents --json` session `state` (currently inert against real output; kept because it
//!    is free and forward-compatible).
//! 3. The **explicit diagnostic probe** (`--probe`): a real `claude -p` request that **spends
//!    real API usage**. It is never required for normal automatic handoff, is never run against a
//!    profile already known exhausted, and only runs when nothing free was conclusive. Its
//!    stream-json `rate_limit_event`s are recorded like any other structured signal; a phrase match
//!    on a real limit message counts only together with statusline corroboration.
//!
//! Anything ambiguous or stale resolves to [`UsageState::Unknown`], which never triggers a handoff.

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

use crate::{
    AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector, session_registry,
    usage_policy::{PolicyConfig, PolicyInputs, evaluate},
    usage_signals::{
        LimitKind, RateLimitEventRecord, classify_limit_message, parse_rate_limit_events,
        read_profile_signals, record_rate_limit_events,
    },
};

/// Session-record `state` values that would indicate exhaustion, if Claude Code ever emits one.
/// Not confirmed against real live output.
const EXHAUSTED_SESSION_STATES: &[&str] = &["rate_limited", "usage_limited", "exhausted"];

const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_OUTPUT_LIMIT: usize = 256 * 1024;
const PROBE_PROMPT: &str = "Reply with exactly this text and nothing else: RELAY_USAGE_PROBE_OK";

#[derive(Clone, Debug, Default)]
pub struct ClaudeUsageSignal {
    claude_executable: Option<PathBuf>,
    /// Allows the diagnostic probe, which spends a small amount of real API usage each time it
    /// runs. Off by default; only ever a last resort.
    probe_enabled: bool,
    /// The model the watched workload runs, so model-scoped limits are applied correctly.
    workload_model: Option<String>,
}

impl ClaudeUsageSignal {
    #[must_use]
    pub const fn new(claude_executable: Option<PathBuf>, probe_enabled: bool) -> Self {
        Self {
            claude_executable,
            probe_enabled,
            workload_model: None,
        }
    }

    #[must_use]
    pub fn with_workload_model(mut self, workload_model: Option<String>) -> Self {
        self.workload_model = workload_model;
        self
    }

    fn policy(
        &self,
        config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
        now: u64,
        phrase_hit: Option<LimitKind>,
    ) -> UsageObservation {
        let signals = read_profile_signals(config_dir);
        evaluate(
            &PolicyInputs {
                now_unix_ms: now,
                project_dir,
                session_id,
                workload_model: self.workload_model.as_deref(),
                signals: &signals,
                phrase_hit,
            },
            &PolicyConfig::default(),
        )
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
        let free = self.policy(config_dir, project_dir, session_id, now, None);
        if free.state != UsageState::Unknown {
            return Ok(free);
        }

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
            return Ok(free);
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
            .arg("stream-json")
            .arg("--verbose")
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
        let findings = parse_probe_output(&stdout, &stderr, now, Some(session_id));
        // Persist the structured events like any other signal; a profile without the integration
        // installed simply has nowhere to record them, which is not an error here.
        let _ignored = record_rate_limit_events(config_dir, findings.events.clone());
        let mut signals = read_profile_signals(config_dir);
        for event in &findings.events {
            signals
                .rate_limit_events
                .retain(|existing| existing.rate_limit_type != event.rate_limit_type);
            signals.rate_limit_events.push(event.clone());
        }
        let observation = evaluate(
            &PolicyInputs {
                now_unix_ms: now,
                project_dir,
                session_id,
                workload_model: self.workload_model.as_deref(),
                signals: &signals,
                phrase_hit: findings.phrase,
            },
            &PolicyConfig::default(),
        );
        if observation.state == UsageState::Unknown && findings.succeeded {
            return Ok(UsageObservation {
                state: UsageState::Available,
                evidence: UsageEvidence::StructuredProbeResult,
                detected_via: "diagnostic probe request succeeded".to_owned(),
                observed_unix_ms: now,
                reset_unix_ms: None,
            });
        }
        Ok(observation)
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

/// What a probe run revealed, before the policy weighs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeFindings {
    pub events: Vec<RateLimitEventRecord>,
    /// A real limit message found in the probe's result/error text (opt-in tier; needs
    /// corroboration).
    pub phrase: Option<LimitKind>,
    /// The probe request completed successfully.
    pub succeeded: bool,
}

/// Pure and directly unit-testable. Reads stream-json lines (or a single `--output-format json`
/// object) for `rate_limit_event`s and the final `result`, then scans only result/error text for a
/// real limit phrase.
#[must_use]
pub fn parse_probe_output(
    stdout: &[u8],
    stderr: &[u8],
    observed_unix_ms: u64,
    session_id: Option<&str>,
) -> ProbeFindings {
    let events = parse_rate_limit_events(stdout, observed_unix_ms, session_id);
    let text = String::from_utf8_lossy(stdout);
    let mut phrase = None;
    let mut succeeded = false;
    let mut consider = |value: &Value| {
        if value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "result")
        {
            return;
        }
        let Some(is_error) = value.get("is_error").and_then(Value::as_bool) else {
            return;
        };
        if is_error {
            phrase = phrase.or_else(|| {
                value
                    .get("result")
                    .and_then(Value::as_str)
                    .and_then(classify_limit_message)
            });
        } else {
            succeeded = true;
        }
    };
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        consider(&value);
    } else {
        for line in text.lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                consider(&value);
            }
        }
    }
    if phrase.is_none() && !succeeded {
        phrase = classify_limit_message(&String::from_utf8_lossy(stderr));
    }
    ProbeFindings {
        events,
        phrase,
        succeeded: succeeded && phrase.is_none(),
    }
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
    use super::parse_probe_output;
    use crate::usage_signals::LimitKind;

    #[test]
    fn real_limit_error_result_is_recognized() {
        let stdout = br#"{"type":"result","subtype":"success","is_error":true,"result":"You've hit your session limit \u00b7 resets 3pm"}"#;
        let findings = parse_probe_output(stdout, b"", 1_000, None);
        assert_eq!(findings.phrase, Some(LimitKind::Session));
        assert!(!findings.succeeded);
    }

    #[test]
    fn stream_json_events_and_success_are_extracted() {
        let stdout = b"{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\",\"resetsAt\":2000000000,\"rateLimitType\":\"five_hour\"}}\n{\"type\":\"result\",\"is_error\":false,\"result\":\"RELAY_USAGE_PROBE_OK\"}";
        let findings = parse_probe_output(stdout, b"", 1_000, Some("s1"));
        assert_eq!(findings.events.len(), 1);
        assert!(findings.succeeded);
        assert_eq!(findings.phrase, None);
    }

    #[test]
    fn a_generic_429_error_is_never_a_limit_phrase() {
        let stdout = br#"{"type":"result","is_error":true,"result":"Request rejected (429) \u00b7 this may be a temporary capacity issue."}"#;
        let findings = parse_probe_output(stdout, b"rate limit exceeded", 1_000, None);
        assert_eq!(findings.phrase, None);
        assert!(!findings.succeeded);
    }

    #[test]
    fn stderr_limit_text_is_recognized_when_stdout_has_no_result() {
        let findings = parse_probe_output(b"", b"You've hit your weekly limit", 1_000, None);
        assert_eq!(findings.phrase, Some(LimitKind::Weekly));
    }

    #[test]
    fn empty_or_unrelated_output_is_inconclusive() {
        for stdout in [&b""[..], b"hello world"] {
            let findings = parse_probe_output(stdout, b"", 1_000, None);
            assert_eq!(findings, super::ProbeFindings::default());
        }
    }

    // End-to-end through the recorded per-profile files, as `relay watch run` reads them.
    mod detect {
        use std::{fs, path::PathBuf};

        use relay_core::usage::{UsageEvidence, UsageSignal, UsageState};
        use tempfile::tempdir;

        use crate::{
            ClaudeUsageSignal,
            usage_signals::{
                INTEGRATION_DIR, parse_rate_limit_events, parse_statusline_input,
                parse_stop_failure_input, record_rate_limit_events, record_statusline,
                record_stop_failure,
            },
        };

        fn now() -> u64 {
            super::super::now_unix_ms()
        }

        fn signal() -> ClaudeUsageSignal {
            // A nonexistent executable keeps the free tiers from ever spawning a real Claude.
            ClaudeUsageSignal::new(Some(PathBuf::from("/nonexistent/claude")), false)
        }

        fn statusline_json(pct: u32, resets_at: u64) -> String {
            format!(
                r#"{{"session_id":"s1","rate_limits":{{"five_hour":{{"used_percentage":{pct},"resets_at":{resets_at}}}}}}}"#
            )
        }

        fn stop_failure_json() -> &'static [u8] {
            br#"{"hook_event_name":"StopFailure","session_id":"s1","cwd":"/p","error":"rate_limit"}"#
        }

        fn profile() -> tempfile::TempDir {
            let dir = tempdir().expect("temp");
            fs::create_dir(dir.path().join(INTEGRATION_DIR)).expect("integration dir");
            dir
        }

        #[test]
        fn recorded_stop_failure_plus_fresh_statusline_at_100_is_exhausted() {
            let dir = profile();
            let far = now() / 1000 + 7200;
            record_statusline(
                dir.path(),
                &parse_statusline_input(statusline_json(100, far).as_bytes(), now()).expect("s"),
            )
            .expect("record");
            record_stop_failure(
                dir.path(),
                parse_stop_failure_input(stop_failure_json(), now()).expect("f"),
            )
            .expect("record");
            let observation = signal()
                .detect(dir.path(), dir.path(), "s1")
                .expect("detect");
            assert_eq!(observation.state, UsageState::Exhausted);
            assert_eq!(observation.evidence, UsageEvidence::StopFailureCorroborated);
            assert_eq!(observation.reset_unix_ms, Some(far * 1000));
        }

        #[test]
        fn stop_failure_alone_stale_statusline_and_near_limit_never_exhaust() {
            let far = now() / 1000 + 7200;
            // StopFailure alone.
            let alone = profile();
            record_stop_failure(
                alone.path(),
                parse_stop_failure_input(stop_failure_json(), now()).expect("f"),
            )
            .expect("record");
            assert_eq!(
                signal()
                    .detect(alone.path(), alone.path(), "s1")
                    .expect("d")
                    .state,
                UsageState::Unknown
            );
            // Stale statusline at 100% plus a StopFailure.
            let stale = profile();
            let old = now() - 60 * 60 * 1000;
            record_statusline(
                stale.path(),
                &parse_statusline_input(statusline_json(100, far).as_bytes(), old).expect("s"),
            )
            .expect("record");
            record_stop_failure(
                stale.path(),
                parse_stop_failure_input(stop_failure_json(), now()).expect("f"),
            )
            .expect("record");
            assert_eq!(
                signal()
                    .detect(stale.path(), stale.path(), "s1")
                    .expect("d")
                    .state,
                UsageState::Unknown
            );
            // Near limit with a StopFailure (a transient 429 while at 95%).
            let near = profile();
            record_statusline(
                near.path(),
                &parse_statusline_input(statusline_json(95, far).as_bytes(), now()).expect("s"),
            )
            .expect("record");
            record_stop_failure(
                near.path(),
                parse_stop_failure_input(stop_failure_json(), now()).expect("f"),
            )
            .expect("record");
            assert_eq!(
                signal()
                    .detect(near.path(), near.path(), "s1")
                    .expect("d")
                    .state,
                UsageState::NearLimit
            );
        }

        #[test]
        fn a_recorded_rejected_stream_event_is_exhausted_and_overage_is_not() {
            let far = now() / 1000 + 7200;
            let rejected = format!(
                r#"{{"type":"rate_limit_event","rate_limit_info":{{"status":"rejected","resetsAt":{far},"rateLimitType":"five_hour","isUsingOverage":false}},"session_id":"s1"}}"#
            );
            let dir = profile();
            record_rate_limit_events(
                dir.path(),
                parse_rate_limit_events(rejected.as_bytes(), now(), None),
            )
            .expect("record");
            let observation = signal().detect(dir.path(), dir.path(), "s1").expect("d");
            assert_eq!(observation.state, UsageState::Exhausted);
            assert_eq!(observation.evidence, UsageEvidence::RateLimitEvent);

            let overage = rejected.replace(r#""isUsingOverage":false"#, r#""isUsingOverage":true"#);
            let other = profile();
            record_rate_limit_events(
                other.path(),
                parse_rate_limit_events(overage.as_bytes(), now(), None),
            )
            .expect("record");
            assert_eq!(
                signal()
                    .detect(other.path(), other.path(), "s1")
                    .expect("d")
                    .state,
                UsageState::Unknown
            );
        }

        #[test]
        fn a_profile_without_the_integration_reads_as_unknown_and_records_nothing() {
            let dir = tempdir().expect("temp");
            assert_eq!(
                signal()
                    .detect(dir.path(), dir.path(), "s1")
                    .expect("d")
                    .state,
                UsageState::Unknown
            );
            assert!(
                record_statusline(dir.path(), &parse_statusline_input(b"{}", 1).expect("s"))
                    .is_err()
            );
            assert!(!dir.path().join(INTEGRATION_DIR).exists());
        }
    }
}
