//! Codex usage/exhaustion detection from Codex's own structured rate-limit interface.
//!
//! Source: `codex app-server`'s `account/rateLimits/read` (see [`crate::app_server`]) — typed
//! JSON, read through the same official local protocol first-party clients use, against the
//! profile's isolated `CODEX_HOME`, with no credential ever read by Relay. Relay never parses
//! `codex exec --json` error prose for this.
//!
//! Mapping (fail closed everywhere it is not unambiguous):
//!
//! * `ordinaryUsageAllowed == false` → [`UsageState::Exhausted`]. This is the backend's own
//!   verdict "validated against the active account" and the only signal that can make a profile
//!   blocking. The reset time is the latest still-future `resetsAt` among windows at 100% (every
//!   blocking window must reset); with none known the record has no reset time and stays blocking
//!   until the operator clears it.
//! * `ordinaryUsageAllowed == true`, no reached-type → [`UsageState::Available`], or
//!   [`UsageState::NearLimit`] once any window is at or above 90%.
//! * anything else — `null`/absent flag (the protocol says clients must not infer from
//!   percentages), a reached-type that contradicts an "allowed" flag, a non-plan account (API
//!   key), no account identified, a different `CODEX_HOME`, an app-server that fails, times out
//!   or answers malformed data — → [`UsageState::Unknown`], which routing never acts on.

use std::path::{Path, PathBuf};

use relay_core::{
    Error, Result,
    usage::{UsageEvidence, UsageObservation, UsageSignal, UsageState},
};

use crate::{
    CodexInspector,
    app_server::{self, RateLimitsReport},
};

const NEAR_LIMIT_PERCENT: u32 = 90;

/// Bounded, secret-free failure categories. These are diagnostics, never usage authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexUsageDiagnostic {
    AuthenticationRevoked,
    AuthenticationInvalid,
    Timeout,
    UnsupportedSchema,
    UnsupportedInterface,
    ExecutableUnavailable,
    Transport,
    ProviderFailure,
    HomeMismatch,
}

impl CodexUsageDiagnostic {
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AuthenticationRevoked => {
                "authentication_revoked: Codex rejected this profile's revoked credentials (HTTP 401; token_revoked)"
            }
            Self::AuthenticationInvalid => {
                "authentication_invalid: Codex rejected this profile's invalid or expired credentials (HTTP 401)"
            }
            Self::Timeout => {
                "timeout: Codex app-server did not answer within the bounded session timeout"
            }
            Self::UnsupportedSchema => {
                "unsupported_schema: Codex returned an unsupported or malformed structured usage response"
            }
            Self::UnsupportedInterface => {
                "unsupported_interface: this Codex app-server does not support the structured method Relay needs"
            }
            Self::ExecutableUnavailable => {
                "executable_unavailable: Codex could not be found, safely resolved, or started"
            }
            Self::Transport => "transport_failure: Codex app-server closed its connection",
            Self::ProviderFailure => {
                "provider_failure: Codex rejected the structured usage request"
            }
            Self::HomeMismatch => {
                "profile_mismatch: Codex app-server resolved a different CODEX_HOME"
            }
        }
    }

    #[must_use]
    pub const fn authentication_rejected(self) -> bool {
        matches!(
            self,
            Self::AuthenticationRevoked | Self::AuthenticationInvalid
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct CodexUsageSignal {
    executable: Option<PathBuf>,
}

/// One authoritative Codex structured read, typed to serve both consumers it must feed (GitHub
/// #13): the existing provider-neutral [`UsageObservation`] safety/routing decision, and a
/// sanitized scheduling hint for adaptive polling cadence. Never a second provider call — both
/// fields come from the same [`app_server::read_rate_limits`] round trip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexUsageReading {
    pub observation: UsageObservation,
    pub diagnostic: Option<CodexUsageDiagnostic>,
    /// The highest window `usedPercent` seen in this read. Trustworthy for adaptive polling
    /// cadence ONLY — `Some` exactly when [`Self::observation`] is a validated
    /// `ordinaryUsageAllowed == true` reading (`Available`/`NearLimit`); `None` for `Exhausted`
    /// (scheduling is moot — a handoff evaluation follows) and for `Unknown` (never inferred from
    /// a percentage). Never used, and must never be used, to decide exhaustion itself.
    pub max_used_percent: Option<u32>,
}

impl CodexUsageReading {
    #[must_use]
    pub fn recovery_command(&self, profile: &str) -> Option<String> {
        self.diagnostic
            .filter(|d| d.authentication_rejected())
            .map(|_| format!("relay logout {profile}\nrelay login {profile}"))
    }

    #[must_use]
    pub fn refusal(&self, profile: &str) -> Error {
        let mut detail = self.observation.detected_via.clone();
        if let Some(command) = self.recovery_command(profile) {
            detail.push_str(&format!(
                "\nThis isolated profile needs its own login. Run:\n{command}"
            ));
        }
        Error::CodexUsageUnverified {
            profile: profile.to_owned(),
            detail,
        }
    }
}

impl CodexUsageSignal {
    /// `executable` overrides discovery (`PATH`), as elsewhere in Relay.
    #[must_use]
    pub fn new(executable: Option<PathBuf>) -> Self {
        Self { executable }
    }

    /// The full typed reading — observation plus the sanitized scheduling hint. Infallible: any
    /// failure to reach or parse the app-server collapses to [`UsageState::Unknown`], exactly as
    /// [`UsageSignal::detect`] does.
    #[must_use]
    pub fn read(&self, config_dir: &Path) -> CodexUsageReading {
        let now = now_unix_ms();
        let outcome = CodexInspector::discover(self.executable.as_deref())
            .map_err(|_| CodexUsageDiagnostic::ExecutableUnavailable)
            .and_then(|inspector| {
                app_server::read_rate_limits(inspector.executable(), config_dir).map_err(|error| {
                    match error {
                        app_server::AppServerError::AuthenticationRejected { revoked: true } => {
                            CodexUsageDiagnostic::AuthenticationRevoked
                        }
                        app_server::AppServerError::AuthenticationRejected { revoked: false } => {
                            CodexUsageDiagnostic::AuthenticationInvalid
                        }
                        app_server::AppServerError::Spawn => {
                            CodexUsageDiagnostic::ExecutableUnavailable
                        }
                        app_server::AppServerError::Timeout => CodexUsageDiagnostic::Timeout,
                        app_server::AppServerError::HomeMismatch => {
                            CodexUsageDiagnostic::HomeMismatch
                        }
                        app_server::AppServerError::Rpc { code: -32601, .. } => {
                            CodexUsageDiagnostic::UnsupportedInterface
                        }
                        app_server::AppServerError::Rpc { .. } => {
                            CodexUsageDiagnostic::ProviderFailure
                        }
                        app_server::AppServerError::Protocol(ref detail)
                            if detail.starts_with("server closed") =>
                        {
                            CodexUsageDiagnostic::Transport
                        }
                        app_server::AppServerError::Protocol(_) => {
                            CodexUsageDiagnostic::UnsupportedSchema
                        }
                    }
                })
            });
        match outcome {
            Ok(report) => interpret(&report, now),
            Err(reason) => CodexUsageReading {
                observation: unknown(now, reason.description()),
                diagnostic: Some(reason),
                max_used_percent: None,
            },
        }
    }
}

impl UsageSignal for CodexUsageSignal {
    fn detect(
        &self,
        config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> Result<UsageObservation> {
        Ok(self.read(config_dir).observation)
    }
}

/// Pure mapping from a parsed report to Relay's usage model, plus the sanitized scheduling hint
/// derived from the same report.
#[must_use]
pub fn interpret(report: &RateLimitsReport, now_unix_ms: u64) -> CodexUsageReading {
    if report.account_kind.as_deref() != Some("chatgpt") {
        return CodexUsageReading {
            observation: unknown(
                now_unix_ms,
                "codex account is not a plan-based ChatGPT account; no usage windows to read",
            ),
            diagnostic: None,
            max_used_percent: None,
        };
    }
    if !report.account_identified {
        return CodexUsageReading {
            observation: unknown(
                now_unix_ms,
                "codex usage snapshot did not identify its account",
            ),
            diagnostic: None,
            max_used_percent: None,
        };
    }
    let via = "codex app-server account/rateLimits/read";
    let max_used = report
        .windows
        .iter()
        .map(|window| window.used_percent)
        .max()
        .unwrap_or(0);
    match report.ordinary_usage_allowed {
        Some(false) => {
            let reset = report
                .windows
                .iter()
                .filter(|window| window.used_percent >= 100)
                .filter_map(|window| window.resets_at_unix_s)
                .map(|seconds| seconds.saturating_mul(1000))
                .filter(|reset| *reset > now_unix_ms)
                .max();
            CodexUsageReading {
                observation: UsageObservation {
                    state: UsageState::Exhausted,
                    evidence: UsageEvidence::ProviderRateLimitApi,
                    detected_via: format!("{via}: ordinaryUsageAllowed=false"),
                    observed_unix_ms: now_unix_ms,
                    reset_unix_ms: reset,
                },
                // Exhausted routes straight to a handoff evaluation; cadence is moot.
                diagnostic: None,
                max_used_percent: None,
            }
        }
        Some(true) if report.reached_type.is_some() => CodexUsageReading {
            observation: unknown(
                now_unix_ms,
                "codex usage snapshot contradicts itself (usage allowed but a limit is reported reached)",
            ),
            diagnostic: None,
            max_used_percent: None,
        },
        Some(true) => CodexUsageReading {
            observation: UsageObservation {
                state: if max_used >= NEAR_LIMIT_PERCENT {
                    UsageState::NearLimit
                } else {
                    UsageState::Available
                },
                evidence: UsageEvidence::ProviderRateLimitApi,
                detected_via: format!("{via}: ordinaryUsageAllowed=true"),
                observed_unix_ms: now_unix_ms,
                reset_unix_ms: None,
            },
            diagnostic: None,
            max_used_percent: Some(max_used),
        },
        None => CodexUsageReading {
            observation: unknown(
                now_unix_ms,
                "codex reported no ordinaryUsageAllowed verdict; not inferring from percentages",
            ),
            diagnostic: None,
            max_used_percent: None,
        },
    }
}

fn unknown(now_unix_ms: u64, reason: &str) -> UsageObservation {
    UsageObservation {
        state: UsageState::Unknown,
        evidence: UsageEvidence::ProviderRateLimitApi,
        detected_via: reason.to_owned(),
        observed_unix_ms: now_unix_ms,
        reset_unix_ms: None,
    }
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::{RateLimitWindow, fake};

    const NOW: u64 = 1_800_000_000_000;

    fn report(allowed: Option<bool>, windows: &[(u32, Option<u64>)]) -> RateLimitsReport {
        RateLimitsReport {
            ordinary_usage_allowed: allowed,
            account_identified: true,
            account_kind: Some("chatgpt".to_owned()),
            windows: windows
                .iter()
                .map(|(used, reset)| RateLimitWindow {
                    used_percent: *used,
                    resets_at_unix_s: *reset,
                })
                .collect(),
            reached_type: None,
        }
    }

    #[test]
    fn allowed_with_headroom_is_available() {
        let reading = interpret(&report(Some(true), &[(10, None), (40, None)]), NOW);
        assert_eq!(reading.observation.state, UsageState::Available);
        assert!(!reading.observation.state.is_blocking());
        assert_eq!(reading.max_used_percent, Some(40));
    }

    #[test]
    fn allowed_but_a_window_at_ninety_percent_is_near_limit_and_still_not_blocking() {
        let reading = interpret(&report(Some(true), &[(10, None), (95, None)]), NOW);
        assert_eq!(reading.observation.state, UsageState::NearLimit);
        assert!(!reading.observation.state.is_blocking());
        assert_eq!(reading.max_used_percent, Some(95));
    }

    #[test]
    fn not_allowed_is_exhausted_with_the_latest_future_reset_of_the_full_windows() {
        let future_short = NOW / 1000 + 3_600;
        let future_long = NOW / 1000 + 86_400;
        let reading = interpret(
            &report(
                Some(false),
                &[
                    (100, Some(future_short)),
                    (100, Some(future_long)),
                    (30, Some(NOW / 1000 + 1)),
                ],
            ),
            NOW,
        );
        assert_eq!(reading.observation.state, UsageState::Exhausted);
        assert!(reading.observation.state.is_blocking());
        assert_eq!(reading.observation.reset_unix_ms, Some(future_long * 1000));
        // Exhausted routes straight to a handoff evaluation; cadence is never derived from it.
        assert_eq!(reading.max_used_percent, None);
    }

    #[test]
    fn not_allowed_with_only_a_past_or_missing_reset_has_no_reset_time_and_stays_blocking() {
        let reading = interpret(&report(Some(false), &[(100, Some(NOW / 1000 - 5))]), NOW);
        assert_eq!(reading.observation.state, UsageState::Exhausted);
        assert_eq!(reading.observation.reset_unix_ms, None);
    }

    #[test]
    fn a_missing_verdict_is_unknown_even_when_every_window_shows_full() {
        let reading = interpret(&report(None, &[(100, Some(NOW / 1000 + 60))]), NOW);
        assert_eq!(reading.observation.state, UsageState::Unknown);
        assert!(!reading.observation.state.is_blocking());
        // UNKNOWN must never carry a percentage forward for scheduling either.
        assert_eq!(reading.max_used_percent, None);
    }

    #[test]
    fn a_contradictory_snapshot_an_api_key_account_or_no_account_is_unknown() {
        let mut contradictory = report(Some(true), &[(10, None)]);
        contradictory.reached_type = Some("rate_limit_reached".to_owned());
        let reading = interpret(&contradictory, NOW);
        assert_eq!(reading.observation.state, UsageState::Unknown);
        assert_eq!(reading.max_used_percent, None);
        let mut api_key = report(Some(false), &[(100, None)]);
        api_key.account_kind = Some("apiKey".to_owned());
        assert_eq!(
            interpret(&api_key, NOW).observation.state,
            UsageState::Unknown
        );
        let mut anonymous = report(Some(false), &[(100, None)]);
        anonymous.account_identified = false;
        assert_eq!(
            interpret(&anonymous, NOW).observation.state,
            UsageState::Unknown
        );
    }

    fn fixture(limits: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("account.json"), r#"{"account":{"type":"chatgpt","email":"x","planType":"plus"},"requiresOpenaiAuth":true}"#).unwrap();
        std::fs::write(dir.path().join("limits.json"), limits).unwrap();
        dir
    }

    fn detect(dir: &tempfile::TempDir) -> UsageObservation {
        let exe = fake::install(dir.path());
        CodexUsageSignal::new(Some(exe))
            .detect(dir.path(), Path::new("/project"), "thread")
            .expect("detect never errors")
    }

    fn read(dir: &tempfile::TempDir) -> CodexUsageReading {
        let exe = fake::install(dir.path());
        CodexUsageSignal::new(Some(exe)).read(dir.path())
    }

    #[test]
    fn end_to_end_against_a_scripted_app_server_exhausted_available_and_failures() {
        let exhausted = fixture(
            r#"{"ordinaryUsageAllowed":false,"accountId":"a","rateLimits":{"primary":{"usedPercent":100,"resetsAt":4000000000},"secondary":{"usedPercent":20,"resetsAt":4000100000}}}"#,
        );
        let o = detect(&exhausted);
        assert_eq!(o.state, UsageState::Exhausted);
        assert_eq!(o.reset_unix_ms, Some(4_000_000_000_000));
        assert_eq!(o.evidence, UsageEvidence::ProviderRateLimitApi);

        let available = fixture(
            r#"{"ordinaryUsageAllowed":true,"accountId":"a","rateLimits":{"primary":{"usedPercent":1,"resetsAt":4000000000}}}"#,
        );
        assert_eq!(detect(&available).state, UsageState::Available);

        // app-server failures and a wrong-home server never produce a blocking state
        let dead = fixture("{}");
        std::fs::write(dead.path().join("mode"), "exit").unwrap();
        assert_eq!(detect(&dead).state, UsageState::Unknown);
        let wrong_home = fixture(
            r#"{"ordinaryUsageAllowed":false,"accountId":"a","rateLimits":{"primary":{"usedPercent":100}}}"#,
        );
        std::fs::write(wrong_home.path().join("mode"), "wronghome").unwrap();
        let o = detect(&wrong_home);
        assert_eq!(o.state, UsageState::Unknown);
        assert!(!o.state.is_blocking());
    }

    #[test]
    fn a_missing_codex_executable_is_unknown() {
        let dir = fixture("{}");
        let o = CodexUsageSignal::new(Some(PathBuf::from("/definitely/not/codex")))
            .detect(dir.path(), Path::new("/p"), "t")
            .expect("detect");
        assert_eq!(o.state, UsageState::Unknown);
    }

    #[test]
    fn rejected_auth_is_unknown_with_a_sanitized_category_and_profile_specific_recovery() {
        for auth_code in [
            "token_revoked",
            "invalid_token",
            "token_expired",
            "invalid_api_key",
        ] {
            let dir = fixture("{}");
            let body = serde_json::json!({"error":{"code":auth_code,"message":"SECRET_CANARY"},"status":401});
            let error = serde_json::json!({"code":-32603,"message":format!(
                "failed to fetch codex rate limits: GET https://chatgpt.com/backend-api/wham/usage failed: 401 Unauthorized; content-type=text/plain; body={body}"
            )});
            std::fs::write(dir.path().join("limits_error.json"), error.to_string()).unwrap();
            let reading = read(&dir);
            assert_eq!(reading.observation.state, UsageState::Unknown);
            assert_eq!(reading.max_used_percent, None);
            assert!(reading.diagnostic.unwrap().authentication_rejected());
            assert_eq!(
                reading.recovery_command("codex-main").as_deref(),
                Some("relay logout codex-main\nrelay login codex-main")
            );
            let display = format!("{:?} {}", reading, reading.refusal("codex-main"));
            assert!(!display.contains("SECRET_CANARY"));
            assert!(!display.contains("body="));
            assert!(!display.contains("https://"));
        }
    }

    #[test]
    fn timeout_schema_and_generic_errors_remain_unknown_without_login_advice() {
        for mode in ["hang", "schema", "unsupported", "provider"] {
            let dir = fixture("{}");
            let expected = match mode {
                "hang" => {
                    std::fs::write(dir.path().join("mode"), "hang").unwrap();
                    CodexUsageDiagnostic::Timeout
                }
                "unsupported" | "provider" => {
                    let code = if mode == "unsupported" {
                        -32601
                    } else {
                        -32603
                    };
                    std::fs::write(dir.path().join("limits_error.json"), serde_json::json!({"code":code,"message":"SECRET_CANARY token_revoked 401 please login"}).to_string()).unwrap();
                    if mode == "unsupported" {
                        CodexUsageDiagnostic::UnsupportedInterface
                    } else {
                        CodexUsageDiagnostic::ProviderFailure
                    }
                }
                _ => CodexUsageDiagnostic::UnsupportedSchema,
            };
            let reading = read(&dir);
            assert_eq!(reading.observation.state, UsageState::Unknown);
            assert_eq!(reading.diagnostic, Some(expected));
            assert!(reading.recovery_command("codex-main").is_none());
            let message = reading.refusal("codex-main").to_string();
            assert!(!message.contains("relay login"));
            assert!(!message.contains("SECRET_CANARY"));
        }
    }

    /// GitHub #13: `read()` is the one authoritative call adaptive polling seeds itself from — it
    /// must expose the same observation `detect()` would, plus the trustworthy percentage, from a
    /// single round trip.
    #[test]
    fn read_exposes_the_scheduling_hint_alongside_the_same_observation_detect_returns() {
        let near_limit = fixture(
            r#"{"ordinaryUsageAllowed":true,"accountId":"a","rateLimits":{"primary":{"usedPercent":99,"resetsAt":4000000000}}}"#,
        );
        let reading = read(&near_limit);
        assert_eq!(reading.observation.state, UsageState::NearLimit);
        assert_eq!(reading.max_used_percent, Some(99));

        let exhausted = fixture(
            r#"{"ordinaryUsageAllowed":false,"accountId":"a","rateLimits":{"primary":{"usedPercent":100,"resetsAt":4000000000}}}"#,
        );
        let reading = read(&exhausted);
        assert_eq!(reading.observation.state, UsageState::Exhausted);
        assert_eq!(reading.max_used_percent, None);

        let dead = fixture("{}");
        std::fs::write(dead.path().join("mode"), "exit").unwrap();
        let reading = read(&dead);
        assert_eq!(reading.observation.state, UsageState::Unknown);
        assert_eq!(reading.max_used_percent, None);
    }
}
