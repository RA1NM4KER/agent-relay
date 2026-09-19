//! Interoperability rules between Agent Relay and the existing `herdr-claude-auto-retry` /
//! `herdr-agent-usage` plugins (see `docs/herdr-integration.md`).
//!
//! Agent Relay must not become another generic 429-retry mechanism or another quota dashboard —
//! those plugins already do that job well and Relay must not race them. This module is a pure
//! classifier: it never reads Relay's own usage state itself, and it makes no decision Relay's
//! own `usage_policy.rs` doesn't already make. It exists only so a Herdr plugin action/event
//! handler can decide, from a Relay-reported [`relay_core`]-shaped usage state, whether it is
//! Relay's turn to act at all.
//!
//! Precedence (see `docs/herdr-integration.md#auto-retry--usage-plugin-interoperability`):
//!
//! | Condition                                            | Owner                              |
//! |-------------------------------------------------------|-----------------------------------|
//! | Transient rate limit / 5xx / overload (a bare 429, no corroborating fresh signal) | `herdr-claude-auto-retry` may retry in place; Relay does nothing |
//! | `NEAR_LIMIT`                                          | Informational only; no migration  |
//! | `EXHAUSTED` / `RESET_PENDING`, corroborated, future reset window | Relay may perform a profile handoff |
//!
//! Relay's own `relay watch run` already refuses to act on anything but a corroborated
//! `EXHAUSTED`/`RESET_PENDING` reading (see `usage_policy.rs`), so in practice this classifier is
//! a *display/gating* helper for the plugin UI, not a second enforcement point: even if a caller
//! ignored this module entirely and called `watch run` unconditionally, Relay's own policy would
//! still refuse to hand off on a transient or near-limit reading. This module exists so the
//! plugin does not even *offer* a handoff action, or does not race a simultaneous auto-retry
//! attempt, while a transient condition is still resolving.

use serde::{Deserialize, Serialize};

/// Mirrors `relay_core::usage::UsageState`'s wire representation (`profile.status`'s
/// `availability.state`, itself `relay_core::model::Availability`, and `watch.run`'s
/// `no_action_needed.source_usage`) without depending on `relay-core` directly, since this
/// crate treats Relay's CLI/JSON surface as the stable contract, not its internal types.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReportedUsageState {
    Available,
    NearLimit,
    Exhausted,
    ResetPending,
    Unknown,
}

/// Who is responsible for acting on the current condition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActingParty {
    /// A transient 429/5xx/overload signal with no corroborating fresh evidence of a real
    /// account-level limit. `herdr-claude-auto-retry` (or an equivalent in-place retry) owns
    /// this; Relay never treats a bare transient error as exhaustion (matches `usage_policy.rs`:
    /// `error=rate_limit` alone is also produced for generic 429 capacity errors and is never
    /// trusted alone).
    AutoRetry,
    /// Approaching a limit but not yet corroborated as exhausted. Surface it to the developer
    /// (a `herdr-agent-usage`-style meter is exactly right for this); never migrate a profile
    /// on this alone.
    InformationalOnly,
    /// A corroborated exhaustion (or a recorded reset window still in the future). This is the
    /// one condition where Agent Relay's transactional handoff is the correct next action.
    AgentRelay,
    /// Not enough signal to classify safely; do nothing rather than guess.
    Unknown,
}

/// Classifies a Relay-reported usage state for **display/gating purposes only** in a Herdr
/// plugin. The actual accept/refuse decision for a real handoff always still runs through
/// `relay watch run`'s own policy — this function must never be used to bypass that call, only
/// to decide whether to *offer* it (or to stay out of `herdr-claude-auto-retry`'s way).
#[must_use]
pub fn acting_party(state: ReportedUsageState) -> ActingParty {
    match state {
        ReportedUsageState::Available => ActingParty::Unknown,
        ReportedUsageState::NearLimit => ActingParty::InformationalOnly,
        ReportedUsageState::Exhausted | ReportedUsageState::ResetPending => ActingParty::AgentRelay,
        ReportedUsageState::Unknown => ActingParty::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{ActingParty, ReportedUsageState, acting_party};

    #[test]
    fn near_limit_is_informational_only() {
        assert_eq!(
            acting_party(ReportedUsageState::NearLimit),
            ActingParty::InformationalOnly
        );
    }

    #[test]
    fn exhausted_and_reset_pending_belong_to_relay() {
        assert_eq!(
            acting_party(ReportedUsageState::Exhausted),
            ActingParty::AgentRelay
        );
        assert_eq!(
            acting_party(ReportedUsageState::ResetPending),
            ActingParty::AgentRelay
        );
    }

    #[test]
    fn available_and_unknown_are_not_relays_turn() {
        assert_eq!(
            acting_party(ReportedUsageState::Available),
            ActingParty::Unknown
        );
        assert_eq!(
            acting_party(ReportedUsageState::Unknown),
            ActingParty::Unknown
        );
    }
}
