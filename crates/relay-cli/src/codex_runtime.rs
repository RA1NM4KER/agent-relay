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
//!                 │  (worker succeeds)                │ (daemon dies / stop() called
//!                 ▼                                    │  before first success)
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

use relay_core::handoff::ProcessIdentity;
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
/// How often the backoff sleep wakes up to check the stop flag — keeps `stop()` responsive
/// without a tight loop.
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(100);

#[must_use]
fn event_driven_enabled() -> bool {
    std::env::var(EVENT_DRIVEN_ENV).ok().as_deref() == Some("1")
}

/// What the background attach/reconnect worker sends back once it either succeeds or gives up
/// permanently. There is no "retry failed, try again" message: the worker keeps retrying
/// silently on its own local backoff and only ever reports a *terminal* outcome for one attempt
/// cycle.
enum AttachOutcome {
    Attached(Observer),
    /// The app-server was confirmed gone, or `stop()` was called before any attach succeeded.
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
    /// backoff first.
    Attaching {
        outcome_rx: mpsc::Receiver<AttachOutcome>,
        stop_flag: Arc<AtomicBool>,
    },
    /// Currently attached and receiving live events.
    Attached(Observer),
    /// Gave up permanently for this session (app-server confirmed dead, or shutdown was
    /// requested before a first successful attach). Terminal — nothing restarts this.
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
    /// GitHub #18: where this project's durable auto-handoff trace lives — used only to record
    /// event-receipt/reconciliation-request provenance (see [`Self::tick`]).
    project_state_dir: PathBuf,
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
        let Ok(daemon) = ensure_managed_daemon(codex_executable, config_dir) else {
            crate::auto_handoff::trace_event(
                project_state_dir,
                "codex_event_driven_daemon_unavailable",
            );
            return None;
        };
        crate::auto_handoff::trace_event(project_state_dir, "codex_event_driven_daemon_ready");
        let state = spawn_attach_worker(
            daemon.endpoint.clone(),
            config_dir.to_path_buf(),
            thread_id.map(str::to_owned),
            daemon.identity.clone(),
            project_state_dir.to_path_buf(),
        );
        Some(Self {
            endpoint: daemon.endpoint,
            daemon_identity: daemon.identity,
            codex_home: config_dir.to_path_buf(),
            project_state_dir: project_state_dir.to_path_buf(),
            state,
        })
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
                Ok(AttachOutcome::Attached(observer)) => {
                    let thread_id = observer.thread_id.clone();
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
                for event in observer.drain_events() {
                    if matches!(event, ObserverEvent::Disconnected(_)) {
                        disconnected = true;
                    } else {
                        // GitHub #18: durable proof that Relay actually received this event, recorded
                        // at the point it is drained/accepted from the observer — before it is ever
                        // handed to the scheduler, so a later evaluation can never be misattributed
                        // to timing alone.
                        if let Some(label) = event_receipt_trace_label(&event) {
                            crate::auto_handoff::trace_event(&self.project_state_dir, label);
                        }
                        scheduler.notify_event(event);
                    }
                }
                if disconnected {
                    let known_thread_id = observer.thread_id.clone();
                    // GitHub #18 (observability follow-up): a mid-session disconnect previously
                    // left the same kind of durable gap as a `GaveUp` that never attached.
                    crate::auto_handoff::trace_event(
                        &self.project_state_dir,
                        "codex_event_driven_observer_disconnected",
                    );
                    self.state = spawn_attach_worker(
                        self.endpoint.clone(),
                        self.codex_home.clone(),
                        Some(known_thread_id.clone()),
                        self.daemon_identity.clone(),
                        self.project_state_dir.clone(),
                    );
                    TickEvent::Reconnecting(known_thread_id)
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
                // Signalled, not joined: the worker's own per-attempt bound
                // (`Observer::attach`'s internal timeout) guarantees it notices this and exits on
                // its own within that bound even if it is mid-attempt right now, and this
                // process's own exit (see `terminal_session::run_managed_terminal`) reaps any
                // thread outright regardless — joining here would risk blocking ordinary shutdown
                // on a worker that is, at worst, seconds from exiting on its own.
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
/// should install immediately (never blocking on the worker's first result).
fn spawn_attach_worker(
    endpoint: PathBuf,
    codex_home: PathBuf,
    known_thread_id: Option<String>,
    app_server_identity: ProcessIdentity,
    project_state_dir: PathBuf,
) -> AttachState {
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let worker_stop = stop_flag.clone();
    thread::spawn(move || {
        attach_worker_loop(
            &endpoint,
            &codex_home,
            known_thread_id,
            &app_server_identity,
            &worker_stop,
            &outcome_tx,
            &project_state_dir,
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
    }
}

/// The retry loop itself: attempts [`Observer::attach`], remembering a learned thread id across
/// attempts (GitHub #17: `thread/resume` failing with `ResumeNotYetReady` means "no turn has
/// started yet," not "give up" — and re-waiting for `thread/started` on a later attempt would
/// hang for that call's own full internal timeout, since the broadcast never repeats). Stops
/// retrying the moment any of the following becomes true: attach succeeds; `stop_flag` is set;
/// the app-server is confirmed (not merely unconfirmable) gone.
///
/// GitHub #18 (observability follow-up): a real natural exhaustion left no durable evidence of
/// *why* this loop never once succeeded — only ephemeral stderr, never captured. Every *distinct*
/// failure kind is now traced durably the first time this generation of the loop sees it (never
/// per-retry, which at this loop's backoff could otherwise write hundreds of near-identical lines
/// over a long wait) — enough to prove, after the fact, whether the loop ever got past connect,
/// handshake, or resume, without flooding the trace.
fn attach_worker_loop(
    endpoint: &Path,
    codex_home: &Path,
    mut known_thread_id: Option<String>,
    app_server_identity: &ProcessIdentity,
    stop_flag: &AtomicBool,
    outcome_tx: &mpsc::Sender<AttachOutcome>,
    project_state_dir: &Path,
) {
    let mut backoff = RECONNECT_BACKOFF_MIN;
    let mut last_traced_error: Option<&'static str> = None;
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            let _ignored = outcome_tx.send(AttachOutcome::GaveUp);
            return;
        }
        // A merely-unconfirmable liveness reading (`None`) is not a reason to give up retrying —
        // that would be over-eager for what is only a "should I keep trying" decision, not a
        // safety-relevant ownership one. Only a definite `Some(false)` (the app-server is
        // provably gone) ends the retry loop.
        if app_server_identity.is_still_the_same_process() == Some(false) {
            let _ignored = outcome_tx.send(AttachOutcome::GaveUp);
            return;
        }
        match Observer::attach(endpoint, codex_home, known_thread_id.as_deref()) {
            Ok(observer) => {
                let _ignored = outcome_tx.send(AttachOutcome::Attached(observer));
                return;
            }
            Err(error) => {
                let label = attach_error_label(&error);
                if last_traced_error != Some(label) {
                    crate::auto_handoff::trace_event(project_state_dir, label);
                    last_traced_error = Some(label);
                }
                if let ObserverError::ResumeNotYetReady(id) = error {
                    known_thread_id = Some(id);
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
/// live-verified `--listen`/`--remote`/the two structured signals against (agent-relay#15, pinned
/// to `codex-cli 0.155.0` — see `relay_provider_codex::inspection::VERIFIED_VERSIONS`). An
/// unverified or unknown version — including a version whose own `--version` cannot even be
/// read — silently declines rather than guessing at flags/behavior a future or older Codex
/// release may not actually support; #13 remains fully active regardless.
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
        );
        assert!(result.is_none());
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
            &socket_path,
            &codex_home,
            Some("thread-1".to_owned()),
            &identity,
            &stop_flag,
            &tx,
            scratch.path(),
        );
        match rx.recv().expect("outcome") {
            AttachOutcome::Attached(observer) => {
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
            &socket_path,
            &codex_home,
            Some("thread-1".to_owned()),
            &identity,
            &stop_flag,
            &tx,
            scratch.path(),
        );
        let observer = match rx.recv().expect("outcome") {
            AttachOutcome::Attached(observer) => observer,
            AttachOutcome::GaveUp => panic!("must succeed against a fake that never fails resume"),
        };

        // Feed the already-resolved outcome through `EventDrivenRuntime::tick` itself, exactly as
        // `spawn_attach_worker`'s background thread would deliver it, so this test exercises the
        // real transition that decides what gets traced.
        let (outcome_tx, outcome_rx) = mpsc::channel();
        outcome_tx
            .send(AttachOutcome::Attached(observer))
            .expect("queue outcome");
        let mut runtime = EventDrivenRuntime {
            endpoint: socket_path,
            daemon_identity: identity,
            codex_home,
            project_state_dir: scratch.path().to_path_buf(),
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
                &socket_path,
                &codex_home,
                Some("thread-1".to_owned()),
                &identity,
                &worker_stop,
                &tx,
                &project_state_dir,
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

    #[test]
    fn the_worker_gives_up_once_the_app_server_is_confirmed_dead() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let _listener = install_fake_app_server_ws(&socket_path, &codex_home, usize::MAX);
        let dead = dead_identity();

        let stop_flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        attach_worker_loop(
            &socket_path,
            &codex_home,
            Some("thread-1".to_owned()),
            &dead,
            &stop_flag,
            &tx,
            scratch.path(),
        );
        let outcome = rx.recv_timeout(Duration::from_secs(2)).expect("outcome");
        assert!(matches!(outcome, AttachOutcome::GaveUp));
    }
}
