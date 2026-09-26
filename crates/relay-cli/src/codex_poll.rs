//! GitHub #13: adaptive polling cadence for a supervised Codex terminal, plus the tiny sanitized
//! feedback path that lets it work.
//!
//! Codex has no `StopFailure`-style exhaustion event (see `auto_handoff`'s module doc), so while a
//! Codex terminal is in the foreground, [`crate::terminal_session::run_managed_terminal`]
//! periodically starts the same bounded, detached one-shot evaluation the Claude hook starts
//! (`auto_handoff::plan_poll` + `auto_handoff::spawn_detached`) — never in-process, so a ~0.7-1.7s
//! structured `account/rateLimits/read` round trip never freezes the user's terminal.
//!
//! A real live incident found the *default* cadence (120s) let 10-20s of already-exhausted Codex
//! UI pass, visibly, before Relay ever reacted. [`CodexPollScheduler`] fixes that by adapting the
//! interval to how close the account's own structured read says it is to its limit
//! (`relay_provider_codex::polling::poll_interval_secs`) — but the read that would tell it that
//! only ever happens inside the detached child process, never in this supervisor. The gap: give
//! the detached evaluator (`relay watch run`, see `crate::commands::watch`) a tiny, sanitized,
//! file-based way to publish what it just read back to the one supervisor that is allowed to
//! spawn another one ([`CodexPollDiagnostic`]) — never raw provider output, an account id, or
//! conversation content; never a second provider call merely to learn it.
//!
//! [`CodexPollScheduler`] itself never runs anything and never decides the routing/handoff
//! outcome (`relay_core::automation` remains the sole authority for that, unchanged) — it only
//! ever decides how long to wait before asking again, and guarantees at most one such evaluation
//! is ever in flight for the terminal it belongs to.

use std::{
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, Instant},
};

use relay_core::usage::UsageState;
use relay_provider_codex::polling::{COMFORTABLE_POLL_SECS, poll_interval_secs};
use serde::{Deserialize, Serialize};

use crate::auto_handoff::AutoWatchPlan;

/// Preserved diagnostic/operator override, unchanged in spelling from before GitHub #13. Absent:
/// adaptive cadence. `0`: periodic polling stays disabled, exactly as before. `N > 0`: a fixed
/// interval override, exactly as before — adaptive cadence is disabled for that process only.
pub(crate) const CODEX_POLL_ENV: &str = "RELAY_CODEX_POLL_SECS";

const POLL_STATE_FILE_NAME: &str = "codex_poll_state.json";

/// The one sanitized fact the detached evaluator persists about a Codex structured read, so the
/// supervisor that cannot make the read itself can still schedule around it. Bounded (one small
/// JSON object, overwritten in place every poll — never an appended log) and privacy-safe: no
/// account id, no raw provider output, no conversation content. `observed_unix_ms` and
/// `state` alone are enough to explain detection latency after the fact without ever claiming
/// Relay knew the exact instant the provider became exhausted between polls.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct CodexPollDiagnostic {
    pub observed_unix_ms: u64,
    pub state: UsageState,
    /// Trustworthy for scheduling only — see
    /// [`relay_provider_codex::usage::CodexUsageReading::max_used_percent`]'s own doc comment.
    pub max_used_percent: Option<u32>,
    /// What the pure adaptive policy alone selects from this reading, ignoring any
    /// `RELAY_CODEX_POLL_SECS` override — a diagnostic value only; the scheduler that actually
    /// owns cadence applies the override itself rather than trusting this field.
    pub selected_next_poll_secs: u64,
}

impl CodexPollDiagnostic {
    #[must_use]
    pub(crate) fn new(
        observed_unix_ms: u64,
        state: UsageState,
        max_used_percent: Option<u32>,
    ) -> Self {
        Self {
            observed_unix_ms,
            state,
            max_used_percent,
            selected_next_poll_secs: poll_interval_secs(max_used_percent),
        }
    }

    /// Best-effort: a poll diagnostic missing or unreadable is never a safety condition, only a
    /// reason to fall back to the conservative default cadence.
    pub(crate) fn write(&self, state_dir: &Path) {
        if let Ok(text) = serde_json::to_string(self) {
            let _ignored = std::fs::write(state_dir.join(POLL_STATE_FILE_NAME), text);
        }
    }

    pub(crate) fn read(state_dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(state_dir.join(POLL_STATE_FILE_NAME)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PollMode {
    Disabled,
    Fixed(u64),
    Adaptive,
}

/// This workspace forbids `unsafe`, which `std::env::set_var` requires since Rust 2024 — so, like
/// `relay_herdr::install::resolve_plugin_path`, the env read is a thin wrapper around a pure
/// function tests call directly with a value instead of mutating process-global env (see that
/// module's own doc comment for the same rationale).
fn poll_mode_from_env() -> PollMode {
    poll_mode_from_value(std::env::var(CODEX_POLL_ENV).ok().as_deref())
}

fn poll_mode_from_value(value: Option<&str>) -> PollMode {
    match value.and_then(|value| value.parse::<u64>().ok()) {
        None => PollMode::Adaptive,
        Some(0) => PollMode::Disabled,
        Some(fixed) => PollMode::Fixed(fixed),
    }
}

/// Owns exactly one supervised Codex terminal's polling cadence and its at-most-one-in-flight
/// evaluation. Lives only as long as the loop iteration that created it (see
/// `terminal_session::run_managed_terminal_inner`) — it is not a daemon and does not outlive the
/// terminal it supervises.
pub(crate) struct CodexPollScheduler {
    mode: PollMode,
    state_dir: PathBuf,
    child: Option<Child>,
    next_poll_at: Instant,
}

impl CodexPollScheduler {
    /// `seed_max_used_percent` is only ever consulted under [`PollMode::Adaptive`] — GitHub #13's
    /// initial-cadence requirement: a session that already knows (from its own pre-launch/resume
    /// preflight — see `commands::codex::codex_usage_now`) that the account is near its limit
    /// begins on the fast cadence immediately, instead of waiting out one full comfortable
    /// interval before its first poll ever runs.
    #[must_use]
    pub(crate) fn new(state_dir: PathBuf, seed_max_used_percent: Option<u32>) -> Self {
        Self::with_mode(poll_mode_from_env(), state_dir, seed_max_used_percent)
    }

    fn with_mode(mode: PollMode, state_dir: PathBuf, seed_max_used_percent: Option<u32>) -> Self {
        let initial_secs = match mode {
            PollMode::Disabled => 0,
            PollMode::Fixed(fixed) => fixed,
            PollMode::Adaptive => poll_interval_secs(seed_max_used_percent),
        };
        Self {
            mode,
            state_dir,
            child: None,
            next_poll_at: Instant::now() + Duration::from_secs(initial_secs),
        }
    }

    /// One supervision tick (called on the existing 300ms cadence — see
    /// `terminal_session::run_managed_terminal_inner`). `plan` is only invoked when a poll is
    /// actually due (it does a lease/profile lookup, so it is not free); `spawn` starts it
    /// detached and must return the child so at most one can ever be in flight here.
    pub(crate) fn tick(
        &mut self,
        plan: impl FnOnce() -> Option<AutoWatchPlan>,
        spawn: impl FnOnce(&AutoWatchPlan) -> Option<Child>,
    ) {
        if matches!(self.mode, PollMode::Disabled) {
            return;
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    self.child = None;
                    self.reschedule_from_last_result();
                }
                Ok(None) => {}
                Err(_) => {
                    // Reaping failed (rare, platform-dependent): never get stuck believing a poll
                    // is perpetually in flight — retry after the conservative fallback.
                    self.child = None;
                    self.next_poll_at = Instant::now() + Duration::from_secs(self.fallback_secs());
                }
            }
            return;
        }
        if Instant::now() < self.next_poll_at {
            return;
        }
        let Some(plan) = plan() else {
            // Nothing to evaluate right now (no fallback configured, or the lease moved under us)
            // — no read happened, so there is nothing fresh to schedule from.
            self.next_poll_at = Instant::now() + Duration::from_secs(self.fallback_secs());
            return;
        };
        match spawn(&plan) {
            Some(child) => self.child = Some(child),
            None => {
                self.next_poll_at = Instant::now() + Duration::from_secs(self.fallback_secs());
            }
        }
    }

    fn fallback_secs(&self) -> u64 {
        match self.mode {
            PollMode::Fixed(fixed) => fixed,
            PollMode::Adaptive | PollMode::Disabled => COMFORTABLE_POLL_SECS,
        }
    }

    /// Advances based on the completion of the poll that just finished, per GitHub #13 (never a
    /// blind fixed-tick relaunch): a fixed override always reschedules at that same interval;
    /// adaptive mode reads back the diagnostic that evaluation just published and re-derives the
    /// cadence from its own pure policy — never trusting a stale file from an earlier cycle
    /// silently, since it is overwritten atomically on every poll and a missing/unreadable file
    /// falls back to the conservative default exactly like an `UNKNOWN` reading would.
    fn reschedule_from_last_result(&mut self) {
        let secs = match self.mode {
            PollMode::Fixed(fixed) => fixed,
            PollMode::Adaptive => CodexPollDiagnostic::read(&self.state_dir)
                .map(|diagnostic| poll_interval_secs(diagnostic.max_used_percent))
                .unwrap_or(COMFORTABLE_POLL_SECS),
            PollMode::Disabled => unreachable!("checked at tick entry"),
        };
        self.next_poll_at = Instant::now() + Duration::from_secs(secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // --- RELAY_CODEX_POLL_SECS compatibility (never mutates real process env — see
    // `poll_mode_from_env`'s doc comment) ---

    #[test]
    fn absent_env_selects_adaptive_mode() {
        assert_eq!(poll_mode_from_value(None), PollMode::Adaptive);
    }

    #[test]
    fn env_zero_disables_polling_exactly_as_before() {
        assert_eq!(poll_mode_from_value(Some("0")), PollMode::Disabled);
    }

    #[test]
    fn env_positive_is_a_deterministic_fixed_override() {
        assert_eq!(poll_mode_from_value(Some("45")), PollMode::Fixed(45));
    }

    #[test]
    fn env_garbage_fails_open_to_adaptive_rather_than_panicking() {
        assert_eq!(
            poll_mode_from_value(Some("not-a-number")),
            PollMode::Adaptive
        );
    }

    // --- initial cadence: reuse of a pre-launch/resume seed (GitHub #13's core ask) ---

    #[test]
    fn adaptive_mode_seeds_the_fast_cadence_from_a_trustworthy_initial_reading() {
        let scheduler = CodexPollScheduler::with_mode(PollMode::Adaptive, PathBuf::new(), Some(99));
        assert!(scheduler.next_poll_at <= Instant::now() + Duration::from_secs(5));
    }

    #[test]
    fn adaptive_mode_without_a_seed_starts_on_the_comfortable_default() {
        let scheduler = CodexPollScheduler::with_mode(PollMode::Adaptive, PathBuf::new(), None);
        let delay = scheduler
            .next_poll_at
            .saturating_duration_since(Instant::now());
        assert!(delay > Duration::from_secs(100));
    }

    #[test]
    fn a_fixed_override_ignores_any_seed() {
        let scheduler = CodexPollScheduler::with_mode(PollMode::Fixed(7), PathBuf::new(), Some(99));
        let delay = scheduler
            .next_poll_at
            .saturating_duration_since(Instant::now());
        assert!(delay <= Duration::from_secs(7));
        assert!(delay > Duration::from_secs(5));
    }

    // --- diagnostic file: the sanitized feedback path ---

    #[test]
    fn diagnostic_round_trips_through_the_sanitized_state_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let diagnostic = CodexPollDiagnostic::new(1_000, UsageState::NearLimit, Some(93));
        diagnostic.write(dir.path());
        let read_back = CodexPollDiagnostic::read(dir.path()).expect("diagnostic");
        assert_eq!(read_back.max_used_percent, Some(93));
        assert_eq!(read_back.selected_next_poll_secs, 15);
    }

    #[test]
    fn a_missing_diagnostic_file_is_read_as_none_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(CodexPollDiagnostic::read(dir.path()).is_none());
    }

    // --- scheduler behavior: single flight, completion-driven rescheduling, disabled mode ---

    fn quick_child() -> Child {
        Command::new("true").spawn().expect("spawn `true`")
    }

    fn sleeper_child() -> Child {
        Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn `sleep`")
    }

    fn wait_until<F: FnMut() -> bool>(mut condition: F) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "condition never became true");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn due_scheduler(mode: PollMode, state_dir: PathBuf) -> CodexPollScheduler {
        let mut scheduler = CodexPollScheduler::with_mode(mode, state_dir, None);
        scheduler.next_poll_at = Instant::now();
        scheduler
    }

    #[test]
    fn a_disabled_scheduler_never_builds_a_plan_or_spawns() {
        let mut scheduler = due_scheduler(PollMode::Disabled, PathBuf::new());
        scheduler.tick(
            || panic!("disabled must never plan"),
            |_| panic!("disabled must never spawn"),
        );
    }

    #[test]
    fn at_most_one_poll_is_ever_in_flight() {
        let mut scheduler = due_scheduler(PollMode::Fixed(60), PathBuf::new());
        let fake_plan = || {
            Some(AutoWatchPlan {
                args: Vec::new(),
                log_path: PathBuf::new(),
                triggered_unix_ms: 0,
                trigger: "test",
            })
        };
        scheduler.tick(fake_plan, |_| Some(sleeper_child()));
        assert!(scheduler.child.is_some(), "the first due tick must spawn");
        // Still due (nothing advanced `next_poll_at`), but a child is in flight: must not spawn
        // a second one, and must not even build a new plan.
        scheduler.tick(
            || panic!("must not plan again while a poll is in flight"),
            |_| panic!("must not spawn again while a poll is in flight"),
        );
        if let Some(child) = scheduler.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn poll_completion_reschedules_adaptively_from_the_published_diagnostic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut scheduler = due_scheduler(PollMode::Adaptive, dir.path().to_path_buf());
        let fake_plan = || {
            Some(AutoWatchPlan {
                args: Vec::new(),
                log_path: PathBuf::new(),
                triggered_unix_ms: 0,
                trigger: "test",
            })
        };
        scheduler.tick(fake_plan, |_| Some(quick_child()));
        assert!(scheduler.child.is_some());
        // The evaluator (a real `relay watch run`, simulated here) publishes what it read before
        // it exits.
        CodexPollDiagnostic::new(2_000, UsageState::NearLimit, Some(99)).write(dir.path());
        wait_until(|| {
            scheduler.tick(
                || panic!("must not plan while reaping the in-flight child"),
                |_| panic!("must not spawn while reaping the in-flight child"),
            );
            scheduler.child.is_none()
        });
        let delay = scheduler
            .next_poll_at
            .saturating_duration_since(Instant::now());
        assert!(
            delay <= Duration::from_secs(5),
            "99% must reschedule onto the critical (5s) cadence, got {delay:?}"
        );
    }

    #[test]
    fn a_fixed_override_reschedules_at_the_same_interval_regardless_of_the_diagnostic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut scheduler = due_scheduler(PollMode::Fixed(45), dir.path().to_path_buf());
        let fake_plan = || {
            Some(AutoWatchPlan {
                args: Vec::new(),
                log_path: PathBuf::new(),
                triggered_unix_ms: 0,
                trigger: "test",
            })
        };
        scheduler.tick(fake_plan, |_| Some(quick_child()));
        // A published diagnostic that would otherwise select the critical cadence must be
        // ignored entirely under a fixed override.
        CodexPollDiagnostic::new(2_000, UsageState::NearLimit, Some(99)).write(dir.path());
        wait_until(|| {
            scheduler.tick(|| None, |_| None);
            scheduler.child.is_none()
        });
        let delay = scheduler
            .next_poll_at
            .saturating_duration_since(Instant::now());
        assert!(delay > Duration::from_secs(40) && delay <= Duration::from_secs(45));
    }

    #[test]
    fn no_fallback_configured_still_reschedules_rather_than_spinning() {
        let mut scheduler = due_scheduler(PollMode::Fixed(30), PathBuf::new());
        scheduler.tick(|| None, |_| panic!("spawn must never run without a plan"));
        assert!(scheduler.child.is_none());
        let delay = scheduler
            .next_poll_at
            .saturating_duration_since(Instant::now());
        assert!(delay > Duration::from_secs(25));
    }
}
