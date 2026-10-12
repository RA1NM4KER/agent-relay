//! GitHub #16/#17/#18: opt-in wiring for the event-driven observer runtime that lets a supervised
//! Codex terminal wake up on a structured `usageLimitExceeded` error or
//! `account/rateLimits/updated` hint, instead of only ever finding out on GitHub #13's own next
//! poll. See `relay_provider_codex::runtime`/`observer`/`events` for the research findings and the
//! actual protocol mechanics this wires together; this module owns only the session-scoped
//! orchestration (when to start it, when to stop it, and how it recovers from its own observer
//! dying and how a fresh launch's not-yet-existing thread gets attached to at all).
//!
//! **Off by default.** [`event_driven_enabled`] gates every entry point in this module —
//! unset/not `"1"`, every Codex launch/resume/switch path is byte-for-byte what it was before
//! GitHub #16. GitHub #13 is completely unaffected either way: its own polling never depends on
//! anything in this module, by construction (see `relay_provider_codex::app_server`'s ephemeral
//! one-shot reads, unchanged, and `crate::codex_poll`'s cadence table, also unchanged by this
//! module — see `CodexPollScheduler::notify_event`/`request_reconciliation`, which this module
//! calls but never duplicates).
//!
//! Every entry point here is best-effort in the same sense the rest of Codex supervision already
//! is: any failure at any step (version unverified, daemon unavailable, handshake failed) falls
//! back to launching the interactive command exactly as before, and is never surfaced as a
//! startup failure.
//!
//! **agent-relay#18: the interactive command's own arguments are never touched by this module.**
//! An earlier version appended `--remote <endpoint>` here so the interactive client would attach
//! to a private app-server this module spawned — live-verified against `codex-cli 0.155.0` to
//! silently break the user's own permission arguments (`--yolo`, `--sandbox`, etc.): Codex's own
//! TUI treats `--remote` as a "remote task" and explicitly refuses to carry a CLI permission
//! override into it, restoring whatever the resumed thread's saved settings are instead. This
//! module now only *observes*: [`EventDrivenRuntime::start`] ensures Codex's own shared local
//! app-server daemon is running (`relay_provider_codex::runtime::ensure_managed_daemon`) and
//! attaches its own separate observer connection to it; the caller's `TerminalCommand` is launched
//! completely unmodified, and an ordinary local `codex resume` (no `--remote` flag at all)
//! auto-discovers and reuses that same daemon on its own, keeping ordinary local permission
//! semantics. See `relay_provider_codex::runtime`'s own module doc for the full finding.
//!
//! ## Attach/reconnect state machine (GitHub #18)
//!
//! ```text
//! start() ──▶ Attaching { worker retrying Observer::attach in the background }
//!                 │  (worker succeeds)                │ (stop() called before
//!                 ▼                                    │  first success)
//!             Attached(Observer) ──▶ request_reconciliation()   Dead (terminal)
//!                 │
//!                 │ observer reports ObserverEvent::Disconnected
//!                 ▼
//!             Attaching { new worker, same known thread id — never re-learned }
//! ```
//!
//! `start()` itself never blocks on attach succeeding — only on confirming the daemon is running,
//! which is already fast and already bounded
//! (`relay_provider_codex::runtime::ensure_managed_daemon`'s own timeout). Learning *and
//! reconnecting to* a thread happens entirely on a background worker thread so a fresh launch (no
//! thread id yet, and no bound on how long a real user takes to send their first message — GitHub
//! #17 live-verified `thread/resume` only needs that turn to have *started*, not completed) never
//! blocks `relay`'s own 300ms supervision tick.
//!
//! ### Daemon replacement is expected lifecycle, not failure (GitHub #18 follow-up)
//!
//! A real natural exhaustion in `~/repos/newinmeter` live-verified that Codex's own shared daemon
//! can self-update/restart mid-session (its own `app-server-daemon` package ships a scheduled
//! updater) with no action from Relay at all. The attach worker's retry loop previously treated
//! the old daemon's `ProcessIdentity` being confirmed gone as equivalent to "this app-server will
//! never come back" and gave up permanently (`Dead`) — correct for a daemon Relay itself spawned
//! and owns, wrong for one Codex owns and can legitimately replace out from under an observer.
//! The loop now treats a confirmed-gone identity as a cue to *rediscover* the current daemon via
//! the same [`ensure_managed_daemon`] call `start()` itself uses (never a guessed socket path or
//! pid), update its own stored endpoint/identity, and keep retrying the attach against the
//! rediscovered daemon — using the already-known thread id, never re-learned. `Dead` is now
//! reachable only via an explicit `stop()` before any attach ever succeeded; a confirmed daemon
//! replacement, however many times it repeats within one session, is never alone a reason to stop
//! retrying.
//!
//! ### A thread that will never resume (GitHub #18 follow-up)
//!
//! A real natural exhaustion in `~/repos` left the observer retrying a known thread id for ~14
//! minutes through an exhaustion it never saw. Confirmed against the real `codex app-server
//! generate-json-schema` output: `thread/resume` only returns the one confirmed-benign "no
//! rollout found" signature for a thread that is **neither currently running nor has anything
//! persisted on disk** — a thread the TUI is still genuinely driving always succeeds via "rejoin,"
//! regardless of daemon replacement (also live-confirmed: the `newinmeter` incident's own TUI
//! reconnected to the *same* thread across a real daemon replacement, not a different one). The
//! realistic way a known id can answer that one benign signature forever is a thread genuinely
//! abandoned before its first turn. `thread/resume`'s own error handling no longer discards *why*
//! it failed (see [`relay_provider_codex::observer::ObserverError`]'s own doc comment): an
//! explicit rejection, a persistent timeout, or — strictly more authoritative than either, and
//! checked even while still waiting for `thread/resume`'s own response — the app-server's own
//! pushed `thread/closed`/`thread/archived`/`thread/deleted`/`notLoaded`-status signal for that
//! exact id (see [`relay_provider_codex::events::notification_reports_this_thread_ended`]) all now
//! trigger the same bounded, protocol-authoritative relearn: listen for a genuinely fresh
//! `thread/started` broadcast instead of repeating a stuck id, restoring the original id if
//! nothing new is learned.
//!
//! One thing this does **not** close, honestly: if the app-server legitimately never emits *any*
//! of those signals for a silently-abandoned thread, nothing short of guessing can tell that apart
//! from a thread genuinely about to receive its first message — the worker correctly keeps
//! retrying, by design, exactly as the confirmed-benign case requires. There is no safety gap
//! here: this is the documented, intended behavior for a thread that has not yet started, not an
//! unobserved live session — polling (GitHub #13) remains active throughout, unaffected.
//!
//! ### Proving `thread/started` actually belongs to this session (GitHub #18 zero-poll follow-up)
//!
//! `thread/started` broadcasts to *every* client connected to the same `CODEX_HOME`, with no
//! client/session-scoped filter anywhere in the app-server's own protocol (confirmed exhaustively
//! against its generated schema: 170 client request methods, 84 server notifications, none scoped
//! by client/session identity — `thread/list`/`thread/loaded/list` filter by project/cwd/provider
//! only). A second live session sharing one Relay Codex profile could therefore cause the relearn
//! path above (or the pre-existing fresh-launch thread-learning path it reuses) to attach to a
//! thread that belongs to *that* session instead — a configuration this codebase's own history
//! shows actually happens (the same profile used by two simultaneous Relay-supervised sessions).
//!
//! No protocol operation closes this by itself: there is no supported way to bind a native TUI
//! process to "its" thread more strongly than resume-by-id already does. The fix is architectural,
//! not protocol-level: [`EventDrivenRuntime::start`] now acquires a
//! [`relay_core::handoff::ProfileSessionLock`] for this exact `CODEX_HOME` before doing anything
//! else. That lock makes "at most one live Relay-managed session observes this profile's events at
//! a time" a provable invariant (a genuine OS `flock`, not a pid file — see that type's own doc
//! comment for why this is crash-safe with no staleness window) rather than a hope, which is what
//! makes trusting a freshly-learned `thread/started` broadcast *safe* rather than a guess: there
//! is no second Relay-managed listener left for it to have come from. A second session attempting
//! to start while the first is still live is refused *visibly* (an actionable, non-silent message
//! — see `acquire_profile_lock`) and falls back to launching the interactive command unmodified,
//! still fully supervised by GitHub #13's own polling, which never depended on this invariant.
//!
//! The lock's own ownership, not its mere existence, is what took a second attempt to get right.
//! A first version held it as a plain field on [`EventDrivenRuntime`] for the runtime's entire
//! lifetime and had [`Self::stop`] join the background attach worker before releasing it — correct
//! in principle, but live-measured at up to a full `Observer::attach` connect-retry timeout (15s):
//! that inner loop has no way to notice `stop()` mid-attempt, so joining made ordinary shutdown
//! itself take as long as the worker's own worst case. The lock instead *moves*: into the worker
//! thread's own closure for the span [`AttachState::Attaching`] covers (see
//! `spawn_attach_worker`/`attach_worker_loop`), and back onto the runtime struct the moment an
//! attach actually succeeds (via [`AttachOutcome::Attached`]'s own `profile_lock` field, reclaimed
//! in [`Self::tick`]). This lets [`Self::stop`] go back to merely *signalling* the worker — fast,
//! never blocking — while the invariant still holds exactly: the lock is only ever released when
//! whoever is holding it (the worker thread, or the runtime struct via `Observer::stop`'s own fast
//! socket-shutdown-then-join) actually finishes, however long that takes, and a competing
//! acquisition in the meantime is refused, never silently allowed through.
//!
//! What this still does **not** close, honestly: an entirely unmanaged `codex` process a user
//! points at this same `CODEX_HOME` by hand, bypassing Relay — no mechanism could close that
//! short of a Codex protocol change, since Codex itself has no notion of this lock. That boundary
//! is the same one profile isolation already draws everywhere else (`docs/security.md`): Relay
//! guarantees what it controls, never a user's own direct misuse of their own files.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use relay_core::handoff::{ProcessIdentity, ProfileSessionLock};
use relay_provider_codex::{
    VersionStatus, assess_version,
    observer::{Observer, ObserverError, ObserverEvent},
    runtime::ensure_managed_daemon,
};

pub const EVENT_DRIVEN_ENV: &str = "RELAY_CODEX_EVENT_DRIVEN";

/// Local retry cadence for the attach/reconnect worker — a socket connect+handshake against
/// Codex's own shared local app-server daemon, never a provider call. Starts responsive enough to
/// still catch most of a first turn that begins moments after a retry (agent-relay#17
/// live-verified catching a turn resumed ~150ms after it started), grows only up to a small cap
/// so an indefinite wait (a real user staring at an empty prompt) costs nothing meaningful, and
/// resets on every successful attach.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(500);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(3);
/// How many consecutive full `thread/resume` timeouts (never an explicit response either way)
/// against the *same* known thread id it takes to earn the same bounded, authoritative relearn
/// fallback an explicit rejection gets immediately — see `attach_worker_loop`'s own doc comment.
/// High enough that ordinary, briefly-slow daemon traffic (one or two slow responses) never
/// triggers it; low enough that a genuinely stuck id is not retried for the rest of a long session.
const TIMEOUT_RELEARN_THRESHOLD: u32 = 3;
/// How often the backoff sleep wakes up to check the stop flag — keeps `stop()` responsive
/// without a tight loop.
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(100);

#[must_use]
fn event_driven_enabled() -> bool {
    std::env::var(EVENT_DRIVEN_ENV).ok().as_deref() == Some("1")
}

/// Bundled so [`attach_worker_loop`] stays under clippy's argument-count lint. `codex_executable`
/// and `codex_home` never change within a worker generation; `endpoint`/`app_server_identity` do,
/// on a confirmed daemon replacement — see [`attach_worker_loop`]'s own doc comment.
struct AttachTarget {
    codex_executable: PathBuf,
    codex_home: PathBuf,
    endpoint: PathBuf,
    app_server_identity: ProcessIdentity,
}

/// What the background attach/reconnect worker sends back once it either succeeds or gives up
/// permanently. There is no "retry failed, try again" message: the worker keeps retrying
/// silently on its own local backoff and only ever reports a *terminal* outcome for one attempt
/// cycle.
enum AttachOutcome {
    /// Carries back the endpoint/identity actually used for this success — which, after a daemon
    /// replacement mid-retry, is the *rediscovered* daemon, never the stale one the worker was
    /// started with. The caller must store these, not just the `Observer`, so the *next*
    /// disconnect reconnects against the daemon that is actually current.
    Attached {
        observer: Observer,
        endpoint: PathBuf,
        identity: ProcessIdentity,
        /// agent-relay#18 (zero-poll architecture): handed back here, never left behind at the
        /// top-level runtime during `Attaching` — see [`attach_worker_loop`]'s own doc comment
        /// for why ownership living with whoever can actually touch the daemon (the worker
        /// thread while attaching, the runtime struct once attached) is what lets `stop()` stay
        /// fast *and* exclusivity stay correct, with no join required.
        profile_lock: ProfileSessionLock,
    },
    /// `stop()` was called before any attach succeeded. A confirmed-gone app-server identity is no
    /// longer a reason for this on its own — see this module's own doc comment — so this is now
    /// reachable only via the stop flag. The profile lock this worker generation held simply
    /// drops here, at the end of its own stack frame — never explicitly handled, and never
    /// visible to a caller who only signalled and moved on.
    GaveUp,
}

/// What [`EventDrivenRuntime::tick`] observed this call, for a caller's own one-time diagnostics
/// only — never a safety-relevant signal on its own.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TickEvent {
    /// No state transition this tick.
    Nothing,
    /// Just attached (first time, or a reconnect) to this thread.
    Attached(String),
    /// The observer just disconnected; a fresh attach worker was started for this same thread.
    Reconnecting(String),
    /// The attach worker gave up permanently (app-server confirmed dead, or shutdown requested
    /// before a first successful attach).
    GaveUp,
}

/// Where this runtime's attach/subscribe attempt currently stands. See this module's own doc
/// comment for the full transition diagram.
enum AttachState {
    /// A background worker is trying to (re)attach; not yet successful. `stop_flag` lets
    /// [`EventDrivenRuntime::stop`] ask it to give up promptly rather than exhausting its own
    /// backoff first — deliberately signalled, never joined (see [`EventDrivenRuntime::stop`]'s
    /// own doc comment): the profile lock for this generation lives *inside* the worker thread
    /// itself while this state holds, so `stop()` stays fast and exclusivity still holds without
    /// a join — a second acquisition attempt is correctly refused until this worker's own thread
    /// actually drops it, however long that takes.
    Attaching {
        outcome_rx: mpsc::Receiver<AttachOutcome>,
        stop_flag: Arc<AtomicBool>,
    },
    /// Currently attached and receiving live events.
    Attached(Observer),
    /// Gave up permanently for this session (shutdown was requested before a first successful
    /// attach — see this module's own doc comment for why a confirmed-gone daemon identity no
    /// longer leads here). Terminal — nothing restarts this.
    Dead,
}

/// One started event-driven runtime: Codex's own shared local app-server daemon this invocation
/// confirmed running (never spawned or owned — see `relay_provider_codex::runtime`'s own module
/// doc for why), and (once attached) Relay's own passive observer connected to it. There is
/// deliberately no method here that returns anything for a caller to append to the interactive
/// command's own arguments — agent-relay#18's whole fix is that the interactive `TerminalCommand`
/// is launched completely unmodified and auto-discovers this same daemon on its own.
pub(crate) struct EventDrivenRuntime {
    endpoint: PathBuf,
    daemon_identity: ProcessIdentity,
    codex_home: PathBuf,
    /// The pinned, version-verified executable [`ensure_managed_daemon`] was already confirmed
    /// against in [`Self::start`] — kept so a later rediscovery (the old daemon identity confirmed
    /// gone) calls the exact same, already-verified binary, never a different one guessed at.
    codex_executable: PathBuf,
    /// GitHub #18: where this project's durable auto-handoff trace lives — used only to record
    /// event-receipt/reconciliation-request provenance (see [`Self::tick`]).
    project_state_dir: PathBuf,
    /// agent-relay#18 (zero-poll architecture): `Some` exactly while `state` is
    /// [`AttachState::Attached`] — proving no second Relay-managed session can be observing the
    /// same `CODEX_HOME` concurrently (see [`ProfileSessionLock`]'s own doc comment for the full
    /// invariant and its honest boundary). `None` while `Attaching`: ownership lives inside the
    /// background worker thread itself for that span instead (see [`attach_worker_loop`]'s own
    /// doc comment for why), so [`Self::stop`] never has to join anything to stay correct. Moved
    /// out to the worker on every reconnect, moved back in on every successful (re)attach — see
    /// [`Self::tick`].
    profile_lock: Option<ProfileSessionLock>,
    state: AttachState,
}

impl EventDrivenRuntime {
    /// Best-effort, non-blocking start (beyond confirming the daemon itself, already fast and
    /// already bounded — see `relay_provider_codex::runtime::ensure_managed_daemon`). `thread_id`
    /// is `Some` for a resume (Relay's own lease already names the thread) or `None` for a fresh
    /// launch — GitHub #18 activates both: the attach/reconnect worker's retry loop handles
    /// "learn the id from `thread/started`, then retry `thread/resume` until the first turn
    /// actually starts" the same way it handles a plain reconnect, since they are the same
    /// operation (see this module's own doc comment). Returns `None` on any failure up to and
    /// including confirming the daemon itself, silently — callers must always still launch the
    /// interactive command exactly as planned, with or without this runtime.
    ///
    /// GitHub #18 (observability follow-up): a real natural exhaustion, dogfooded under
    /// `RELAY_CODEX_EVENT_DRIVEN=1`, produced zero `codex_usage_limit_event_received` or
    /// `codex_reconnect_reconciliation_requested` trace lines across the entire supervised
    /// session, and this method's own silent-`None` design meant there was no durable way to tell
    /// "never even attempted" from "attempted and failed" after the fact. Every early exit past
    /// the env-gate check (an operator's own explicit opt-out, not worth a trace) now leaves a
    /// bounded, durable breadcrumb — never raw provider output, an account id, or conversation
    /// content — so the *next* natural exhaustion can prove exactly how far this got.
    pub(crate) fn start(
        codex_executable: &Path,
        config_dir: &Path,
        project_state_dir: &Path,
        thread_id: Option<&str>,
        json_mode: bool,
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
        // agent-relay#18 (zero-poll architecture): acquired before anything else that would make
        // this runtime start consuming/relying on broadcast events — see `ProfileSessionLock`'s
        // own doc comment for the full invariant. Unlike every other early-decline reason in this
        // function, this one is deliberately NOT silent: a second live session on the same
        // profile is operator-actionable (switch profile, or stop the other session), not an
        // internal detail worth suppressing. Decline is otherwise identical to every other
        // reason: fall back to launching the interactive command unmodified, still fully
        // supervised by GitHub #13's own polling, which never depended on this invariant at all.
        let profile_lock = acquire_profile_lock(config_dir, project_state_dir, json_mode)?;
        let Ok(daemon) = ensure_managed_daemon(codex_executable, config_dir) else {
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_daemon_unavailable",
            );
            return None;
        };
        crate::auto_handoff::trace_event(project_state_dir, "codex_event_driven_daemon_ready");
        let state = spawn_attach_worker(
            codex_executable.to_path_buf(),
            daemon.endpoint.clone(),
            config_dir.to_path_buf(),
            thread_id.map(str::to_owned),
            daemon.identity.clone(),
            project_state_dir.to_path_buf(),
            profile_lock,
        );
        Some(Self {
            endpoint: daemon.endpoint,
            daemon_identity: daemon.identity,
            codex_home: config_dir.to_path_buf(),
            codex_executable: codex_executable.to_path_buf(),
            project_state_dir: project_state_dir.to_path_buf(),
            profile_lock: None,
            state,
        })
    }

    /// agent-relay#18 (zero-poll architecture): whether this runtime is currently attached and
    /// actively receiving events — the one condition under which `codex_poll`'s own periodic
    /// timer-driven cadence may be safely suppressed (see `codex_poll::CodexPollScheduler`'s own
    /// `set_event_driven_healthy`). `Attaching`/`Dead` are both "not currently proven healthy,"
    /// for the same reason: a worker mid-(re)attach has not yet established live coverage, and a
    /// permanently-gave-up runtime never will again.
    #[must_use]
    pub(crate) fn is_healthy(&self) -> bool {
        matches!(self.state, AttachState::Attached(_))
    }

    /// One supervision tick (the same 300ms cadence that already drives GitHub #13's own
    /// scheduler, never a separate timer): while attaching, checks whether the background worker
    /// has produced an outcome yet; while attached, drains decoded events into `scheduler`
    /// (`crate::codex_poll::CodexPollScheduler::notify_event`) and, on an
    /// [`ObserverEvent::Disconnected`], starts a fresh attach worker with the now-known thread id
    /// (never re-learned via the broadcast — GitHub #17: `thread/started` fires only once per
    /// thread). Returns whichever transition happened *this* tick, if any, purely so a caller can
    /// print a one-time diagnostic — nothing in this module's own safety behavior depends on the
    /// caller ever inspecting it.
    pub(crate) fn tick(
        &mut self,
        scheduler: &mut crate::codex_poll::CodexPollScheduler,
    ) -> TickEvent {
        match &mut self.state {
            AttachState::Attaching { outcome_rx, .. } => match outcome_rx.try_recv() {
                Ok(AttachOutcome::Attached {
                    observer,
                    endpoint,
                    identity,
                    profile_lock,
                }) => {
                    let thread_id = observer.thread_id.clone();
                    // GitHub #18 follow-up: store whatever daemon this attach actually succeeded
                    // against — after a rediscovery, this is the *replacement* daemon, never the
                    // stale one this worker generation was started with. The next disconnect must
                    // reconnect against the daemon that is actually current, not re-check an
                    // already-confirmed-gone identity only to rediscover the same replacement
                    // again from scratch.
                    self.endpoint = endpoint;
                    self.daemon_identity = identity;
                    // agent-relay#18 (zero-poll architecture): reclaimed from the worker thread
                    // that held it while attaching — see `EventDrivenRuntime`'s own field doc.
                    self.profile_lock = Some(profile_lock);
                    // Preflight requirement (agent-relay#18): a successful attach previously had
                    // no durable evidence of its own — only the ephemeral terminal print in
                    // `terminal_session::report_event_driven_transition` and the differently
                    // -purposed `codex_reconnect_reconciliation_requested` line below, which
                    // proves attach only by inference. A trace review after the fact (e.g. once a
                    // real natural exhaustion has already happened) could not directly confirm
                    // "the observer attached to the real interactive thread" without that
                    // inference step. This line exists for exactly that direct confirmation.
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        "codex_event_driven_attached",
                    );
                    // GitHub #18's reconciliation requirement: a successful (re)attach — whether
                    // the very first one or a reconnect after a gap of unknown length — always
                    // triggers exactly one authoritative evaluation through the existing
                    // single-flight machinery, never a handoff decision on its own. Recorded
                    // *before* handing it to the scheduler so the durable trace can never show the
                    // resulting evaluation without also showing why it was requested.
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        "codex_reconnect_reconciliation_requested",
                    );
                    scheduler.request_reconciliation();
                    self.state = AttachState::Attached(observer);
                    TickEvent::Attached(thread_id)
                }
                Ok(AttachOutcome::GaveUp) => {
                    // GitHub #18 (observability follow-up): previously only an ephemeral stderr
                    // print — a session that gave up permanently before ever attaching left no
                    // durable trace distinguishing it from "never opted in at all."
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        "codex_event_driven_attach_worker_gave_up",
                    );
                    self.state = AttachState::Dead;
                    TickEvent::GaveUp
                }
                Err(_) => TickEvent::Nothing,
            },
            AttachState::Attached(observer) => {
                let mut disconnected = false;
                // GitHub #18 follow-up: distinct from a mere socket `Disconnected` — the server
                // itself authoritatively confirmed this exact subscribed thread has ended, so the
                // reconnect below must re-learn the active thread rather than keep resuming it.
                let mut thread_ended = false;
                for event in observer.drain_events() {
                    match event {
                        ObserverEvent::Disconnected(_) => disconnected = true,
                        ObserverEvent::ThreadEnded => {
                            disconnected = true;
                            thread_ended = true;
                        }
                        _ => {
                            // GitHub #18: durable proof that Relay actually received this event,
                            // recorded at the point it is drained/accepted from the observer —
                            // before it is ever handed to the scheduler, so a later evaluation can
                            // never be misattributed to timing alone.
                            if let Some(label) = event_receipt_trace_label(&event) {
                                crate::auto_handoff::trace_event(&self.project_state_dir, label);
                            }
                            scheduler.notify_event(event);
                        }
                    }
                }
                if disconnected {
                    let known_thread_id = observer.thread_id.clone();
                    // agent-relay#18 (zero-poll architecture): handed to the fresh worker below —
                    // `self.profile_lock` is always `Some` here, since reaching `Attached(_)` at
                    // all is only possible via `tick`'s own success branch, which always refills
                    // it (see that branch and `EventDrivenRuntime`'s own field doc).
                    let profile_lock = self
                        .profile_lock
                        .take()
                        .expect("profile_lock is always Some while Attached");
                    if thread_ended {
                        // GitHub #18 follow-up: the authoritative signal that beat the "benign
                        // forever" case this follow-up exists for — re-learn the active thread
                        // via a fresh `thread/started` broadcast instead of resuming a thread the
                        // server itself just confirmed is done.
                        crate::auto_handoff::trace_event(
                            &self.project_state_dir,
                            "codex_event_driven_thread_ended_authoritatively",
                        );
                        self.state = spawn_attach_worker(
                            self.codex_executable.clone(),
                            self.endpoint.clone(),
                            self.codex_home.clone(),
                            None,
                            self.daemon_identity.clone(),
                            self.project_state_dir.clone(),
                            profile_lock,
                        );
                        TickEvent::Reconnecting(known_thread_id)
                    } else {
                        // GitHub #18 (observability follow-up): a mid-session disconnect
                        // previously left the same kind of durable gap as a `GaveUp` that never
                        // attached.
                        crate::auto_handoff::trace_event(
                            &self.project_state_dir,
                            "codex_event_driven_observer_disconnected",
                        );
                        self.state = spawn_attach_worker(
                            self.codex_executable.clone(),
                            self.endpoint.clone(),
                            self.codex_home.clone(),
                            Some(known_thread_id.clone()),
                            self.daemon_identity.clone(),
                            self.project_state_dir.clone(),
                            profile_lock,
                        );
                        TickEvent::Reconnecting(known_thread_id)
                    }
                } else {
                    TickEvent::Nothing
                }
            }
            AttachState::Dead => TickEvent::Nothing,
        }
    }

    /// Normal/handled session end: stop whatever this runtime is currently doing (an attached
    /// observer, or a still-retrying background worker). Deliberately does NOT touch the daemon
    /// itself — agent-relay#18: it is shared and Codex-owned, potentially serving other sessions
    /// or tools that have nothing to do with this Relay Session, and must keep running exactly as
    /// if Relay had never been involved. Only [`Observer::stop`]/the attach worker's own stop flag
    /// are this runtime's to release.
    pub(crate) fn stop(self) {
        match self.state {
            AttachState::Attached(observer) => observer.stop(),
            AttachState::Attaching { stop_flag, .. } => {
                // Signalled, not joined — deliberately, and still safe: while `Attaching`,
                // `self.profile_lock` is `None` (see this struct's own field doc) because
                // ownership of the actual `ProfileSessionLock` for this generation was moved
                // into the worker thread's own closure when it was spawned (see
                // `spawn_attach_worker`). That lock is released only when the worker thread's own
                // stack frame ends — whether that is right away or, worst case, after its current
                // attach attempt's own bounded internal timeout — and a competing acquisition
                // attempt in the meantime is correctly refused, never silently allowed. This is
                // what lets `stop()` itself return immediately (never blocking ordinary shutdown
                // on a worker that may, at worst, still be seconds from exiting) without ever
                // reopening the cross-talk window the lock exists to close: nothing here drops or
                // transfers a lock this struct was never holding in the first place.
                stop_flag.store(true, Ordering::Relaxed);
            }
            AttachState::Dead => {}
        }
    }
}

/// GitHub #18: which drained [`ObserverEvent`], if any, is itself durable-trace-worthy on receipt
/// alone — a pure decision, deliberately separated from [`EventDrivenRuntime::tick`]'s own I/O, so
/// it stays trivially testable. Only `UsageLimitExceeded` qualifies: a `RateLimitsHint` is a
/// scheduling nudge, not a signal worth a durable receipt record on its own (see
/// `crate::codex_poll::CodexPollScheduler::notify_event`'s own doc comment), and `Disconnected` is
/// handled entirely by the reconnect path above, never forwarded here. Returning `None` for both
/// is what guarantees a timer expiry or a reconnect can never masquerade as this event: neither of
/// them ever flows through this function at all.
#[must_use]
fn event_receipt_trace_label(event: &ObserverEvent) -> Option<&'static str> {
    matches!(event, ObserverEvent::UsageLimitExceeded).then_some("codex_usage_limit_event_received")
}

/// Spawns the background attach/reconnect worker and returns the `Attaching` state a caller
/// should install immediately (never blocking on the worker's first result). `profile_lock` is
/// moved into the worker's own closure — see `EventDrivenRuntime`'s own field doc for why its
/// lifetime deliberately lives here, not on the runtime struct, for the span this state covers.
fn spawn_attach_worker(
    codex_executable: PathBuf,
    endpoint: PathBuf,
    codex_home: PathBuf,
    known_thread_id: Option<String>,
    app_server_identity: ProcessIdentity,
    project_state_dir: PathBuf,
    profile_lock: ProfileSessionLock,
) -> AttachState {
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let worker_stop = stop_flag.clone();
    thread::spawn(move || {
        let target = AttachTarget {
            codex_executable,
            codex_home,
            endpoint,
            app_server_identity,
        };
        attach_worker_loop(
            target,
            known_thread_id,
            &worker_stop,
            &outcome_tx,
            &project_state_dir,
            profile_lock,
        );
    });
    AttachState::Attaching {
        outcome_rx,
        stop_flag,
    }
}

/// GitHub #18 (observability follow-up): a short, bounded label for why a single attach attempt
/// failed — never the offending payload/path itself, only which of [`ObserverError`]'s own
/// variants it was. `ResumeNotYetReady` carries a learned thread id that must never be logged
/// (it is not sensitive, but this vocabulary stays deliberately payload-free across every variant
/// so no future variant addition can accidentally start leaking one).
#[must_use]
fn attach_error_label(error: &ObserverError) -> &'static str {
    match error {
        ObserverError::Connect => "codex_event_driven_attach_error_connect",
        ObserverError::Handshake => "codex_event_driven_attach_error_handshake",
        ObserverError::HomeMismatch => "codex_event_driven_attach_error_home_mismatch",
        ObserverError::NoThread => "codex_event_driven_attach_error_no_thread",
        ObserverError::ResumeNotYetReady(_) => {
            "codex_event_driven_attach_error_resume_not_yet_ready"
        }
        ObserverError::ResumeRejected(_) => "codex_event_driven_attach_error_resume_rejected",
        ObserverError::ResumeTimedOut(_) => "codex_event_driven_attach_error_resume_timed_out",
        ObserverError::ResumeTargetEnded(_) => {
            "codex_event_driven_attach_error_resume_target_ended"
        }
    }
}

/// The retry loop itself: attempts [`Observer::attach`], remembering a learned thread id across
/// attempts (GitHub #17: `thread/resume` failing with `ResumeNotYetReady` means "no turn has
/// started yet," not "give up" — and re-waiting for `thread/started` on a later attempt would
/// hang for that call's own full internal timeout, since the broadcast never repeats). Stops
/// retrying only when attach succeeds or `stop_flag` is set — a confirmed-gone app-server
/// identity no longer ends the loop (see below).
///
/// GitHub #18 (observability follow-up): a real natural exhaustion left no durable evidence of
/// *why* this loop never once succeeded — only ephemeral stderr, never captured. Every *distinct*
/// failure kind is now traced durably the first time this generation of the loop sees it (never
/// per-retry, which at this loop's backoff could otherwise write hundreds of near-identical lines
/// over a long wait) — enough to prove, after the fact, whether the loop ever got past connect,
/// handshake, or resume, without flooding the trace.
///
/// GitHub #18 follow-up (`~/repos/newinmeter` live finding): a *confirmed* `Some(false)` on
/// `app_server_identity` used to end the loop outright (`AttachOutcome::GaveUp`). That is correct
/// for a daemon Relay itself spawned and owns, but Codex's own shared daemon can legitimately
/// self-update/restart mid-session — expected external lifecycle, not failure. A confirmed-gone
/// identity now calls the same [`ensure_managed_daemon`] `start()` itself used, to rediscover
/// whatever daemon is current for this `codex_home`, and — on success — replaces `endpoint` and
/// `app_server_identity` with the rediscovered values and immediately tries `Observer::attach`
/// against them (same `known_thread_id`, never re-learned). A `None` (merely unconfirmable)
/// reading is never treated as proof of replacement, exactly as before — only a definite
/// `Some(false)` triggers rediscovery. Rediscovery failure is treated as transient, exactly like
/// an ordinary attach failure: traced once per distinct generation, then backed off and retried —
/// never a reason to give up. This naturally supports repeated replacement (A → B → C, ...) within
/// one worker generation: each newly rediscovered identity becomes the one compared against on the
/// next iteration, so a second replacement is detected and recovered from exactly like the first.
///
/// GitHub #18 follow-up (`~/repos` live finding, second occurrence of the daemon-replacement
/// finding above's own class of gap): a real natural exhaustion showed the daemon-replacement
/// recovery above working cleanly — `codex_event_driven_daemon_replaced` →
/// `codex_event_driven_daemon_rediscovered` — immediately followed by `ResumeNotYetReady` against
/// the *correct*, already-verified-current thread id, which then never once succeeded for the
/// ~14 minutes up to and including the exhaustion the observer never saw, despite the native
/// session's own rollout showing continuous heavy turn activity throughout. Before this follow-up,
/// every `thread/resume` failure — an explicit rejection, a bare timeout, or the one genuinely
/// benign "no rollout found" signature — collapsed into the same `ResumeNotYetReady`, so the loop
/// (correctly, by its own stated contract) kept the same id and retried forever, with no way for
/// anyone reviewing the trace afterward to tell *which* of those three it actually was, and no way
/// for the loop itself to ever consider that the id might not be the problem at all. `thread/resume`
/// failures are now split three ways (see [`ObserverError`]'s own doc comment): `ResumeNotYetReady`
/// and `ResumeTimedOut` both keep the same id (neither is evidence the id is wrong), but
/// `ResumeRejected` — an affirmative server-side rejection, not "just wait" — now earns exactly one
/// bounded, authoritative re-check: the next attempt listens for a fresh `thread/started` broadcast
/// instead, and if nothing new is learned (`NoThread`), the rejected id is restored rather than
/// left permanently cleared, so a generation that keeps hitting rejections never degrades into
/// paying the full listen timeout on every retry forever.
fn attach_worker_loop(
    mut target: AttachTarget,
    mut known_thread_id: Option<String>,
    stop_flag: &AtomicBool,
    outcome_tx: &mpsc::Sender<AttachOutcome>,
    project_state_dir: &Path,
    // agent-relay#18 (zero-poll architecture): carried through unexamined — its only job is to
    // stay alive for exactly this function's own lifetime (handed back on success, below;
    // dropped here, at this function's own end, on every `GaveUp` exit) — see
    // `EventDrivenRuntime`'s own field doc for the full reasoning.
    profile_lock: ProfileSessionLock,
) {
    let mut backoff = RECONNECT_BACKOFF_MIN;
    let mut last_traced_error: Option<&'static str> = None;
    // Tracks whether the *current* confirmed-gone identity has already been traced, so a
    // generation stuck retrying a failing rediscovery logs the "confirmed replaced" fact exactly
    // once, not on every retry — reset to `false` the moment rediscovery succeeds, so a *second*
    // replacement later in the same generation is traced as its own distinct event.
    let mut daemon_replaced_traced = false;
    // GitHub #18 follow-up: an explicit `ResumeRejected` clears `known_thread_id` for exactly one
    // bounded re-listen attempt (authoritative — a fresh `thread/started` broadcast, never a
    // guess). If that attempt learns nothing new (`NoThread`), the rejected id is restored here
    // rather than left permanently cleared, so a generation that keeps hitting rejections never
    // degrades into paying the full listen timeout on every single retry forever.
    let mut rejected_fallback_id: Option<String> = None;
    // Consecutive full `ResumeTimedOut` failures against the current `known_thread_id` — reset
    // whenever that id changes or a non-timeout resume outcome occurs. See
    // `TIMEOUT_RELEARN_THRESHOLD`'s own doc comment.
    let mut consecutive_timeouts: u32 = 0;
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            let _ignored = outcome_tx.send(AttachOutcome::GaveUp);
            return;
        }
        // A merely-unconfirmable liveness reading (`None`) is not proof of replacement — that
        // would be over-eager for what is only a "should I keep trying this endpoint" decision,
        // not a safety-relevant ownership one. Only a definite `Some(false)` (the app-server is
        // provably gone) triggers rediscovery.
        if target.app_server_identity.is_still_the_same_process() == Some(false) {
            if !daemon_replaced_traced {
                crate::auto_handoff::trace_event(
                    project_state_dir,
                    "codex_event_driven_daemon_replaced",
                );
                daemon_replaced_traced = true;
            }
            match ensure_managed_daemon(&target.codex_executable, &target.codex_home) {
                Ok(daemon) => {
                    crate::auto_handoff::trace_event(
                        project_state_dir,
                        "codex_event_driven_daemon_rediscovered",
                    );
                    target.endpoint = daemon.endpoint;
                    target.app_server_identity = daemon.identity;
                    daemon_replaced_traced = false;
                    last_traced_error = None;
                    backoff = RECONNECT_BACKOFF_MIN;
                    // Fall through to attempt `Observer::attach` against the rediscovered daemon
                    // immediately, in this same iteration — never an extra sleep purely for
                    // having rediscovered successfully.
                }
                Err(_) => {
                    let label = "codex_event_driven_daemon_rediscovery_failed";
                    if last_traced_error != Some(label) {
                        crate::auto_handoff::trace_event(project_state_dir, label);
                        last_traced_error = Some(label);
                    }
                    sleep_checking_stop(backoff, stop_flag);
                    backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                    continue;
                }
            }
        }
        match Observer::attach(
            &target.endpoint,
            &target.codex_home,
            known_thread_id.as_deref(),
        ) {
            Ok(observer) => {
                let _ignored = outcome_tx.send(AttachOutcome::Attached {
                    observer,
                    endpoint: target.endpoint.clone(),
                    identity: target.app_server_identity.clone(),
                    profile_lock,
                });
                return;
            }
            Err(error) => {
                let label = attach_error_label(&error);
                if last_traced_error != Some(label) {
                    crate::auto_handoff::trace_event(project_state_dir, label);
                    last_traced_error = Some(label);
                }
                match error {
                    ObserverError::ResumeNotYetReady(id) => {
                        known_thread_id = Some(id);
                        consecutive_timeouts = 0;
                    }
                    ObserverError::ResumeTimedOut(id) => {
                        // GitHub #18 follow-up: a bare timeout is not proof the id is wrong — a
                        // single one keeps the same id, same as the confirmed-benign case. But a
                        // *persistent* timeout (unlike an explicit rejection, which answers fast)
                        // can otherwise hide a genuinely stale id forever behind "it just hasn't
                        // responded yet" — indistinguishable in principle from a daemon that will
                        // never, ever answer this specific id again. After enough consecutive full
                        // timeouts against the same id to rule out ordinary slowness (at least
                        // `TIMEOUT_RELEARN_THRESHOLD` full `ATTACH_TIMEOUT` waits with zero
                        // response), this earns the same bounded, authoritative re-check a
                        // rejection does, for the same reason.
                        consecutive_timeouts += 1;
                        if consecutive_timeouts >= TIMEOUT_RELEARN_THRESHOLD {
                            crate::auto_handoff::trace_event(
                                project_state_dir,
                                "codex_event_driven_thread_relearn_after_persistent_timeout",
                            );
                            rejected_fallback_id = Some(id);
                            known_thread_id = None;
                            consecutive_timeouts = 0;
                        } else {
                            known_thread_id = Some(id);
                        }
                    }
                    ObserverError::ResumeRejected(id) => {
                        // GitHub #18 follow-up: a real `~/repos` natural exhaustion left the
                        // observer retrying the same thread id, indistinguishable from the
                        // confirmed-benign case, for the ~14 minutes leading up to and including
                        // an exhaustion it never saw. An explicit rejection is not proof the id is
                        // permanently wrong, but is distinct enough from "just wait" to earn one
                        // bounded, authoritative re-check — never a guess, never a busy loop: the
                        // very next attempt listens for a genuinely fresh `thread/started`
                        // broadcast instead of repeating the rejected id, and `NoThread` below
                        // restores it if nothing new was learned.
                        crate::auto_handoff::trace_event(
                            project_state_dir,
                            "codex_event_driven_thread_relearn_after_rejection",
                        );
                        rejected_fallback_id = Some(id);
                        known_thread_id = None;
                        consecutive_timeouts = 0;
                    }
                    ObserverError::ResumeTargetEnded(id) => {
                        // GitHub #18 follow-up: the app-server's own authoritative lifecycle
                        // signal for this exact id arrived — strictly stronger evidence than a
                        // rejection or a timeout, so this relearns immediately, with no threshold
                        // to wait out first. Same bounded, restore-if-nothing-found mechanics.
                        crate::auto_handoff::trace_event(
                            project_state_dir,
                            "codex_event_driven_thread_relearn_after_authoritative_end",
                        );
                        rejected_fallback_id = Some(id);
                        known_thread_id = None;
                        consecutive_timeouts = 0;
                    }
                    ObserverError::NoThread => {
                        // Reached either by a genuinely fresh launch's own listen timeout
                        // (nothing to restore, `known_thread_id` was already `None`) or by the
                        // one-shot re-listen just above finding nothing new — in the latter case,
                        // go back to the rejected id rather than listening forever on every
                        // subsequent retry.
                        known_thread_id = rejected_fallback_id.take();
                        consecutive_timeouts = 0;
                    }
                    ObserverError::Connect
                    | ObserverError::Handshake
                    | ObserverError::HomeMismatch => {}
                }
            }
        }
        sleep_checking_stop(backoff, stop_flag);
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

/// Sleeps for `duration`, waking every [`STOP_CHECK_INTERVAL`] to check `stop_flag` so a request
/// to stop is noticed promptly rather than only after the full backoff elapses.
fn sleep_checking_stop(duration: Duration, stop_flag: &AtomicBool) {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(STOP_CHECK_INTERVAL.min(duration));
    }
}

/// GitHub #16's capability gate: only ever attempt this on a Codex CLI version this crate has
/// live-verified the daemon-bootstrap mechanism and the two structured signals against (see
/// `relay_provider_codex::inspection::VERIFIED_VERSIONS` for exactly which versions and why). An
/// unverified or unknown version — including a version whose own `--version` cannot even be
/// read — silently declines rather than guessing at flags/behavior a future or older Codex
/// release may not actually support; #13 remains fully active regardless.
/// agent-relay#18 (zero-poll architecture): the pure/testable half of [`EventDrivenRuntime::start`]'s
/// exclusivity check, separated from its own gate chain (env opt-in, version verification) so this
/// specific decision — and its visible, non-silent failure message — is directly testable without
/// needing a live, verified `codex` executable (exactly the same reason `attach_worker_loop` is
/// tested directly rather than only through `start()`).
#[must_use]
fn acquire_profile_lock(
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
                "codex_event_driven_profile_already_in_use",
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

    fn dead_identity() -> ProcessIdentity {
        let mut child = std::process::Command::new("sleep")
            .arg("2")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::query(child.id());
        let _ = child.kill();
        child.wait().expect("reap");
        std::thread::sleep(std::time::Duration::from_millis(100));
        identity
    }

    fn live_identity() -> (std::process::Child, ProcessIdentity) {
        let child = std::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::query(child.id());
        (child, identity)
    }

    fn attach_target(
        codex_executable: &Path,
        codex_home: &Path,
        endpoint: PathBuf,
        app_server_identity: ProcessIdentity,
    ) -> AttachTarget {
        AttachTarget {
            codex_executable: codex_executable.to_path_buf(),
            codex_home: codex_home.to_path_buf(),
            endpoint,
            app_server_identity,
        }
    }

    #[test]
    fn env_var_gate_defaults_to_off() {
        // Never mutates real process env (this workspace forbids `unsafe`, which `set_var`
        // requires since Rust 2024) — matches `codex_poll::poll_mode_from_value`'s own rationale.
        // This test only documents the constant name/contract other tests and callers rely on.
        assert_eq!(EVENT_DRIVEN_ENV, "RELAY_CODEX_EVENT_DRIVEN");
    }

    #[test]
    fn start_is_a_no_op_when_the_feature_is_not_enabled() {
        // `event_driven_enabled()` reads the real env var; this test only exercises the case the
        // default test environment already is — unset — matching every CI run and every real
        // user who has never opted in.
        let dir = tempfile::tempdir().expect("tempdir");
        let result = EventDrivenRuntime::start(
            Path::new("/definitely/not/codex"),
            dir.path(),
            dir.path(),
            Some("thread-1"),
            false,
        );
        assert!(result.is_none());
    }

    // --- agent-relay#18 (zero-poll architecture): profile exclusivity ---

    #[test]
    fn a_free_profile_is_locked_successfully() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = acquire_profile_lock(dir.path(), dir.path(), false);
        assert!(lock.is_some());
    }

    #[test]
    fn a_profile_already_locked_by_another_live_session_is_refused_and_traced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = ProfileSessionLock::try_acquire(dir.path()).expect("first holder");
        let second = acquire_profile_lock(dir.path(), dir.path(), false);
        assert!(
            second.is_none(),
            "a second live session on the same profile must be refused, never silently allowed"
        );
        let log = std::fs::read_to_string(dir.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_profile_already_in_use"),
            "the refusal must be durably traced, not only printed: {log}"
        );
    }

    #[test]
    fn a_profile_lock_is_available_again_immediately_after_the_holder_releases_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = ProfileSessionLock::try_acquire(dir.path()).expect("first holder");
        drop(held);
        let second = acquire_profile_lock(dir.path(), dir.path(), false);
        assert!(
            second.is_some(),
            "a profile must be immediately acquirable again once the prior holder releases it"
        );
    }

    #[test]
    fn two_different_profiles_never_contend_with_each_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("profile-a");
        let b = dir.path().join("profile-b");
        std::fs::create_dir_all(&a).expect("profile a");
        std::fs::create_dir_all(&b).expect("profile b");
        let _lock_a = acquire_profile_lock(&a, &a, false).expect("acquire a");
        let lock_b = acquire_profile_lock(&b, &b, false);
        assert!(
            lock_b.is_some(),
            "two simultaneous sessions on two different profiles must never contend"
        );
    }

    #[test]
    fn stop_is_fast_and_a_racing_acquisition_is_still_correctly_refused_while_the_worker_lives() {
        // agent-relay#18 (zero-poll architecture): the final design, after rejecting an earlier
        // join-based attempt at this same test (measured at 15s — `Observer::attach`'s own
        // connect retry loop is not interruptible mid-attempt, so joining made `stop()` itself as
        // slow as a full `ATTACH_TIMEOUT`, a real UX regression). Ownership of the
        // `ProfileSessionLock` instead lives *inside* the worker thread's own closure while
        // `Attaching` (see `EventDrivenRuntime`'s own field doc), so `stop()` can stay fast
        // (merely signals) while still never reopening the cross-talk window: a competing
        // acquisition attempt for the same profile must be refused for as long as the old
        // worker thread is actually still alive, proven here by pointing it at a socket path
        // that will never accept a connection, so it is reliably still mid-`connect_unix` (which
        // does not poll the stop flag at all) immediately after `stop()` returns.
        let scratch = tempfile::tempdir().expect("tempdir");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let (_child, identity) = live_identity();
        let profile_lock =
            ProfileSessionLock::try_acquire(&codex_home).expect("acquire for the runtime");
        let state = spawn_attach_worker(
            PathBuf::from("/definitely/not/codex"),
            scratch.path().join("nobody-is-listening.sock"),
            codex_home.clone(),
            Some("thread-1".to_owned()),
            identity.clone(),
            scratch.path().to_path_buf(),
            profile_lock,
        );
        let runtime = EventDrivenRuntime {
            endpoint: scratch.path().join("nobody-is-listening.sock"),
            daemon_identity: identity,
            codex_home: codex_home.clone(),
            codex_executable: PathBuf::from("/definitely/not/codex"),
            project_state_dir: scratch.path().to_path_buf(),
            profile_lock: None,
            state,
        };
        // Give the worker a moment to actually start its connect attempt before stopping it.
        thread::sleep(Duration::from_millis(20));
        let before = std::time::Instant::now();
        runtime.stop();
        let elapsed = before.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "stop() took {elapsed:?} — it must return promptly, never block on the worker's own \
             unbounded-feeling connect retry (that regression is exactly what moving the lock's \
             ownership into the worker thread was meant to avoid)"
        );
        match ProfileSessionLock::try_acquire(&codex_home) {
            Err(relay_core::Error::ProfileSessionLockHeld(_)) => {}
            Err(other) => panic!("expected ProfileSessionLockHeld, got {other:?}"),
            Ok(_) => panic!(
                "a second acquisition immediately after a fast stop() must still be refused — \
                 the old worker is still genuinely alive inside its own connect retry, so \
                 nothing has actually released the lock yet"
            ),
        }
    }

    // --- GitHub #18: event receipt is recorded exactly at the point it is drained ---

    #[test]
    fn only_usage_limit_exceeded_is_ever_event_receipt_trace_worthy() {
        assert_eq!(
            event_receipt_trace_label(&ObserverEvent::UsageLimitExceeded),
            Some("codex_usage_limit_event_received")
        );
        assert_eq!(
            event_receipt_trace_label(&ObserverEvent::RateLimitsHint(Some(90))),
            None,
            "a scheduling hint must never masquerade as a real usage-limit event receipt"
        );
        assert_eq!(
            event_receipt_trace_label(&ObserverEvent::RateLimitsHint(None)),
            None
        );
        assert_eq!(
            event_receipt_trace_label(&ObserverEvent::Disconnected(
                relay_provider_codex::observer::DisconnectReason::ReadError
            )),
            None,
            "disconnects are handled entirely by the reconnect path, never this one"
        );
    }

    // --- GitHub #18: the attach/reconnect worker's own retry-loop behavior, tested directly ---
    // (not through `EventDrivenRuntime::start`, which requires the real `codex` executable to
    // get past `version_verified` — these exercise `attach_worker_loop` against a fake app-server
    // the same way `relay_provider_codex::observer`'s own tests do, without needing a full
    // `EventDrivenRuntime`.)

    fn install_fake_app_server_ws(
        socket_path: &Path,
        codex_home: &Path,
        fail_resume_times: usize,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            let mut resume_failures_left = fail_resume_times;
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                loop {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match value.get("method").and_then(serde_json::Value::as_str) {
                        Some("initialize") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("thread/resume") => {
                            let id = value["id"].clone();
                            if resume_failures_left > 0 {
                                resume_failures_left -= 1;
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "error": {"code": -32600, "message": "no rollout found"}})
                                        .to_string()
                                        .into(),
                                ));
                            } else {
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "result": {}})
                                        .to_string()
                                        .into(),
                                ));
                            }
                            let _ = socket.flush();
                        }
                        _ => {}
                    }
                }
            }
        });
        listener
    }

    /// GitHub #18 follow-up: a fake app-server that always, affirmatively rejects
    /// `rejected_thread_id` (a message that does **not** match the "no rollout found" signature —
    /// simulating the live `~/repos` finding's own symptom, a `thread/resume` failure that is
    /// never going to resolve no matter how many times it is retried) while broadcasting
    /// `relearned_thread_id` as a fresh `thread/started` on every connection right after that
    /// connection's own `initialized`, and succeeding `thread/resume` against *that* id instead.
    /// Each incoming connection gets its own broadcast — exactly what a real app-server would do
    /// for a genuinely new thread any newly connecting client (including a reconnecting observer)
    /// would learn about.
    fn install_fake_app_server_ws_with_relearnable_thread(
        socket_path: &Path,
        codex_home: &Path,
        rejected_thread_id: &str,
        relearned_thread_id: &str,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let rejected_thread_id = rejected_thread_id.to_owned();
        let relearned_thread_id = relearned_thread_id.to_owned();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                loop {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match value.get("method").and_then(serde_json::Value::as_str) {
                        Some("initialize") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("initialized") => {
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({
                                    "method": "thread/started",
                                    "params": {"thread": {"id": relearned_thread_id}},
                                })
                                .to_string()
                                .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("thread/resume") => {
                            let id = value["id"].clone();
                            let requested = value["params"]["threadId"].as_str().unwrap_or("");
                            if requested == rejected_thread_id {
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "error": {"code": -32602, "message": "thread not found"}})
                                        .to_string()
                                        .into(),
                                ));
                            } else {
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "result": {}})
                                        .to_string()
                                        .into(),
                                ));
                            }
                            let _ = socket.flush();
                        }
                        _ => {}
                    }
                }
            }
        });
        listener
    }

    #[test]
    fn a_rejected_thread_falls_back_to_relearning_the_active_thread_then_succeeds() {
        // GitHub #18 follow-up — reproduces the real `~/repos` natural exhaustion's own structural
        // failure deterministically: before this fix, ANY `thread/resume` failure (an affirmative
        // rejection included) was reported as `ResumeNotYetReady`, so the loop kept the same,
        // never-going-to-succeed id forever. This must now fall back to relearning and succeed
        // against the thread that was actually resumable all along.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws_with_relearnable_thread(
            &socket_path,
            &codex_home,
            "stale-thread",
            "relearned-thread",
        );
        let (mut child, identity) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_path,
                identity,
            ),
            Some("stale-thread".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        match rx.recv_timeout(Duration::from_secs(10)).expect("outcome") {
            AttachOutcome::Attached { observer, .. } => {
                assert_eq!(
                    observer.thread_id, "relearned-thread",
                    "must recover by relearning the thread that was actually resumable, \
                     never keep retrying the permanently-rejected one"
                );
                observer.stop();
            }
            AttachOutcome::GaveUp => {
                panic!("must recover by relearning the active thread, never give up")
            }
        }
        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_attach_error_resume_rejected"),
            "an affirmative rejection must be traced distinctly from the benign \
             not-yet-ready case: {log}"
        );
        assert!(
            log.contains("codex_event_driven_thread_relearn_after_rejection"),
            "the fallback to relearning must itself be traced: {log}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Never answers `thread/resume` at all for `timed_out_thread_id` (so every attempt against
    /// it genuinely exhausts `ATTACH_TIMEOUT`, never an explicit response either way), while
    /// broadcasting and succeeding `relearned_thread_id` exactly like
    /// [`install_fake_app_server_ws_with_relearnable_thread`] — the persistent-timeout analogue of
    /// that rejection-triggered fallback.
    fn install_fake_app_server_ws_timing_out_then_relearnable(
        socket_path: &Path,
        codex_home: &Path,
        timed_out_thread_id: &str,
        relearned_thread_id: &str,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let timed_out_thread_id = timed_out_thread_id.to_owned();
        let relearned_thread_id = relearned_thread_id.to_owned();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                loop {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match value.get("method").and_then(serde_json::Value::as_str) {
                        Some("initialize") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("initialized") => {
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({
                                    "method": "thread/started",
                                    "params": {"thread": {"id": relearned_thread_id}},
                                })
                                .to_string()
                                .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("thread/resume") => {
                            let id = value["id"].clone();
                            let requested = value["params"]["threadId"].as_str().unwrap_or("");
                            if requested == timed_out_thread_id {
                                // Deliberately no response at all — forces the real
                                // `ATTACH_TIMEOUT` wait, never an explicit error.
                            } else {
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "result": {}})
                                        .to_string()
                                        .into(),
                                ));
                                let _ = socket.flush();
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        listener
    }

    #[test]
    fn persistent_resume_timeouts_against_the_same_thread_fall_back_to_relearning() {
        // GitHub #18 follow-up: unlike an explicit rejection (fast, unambiguous), a bare timeout
        // alone is never proof the id is wrong — but a *persistent* one (never a single response
        // either way, repeatedly) must not be allowed to hide a genuinely stuck id for the rest of
        // a session the way the live `~/repos` exhaustion's own observer was stuck. This is slow
        // by construction (`TIMEOUT_RELEARN_THRESHOLD` full `ATTACH_TIMEOUT` waits, ~45s) — no
        // shortcut exists without weakening the real timeout this is proving against.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws_timing_out_then_relearnable(
            &socket_path,
            &codex_home,
            "stuck-thread",
            "relearned-thread",
        );
        let (mut child, identity) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_path,
                identity,
            ),
            Some("stuck-thread".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        match rx.recv_timeout(Duration::from_secs(90)).expect("outcome") {
            AttachOutcome::Attached { observer, .. } => {
                assert_eq!(
                    observer.thread_id, "relearned-thread",
                    "must eventually recover by relearning after persistent timeouts, \
                     never keep retrying a thread id that never answers at all"
                );
                observer.stop();
            }
            AttachOutcome::GaveUp => {
                panic!("must recover by relearning after persistent timeouts, never give up")
            }
        }
        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_attach_error_resume_timed_out"),
            "each timeout must be traced: {log}"
        );
        assert!(
            log.contains("codex_event_driven_thread_relearn_after_persistent_timeout"),
            "the persistent-timeout fallback must itself be traced, distinctly from the \
             rejection-triggered one: {log}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Always rejects `rejected_thread_id` (a non-benign message) and never reacts to
    /// `initialized` at all — no broadcast, ever. Used to prove the one-shot re-listen fallback
    /// genuinely times out (`NoThread`) rather than finding something to relearn, so the restore
    /// path can be exercised deterministically.
    fn install_fake_app_server_ws_always_rejecting(
        socket_path: &Path,
        codex_home: &Path,
        rejected_thread_id: &str,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let rejected_thread_id = rejected_thread_id.to_owned();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                loop {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match value.get("method").and_then(serde_json::Value::as_str) {
                        Some("initialize") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("thread/resume") => {
                            let id = value["id"].clone();
                            let requested = value["params"]["threadId"].as_str().unwrap_or("");
                            if requested == rejected_thread_id {
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "error": {"code": -32602, "message": "thread not found"}})
                                        .to_string()
                                        .into(),
                                ));
                                let _ = socket.flush();
                            }
                            // Any other requested thread id (there should never be one in this
                            // fake, since it never broadcasts anything to relearn) deliberately
                            // gets no response at all, so a buggy implementation that somehow
                            // resumed something else would hang rather than silently "pass."
                        }
                        _ => {}
                    }
                }
            }
        });
        listener
    }

    #[test]
    fn a_rejected_thread_with_nothing_new_to_relearn_falls_back_to_the_original_id() {
        // GitHub #18 follow-up: if the one-shot re-listen after a rejection learns nothing new
        // (no genuinely fresh thread exists), the rejected id must be restored rather than left
        // permanently cleared — otherwise every subsequent retry would pay the full listen
        // timeout forever instead of the much cheaper connect+resume cycle, for no benefit.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener =
            install_fake_app_server_ws_always_rejecting(&socket_path, &codex_home, "stale-thread");
        let (mut child, identity) = live_identity();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker_stop = stop_flag.clone();
        let project_state_dir = scratch.path().to_path_buf();
        let profile_lock =
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock");
        let handle = thread::spawn(move || {
            attach_worker_loop(
                attach_target(
                    Path::new("/definitely/not/codex"),
                    &codex_home,
                    socket_path,
                    identity,
                ),
                Some("stale-thread".to_owned()),
                &worker_stop,
                &tx,
                &project_state_dir,
                profile_lock,
            );
        });
        // The first (near-instant) rejection must trigger exactly one relearn attempt — proven by
        // durable evidence, never a fixed sleep racing however long that takes.
        wait_for_trace_line(
            scratch.path(),
            "codex_event_driven_thread_relearn_after_rejection",
            1,
            Duration::from_secs(5),
        );
        stop_flag.store(true, Ordering::Relaxed);
        // Honoring stop() is bounded by whichever attempt is currently in flight — documented
        // (`EventDrivenRuntime::stop`'s own comment) to be at most one attach attempt's own
        // internal bound, which for a re-listen is `ATTACH_TIMEOUT` (15s) if this lands mid-wait.
        // Never instant, but always bounded — no busy loop, no unbounded wait.
        let outcome = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("bounded outcome after stop");
        assert!(matches!(outcome, AttachOutcome::GaveUp));
        handle.join().expect("worker thread joins promptly");

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_thread_relearn_after_rejection"),
            "the rejection must trigger exactly one relearn attempt: {log}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    // --- GitHub #18 follow-up: the "benign forever" gap (previous audit's one remaining known
    // hole). Phase 1 finding, confirmed against the real `codex app-server generate-json-schema`
    // output: `thread/resume`'s "if thread_id identifies a running thread, app-server rejoins
    // that thread" rule (`ThreadResumeParams`'s own doc comment) means a thread the TUI is
    // genuinely still driving always succeeds via rejoin, regardless of rollout history — "no
    // rollout found" specifically means the thread is NEITHER running NOR has anything on disk.
    // Live evidence from the real `~/repos/newinmeter` daemon-replacement incident also shows the
    // TUI reconnecting to the SAME thread across a replacement, not a different one. The
    // previous audit's exact framing ("daemon replacement causes the TUI to rebind onto a
    // different thread") is therefore not well-supported as the mechanism — but the *observable*
    // risk it was pointing at (a known thread id that will never resume, while a different thread
    // is the one actually being driven) is real and reproducible through the one case the schema
    // does confirm: a thread genuinely abandoned before completing a first turn. The tests below
    // reproduce that directly (no speculative daemon-replacement wiring needed) and prove both
    // what the new authoritative signal recovers and the one honest limit of what it cannot.

    /// Simulates a thread `abandoned_thread_id` that is permanently, genuinely abandoned before
    /// its first turn (always "no rollout found" — the one confirmed-benign signature, never a
    /// rejection, never silent) while the real app-server *also* pushes the one authoritative
    /// lifecycle signal (`thread/closed`) for it, followed by a fresh `thread/started` broadcast
    /// for `relearned_thread_id`, which resumes successfully. Every connection gets its own
    /// `thread/closed` + broadcast, exactly like the other relearnable-thread fakes.
    fn install_fake_app_server_ws_benign_forever_with_authoritative_close(
        socket_path: &Path,
        codex_home: &Path,
        abandoned_thread_id: &str,
        relearned_thread_id: &str,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let abandoned_thread_id = abandoned_thread_id.to_owned();
        let relearned_thread_id = relearned_thread_id.to_owned();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                loop {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match value.get("method").and_then(serde_json::Value::as_str) {
                        Some("initialize") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("thread/resume") => {
                            let id = value["id"].clone();
                            let requested = value["params"]["threadId"].as_str().unwrap_or("");
                            if requested == abandoned_thread_id {
                                // The one confirmed-benign signature, every single time — never a
                                // rejection, never silence. Immediately followed by the
                                // authoritative close + a fresh broadcast on this SAME connection,
                                // so the short-circuit inside the resume wait itself is exercised.
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "error": {"code": -32600, "message": "no rollout found for thread id"}})
                                        .to_string()
                                        .into(),
                                ));
                                let _ = socket.flush();
                            } else {
                                let _ = socket.send(tungstenite::Message::Text(
                                    serde_json::json!({"id": id, "result": {}})
                                        .to_string()
                                        .into(),
                                ));
                                let _ = socket.flush();
                            }
                        }
                        Some("initialized") => {
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({
                                    "method": "thread/closed",
                                    "params": {"threadId": abandoned_thread_id},
                                })
                                .to_string()
                                .into(),
                            ));
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({
                                    "method": "thread/started",
                                    "params": {"thread": {"id": relearned_thread_id}},
                                })
                                .to_string()
                                .into(),
                            ));
                            let _ = socket.flush();
                        }
                        _ => {}
                    }
                }
            }
        });
        listener
    }

    #[test]
    fn an_authoritative_thread_closed_signal_recovers_a_benign_forever_resume() {
        // Phase 2 regression: the one scenario the previous audit could not rule out — a known
        // thread id that will *never* stop answering the confirmed-benign "no rollout found"
        // response — is now recoverable, because the server's own authoritative `thread/closed`
        // push for that exact id short-circuits the resume wait immediately, never waiting for a
        // rejection or a timeout threshold that this input shape would never produce on its own.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws_benign_forever_with_authoritative_close(
            &socket_path,
            &codex_home,
            "abandoned-thread",
            "relearned-thread",
        );
        let (mut child, identity) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_path,
                identity,
            ),
            Some("abandoned-thread".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        match rx.recv_timeout(Duration::from_secs(10)).expect("outcome") {
            AttachOutcome::Attached { observer, .. } => {
                assert_eq!(
                    observer.thread_id, "relearned-thread",
                    "must recover via the authoritative close signal, never keep retrying a \
                     thread id that will never stop answering the confirmed-benign response"
                );
                observer.stop();
            }
            AttachOutcome::GaveUp => {
                panic!("must recover via the authoritative close signal, never give up")
            }
        }
        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_attach_error_resume_target_ended"),
            "the authoritative end signal must be traced distinctly: {log}"
        );
        assert!(
            log.contains("codex_event_driven_thread_relearn_after_authoritative_end"),
            "the fallback itself must be traced distinctly from the rejection/timeout ones: {log}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Never reacts to `initialized` at all (no `thread/closed`, no broadcast — simulating an
    /// app-server that never emits *any* authoritative signal for a silently abandoned thread),
    /// and always answers `thread/resume` with the one confirmed-benign "no rollout found"
    /// signature, forever, for every connection.
    fn install_fake_app_server_ws_benign_forever_with_no_signal_at_all(
        socket_path: &Path,
        codex_home: &Path,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                loop {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match value.get("method").and_then(serde_json::Value::as_str) {
                        Some("initialize") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        Some("thread/resume") => {
                            let id = value["id"].clone();
                            let _ = socket.send(tungstenite::Message::Text(
                                serde_json::json!({"id": id, "error": {"code": -32600, "message": "no rollout found for thread id"}})
                                    .to_string()
                                    .into(),
                            ));
                            let _ = socket.flush();
                        }
                        _ => {}
                    }
                }
            }
        });
        listener
    }

    #[test]
    fn a_benign_forever_resume_with_no_authoritative_signal_at_all_is_the_one_documented_residual_risk()
     {
        // This is NOT a bug this change fixes — it is the honest, named boundary of what the
        // authoritative-signal recovery above can and cannot do. If the app-server legitimately
        // never emits any lifecycle signal for an abandoned-before-first-turn thread (no
        // `thread/closed`/`thread/archived`/`thread/deleted`, no `notLoaded` status change), and
        // `thread/resume` never once returns anything but the confirmed-benign response, nothing
        // short of guessing can tell this apart from a thread that is simply, legitimately about
        // to receive its first message any moment now — so the worker correctly keeps retrying
        // it, exactly as the confirmed-benign case requires. This test documents that this is
        // still true after this change (not a regression introduced by it, not newly fixed by it)
        // so a future reader does not have to rediscover it from scratch.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws_benign_forever_with_no_signal_at_all(
            &socket_path,
            &codex_home,
        );
        let (mut child, identity) = live_identity();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker_stop = stop_flag.clone();
        let project_state_dir = scratch.path().to_path_buf();
        let handle = thread::spawn(move || {
            attach_worker_loop(
                attach_target(
                    Path::new("/definitely/not/codex"),
                    &codex_home,
                    socket_path,
                    identity,
                ),
                Some("abandoned-thread".to_owned()),
                &worker_stop,
                &tx,
                &project_state_dir,
                ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
            );
        });
        // Several retry cycles' worth of time against the purely-benign-forever fake.
        thread::sleep(Duration::from_secs(2));
        assert!(
            rx.try_recv().is_err(),
            "with zero authoritative signal and zero rejection/timeout, the worker correctly \
             keeps retrying — this is the confirmed-benign case's own required behavior, not a \
             hang bug"
        );
        stop_flag.store(true, Ordering::Relaxed);
        let outcome = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("prompt outcome once stopped");
        assert!(matches!(outcome, AttachOutcome::GaveUp));
        handle.join().expect("worker thread joins promptly");
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Succeeds `thread/resume` for `thread_id` immediately, then — on the *same* connection,
    /// right after that success response, before ever reading another client message — pushes a
    /// `thread/closed` notification for that same id. Simulates the already-attached, already-
    /// subscribed case: the live connection stays open and usable, but the specific thread it was
    /// watching is authoritatively reported done shortly after.
    fn install_fake_app_server_ws_closes_its_thread_right_after_attach(
        socket_path: &Path,
        codex_home: &Path,
        thread_id: &str,
    ) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind");
        let codex_home = codex_home.to_path_buf();
        let thread_id = thread_id.to_owned();
        let listener_clone = listener.try_clone().expect("clone listener");
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener_clone.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(serde_json::Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        let _ = socket.send(tungstenite::Message::Text(
                            serde_json::json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        let _ = socket.send(tungstenite::Message::Text(
                            serde_json::json!({"id": id, "result": {}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.send(tungstenite::Message::Text(
                            serde_json::json!({
                                "method": "thread/closed",
                                "params": {"threadId": thread_id},
                            })
                            .to_string()
                            .into(),
                        ));
                        let _ = socket.flush();
                    }
                    _ => {}
                }
            }
        });
        listener
    }

    #[test]
    fn an_attached_observers_authoritatively_ended_thread_relearns_the_active_thread_through_tick()
    {
        // Phase 4 adversarial case: unlike the resume-wait tests above, this is the
        // *already-attached* case — the observer is live and subscribed, and only *afterward*
        // does the server report this exact thread ended. `EventDrivenRuntime::tick` must treat
        // this distinctly from an ordinary `Disconnected`: the reconnect it drives must re-learn
        // (`known_thread_id: None`), never resume the same, now-confirmed-ended id.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_a = scratch.path().join("a.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener_a = install_fake_app_server_ws_closes_its_thread_right_after_attach(
            &socket_a,
            &codex_home,
            "thread-1",
        );
        let (mut child, identity) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_a,
                identity.clone(),
            ),
            Some("thread-1".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        let observer = match rx.recv_timeout(Duration::from_secs(5)).expect("outcome") {
            AttachOutcome::Attached { observer, .. } => observer,
            AttachOutcome::GaveUp => panic!("must succeed against the first fake daemon"),
        };
        assert_eq!(observer.thread_id, "thread-1");

        // The relearn attempt the reconnect below must reach: broadcasts a fresh thread and
        // succeeds resume against it, exactly like the other relearnable fakes.
        let socket_b = scratch.path().join("b.sock");
        let _listener_b = install_fake_app_server_ws_with_relearnable_thread(
            &socket_b,
            &codex_home,
            "thread-1",
            "thread-2",
        );

        let (outcome_tx, outcome_rx) = mpsc::channel();
        outcome_tx
            .send(AttachOutcome::Attached {
                observer,
                endpoint: socket_b.clone(),
                identity: identity.clone(),
                profile_lock: ProfileSessionLock::try_acquire(scratch.path())
                    .expect("test profile lock"),
            })
            .expect("queue outcome");
        let mut runtime = EventDrivenRuntime {
            endpoint: socket_b,
            daemon_identity: identity,
            codex_home,
            codex_executable: PathBuf::from("/definitely/not/codex"),
            project_state_dir: scratch.path().to_path_buf(),
            // `None`: not yet reclaimed from the queued outcome above — `tick()`'s own success
            // branch (exercised below) is what moves it in, exactly like production.
            profile_lock: None,
            state: AttachState::Attaching {
                outcome_rx,
                stop_flag: Arc::new(AtomicBool::new(false)),
            },
        };
        let mut scheduler = crate::codex_poll::CodexPollScheduler::new(
            scratch.path().to_path_buf(),
            scratch.path().to_path_buf(),
            None,
        );
        // First tick: resolves the queued `Attached` outcome.
        let first = runtime.tick(&mut scheduler);
        assert_eq!(first, TickEvent::Attached("thread-1".to_owned()));

        // Give the fake's own `thread/closed` (queued right after the resume success it already
        // answered) time to actually arrive over the real socket and be decoded by the real
        // background reader thread — polled, never a fixed-length guess.
        wait_for(Duration::from_secs(5), || {
            matches!(
                runtime.tick(&mut scheduler),
                TickEvent::Reconnecting(_) | TickEvent::Nothing
            ) && matches!(runtime.state, AttachState::Attaching { .. })
        });

        // The reconnect this triggered must have re-learned rather than resumed "thread-1" — the
        // worker it spawned is seeded with `known_thread_id: None`, so it listens for
        // `install_fake_app_server_ws_with_relearnable_thread`'s own broadcast and succeeds
        // against "thread-2", never "thread-1" again.
        let AttachState::Attaching { outcome_rx, .. } = &runtime.state else {
            panic!("must be attaching again after the authoritative end")
        };
        match outcome_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("outcome")
        {
            AttachOutcome::Attached { observer, .. } => {
                assert_eq!(
                    observer.thread_id, "thread-2",
                    "must have re-learned the active thread, never resumed the ended one"
                );
                observer.stop();
            }
            AttachOutcome::GaveUp => panic!("must recover by relearning, never give up"),
        }

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_thread_ended_authoritatively"),
            "the already-attached authoritative end must be traced distinctly: {log}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_worker_retries_past_resume_not_yet_ready_and_succeeds() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws(&socket_path, &codex_home, 2);
        let (mut child, identity) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_path,
                identity,
            ),
            Some("thread-1".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        match rx.recv().expect("outcome") {
            AttachOutcome::Attached { observer, .. } => {
                assert_eq!(observer.thread_id, "thread-1");
                observer.stop();
            }
            AttachOutcome::GaveUp => panic!("must eventually succeed once resume stops failing"),
        }
        // GitHub #18 (observability follow-up): the two `ResumeNotYetReady` failures before the
        // eventual success must be traced durably, but coalesced to exactly one line, not one per
        // retry.
        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert_eq!(
            log.matches("codex_event_driven_attach_error_resume_not_yet_ready")
                .count(),
            1,
            "repeated identical attach failures must be coalesced to one durable trace line: {log}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_successful_attach_is_traced_durably_and_directly_not_only_by_inference() {
        // Preflight requirement (agent-relay#18): before this, a successful attach had no direct
        // durable evidence of its own — only the ephemeral terminal print and the differently-
        // purposed `codex_reconnect_reconciliation_requested` line, which only proves attach by
        // inference. A trace review after the fact must be able to confirm "the observer attached
        // to the real interactive thread" directly.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws(&socket_path, &codex_home, 0);
        let (mut child, identity) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_path.clone(),
                identity.clone(),
            ),
            Some("thread-1".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        let observer = match rx.recv().expect("outcome") {
            AttachOutcome::Attached { observer, .. } => observer,
            AttachOutcome::GaveUp => panic!("must succeed against a fake that never fails resume"),
        };

        // Feed the already-resolved outcome through `EventDrivenRuntime::tick` itself, exactly as
        // `spawn_attach_worker`'s background thread would deliver it, so this test exercises the
        // real transition that decides what gets traced.
        let (outcome_tx, outcome_rx) = mpsc::channel();
        outcome_tx
            .send(AttachOutcome::Attached {
                observer,
                endpoint: socket_path.clone(),
                identity: identity.clone(),
                profile_lock: ProfileSessionLock::try_acquire(scratch.path())
                    .expect("test profile lock"),
            })
            .expect("queue outcome");
        let mut runtime = EventDrivenRuntime {
            endpoint: socket_path,
            daemon_identity: identity,
            codex_home,
            codex_executable: PathBuf::from("/definitely/not/codex"),
            project_state_dir: scratch.path().to_path_buf(),
            profile_lock: None,
            state: AttachState::Attaching {
                outcome_rx,
                stop_flag: Arc::new(AtomicBool::new(false)),
            },
        };
        let mut scheduler = crate::codex_poll::CodexPollScheduler::new(
            scratch.path().to_path_buf(),
            scratch.path().to_path_buf(),
            None,
        );
        let event = runtime.tick(&mut scheduler);
        assert_eq!(event, TickEvent::Attached("thread-1".to_owned()));

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert!(
            log.contains("codex_event_driven_attached"),
            "a successful attach must be traced directly, not only inferable from a \
             differently-purposed line: {log}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_worker_gives_up_promptly_once_stopped() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        // Always fails resume — the worker would retry forever without the stop signal.
        let _listener = install_fake_app_server_ws(&socket_path, &codex_home, usize::MAX);
        let (mut child, identity) = live_identity();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker_stop = stop_flag.clone();
        let project_state_dir = scratch.path().to_path_buf();
        let handle = thread::spawn(move || {
            attach_worker_loop(
                attach_target(
                    Path::new("/definitely/not/codex"),
                    &codex_home,
                    socket_path,
                    identity,
                ),
                Some("thread-1".to_owned()),
                &worker_stop,
                &tx,
                &project_state_dir,
                ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
            );
        });
        thread::sleep(Duration::from_millis(50));
        stop_flag.store(true, Ordering::Relaxed);
        let outcome = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("prompt outcome");
        assert!(matches!(outcome, AttachOutcome::GaveUp));
        handle.join().expect("worker thread joins promptly");
        let _ = child.kill();
        let _ = child.wait();
    }

    // --- GitHub #18 follow-up: daemon replacement/self-update recovery (`~/repos/newinmeter`
    // live finding — see this module's own doc comment). ---

    fn install_fake_daemon_start(dir: &Path, pid: u32, socket_path: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(format!("fake-codex-daemon-start-{pid}"));
        std::fs::write(
            &path,
            format!(
                r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
printf '{{"status":"started","pid":{pid},"socketPath":"{socket}"}}\n'
"#,
                pid = pid,
                socket = socket_path.display(),
            ),
        )
        .expect("write fake daemon-start");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn install_fake_daemon_start_failing(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-codex-daemon-start-failing");
        std::fs::write(
            &path,
            r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
echo "daemon failed to start" >&2
exit 1
"#,
        )
        .expect("write fake failing daemon-start");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// A `daemon start` fake that answers *differently* on successive invocations (tracked via a
    /// counter file), one entry of `sequence` per call — so a test can make rediscovery report a
    /// different "current" daemon each time it is asked, reproducing repeated replacement (A → B
    /// → C) within a single worker generation without timing races.
    fn install_fake_daemon_start_sequence(
        dir: &Path,
        counter_path: &Path,
        sequence: &[(u32, PathBuf)],
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let mut branches = String::new();
        for (index, (pid, socket)) in sequence.iter().enumerate() {
            let n = index + 1;
            branches.push_str(&format!(
                "if [ \"$N\" -eq {n} ]; then printf '{{\"status\":\"started\",\"pid\":{pid},\"socketPath\":\"{socket}\"}}\\n'; exit 0; fi\n",
                n = n,
                pid = pid,
                socket = socket.display(),
            ));
        }
        let path = dir.join("fake-codex-daemon-start-sequence");
        std::fs::write(
            &path,
            format!(
                r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
N=$(cat "{counter}" 2>/dev/null || echo 0)
N=$((N+1))
echo "$N" > "{counter}"
{branches}echo "sequence exhausted" >&2
exit 1
"#,
                counter = counter_path.display(),
                branches = branches,
            ),
        )
        .expect("write fake sequenced daemon-start");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// Polls a bounded condition instead of guessing a fixed sleep — the only safe way to
    /// synchronize with a background worker's own retry/backoff timing without racing it.
    fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("condition never became true within {timeout:?}");
    }

    /// Polls `project_state_dir`'s durable trace log until `marker` has appeared at least
    /// `at_least` times — the synchronization point these tests use to act on a background
    /// worker's progress only once it is durably proven, never guessed from a fixed sleep that
    /// could race however long the worker's own retry/backoff happens to take.
    fn wait_for_trace_line(
        project_state_dir: &Path,
        marker: &str,
        at_least: usize,
        timeout: Duration,
    ) {
        let path = project_state_dir.join(crate::auto_handoff::LOG_FILE_NAME);
        wait_for(timeout, || {
            std::fs::read_to_string(&path)
                .map(|log| log.matches(marker).count() >= at_least)
                .unwrap_or(false)
        });
    }

    #[test]
    fn the_worker_keeps_retrying_rediscovery_while_the_app_server_is_confirmed_dead_until_stopped()
    {
        // A confirmed-dead app-server identity must no longer end the loop outright — only an
        // explicit stop() may (see this module's own doc comment for why).
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws(&socket_path, &codex_home, usize::MAX);
        let dead = dead_identity();
        let failing_executable = install_fake_daemon_start_failing(scratch.path());

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker_stop = stop_flag.clone();
        let project_state_dir = scratch.path().to_path_buf();
        let profile_lock =
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock");
        let handle = thread::spawn(move || {
            attach_worker_loop(
                attach_target(&failing_executable, &codex_home, socket_path, dead),
                Some("thread-1".to_owned()),
                &worker_stop,
                &tx,
                &project_state_dir,
                profile_lock,
            );
        });
        // Several retry cycles' worth of time against the always-failing rediscovery.
        thread::sleep(Duration::from_millis(400));
        assert!(
            rx.try_recv().is_err(),
            "a confirmed-dead app-server must not end the loop on its own"
        );
        stop_flag.store(true, Ordering::Relaxed);
        let outcome = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("prompt outcome once stopped");
        assert!(matches!(outcome, AttachOutcome::GaveUp));
        handle.join().expect("worker thread joins promptly");

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert_eq!(
            log.matches("codex_event_driven_daemon_replaced").count(),
            1,
            "the confirmed replacement must be traced exactly once, not per retry: {log}"
        );
        assert!(
            log.contains("codex_event_driven_daemon_rediscovery_failed"),
            "a failing rediscovery attempt must be traced: {log}"
        );
    }

    #[test]
    fn a_replaced_daemon_is_rediscovered_and_the_worker_reattaches_to_the_same_thread() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");

        // Daemon A: attach succeeds against it first, exactly like an ordinary first attach.
        let socket_a = scratch.path().join("a.sock");
        let _listener_a = install_fake_app_server_ws(&socket_a, &codex_home, 0);
        let (mut child_a, identity_a) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_a.clone(),
                identity_a.clone(),
            ),
            Some("thread-1".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        match rx.recv().expect("outcome") {
            AttachOutcome::Attached { observer, .. } => observer.stop(),
            AttachOutcome::GaveUp => panic!("must succeed against daemon A"),
        }

        // Daemon A disappears for real; daemon B becomes current for the same codex_home.
        let _ = child_a.kill();
        let _ = child_a.wait();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(
            identity_a.is_still_the_same_process(),
            Some(false),
            "must be confirmed gone, not merely ambiguous, for this test to prove anything"
        );

        let socket_b = scratch.path().join("b.sock");
        let _listener_b = install_fake_app_server_ws(&socket_b, &codex_home, 0);
        let (mut child_b, identity_b) = live_identity();
        let rediscover_executable =
            install_fake_daemon_start(scratch.path(), identity_b.pid, &socket_b);

        // Exactly what `tick()`'s disconnect-handling branch does: spawn a fresh worker
        // generation seeded with the OLD (now-gone) endpoint/identity and the SAME known thread
        // id — never re-learned.
        let stop_flag_2 = AtomicBool::new(false);
        let (tx2, rx2) = mpsc::channel();
        attach_worker_loop(
            attach_target(&rediscover_executable, &codex_home, socket_a, identity_a),
            Some("thread-1".to_owned()),
            &stop_flag_2,
            &tx2,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        let outcome = rx2.recv_timeout(Duration::from_secs(5)).expect("outcome");
        let (observer2, endpoint2, identity2) = match outcome {
            AttachOutcome::Attached {
                observer,
                endpoint,
                identity,
                ..
            } => (observer, endpoint, identity),
            AttachOutcome::GaveUp => panic!("must recover by rediscovering the replacement daemon"),
        };
        assert_eq!(
            observer2.thread_id, "thread-1",
            "must reattach to the SAME known thread, never re-learn it"
        );
        assert_eq!(
            endpoint2, socket_b,
            "must carry back the rediscovered endpoint, not the stale one"
        );
        assert_eq!(
            identity2.pid, identity_b.pid,
            "must carry back the rediscovered identity"
        );
        observer2.stop();

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert_eq!(
            log.matches("codex_event_driven_daemon_replaced").count(),
            1,
            "exactly one replacement must be traced: {log}"
        );
        assert_eq!(
            log.matches("codex_event_driven_daemon_rediscovered")
                .count(),
            1,
            "exactly one successful rediscovery must be traced: {log}"
        );

        let _ = child_b.kill();
        let _ = child_b.wait();
    }

    #[test]
    fn repeated_daemon_replacement_a_then_b_then_c_all_recover_within_one_generation() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");

        let dead_a = dead_identity();
        // "B" is reported by rediscovery as current, and stays genuinely alive (a real `sleep 10`,
        // same as `live_identity()`) so `ensure_managed_daemon`'s own query of it — however long
        // that takes under load — reliably captures a real, confirmable fingerprint rather than
        // the unconfirmable `None` a query against an already-dead pid would (see
        // `relay_provider_codex::runtime`'s own `dead_identity()` test helper for why that
        // distinction matters). Its socket reports the *wrong* codex_home, so every attach against
        // it fails fast (`HomeMismatch`) rather than ever succeeding. The test only kills B once
        // the durable trace log itself confirms rediscovery already saw it — never a fixed sleep
        // racing against however long that query happens to take — so a second, genuinely
        // distinct rediscovery cycle (to C) is forced deterministically, not by timing luck.
        let socket_b = scratch.path().join("b.sock");
        let wrong_codex_home = scratch.path().join("not_the_codex_home");
        std::fs::create_dir_all(&wrong_codex_home).expect("wrong codex home");
        let _listener_b = install_fake_app_server_ws(&socket_b, &wrong_codex_home, 0);
        let (mut child_b, identity_b) = live_identity();

        let socket_c = scratch.path().join("c.sock");
        let _listener_c = install_fake_app_server_ws(&socket_c, &codex_home, 0);
        let (mut child_c, identity_c) = live_identity();

        let counter_path = scratch.path().join("rediscover-count");
        let executable = install_fake_daemon_start_sequence(
            scratch.path(),
            &counter_path,
            &[
                (identity_b.pid, socket_b),
                (identity_c.pid, socket_c.clone()),
            ],
        );

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker_stop = stop_flag.clone();
        let project_state_dir = scratch.path().to_path_buf();
        let a_socket = scratch.path().join("a.sock");
        let profile_lock =
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock");
        let handle = thread::spawn(move || {
            attach_worker_loop(
                attach_target(&executable, &codex_home, a_socket, dead_a),
                Some("thread-1".to_owned()),
                &worker_stop,
                &tx,
                &project_state_dir,
                profile_lock,
            );
        });

        // Order, proven by durable evidence, never assumed by timing: only kill B once the trace
        // log itself shows rediscovery already completed once (B was found and is now the one
        // being retried against).
        wait_for_trace_line(
            scratch.path(),
            "codex_event_driven_daemon_rediscovered",
            1,
            Duration::from_secs(5),
        );
        let _ = child_b.kill();
        let _ = child_b.wait();
        // `ps`-based confirmation that B is gone can itself take a moment under load; wait for it
        // directly rather than assuming any fixed delay is enough.
        wait_for(Duration::from_secs(5), || {
            identity_b.is_still_the_same_process() == Some(false)
        });

        let outcome = rx.recv_timeout(Duration::from_secs(10)).expect("outcome");
        let (observer, endpoint, identity) = match outcome {
            AttachOutcome::Attached {
                observer,
                endpoint,
                identity,
                ..
            } => (observer, endpoint, identity),
            AttachOutcome::GaveUp => panic!("must eventually recover via daemon C"),
        };
        assert_eq!(observer.thread_id, "thread-1");
        assert_eq!(endpoint, socket_c);
        assert_eq!(identity.pid, identity_c.pid);
        observer.stop();
        handle
            .join()
            .expect("worker thread joins after returning its outcome");

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert_eq!(
            log.matches("codex_event_driven_daemon_replaced").count(),
            2,
            "both A's and B's replacement must each be traced once: {log}"
        );
        assert_eq!(
            log.matches("codex_event_driven_daemon_rediscovered")
                .count(),
            2,
            "both successful rediscoveries (B, then C) must be traced: {log}"
        );

        let _ = child_c.kill();
        let _ = child_c.wait();
    }

    #[test]
    fn a_rediscovered_reattach_requests_reconciliation_exactly_once_through_tick() {
        // Feed an already-resolved "daemon replaced, rediscovered, reattached" AttachOutcome
        // through EventDrivenRuntime::tick(), exactly as the real background worker would deliver
        // it, to verify the higher-level wiring: reconciliation requested exactly once, and the
        // runtime's own stored endpoint/identity are updated to the rediscovered daemon rather
        // than left stale.
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_b = scratch.path().join("b.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener_b = install_fake_app_server_ws(&socket_b, &codex_home, 0);
        let (mut child_b, identity_b) = live_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            attach_target(
                Path::new("/definitely/not/codex"),
                &codex_home,
                socket_b.clone(),
                identity_b.clone(),
            ),
            Some("thread-1".to_owned()),
            &stop_flag,
            &tx,
            scratch.path(),
            ProfileSessionLock::try_acquire(scratch.path()).expect("test profile lock"),
        );
        let observer = match rx.recv().expect("outcome") {
            AttachOutcome::Attached { observer, .. } => observer,
            AttachOutcome::GaveUp => panic!("must succeed against daemon B"),
        };

        let (outcome_tx, outcome_rx) = mpsc::channel();
        outcome_tx
            .send(AttachOutcome::Attached {
                observer,
                endpoint: socket_b.clone(),
                identity: identity_b.clone(),
                profile_lock: ProfileSessionLock::try_acquire(scratch.path())
                    .expect("test profile lock"),
            })
            .expect("queue outcome");
        // Seed the runtime with a STALE endpoint/identity (as if still attached to the old,
        // now-replaced daemon A) to prove tick() overwrites them with the rediscovered values.
        let stale_identity = dead_identity();
        let mut runtime = EventDrivenRuntime {
            endpoint: scratch.path().join("stale-a.sock"),
            daemon_identity: stale_identity,
            codex_home: codex_home.clone(),
            codex_executable: PathBuf::from("/definitely/not/codex"),
            project_state_dir: scratch.path().to_path_buf(),
            profile_lock: None,
            state: AttachState::Attaching {
                outcome_rx,
                stop_flag: Arc::new(AtomicBool::new(false)),
            },
        };
        let mut scheduler = crate::codex_poll::CodexPollScheduler::new(
            scratch.path().to_path_buf(),
            scratch.path().to_path_buf(),
            None,
        );
        let event = runtime.tick(&mut scheduler);
        assert_eq!(event, TickEvent::Attached("thread-1".to_owned()));
        assert_eq!(
            runtime.endpoint, socket_b,
            "must adopt the rediscovered endpoint, not keep the stale one"
        );
        assert_eq!(
            runtime.daemon_identity, identity_b,
            "must adopt the rediscovered identity, not keep the stale one"
        );

        let log = std::fs::read_to_string(scratch.path().join(crate::auto_handoff::LOG_FILE_NAME))
            .expect("durable trace log");
        assert_eq!(
            log.matches("codex_reconnect_reconciliation_requested")
                .count(),
            1,
            "reconciliation must be requested exactly once per reattach: {log}"
        );
        assert!(log.contains("codex_event_driven_attached"));

        let _ = child_b.kill();
        let _ = child_b.wait();
    }
}
