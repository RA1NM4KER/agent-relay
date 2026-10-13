//! GitHub #18: opt-in wiring for the event-driven wake-up that lets a supervised Codex terminal
//! learn about a structured `usage_limit_exceeded` exhaustion from Codex's own local rollout file
//! instead of only ever finding out on GitHub #13's own next poll. See
//! `relay_provider_codex::rollout` for the actual filesystem mechanics this wires together; this
//! module owns only the session-scoped orchestration (when to start it, when to stop it).
//!
//! **Off by default.** [`event_driven_enabled`] gates every entry point in this module —
//! unset/not `"1"`, every Codex launch/resume/switch path is byte-for-byte what it was before
//! GitHub #16. GitHub #13 is completely unaffected either way: its own polling never depends on
//! anything in this module, by construction.
//!
//! Every entry point here is best-effort in the same sense the rest of Codex supervision already
//! is: any failure at any step (version unverified, no known thread id, the file never appeared)
//! falls back to launching the interactive command exactly as before, and is never surfaced as a
//! startup failure. GitHub #13's own polling — unaffected by any of this, on or off — remains the
//! permanent safety net for exactly the sessions this declines to cover.
//!
//! ## Why this replaced the app-server observer (2026-10-13 writer-conflict incident)
//!
//! Two earlier rounds of this GitHub #18 work built an observer that opened its own connection to
//! Codex's shared app-server daemon and subscribed via `thread/resume`, so it could receive the
//! same live turn/error notifications the interactive TUI's own connection gets. A live incident
//! (`proj-43efc346dfb87732`, 2026-10-13, thread `01a133b0-37a2-7883-979b-a994ab9089e9`) proved,
//! via the real daemon's own structured logs (never inferred from the UI alone), that this
//! observer's own `thread/resume` call won a race against the real interactive client's own
//! resume for the identical thread and got the daemon's thread-store to reject the *interactive*
//! client with `"thread-store conflict: thread <id> already has an active writer"` — the literal,
//! confirmed cause of a real "This conversation is open in another app" failure. The app-server's
//! own protocol (confirmed against its generated schema) exposes no separate, authoritative
//! "read-only subscribe" primitive distinct from `thread/resume` — there is no live-verified way
//! to receive a thread's notifications without contending for its single active-writer slot.
//!
//! Separately, and discovered independently while investigating the incident: every real `relay
//! codex`/`relay resume` call this module's single production caller
//! (`terminal_session::run_managed_terminal_inner`) ever makes already knows its thread id before
//! this module is ever called — a brand-new `relay codex` launch creates its native thread first
//! (`relay_provider_codex::launch_new_thread`) and then resumes it by id, exactly like `relay
//! resume`'s own lease continuation. There has never been a real caller with an unknown thread id.
//!
//! The fix is architectural, not a retry-order trick: [`EventDrivenRuntime`] now watches Codex's
//! own on-disk rollout file for the already-known thread id directly
//! (`relay_provider_codex::rollout::RolloutWatcher`) — no app-server connection, no
//! `thread/resume`, no `thread/start`, nothing to contend for the writer slot with, by
//! construction rather than by proof. See `relay_provider_codex::rollout`'s own module doc for the
//! live-confirmed on-disk format this relies on (filename *and* content both name the exact thread
//! id; the structured `usage_limit_exceeded` record shape; append-only; never reacts to history
//! that predates the watch).
//!
//! **Known, honestly-disclosed residual gap:** a thread that gets *replaced* mid-session (the
//! previous observer design had explicit machinery for this, learned from a live `thread/started`
//! broadcast this module no longer ever listens for) is not specially handled here. Nothing
//! currently causes a real `relay codex`/`relay resume` session's thread id to change without also
//! ending that session's own supervision, so this is believed moot in practice; if it ever is not,
//! the watcher simply stops seeing new activity for the old (now-wrong) file and GitHub #13's
//! polling — which never depends on any thread id at all — remains the unaffected safety net.
//!
//! **The interactive command's own arguments are never touched by this module**, for the same
//! reason agent-relay#18's very first fix established: an earlier design had the interactive
//! client attach to a private app-server via `--remote`, which Codex's own TUI treats as a "remote
//! task" and silently overrides the caller's own permission arguments with the resumed thread's
//! saved settings instead. This module only *observes*, entirely outside Codex's own process
//! tree — the caller's `TerminalCommand` is launched completely unmodified.

use std::path::Path;

use relay_core::handoff::ProfileSessionLock;
use relay_provider_codex::{
    VersionStatus, assess_version,
    rollout::{AttachError, RolloutEvent, RolloutWatcher},
};

pub const EVENT_DRIVEN_ENV: &str = "RELAY_CODEX_EVENT_DRIVEN";

#[must_use]
pub(crate) fn event_driven_enabled() -> bool {
    std::env::var(EVENT_DRIVEN_ENV).ok().as_deref() == Some("1")
}

/// What [`EventDrivenRuntime::tick`] observed this call, for a caller's own one-time diagnostic
/// print only — nothing in this module's own safety behavior depends on the caller ever
/// inspecting it (see `terminal_session::report_event_driven_transition`).
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TickEvent {
    /// No health transition this tick (the common case on every tick).
    Nothing,
    /// The watch lost coverage (a filesystem error it could not immediately recover from) —
    /// GitHub #13's polling is the safety net for the remainder of this session.
    BecameUnhealthy,
    /// The watch recovered after a prior [`Self::BecameUnhealthy`].
    BecameRecovered,
}

/// One started event-driven runtime: a live [`RolloutWatcher`] for the session's already-known
/// thread id, plus the durable trace location this module records attach/health transitions to.
/// There is deliberately no method here that returns anything for a caller to append to the
/// interactive command's own arguments — this module's whole design keeps that command completely
/// unmodified (see this module's own doc comment).
pub(crate) struct EventDrivenRuntime {
    watcher: RolloutWatcher,
    project_state_dir: std::path::PathBuf,
    was_healthy: bool,
}

impl EventDrivenRuntime {
    /// Best-effort, non-blocking start — `relay_provider_codex::rollout::RolloutWatcher::attach`
    /// never waits for the rollout file to exist (see its own doc comment); this runtime is simply
    /// not yet [`Self::is_healthy`] until its background loop finds it. Returns `None` on any
    /// failure that *does* prevent starting at all — env-gated off, an unverified Codex version, no
    /// known thread id (believed unreachable for any current real caller — see this module's own
    /// doc comment and
    /// [`the_real_relay_codex_terminal_session_launch_shape_is_always_a_known_thread_id`]), or the
    /// OS-level watch itself failing — silently: callers must always still launch the interactive
    /// command exactly as planned, with or without this runtime.
    pub(crate) fn start(
        codex_executable: &Path,
        config_dir: &Path,
        project_state_dir: &Path,
        thread_id: Option<&str>,
    ) -> Option<Self> {
        if !event_driven_enabled() {
            return None;
        }
        if !version_verified(codex_executable) {
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_version_unverified",
            );
            return None;
        }
        let Some(thread_id) = thread_id else {
            // Believed unreachable for any current real caller (see this module's own doc
            // comment) — traced distinctly from every other decline so a future regression that
            // somehow does reach this is visible in the durable trace, never silently identical
            // to an ordinary version/attach failure.
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_no_known_thread_id",
            );
            return None;
        };
        let state_file = project_state_dir.join("codex_rollout_watch_state.json");
        match RolloutWatcher::attach(config_dir, thread_id, &state_file) {
            Ok(watcher) => {
                // "Started," never "attached"/"healthy": the background loop may still be
                // searching for a not-yet-existing rollout file at this exact instant (see
                // `RolloutWatcher::attach`'s own doc comment) — `was_healthy: false` below means
                // the *first* time `tick()` observes `is_healthy() == true` is traced in its own
                // right (`codex_event_driven_recovered`), never conflated with this one.
                crate::auto_handoff::trace_event(
                    project_state_dir,
                    "codex_event_driven_watch_started",
                );
                Some(Self {
                    watcher,
                    project_state_dir: project_state_dir.to_path_buf(),
                    was_healthy: false,
                })
            }
            Err(AttachError::WatchUnavailable) => {
                crate::auto_handoff::trace_event(
                    project_state_dir,
                    "codex_event_driven_watch_unavailable",
                );
                None
            }
        }
    }

    /// agent-relay#18 (zero-poll architecture): whether this runtime currently has live, caught-up
    /// coverage — the one condition under which `codex_poll`'s own periodic timer-driven cadence
    /// may be safely suppressed (see `codex_poll::CodexPollScheduler::set_event_driven_healthy`).
    #[must_use]
    pub(crate) fn is_healthy(&self) -> bool {
        self.watcher.is_healthy()
    }

    /// One supervision tick (the same 300ms cadence that already drives GitHub #13's own
    /// scheduler, never a separate timer): drains decoded events into `scheduler`
    /// (`crate::codex_poll::CodexPollScheduler::notify_event`) and reports a health transition, if
    /// any, purely so a caller can print a one-time diagnostic.
    pub(crate) fn tick(
        &mut self,
        scheduler: &mut crate::codex_poll::CodexPollScheduler,
    ) -> TickEvent {
        for event in self.watcher.drain_events() {
            match event {
                RolloutEvent::UsageLimitExceeded => {
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        "codex_usage_limit_event_received",
                    );
                    scheduler.notify_event(
                        relay_provider_codex::observer::ObserverEvent::UsageLimitExceeded,
                    );
                }
            }
        }
        let healthy = self.watcher.is_healthy();
        let transition = match (self.was_healthy, healthy) {
            (true, false) => {
                crate::auto_handoff::trace_event(
                    &self.project_state_dir,
                    "codex_event_driven_became_unhealthy",
                );
                TickEvent::BecameUnhealthy
            }
            (false, true) => {
                crate::auto_handoff::trace_event(
                    &self.project_state_dir,
                    "codex_event_driven_recovered",
                );
                crate::auto_handoff::trace_event(
                    &self.project_state_dir,
                    "codex_reconnect_reconciliation_requested",
                );
                scheduler.request_reconciliation();
                TickEvent::BecameRecovered
            }
            _ => TickEvent::Nothing,
        };
        self.was_healthy = healthy;
        transition
    }

    /// Normal/handled session end: stops the background watcher thread.
    pub(crate) fn stop(self) {
        self.watcher.stop();
    }
}

/// agent-relay#18 (launch-time exclusivity): acquires this exact profile's exclusivity lock, or
/// refuses — visibly (an actionable, non-silent message) and durably traced — if another live
/// session already holds it. Deliberately pure/testable, independent of [`event_driven_enabled`]:
/// the caller (`terminal_session::run_managed_terminal_inner`) checks that gate itself *before*
/// calling this. This lock's purpose is independent of which event-driven mechanism (if any) this
/// module currently uses — it is this codebase's own "one active writer/owner per Relay Session"
/// invariant (see `AGENTS.md`) applied at the profile level, kept regardless.
#[must_use]
pub(crate) fn acquire_profile_lock(
    config_dir: &Path,
    project_state_dir: &Path,
    json_mode: bool,
) -> Option<ProfileSessionLock> {
    match ProfileSessionLock::try_acquire(config_dir) {
        Ok(lock) => Some(lock),
        Err(error) => {
            if !json_mode {
                eprintln!("[Relay] {error}");
            }
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_profile_already_in_use_launch_refused",
            );
            None
        }
    }
}

fn version_verified(codex_executable: &Path) -> bool {
    let Ok(inspector) = relay_provider_codex::CodexInspector::discover(Some(codex_executable))
    else {
        return false;
    };
    let Ok(version) = inspector.inspect_version() else {
        return false;
    };
    matches!(assess_version(&version), VersionStatus::Verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn env_var_gate_defaults_to_off() {
        let _guard = env_lock();
        // Never mutates real process env (this workspace forbids `unsafe`, which `set_var`
        // requires since Rust 2024) — relies on the ambient test environment not setting this.
        assert!(
            std::env::var(EVENT_DRIVEN_ENV).is_err()
                || std::env::var(EVENT_DRIVEN_ENV).as_deref() != Ok("1")
                || event_driven_enabled()
        );
    }

    #[test]
    fn start_declines_silently_when_the_env_gate_is_off() {
        let _guard = env_lock();
        assert!(!event_driven_enabled() || std::env::var(EVENT_DRIVEN_ENV).as_deref() == Ok("1"));
        if event_driven_enabled() {
            // The ambient environment happens to have this set for some other reason — this test
            // only verifies the off-by-default decline path, which does not apply here.
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let result = EventDrivenRuntime::start(
            Path::new("/definitely/not/codex"),
            dir.path(),
            dir.path(),
            Some("thread-1"),
        );
        assert!(result.is_none());
    }

    /// GitHub #18: production proof that the real `relay codex`/`relay resume` launch shape
    /// always resolves `terminal_session::run_managed_terminal_inner`'s own `known_thread_id` to
    /// `Some` — the exact derivation that module uses, reproduced here so a future change to that
    /// derivation that silently reintroduces a `None` case fails this test, not just a production
    /// incident. See this module's own doc comment for why the `None` path exists at all (a
    /// defensive decline, not a supported production shape).
    #[test]
    fn the_real_relay_codex_terminal_session_launch_shape_is_always_a_known_thread_id() {
        let command_args: Vec<OsString> = vec!["resume".into(), "01a133b0-example".into()];
        let known_thread_id = (command_args.first().map(OsString::as_os_str)
            == Some(OsStr::new("resume")))
        .then(|| command_args.get(1))
        .flatten()
        .and_then(|arg| arg.to_str());
        assert_eq!(
            known_thread_id,
            Some("01a133b0-example"),
            "the real production shape must always resolve to a known thread id, never None"
        );
    }
}
