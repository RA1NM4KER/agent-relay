//! GitHub #16: pure, typed decoding for the two structured Codex app-server signals a live,
//! subscribed connection can observe from the *same interactive runtime* the TUI is using — never
//! an authority on their own (see [`crate::usage`] for the one authoritative verdict,
//! `ordinaryUsageAllowed` from `account/rateLimits/read`, which every path here still defers to).
//!
//! Live-verified against `codex-cli 0.155.0` (agent-relay#15's research pass): a passive,
//! explicitly-subscribed connection to an externally-listening `codex app-server` receives these
//! notifications for a thread it did not itself drive, with no PTY, no ANSI, no terminal text
//! involved. This module only decodes whatever JSON already arrived over that connection — it
//! never spawns a process, opens a socket, or makes a provider call.
//!
//! Both decoders read from `serde_json::Value` rather than a strict, fully-typed enum
//! deserialization on purpose: the app-server's own schema (`codex app-server
//! generate-json-schema`) shows `CodexErrorInfo` as a `oneOf` of a bare-string enum *and* several
//! object variants Relay has no use for, and the exact same latitude protects against a future
//! Codex version adding new enum members or notification fields Relay has never heard of. Every
//! unrecognized shape decodes to "not the signal we're looking for," never an error — GitHub
//! #16's own requirement that unknown notifications stay inert.

use serde_json::Value;

/// Codex's stable, camelCase, structured error classification (`codexErrorInfo` on a turn's
/// error), confirmed present in `codex app-server generate-json-schema --experimental`'s
/// `CodexErrorInfo` definition. Only the bare-string form is ever treated as a match; every other
/// shape (an object variant such as `{"httpConnectionFailed": {...}}`, a string this list does
/// not name, or anything malformed) is inert. Relay only ever acts on
/// [`Self::UsageLimitExceeded`] — the rest exist purely so a decoder can name what it saw for a
/// diagnostic, never to drive behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexErrorInfo {
    UsageLimitExceeded,
    ContextWindowExceeded,
    RateLimitExceeded,
    SessionBudgetExceeded,
    ServerOverloaded,
    Other,
}

impl CodexErrorInfo {
    /// Parses the bare-string enum form only. Any object variant, unrecognized string, or
    /// non-string value is [`None`] — inert, not an error.
    #[must_use]
    fn from_value(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "usageLimitExceeded" => Some(Self::UsageLimitExceeded),
            "contextWindowExceeded" => Some(Self::ContextWindowExceeded),
            "rateLimitExceeded" => Some(Self::RateLimitExceeded),
            "sessionBudgetExceeded" => Some(Self::SessionBudgetExceeded),
            "serverOverloaded" => Some(Self::ServerOverloaded),
            // Every other named or unrecognized variant collapses to `Other`: Relay never
            // branches behavior on any of them, only on `UsageLimitExceeded` (see
            // `is_usage_limit_exceeded`), so a future Codex version adding new members here can
            // never silently change what Relay does.
            _ => Some(Self::Other),
        }
    }
}

/// The one thing this whole module exists to answer: did a `codexErrorInfo` value name a usage
/// limit, and nothing else. `params` is the raw JSON of a `error`/`turn/completed`/`item/completed`
/// notification's own `error`/`item.error`-shaped field, or any ancestor object that directly
/// contains a `codexErrorInfo` key — callers pass whichever JSON object they actually received
/// without needing to pre-validate its shape.
///
/// This is a wake-up hint only. Receiving `true` must trigger the existing authoritative
/// `account/rateLimits/read` evaluation (GitHub #13's unchanged `ordinaryUsageAllowed == false`
/// rule decides exhaustion); it must never itself become a handoff decision.
#[must_use]
pub fn is_usage_limit_exceeded(error: &Value) -> bool {
    error
        .get("codexErrorInfo")
        .and_then(CodexErrorInfo::from_value)
        == Some(CodexErrorInfo::UsageLimitExceeded)
}

/// Convenience for the exact notification shape the app-server sends: `{"method":"error",
/// "params":{"error":{"codexErrorInfo":...}, "threadId":..., "turnId":..., "willRetry":...}}`.
/// Callers on a live connection pass the notification's `params` object directly. Any missing or
/// malformed field is inert (`false`), never a panic or an error the caller must handle.
#[must_use]
pub fn error_notification_is_usage_limit_exceeded(notification_params: &Value) -> bool {
    notification_params
        .get("error")
        .is_some_and(is_usage_limit_exceeded)
}

/// The highest `usedPercent` across `account/rateLimits/updated`'s `primary`/`secondary` windows —
/// a sanitized scheduling hint only, mirroring `crate::usage::interpret`'s own "highest window"
/// rule so the two paths can never develop separate threshold logic (GitHub #16's explicit
/// requirement). `params` is the notification's own `params` object,
/// `{"rateLimits": {"primary": {...}, "secondary": {...}, ...}}`.
///
/// Deliberately **cannot** return anything resembling `ordinaryUsageAllowed`: live-verified
/// (agent-relay#15) that this notification's own schema
/// (`AccountRateLimitsUpdatedNotification`/`RateLimitSnapshot`) never carries that field or an
/// account id — Codex's own protocol design already refuses to make this notification
/// authoritative, independent of any policy choice Relay makes here. This function only ever
/// feeds `crate::polling::poll_interval_secs`'s existing cadence policy; it must never gate a
/// handoff decision on its own.
#[must_use]
pub fn max_used_percent_from_rate_limits_updated(params: &Value) -> Option<u32> {
    let rate_limits = params.get("rateLimits")?;
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| rate_limits.get(key))
        .filter_map(|window| window.get("usedPercent"))
        .filter_map(Value::as_i64)
        .filter_map(|percent| u32::try_from(percent.clamp(0, 1000)).ok())
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_bare_usage_limit_exceeded_string_is_detected() {
        let error = json!({"codexErrorInfo": "usageLimitExceeded", "message": "quota"});
        assert!(is_usage_limit_exceeded(&error));
    }

    #[test]
    fn every_other_named_enum_member_is_inert() {
        for name in [
            "contextWindowExceeded",
            "rateLimitExceeded",
            "sessionBudgetExceeded",
            "serverOverloaded",
            "cyberPolicy",
            "misalignmentPolicyViolation",
            "internalServerError",
            "unauthorized",
            "badRequest",
            "threadRollbackFailed",
            "sandboxError",
            "other",
        ] {
            let error = json!({"codexErrorInfo": name});
            assert!(!is_usage_limit_exceeded(&error), "{name} must be inert");
        }
    }

    #[test]
    fn an_object_variant_codex_error_info_is_inert_not_a_panic() {
        let error = json!({"codexErrorInfo": {"httpConnectionFailed": {"httpStatusCode": 503}}});
        assert!(!is_usage_limit_exceeded(&error));
    }

    #[test]
    fn a_future_unrecognized_string_enum_member_is_inert() {
        let error = json!({"codexErrorInfo": "someBrandNewErrorKindFromAFutureCodex"});
        assert!(!is_usage_limit_exceeded(&error));
    }

    #[test]
    fn missing_or_malformed_input_is_inert_never_a_panic() {
        assert!(!is_usage_limit_exceeded(&json!({})));
        assert!(!is_usage_limit_exceeded(&json!(null)));
        assert!(!is_usage_limit_exceeded(&json!("not an object")));
        assert!(!is_usage_limit_exceeded(&json!({"codexErrorInfo": null})));
        assert!(!is_usage_limit_exceeded(&json!({"codexErrorInfo": 42})));
    }

    #[test]
    fn the_full_error_notification_shape_decodes_correctly() {
        let params = json!({
            "error": {"codexErrorInfo": "usageLimitExceeded", "message": "You've hit your limit"},
            "threadId": "t1",
            "turnId": "u1",
            "willRetry": false,
        });
        assert!(error_notification_is_usage_limit_exceeded(&params));

        let other = json!({
            "error": {"codexErrorInfo": "serverOverloaded"},
            "threadId": "t1",
            "turnId": "u1",
            "willRetry": true,
        });
        assert!(!error_notification_is_usage_limit_exceeded(&other));

        assert!(!error_notification_is_usage_limit_exceeded(&json!({})));
    }

    #[test]
    fn rate_limits_updated_hint_takes_the_highest_window() {
        let params = json!({
            "rateLimits": {
                "primary": {"usedPercent": 42, "windowDurationMins": 300, "resetsAt": 1},
                "secondary": {"usedPercent": 91, "resetsAt": 2},
            }
        });
        assert_eq!(max_used_percent_from_rate_limits_updated(&params), Some(91));
    }

    #[test]
    fn rate_limits_updated_hint_never_carries_ordinary_usage_allowed_or_account_id() {
        // Live-verified shape (agent-relay#15): the notification's own schema has neither field.
        // This test documents that the decoder only ever looks at `usedPercent` and cannot be
        // made to yield an authoritative verdict even if a caller mistakenly passed a full
        // `account/rateLimits/read` response (which DOES carry `ordinaryUsageAllowed`) instead —
        // the return type itself (`Option<u32>`) structurally cannot express that field.
        let full_read_response = json!({
            "ordinaryUsageAllowed": false,
            "accountId": "acct-123",
            "rateLimits": {"primary": {"usedPercent": 100, "resetsAt": 1}},
        });
        assert_eq!(
            max_used_percent_from_rate_limits_updated(&full_read_response),
            Some(100)
        );
    }

    #[test]
    fn missing_or_malformed_rate_limits_is_none_never_a_panic() {
        assert_eq!(max_used_percent_from_rate_limits_updated(&json!({})), None);
        assert_eq!(
            max_used_percent_from_rate_limits_updated(&json!(null)),
            None
        );
        assert_eq!(
            max_used_percent_from_rate_limits_updated(&json!({"rateLimits": {}})),
            None
        );
        assert_eq!(
            max_used_percent_from_rate_limits_updated(
                &json!({"rateLimits": {"primary": {"usedPercent": "not a number"}}})
            ),
            None
        );
    }

    #[test]
    fn an_unknown_notification_shape_never_panics_either_decoder() {
        let weird = json!({"unexpected": [1, 2, {"nested": true}], "codexErrorInfo": 9.99});
        assert!(!is_usage_limit_exceeded(&weird));
        assert_eq!(max_used_percent_from_rate_limits_updated(&weird), None);
    }
}
