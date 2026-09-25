//! M2C.1: the single, pure usage-state decision policy. Nothing here touches the filesystem or a
//! provider; every input is passed in, so each rule is directly unit-testable.
//!
//! ## Policy
//!
//! `EXHAUSTED` — only when one of these holds, and never from anything ambiguous:
//! 1. a `rate_limit_event` with `status = rejected`, a reset time still in the future, not using
//!    overage, scoped to the account (or to the model family the workload runs), and not
//!    contradicted by a newer fresh statusline showing that window below 100%; or
//! 2. a matching `StopFailure(rate_limit)` record **and** a fresh statusline showing an account
//!    window (`five_hour`/`seven_day`) at ≥ 100% with a reset time still in the future; or
//! 3. an explicitly opt-in phrase match on a real limit message **and** that same fresh statusline
//!    corroboration.
//! 4. a named account-window `StopFailure` (`session` or `weekly`) and a fresh, same-native-session
//!    pre-failure statusline showing that exact window at or above the near-limit threshold with a
//!    reset still in the future. This covers Claude's limit modal replacing current windows with
//!    null before the detached watcher can inspect them.
//!
//! `NEAR_LIMIT` — a fresh statusline with an account window ≥ 90% (including ≥ 100% with no failure
//! event, since the session has not actually been refused), or a fresh `allowed_warning` event.
//!
//! `AVAILABLE` — a fresh statusline with every reported account window below 90%.
//!
//! `UNKNOWN` — everything else: no signal, a stale statusline, a `StopFailure` with nothing to
//! corroborate it (a bare `rate_limit` can be a transient 429 capacity error), a limit that does
//! not apply to this workload. `UNKNOWN` never triggers an automatic handoff.
//!
//! `RESET_PENDING` is not decided here: it is a property of the ledger (a profile previously
//! observed exhausted whose recorded reset time is still in the future), applied by the watch
//! coordinator.
//!
//! ### Model-specific limits
//! The statusline only reports account windows. A model-scoped limit (`seven_day_opus`,
//! `seven_day_sonnet`, the "Fable" window) exhausts the profile only when the operator declared a
//! workload model of that family (`--workload-model`); otherwise it is reported as `UNKNOWN` with
//! an explanatory note, because the other model families still work. A fast-mode limit is
//! non-blocking.

use std::path::Path;

use relay_core::usage::{UsageEvidence, UsageObservation, UsageState};

use crate::usage_signals::{
    LimitKind, LimitScope, ProfileSignals, RateLimitEventRecord, StatusLineSnapshot,
    StopFailureRecord, WindowUsage,
};

#[derive(Clone, Copy, Debug)]
pub struct PolicyConfig {
    /// A statusline snapshot older than this is stale. The statusline only refreshes while an
    /// interactive Claude session is drawing it, so this is deliberately short.
    pub statusline_max_age_ms: u64,
    pub stop_failure_max_age_ms: u64,
    pub near_limit_percentage: f64,
    pub exhausted_percentage: f64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            statusline_max_age_ms: 10 * 60 * 1000,
            stop_failure_max_age_ms: 30 * 60 * 1000,
            near_limit_percentage: 90.0,
            exhausted_percentage: 100.0,
        }
    }
}

pub struct PolicyInputs<'a> {
    pub now_unix_ms: u64,
    pub project_dir: &'a Path,
    pub session_id: &'a str,
    /// The model the watched workload runs, if the operator declared one.
    pub workload_model: Option<&'a str>,
    pub signals: &'a ProfileSignals,
    /// An explicitly opted-in phrase match on real limit text (e.g. from the probe). Only ever
    /// counts together with statusline corroboration.
    pub phrase_hit: Option<LimitKind>,
}

fn observation(
    state: UsageState,
    evidence: UsageEvidence,
    detected_via: impl Into<String>,
    now_unix_ms: u64,
    reset_unix_ms: Option<u64>,
) -> UsageObservation {
    UsageObservation {
        state,
        evidence,
        detected_via: detected_via.into(),
        observed_unix_ms: now_unix_ms,
        reset_unix_ms,
    }
}

fn unknown(now_unix_ms: u64, why: impl Into<String>) -> UsageObservation {
    observation(
        UsageState::Unknown,
        UsageEvidence::StatusLine,
        why,
        now_unix_ms,
        None,
    )
}

/// Whether a limit of this kind blocks the watched workload.
fn applies_to_workload(kind: LimitKind, workload_model: Option<&str>) -> bool {
    match kind.scope() {
        LimitScope::Account => true,
        LimitScope::Model(family) => {
            workload_model.is_some_and(|model| family.matches_model(model))
        }
        LimitScope::NonBlocking => false,
    }
}

fn is_fresh_statusline(snapshot: &StatusLineSnapshot, now: u64, config: &PolicyConfig) -> bool {
    snapshot.captured_unix_ms <= now.saturating_add(60_000)
        && now.saturating_sub(snapshot.captured_unix_ms) <= config.statusline_max_age_ms
}

/// Windows that are still open (reset in the future); an elapsed window says nothing about now.
fn open_windows(
    snapshot: &StatusLineSnapshot,
    now_unix_ms: u64,
) -> Vec<(&'static str, WindowUsage)> {
    [
        ("five_hour", snapshot.five_hour),
        ("seven_day", snapshot.seven_day),
    ]
    .into_iter()
    .filter_map(|(name, window)| window.map(|window| (name, window)))
    .filter(|(_, window)| window.resets_at.saturating_mul(1000) > now_unix_ms)
    .collect()
}

fn failure_applies(
    record: &StopFailureRecord,
    inputs: &PolicyInputs<'_>,
    config: &PolicyConfig,
) -> bool {
    if record.error != "rate_limit" {
        return false;
    }
    let age = inputs.now_unix_ms.saturating_sub(record.recorded_unix_ms);
    if age > config.stop_failure_max_age_ms || record.recorded_unix_ms > inputs.now_unix_ms + 60_000
    {
        return false;
    }
    let same_session = record.session_id.as_deref() == Some(inputs.session_id);
    let same_project = record
        .cwd
        .as_deref()
        .is_some_and(|cwd| same_path(Path::new(cwd), inputs.project_dir));
    if !(same_session || same_project) {
        return false;
    }
    // A bare `rate_limit` (no recognized limit text) is ambiguous but still eligible: it only
    // ever counts together with the statusline. A named model-scoped limit for another family
    // is not evidence against this workload.
    record
        .limit_kind
        .is_none_or(|kind| applies_to_workload(kind, inputs.workload_model))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn historical_window_for_failure(
    record: &StopFailureRecord,
    inputs: &PolicyInputs<'_>,
    config: &PolicyConfig,
) -> Option<(&'static str, WindowUsage)> {
    // Historical corroboration deliberately accepts only the two account windows with an exact
    // structured mapping. A bare rate_limit, generic account limit, or model-specific limit is
    // not enough to infer which statusline window the refusal refers to.
    let kind = record.limit_kind?;
    let name = match kind {
        LimitKind::Session => "five_hour",
        LimitKind::Weekly => "seven_day",
        _ => return None,
    };
    if record.session_id.as_deref() != Some(inputs.session_id) {
        return None;
    }
    inputs
        .signals
        .statusline_history
        .iter()
        .filter(|snapshot| {
            snapshot.session_id.as_deref() == Some(inputs.session_id)
                && snapshot.captured_unix_ms <= record.recorded_unix_ms
                && is_fresh_statusline(snapshot, inputs.now_unix_ms, config)
        })
        .filter_map(|snapshot| {
            let window = match kind {
                LimitKind::Session => snapshot.five_hour,
                LimitKind::Weekly => snapshot.seven_day,
                _ => return None,
            }?;
            (window.resets_at.saturating_mul(1000) > inputs.now_unix_ms
                && window.used_percentage >= config.near_limit_percentage)
                .then_some((snapshot.captured_unix_ms, window))
        })
        .max_by_key(|(captured, _)| *captured)
        .map(|(_, window)| (name, window))
}

fn historical_stop_failure_corroboration(
    inputs: &PolicyInputs<'_>,
    config: &PolicyConfig,
) -> Option<(LimitKind, &'static str, WindowUsage)> {
    inputs
        .signals
        .stop_failures
        .iter()
        .filter(|record| failure_applies(record, inputs, config))
        .filter_map(|record| {
            let kind = record.limit_kind?;
            historical_window_for_failure(record, inputs, config)
                .map(|(name, window)| (record.recorded_unix_ms, kind, name, window))
        })
        .max_by_key(|(recorded, _, _, _)| *recorded)
        .map(|(_, kind, name, window)| (kind, name, window))
}

fn rejected_event_is_current(
    event: &RateLimitEventRecord,
    inputs: &PolicyInputs<'_>,
    fresh_statusline: Option<&StatusLineSnapshot>,
) -> Option<(LimitKind, u64)> {
    if event.status != "rejected" || event.is_using_overage {
        return None;
    }
    let resets_at = event.resets_at?;
    if resets_at.saturating_mul(1000) <= inputs.now_unix_ms {
        return None;
    }
    let kind = event
        .rate_limit_type
        .as_deref()
        .and_then(LimitKind::from_rate_limit_type)?;
    if !applies_to_workload(kind, inputs.workload_model) {
        return None;
    }
    // A newer, fresh statusline that shows the very window below 100% means the limit has lifted
    // (plan change, overage, early reset), so the older rejection no longer holds.
    if let Some(snapshot) = fresh_statusline
        && snapshot.captured_unix_ms > event.recorded_unix_ms
    {
        let window = match kind {
            LimitKind::Session => snapshot.five_hour,
            LimitKind::Weekly => snapshot.seven_day,
            _ => None,
        };
        if window.is_some_and(|window| window.used_percentage < 100.0) {
            return None;
        }
    }
    Some((kind, resets_at))
}

#[must_use]
pub fn evaluate(inputs: &PolicyInputs<'_>, config: &PolicyConfig) -> UsageObservation {
    let now = inputs.now_unix_ms;
    let fresh = inputs
        .signals
        .statusline
        .as_ref()
        .filter(|snapshot| is_fresh_statusline(snapshot, now, config));

    // Rule 1: structured rejection.
    if let Some((kind, resets_at)) = inputs
        .signals
        .rate_limit_events
        .iter()
        .filter_map(|event| rejected_event_is_current(event, inputs, fresh))
        .max_by_key(|(_, resets_at)| *resets_at)
    {
        return observation(
            UsageState::Exhausted,
            UsageEvidence::RateLimitEvent,
            format!("rate_limit_event rejected ({kind:?}), resets at {resets_at}"),
            now,
            Some(resets_at.saturating_mul(1000)),
        );
    }

    // Preserve the current-statusline 100% path below as the stronger normal path. This is only
    // for the modal-era gap: an exact named refusal plus a recent matching pre-failure window.
    if let Some((kind, name, window)) = historical_stop_failure_corroboration(inputs, config) {
        let kind = match kind {
            LimitKind::Session => "session",
            LimitKind::Weekly => "weekly",
            _ => return unknown(now, "unsupported historical StopFailure limit kind"),
        };
        return observation(
            UsageState::Exhausted,
            UsageEvidence::StopFailureHistoricalCorroborated,
            format!(
                "StopFailure({kind}) corroborated by recent pre-modal {name} usage at {:.0}%",
                window.used_percentage
            ),
            now,
            Some(window.resets_at.saturating_mul(1000)),
        );
    }

    let Some(snapshot) = fresh else {
        let stale = inputs.signals.statusline.is_some();
        let failure_seen = inputs
            .signals
            .stop_failures
            .iter()
            .any(|record| failure_applies(record, inputs, config));
        return unknown(
            now,
            match (stale, failure_seen) {
                (_, true) => {
                    "StopFailure(rate_limit) seen but no fresh statusline to corroborate it"
                }
                (true, false) => "statusline snapshot is stale",
                (false, false) => "no structured usage signal recorded",
            },
        );
    };

    let windows = open_windows(snapshot, now);
    if windows.is_empty() {
        return unknown(now, "fresh statusline reports no open usage window");
    }
    let exhausted: Vec<_> = windows
        .iter()
        .filter(|(_, window)| window.used_percentage >= config.exhausted_percentage)
        .collect();

    if !exhausted.is_empty() {
        let reset_ms = exhausted
            .iter()
            .map(|(_, window)| window.resets_at.saturating_mul(1000))
            .max();
        let names = exhausted
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join("+");
        if inputs
            .signals
            .stop_failures
            .iter()
            .any(|record| failure_applies(record, inputs, config))
        {
            return observation(
                UsageState::Exhausted,
                UsageEvidence::StopFailureCorroborated,
                format!("StopFailure(rate_limit) corroborated by statusline {names} at 100%"),
                now,
                reset_ms,
            );
        }
        if let Some(kind) = inputs
            .phrase_hit
            .filter(|kind| applies_to_workload(*kind, inputs.workload_model))
        {
            return observation(
                UsageState::Exhausted,
                UsageEvidence::OutputPatternMatch,
                format!(
                    "opt-in phrase match ({kind:?}) corroborated by statusline {names} at 100%"
                ),
                now,
                reset_ms,
            );
        }
        return observation(
            UsageState::NearLimit,
            UsageEvidence::StatusLine,
            format!("statusline {names} at 100% but no refusal was observed"),
            now,
            reset_ms,
        );
    }

    let highest = windows
        .iter()
        .map(|(_, window)| window.used_percentage)
        .fold(0.0_f64, f64::max);
    let warned = inputs.signals.rate_limit_events.iter().any(|event| {
        event.status == "allowed_warning"
            && now.saturating_sub(event.recorded_unix_ms) <= config.statusline_max_age_ms
    });
    if highest >= config.near_limit_percentage || warned {
        return observation(
            UsageState::NearLimit,
            UsageEvidence::StatusLine,
            format!("statusline usage at {highest:.0}%"),
            now,
            None,
        );
    }
    observation(
        UsageState::Available,
        UsageEvidence::StatusLine,
        format!("statusline usage at {highest:.0}%"),
        now,
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use relay_core::usage::{UsageEvidence, UsageState};

    use super::{PolicyConfig, PolicyInputs, evaluate};
    use crate::usage_signals::{
        LimitKind, ProfileSignals, RateLimitEventRecord, StatusLineSnapshot, StopFailureRecord,
        WindowUsage,
    };

    const NOW: u64 = 1_800_000_000_000;
    const FUTURE_S: u64 = 1_800_000_000 + 3_600;
    const PAST_S: u64 = 1_800_000_000 - 3_600;

    fn window(pct: f64, resets_at: u64) -> Option<WindowUsage> {
        Some(WindowUsage {
            used_percentage: pct,
            resets_at,
        })
    }

    fn statusline(
        captured: u64,
        five: Option<WindowUsage>,
        seven: Option<WindowUsage>,
    ) -> StatusLineSnapshot {
        StatusLineSnapshot {
            captured_unix_ms: captured,
            session_id: Some("s1".to_owned()),
            model_id: None,
            five_hour: five,
            seven_day: seven,
        }
    }

    fn failure(kind: Option<LimitKind>, age_ms: u64) -> StopFailureRecord {
        StopFailureRecord {
            recorded_unix_ms: NOW - age_ms,
            session_id: Some("s1".to_owned()),
            cwd: Some("/project".to_owned()),
            error: "rate_limit".to_owned(),
            limit_kind: kind,
        }
    }

    fn event(status: &str, kind: &str, resets: Option<u64>, overage: bool) -> RateLimitEventRecord {
        RateLimitEventRecord {
            recorded_unix_ms: NOW - 1_000,
            session_id: Some("s1".to_owned()),
            status: status.to_owned(),
            rate_limit_type: Some(kind.to_owned()),
            resets_at: resets,
            utilization: None,
            is_using_overage: overage,
        }
    }

    fn run(
        signals: &ProfileSignals,
        model: Option<&str>,
        phrase: Option<LimitKind>,
    ) -> relay_core::usage::UsageObservation {
        evaluate(
            &PolicyInputs {
                now_unix_ms: NOW,
                project_dir: Path::new("/project"),
                session_id: "s1",
                workload_model: model,
                signals,
                phrase_hit: phrase,
            },
            &PolicyConfig::default(),
        )
    }

    #[test]
    fn no_signals_is_unknown() {
        assert_eq!(
            run(&ProfileSignals::default(), None, None).state,
            UsageState::Unknown
        );
    }

    #[test]
    fn rejected_event_with_future_reset_is_exhausted_with_reset_time() {
        let signals = ProfileSignals {
            rate_limit_events: vec![event("rejected", "five_hour", Some(FUTURE_S), false)],
            ..ProfileSignals::default()
        };
        let observation = run(&signals, None, None);
        assert_eq!(observation.state, UsageState::Exhausted);
        assert_eq!(observation.evidence, UsageEvidence::RateLimitEvent);
        assert_eq!(observation.reset_unix_ms, Some(FUTURE_S * 1000));
    }

    #[test]
    fn rejected_event_with_elapsed_or_missing_reset_is_not_exhausted() {
        for resets in [Some(PAST_S), None] {
            let signals = ProfileSignals {
                rate_limit_events: vec![event("rejected", "five_hour", resets, false)],
                ..ProfileSignals::default()
            };
            assert_eq!(run(&signals, None, None).state, UsageState::Unknown);
        }
    }

    #[test]
    fn overage_and_allowed_warning_are_never_exhausted() {
        let overage = ProfileSignals {
            rate_limit_events: vec![event("rejected", "five_hour", Some(FUTURE_S), true)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&overage, None, None).state, UsageState::Unknown);
        let warning = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, window(50.0, FUTURE_S), None)),
            rate_limit_events: vec![event("allowed_warning", "five_hour", Some(FUTURE_S), false)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&warning, None, None).state, UsageState::NearLimit);
    }

    #[test]
    fn model_specific_rejection_only_exhausts_a_matching_workload() {
        let signals = ProfileSignals {
            rate_limit_events: vec![event("rejected", "seven_day_opus", Some(FUTURE_S), false)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::Unknown);
        assert_eq!(
            run(&signals, Some("claude-sonnet-5"), None).state,
            UsageState::Unknown
        );
        assert_eq!(
            run(&signals, Some("claude-opus-5"), None).state,
            UsageState::Exhausted
        );
    }

    #[test]
    fn a_newer_fresh_statusline_below_100_contradicts_an_older_rejection() {
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 10, window(20.0, FUTURE_S), None)),
            rate_limit_events: vec![event("rejected", "five_hour", Some(FUTURE_S), false)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::Available);
    }

    #[test]
    fn stop_failure_alone_is_never_exhausted() {
        let signals = ProfileSignals {
            stop_failures: vec![failure(Some(LimitKind::Session), 1_000)],
            ..ProfileSignals::default()
        };
        let observation = run(&signals, None, None);
        assert_eq!(observation.state, UsageState::Unknown);
    }

    #[test]
    fn stop_failure_plus_fresh_statusline_at_100_is_exhausted() {
        let signals = ProfileSignals {
            statusline: Some(statusline(
                NOW - 5_000,
                window(100.0, FUTURE_S),
                window(30.0, FUTURE_S + 9),
            )),
            stop_failures: vec![failure(None, 1_000)],
            ..ProfileSignals::default()
        };
        let observation = run(&signals, None, None);
        assert_eq!(observation.state, UsageState::Exhausted);
        assert_eq!(observation.evidence, UsageEvidence::StopFailureCorroborated);
        assert_eq!(observation.reset_unix_ms, Some(FUTURE_S * 1000));
    }

    #[test]
    fn weekly_stop_failure_uses_matching_pre_modal_history() {
        let before_modal = statusline(NOW - 5_000, None, window(98.0, FUTURE_S));
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, None, None)),
            statusline_history: vec![before_modal, statusline(NOW - 1_000, None, None)],
            stop_failures: vec![failure(Some(LimitKind::Weekly), 500)],
            ..ProfileSignals::default()
        };
        let observation = run(&signals, None, None);
        assert_eq!(observation.state, UsageState::Exhausted);
        assert_eq!(
            observation.evidence,
            UsageEvidence::StopFailureHistoricalCorroborated
        );
        assert_eq!(observation.reset_unix_ms, Some(FUTURE_S * 1000));
        assert_eq!(
            observation.detected_via,
            "StopFailure(weekly) corroborated by recent pre-modal seven_day usage at 98%"
        );
    }

    #[test]
    fn session_stop_failure_uses_matching_pre_modal_history() {
        let signals = ProfileSignals {
            statusline_history: vec![statusline(NOW - 5_000, window(97.0, FUTURE_S), None)],
            stop_failures: vec![failure(Some(LimitKind::Session), 500)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::Exhausted);
    }

    #[test]
    fn historical_corroboration_requires_named_matching_fresh_same_session_window() {
        let weekly_history = statusline(NOW - 5_000, None, window(98.0, FUTURE_S));
        let bare = ProfileSignals {
            statusline_history: vec![weekly_history.clone()],
            stop_failures: vec![failure(None, 500)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&bare, None, None).state, UsageState::Unknown);

        let wrong_window = ProfileSignals {
            statusline_history: vec![weekly_history.clone()],
            stop_failures: vec![failure(Some(LimitKind::Session), 500)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&wrong_window, None, None).state, UsageState::Unknown);

        let stale = ProfileSignals {
            statusline_history: vec![statusline(
                NOW - 11 * 60 * 1000,
                None,
                window(98.0, FUTURE_S),
            )],
            stop_failures: vec![failure(Some(LimitKind::Weekly), 500)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&stale, None, None).state, UsageState::Unknown);

        let mut other_session = weekly_history;
        other_session.session_id = Some("other".to_owned());
        let other = ProfileSignals {
            statusline_history: vec![other_session],
            stop_failures: vec![failure(Some(LimitKind::Weekly), 500)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&other, None, None).state, UsageState::Unknown);
    }

    #[test]
    fn history_never_exhausts_without_a_refusal() {
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, None, window(98.0, FUTURE_S))),
            statusline_history: vec![statusline(NOW - 1_000, None, window(98.0, FUTURE_S))],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::NearLimit);
    }

    #[test]
    fn a_stale_statusline_never_corroborates() {
        let signals = ProfileSignals {
            statusline: Some(statusline(
                NOW - 11 * 60 * 1000,
                window(100.0, FUTURE_S),
                None,
            )),
            stop_failures: vec![failure(Some(LimitKind::Session), 1_000)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::Unknown);
    }

    #[test]
    fn a_statusline_window_whose_reset_has_passed_is_ignored() {
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, window(100.0, PAST_S), None)),
            stop_failures: vec![failure(None, 1_000)],
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::Unknown);
    }

    #[test]
    fn an_old_stop_failure_or_one_for_another_session_does_not_count() {
        let mut other = failure(None, 1_000);
        other.session_id = Some("other".to_owned());
        other.cwd = Some("/elsewhere".to_owned());
        for record in [failure(None, 31 * 60 * 1000), other] {
            let signals = ProfileSignals {
                statusline: Some(statusline(NOW - 1_000, window(100.0, FUTURE_S), None)),
                stop_failures: vec![record],
                ..ProfileSignals::default()
            };
            assert_eq!(run(&signals, None, None).state, UsageState::NearLimit);
        }
    }

    #[test]
    fn statusline_at_100_without_a_refusal_is_near_limit_not_exhausted() {
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, window(100.0, FUTURE_S), None)),
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::NearLimit);
    }

    #[test]
    fn near_limit_threshold_and_available_below_it() {
        let near = ProfileSignals {
            statusline: Some(statusline(
                NOW - 1_000,
                window(90.0, FUTURE_S),
                window(10.0, FUTURE_S),
            )),
            ..ProfileSignals::default()
        };
        assert_eq!(run(&near, None, None).state, UsageState::NearLimit);
        let fine = ProfileSignals {
            statusline: Some(statusline(
                NOW - 1_000,
                window(89.9, FUTURE_S),
                window(10.0, FUTURE_S),
            )),
            ..ProfileSignals::default()
        };
        assert_eq!(run(&fine, None, None).state, UsageState::Available);
    }

    #[test]
    fn a_named_model_limit_for_another_family_does_not_corroborate() {
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, window(100.0, FUTURE_S), None)),
            stop_failures: vec![failure(Some(LimitKind::Opus), 1_000)],
            ..ProfileSignals::default()
        };
        assert_eq!(
            run(&signals, Some("claude-sonnet-5"), None).state,
            UsageState::NearLimit
        );
        assert_eq!(
            run(&signals, Some("claude-opus-5"), None).state,
            UsageState::Exhausted
        );
    }

    #[test]
    fn phrase_match_needs_corroboration_and_applicability() {
        let uncorroborated = ProfileSignals::default();
        assert_eq!(
            run(&uncorroborated, None, Some(LimitKind::Session)).state,
            UsageState::Unknown
        );
        let corroborated = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, window(100.0, FUTURE_S), None)),
            ..ProfileSignals::default()
        };
        let observation = run(&corroborated, None, Some(LimitKind::Session));
        assert_eq!(observation.state, UsageState::Exhausted);
        assert_eq!(observation.evidence, UsageEvidence::OutputPatternMatch);
        assert_eq!(
            run(&corroborated, None, Some(LimitKind::Fast)).state,
            UsageState::NearLimit
        );
    }

    #[test]
    fn statusline_without_windows_is_unknown() {
        let signals = ProfileSignals {
            statusline: Some(statusline(NOW - 1_000, None, None)),
            ..ProfileSignals::default()
        };
        assert_eq!(run(&signals, None, None).state, UsageState::Unknown);
    }
}
