//! M2C: a provider-neutral usage/rate-limit-exhaustion signal. `relay-core` knows nothing about
//! how Claude (or any provider) reports usage state; it only defines the shape of an observation
//! and the states automatic handoff reacts to. `relay-provider-claude` supplies the real,
//! tiered-detection implementation; tests and live fault-injection supply a fake/simulated one.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;

/// Mirrors the M2C spec's five states. Only [`UsageState::Exhausted`] (and, by extension,
/// [`UsageState::ResetPending`] — a profile already known to be in its exhaustion window) may
/// ever automatically trigger a handoff; [`UsageState::Unknown`] must always fail closed rather
/// than guess.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UsageState {
    Available,
    NearLimit,
    Exhausted,
    /// A profile previously observed [`UsageState::Exhausted`] with a recorded reset time that
    /// has not yet passed. Distinct from a fresh `Exhausted` reading only for reporting clarity;
    /// both are treated identically as blocking by [`UsageState::is_blocking`].
    ResetPending,
    Unknown,
}

impl UsageState {
    /// True for any state that must never be automatically selected as a handoff target, and
    /// that (for the profile currently holding the writer lease) is sufficient grounds to
    /// consider an automatic handoff away from it.
    #[must_use]
    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::Exhausted | Self::ResetPending)
    }
}

/// Which detection mechanism produced an observation, in the spec's priority order. Never
/// implies a particular confidence beyond what [`UsageState`] itself already encodes — this is
/// for auditability (the journal/ledger never records raw provider output, only this).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageEvidence {
    /// Claude Code's own structured session bookkeeping (`claude agents --json`).
    StructuredSessionState,
    /// A structured JSON result from a real, explicit, content-free probe invocation
    /// (`is_error`/`result` fields — the same schema the handoff verification canary parses).
    StructuredProbeResult,
    /// A conservative, allowlisted phrase match against a probe's raw output when it did not
    /// parse as the structured schema above. The last-resort fallback tier.
    OutputPatternMatch,
    /// Injected by an operator or test for controlled validation; never produced by a real
    /// provider adapter's own detection path.
    Simulated,
    /// A `rate_limit_event` from Claude Code's structured stream (`status = rejected` with a
    /// current reset window), whether captured from a headless process Relay controls or from
    /// the explicit probe. Strong on its own.
    RateLimitEvent,
    /// A `StopFailure(rate_limit)` hook record corroborated by a fresh statusline snapshot that
    /// shows the relevant window at or above 100% with a reset time still in the future.
    StopFailureCorroborated,
    /// A fresh statusline `rate_limits` snapshot on its own (used for `AVAILABLE`/`NEAR_LIMIT`
    /// readings, never sufficient by itself for `EXHAUSTED`).
    StatusLine,
}

/// A single usage/exhaustion observation for one profile. Never contains raw provider output —
/// only the resolved state, which tier produced it, and a short, secret-free description.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageObservation {
    pub state: UsageState,
    pub evidence: UsageEvidence,
    /// Short, human-readable, secret-free description of what was checked (e.g. "agents --json
    /// state=rate_limited", "probe result matched phrase: usage limit reached"). Never the raw
    /// provider output itself.
    pub detected_via: String,
    pub observed_unix_ms: u64,
    pub reset_unix_ms: Option<u64>,
}

impl UsageObservation {
    #[must_use]
    pub fn unknown(observed_unix_ms: u64, detected_via: impl Into<String>) -> Self {
        Self {
            state: UsageState::Unknown,
            evidence: UsageEvidence::StructuredSessionState,
            detected_via: detected_via.into(),
            observed_unix_ms,
            reset_unix_ms: None,
        }
    }
}

/// Read-only: what is this profile's current usage state? `relay-core` never knows how to ask a
/// provider this; `relay-provider-claude` supplies the real, tiered implementation.
pub trait UsageSignal: Send + Sync {
    fn detect(
        &self,
        config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<UsageObservation>;
}

#[cfg(test)]
mod tests {
    use super::UsageState;

    #[test]
    fn only_exhausted_and_reset_pending_are_blocking() {
        assert!(UsageState::Exhausted.is_blocking());
        assert!(UsageState::ResetPending.is_blocking());
        assert!(!UsageState::Available.is_blocking());
        assert!(!UsageState::NearLimit.is_blocking());
        assert!(!UsageState::Unknown.is_blocking());
    }
}
