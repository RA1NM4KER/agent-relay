//! GitHub #13: adaptive polling cadence for a supervised Codex terminal.
//!
//! Codex has no `StopFailure`-style exhaustion event, so Relay periodically asks its structured
//! `account/rateLimits/read` interface whether the account is close to its limit (see
//! `crate::usage`). A real live incident found the default 120s cadence let up to ~10-20s of
//! visible, already-exhausted Codex UI pass before Relay ever noticed. This module is the pure,
//! provider-typed cadence policy: given the highest *trustworthy* usedPercent from one read, how
//! long until the next one. It owns no filesystem, process, or environment state — the CLI
//! supervision layer (`relay-cli::codex_poll`) owns scheduling itself, including the
//! `RELAY_CODEX_POLL_SECS` override and single-poll-in-flight guarantee.

/// Comfortable/default cadence: normal usage, or usage that could not be trustworthily read at
/// all. Also `relay-cli`'s existing `RELAY_CODEX_POLL_SECS` default.
pub const COMFORTABLE_POLL_SECS: u64 = 120;
/// `>= 90%` and `< 98%` trustworthy usage.
pub const ELEVATED_POLL_SECS: u64 = 15;
/// `>= 98%` trustworthy usage: the fast final approach to the limit.
pub const CRITICAL_POLL_SECS: u64 = 5;

const ELEVATED_THRESHOLD_PERCENT: u32 = 90;
const CRITICAL_THRESHOLD_PERCENT: u32 = 98;

/// The next polling interval for a supervised Codex terminal, given the highest trustworthy
/// `usedPercent` from the most recent read.
///
/// `max_used_percent` must already be gated as trustworthy by the caller — it may only be `Some`
/// for a validated `ordinaryUsageAllowed == true` reading (see
/// [`crate::usage::CodexUsageReading::max_used_percent`]'s own doc comment). `None` (unknown,
/// untrustworthy, or not yet read) always selects the conservative default: this function never
/// infers exhaustion, or anything else, from a percentage — it only ever picks how soon to look
/// again.
#[must_use]
pub const fn poll_interval_secs(max_used_percent: Option<u32>) -> u64 {
    match max_used_percent {
        Some(percent) if percent >= CRITICAL_THRESHOLD_PERCENT => CRITICAL_POLL_SECS,
        Some(percent) if percent >= ELEVATED_THRESHOLD_PERCENT => ELEVATED_POLL_SECS,
        _ => COMFORTABLE_POLL_SECS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comfortable_usage_stays_on_the_slow_default() {
        assert_eq!(poll_interval_secs(Some(0)), COMFORTABLE_POLL_SECS);
        assert_eq!(poll_interval_secs(Some(42)), COMFORTABLE_POLL_SECS);
        assert_eq!(poll_interval_secs(Some(89)), COMFORTABLE_POLL_SECS);
    }

    #[test]
    fn ninety_percent_is_the_elevated_boundary() {
        assert_eq!(poll_interval_secs(Some(90)), ELEVATED_POLL_SECS);
        assert_eq!(poll_interval_secs(Some(97)), ELEVATED_POLL_SECS);
    }

    #[test]
    fn ninety_eight_percent_is_the_critical_boundary() {
        assert_eq!(poll_interval_secs(Some(98)), CRITICAL_POLL_SECS);
        assert_eq!(poll_interval_secs(Some(99)), CRITICAL_POLL_SECS);
        assert_eq!(poll_interval_secs(Some(100)), CRITICAL_POLL_SECS);
    }

    #[test]
    fn unknown_never_infers_urgency_from_a_percentage_it_does_not_have() {
        assert_eq!(poll_interval_secs(None), COMFORTABLE_POLL_SECS);
    }

    #[test]
    fn a_fresh_lower_reading_after_a_reset_relaxes_cadence_again() {
        assert_eq!(poll_interval_secs(Some(99)), CRITICAL_POLL_SECS);
        assert_eq!(poll_interval_secs(Some(42)), COMFORTABLE_POLL_SECS);
    }

    #[test]
    fn only_the_highest_trustworthy_window_controls_cadence() {
        // The caller is responsible for reducing multiple windows to one `max_used_percent`
        // before calling this function (see `usage::interpret`) — this only proves the function
        // itself treats its single input as already-maximal, i.e. it never second-guesses it.
        let windows = [30u32, 98u32];
        let highest_of_two_windows = windows.into_iter().max();
        assert_eq!(
            poll_interval_secs(highest_of_two_windows),
            CRITICAL_POLL_SECS
        );
    }
}
