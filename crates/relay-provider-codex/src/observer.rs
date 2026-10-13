//! GitHub #16/#18: the shared signal vocabulary `relay_cli::codex_poll::CodexPollScheduler` was
//! originally built against for a live app-server observer connection.
//!
//! ## 2026-10-13 writer-conflict incident: the observer itself is gone
//!
//! That connection (`thread/resume` against Codex's shared app-server daemon) is what a live
//! incident (`proj-43efc346dfb87732`) proved can win a race against the real interactive client's
//! own resume and get the daemon's thread-store to reject it outright — the confirmed cause of a
//! real "This conversation is open in another app" failure. `relay_cli::codex_runtime` now wires
//! `relay_provider_codex::rollout::RolloutWatcher` instead — a filesystem-only signal that never
//! opens an app-server connection and so cannot repeat that failure by construction. See that
//! module's own doc comment for the live-confirmed on-disk mechanics, and
//! `relay_cli::codex_runtime`'s own module doc for the full incident and fix.
//!
//! Only [`ObserverEvent`]/[`DisconnectReason`] remain here: `CodexPollScheduler::notify_event`'s
//! own signature is typed against them, and every other caller in this codebase already matches
//! exhaustively on every variant, so keeping the type in place (never populated by anything but
//! [`ObserverEvent::UsageLimitExceeded`] today) is a smaller, safer diff than rethreading that
//! scheduler's own well-tested cadence logic through a narrower type for variants nothing
//! currently produces. The rest of what used to live in this module — the live socket connection,
//! its handshake, its `thread/resume`-based subscribe, and its reconnect/relearn state machine —
//! is deleted outright (GitHub #18 Phase 11: dead production code must not linger looking
//! production-capable), not merely unused.

/// One decoded, sanitized signal from an observed runtime. Never raw provider output, never an
/// account id, never conversation content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObserverEvent {
    /// A turn on the observed thread failed with the structured `usageLimitExceeded`/
    /// `usage_limit_exceeded` error (consult the producer's own module for the exact wire shape;
    /// both `relay_provider_codex::rollout` and historical app-server-protocol-shaped producers
    /// used this same variant). Callers must treat this as a wake-up hint only: start the existing
    /// authoritative `account/rateLimits/read` evaluation, never hand off directly on this alone.
    UsageLimitExceeded,
    /// A sanitized scheduling hint only (never `ordinaryUsageAllowed`, never an account id) —
    /// feeds `crate::polling::poll_interval_secs`'s existing cadence policy. No current producer
    /// populates this; kept for `CodexPollScheduler`'s own already-tested handling of it.
    RateLimitsHint(Option<u32>),
    /// An observed connection is gone and will never produce another live event. No current
    /// producer populates this; kept for `CodexPollScheduler`'s own already-tested handling of it.
    Disconnected(DisconnectReason),
    /// The previously-observed thread ended. No current producer populates this; kept for
    /// `CodexPollScheduler`'s own already-tested handling of it.
    ThreadEnded,
    /// The previously-observed thread was replaced by this one, carried here. No current producer
    /// populates this; kept for `CodexPollScheduler`'s own already-tested handling of it.
    ThreadReplaced(String),
}

/// Why an observed connection ended — informational only, every variant handled identically by a
/// caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    Closed,
    ReadError,
}
