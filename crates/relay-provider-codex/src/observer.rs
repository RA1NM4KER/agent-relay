//! GitHub #16: a passive, read-mostly WebSocket connection to an external `codex app-server
//! --listen unix://…`, decoding only the two structured signals in [`crate::events`] and handing
//! them to a caller through a plain channel — the same background-thread-plus-channel shape
//! `crate::app_server::Session` already uses for its own stdio reads, just kept open for the
//! whole interactive session instead of one request/response.
//!
//! This never drives a turn, never mutates thread state beyond the one documented subscribe call
//! (`thread/resume`, live-verified by agent-relay#15 to rejoin a thread's live notification
//! stream without disturbing whichever connection is actually driving it), and never becomes
//! authoritative: every event it surfaces is a wake-up hint for [`crate::usage::CodexUsageSignal`]
//! to re-check, exactly as GitHub #13's own polling already does on its own schedule.

use std::{
    cell::Cell,
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::events;

/// One decoded, sanitized signal from the observed runtime. Never raw provider output, never an
/// account id, never conversation content — only what the two functions in `crate::events`
/// already decided, or (GitHub #17/#18) the fact that this connection is no longer usable.
///
/// Not `Copy` (GitHub #18 mid-session thread-replacement follow-up): [`Self::ThreadReplaced`]
/// carries an owned thread id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObserverEvent {
    /// A turn on the observed thread failed with the structured `usageLimitExceeded` error.
    /// Callers must treat this exactly like GitHub #13's own trigger: start the existing
    /// authoritative `account/rateLimits/read` evaluation, never hand off directly on this alone.
    UsageLimitExceeded,
    /// `account/rateLimits/updated` fired with this sanitized highest-window `usedPercent` (never
    /// `ordinaryUsageAllowed`, never an account id — the type itself cannot carry either). Feeds
    /// `crate::polling::poll_interval_secs`'s existing cadence policy only.
    RateLimitsHint(Option<u32>),
    /// The background connection is gone and this `Observer` will never produce another live
    /// event — GitHub #17's identified gap: without an explicit signal, a dead observer was
    /// indistinguishable from "nothing happened yet." Always the *last* event a given `Observer`
    /// ever emits (see [`Observer::drain_events`]'s own doc comment for the exactly-once
    /// guarantee). Callers must reconnect (a fresh `Observer::attach` against the same endpoint
    /// and thread id) and, once that succeeds, run exactly one authoritative reconciliation read
    /// before resuming event-driven supervision — never inferred from event silence alone.
    Disconnected(DisconnectReason),
    /// GitHub #18 follow-up: the app-server's own authoritative, pushed lifecycle signal
    /// (`thread/closed`/`thread/archived`/`thread/deleted`, or `thread/status/changed` reporting
    /// `notLoaded`) reported *this exact subscribed thread* as ended — never inferred from an
    /// absence of other events. Unlike `Disconnected`, the connection itself is still live; only
    /// this specific thread is confirmed done. Informational only as of the GitHub #18 mid-session
    /// thread-replacement follow-up: the underlying connection is **not** torn down over this —
    /// [`Self::ThreadReplaced`] (or [`Self::Disconnected`], if the connection itself also fails
    /// while no replacement has been learned yet) is the actionable follow-up signal.
    ThreadEnded,
    /// GitHub #18 (mid-session thread-replacement follow-up): after a [`Self::ThreadEnded`] for
    /// the previously-subscribed thread, this exact same already-live connection (never a
    /// reconnect) waited for the next `thread/started` broadcast and successfully resumed it.
    /// Carries the new thread id — authoritative under profile exclusivity for the same reason a
    /// fresh launch's self-learned id is (see `relay_cli::codex_runtime::KnownThread`'s own doc
    /// comment): there is no second Relay-managed TUI this broadcast could have come from, and
    /// (unlike a fresh launch) no reconnect ever happened for it to race.
    ThreadReplaced(String),
}

/// Why the background connection ended, for diagnostics only — every variant is handled
/// identically by a caller (reconnect), so this is informational, never a branch point for
/// safety-relevant behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    /// The WebSocket closed normally (a clean close handshake, or our own [`Observer::stop`]).
    Closed,
    /// Anything else: a transport error, a protocol violation the connection can no longer be
    /// trusted after, or the reader thread's channel itself reporting the sender is gone.
    ReadError,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ObserverError {
    Connect,
    Handshake,
    /// The server resolved a different `CODEX_HOME` than expected — the same profile-isolation
    /// check `crate::app_server::Session::handshake` already makes for the ephemeral reads.
    HomeMismatch,
    /// No thread became known within the bounded wait (see [`Observer::attach`]'s doc comment).
    NoThread,
    /// A thread id was learned (carried here), but `thread/resume` itself failed with the one
    /// specific, live-confirmed (agent-relay#17) signature that means "no turn has started on
    /// this thread yet": an explicit JSON-RPC error whose message is `"no rollout found for
    /// thread id …"`. A retrying caller (GitHub #18) uses the carried id on its next attempt
    /// rather than re-learning it — `thread/started` only ever broadcasts once per thread, so
    /// waiting for it again would hang for the full attach timeout, and this specific failure is
    /// confirmed benign/transient.
    ResumeNotYetReady(String),
    /// A thread id was learned (carried here), but `thread/resume` failed with an explicit
    /// JSON-RPC error whose message does **not** match the known "no rollout found" signature
    /// above (GitHub #18 follow-up — a real `~/repos` natural exhaustion left an observer
    /// permanently unattached for ~14 minutes with no durable way to tell whether `thread/resume`
    /// was failing for the confirmed-benign reason or something else entirely). Unlike
    /// `ResumeNotYetReady`, this is an affirmative server-side rejection of this exact id — never
    /// proof the id is permanently wrong (the server's own error vocabulary for `thread/resume` is
    /// not fully enumerated here), but different enough from "just wait" that a caller should
    /// consider re-learning the active thread rather than only ever retrying the same id forever.
    ResumeRejected(String),
    /// A thread id was learned (carried here), but no response to `thread/resume` (matching or
    /// otherwise) arrived within [`ATTACH_TIMEOUT`] at all — distinct from an explicit rejection:
    /// the daemon may simply be busy servicing other traffic (GitHub #18 follow-up: a real natural
    /// exhaustion's own native rollout showed continuous heavy tool-call activity throughout the
    /// window the observer never reattached in, consistent with resume requests timing out against
    /// a busy daemon rather than being explicitly rejected). Never proof the id is wrong — a
    /// retrying caller keeps the same id, exactly like `ResumeNotYetReady`, but traces this
    /// distinctly so a future occurrence is diagnosable instead of indistinguishable from the
    /// confirmed-benign case.
    ResumeTimedOut(String),
    /// A thread id was learned (carried here), and — while still waiting for `thread/resume`'s
    /// own response — the app-server's own authoritative, pushed lifecycle signal
    /// (`thread/closed`/`thread/archived`/`thread/deleted`, or `thread/status/changed` reporting
    /// `notLoaded`) for this *exact* id arrived first (GitHub #18 follow-up: confirmed via the
    /// real `codex app-server generate-json-schema` output, never guessed — see
    /// `crate::events::notification_reports_this_thread_ended`'s own doc comment). Unlike
    /// `ResumeNotYetReady`/`ResumeRejected`/`ResumeTimedOut`, this is not an inference from a
    /// resume outcome at all — it is the server itself saying this id is done. A retrying caller
    /// should re-learn the active thread immediately, with no threshold to wait out first.
    ResumeTargetEnded(String),
}

/// The known, live-confirmed (agent-relay#17) `thread/resume` error signature for "no turn has
/// started on this thread yet" — matched as a case-insensitive substring so minor wording/casing
/// changes across Codex releases don't silently misclassify a benign condition as a rejection.
/// Never the only signal trusted blindly: callers must still retry on any outcome short of
/// success, this only changes which durable trace label and recovery hint a failure gets.
const NO_ROLLOUT_YET_SIGNATURE: &str = "no rollout found";

#[must_use]
fn is_no_rollout_yet(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains(NO_ROLLOUT_YET_SIGNATURE)
}

impl std::fmt::Display for ObserverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect => write!(f, "could not connect to the runtime's private endpoint"),
            Self::Handshake => write!(f, "the runtime did not answer the protocol handshake"),
            Self::HomeMismatch => write!(f, "the runtime resolved a different CODEX_HOME"),
            Self::NoThread => write!(f, "no thread became known within the bounded wait"),
            Self::ResumeNotYetReady(id) => {
                write!(f, "thread {id} has not started its first turn yet")
            }
            Self::ResumeRejected(id) => {
                write!(f, "thread {id} was explicitly rejected by thread/resume")
            }
            Self::ResumeTimedOut(id) => {
                write!(f, "thread {id} got no thread/resume response in time")
            }
            Self::ResumeTargetEnded(id) => {
                write!(f, "thread {id} was authoritatively reported ended")
            }
        }
    }
}

/// Bounded wait for the app-server socket handshake and for learning a thread id when none was
/// supplied up front (a fresh launch, where the interactive client creates the thread itself).
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
/// The underlying socket's own read timeout during attach, so the `ATTACH_TIMEOUT` deadline is
/// actually re-checked this often rather than only between whole blocking reads.
const ATTACH_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// A live, background-threaded subscription to one thread's structured events. Dropping this
/// without calling [`Self::stop`] still cleanly stops the reader thread (the underlying socket is
/// shut down either way), but [`Self::stop`] is preferred so a caller can join deterministically
/// as part of ordered session cleanup.
pub struct Observer {
    events: Receiver<ObserverEvent>,
    shutdown: UnixStream,
    reader: Option<thread::JoinHandle<()>>,
    /// Guards against emitting `Disconnected` more than once from [`Self::drain_events`] — the
    /// channel itself keeps reporting `Disconnected` on every `try_recv` once the sender is gone,
    /// and a caller must see that fact exactly once, not on every 300ms tick forever (GitHub
    /// #18's "duplicate disconnect signals are coalesced" requirement).
    disconnect_reported: Cell<bool>,
    /// GitHub #18 (mid-session thread-replacement follow-up): shared with the background reader
    /// thread, which updates this in place on a successful same-connection [`ObserverEvent::ThreadReplaced`]
    /// — a plain `String` field could not reflect that without unsafe aliasing, since the reader
    /// thread already owns the socket and keeps running. Read via [`Self::thread_id`].
    thread_id: Arc<Mutex<String>>,
}

impl Observer {
    /// Connects to `endpoint` (a unix socket path from [`crate::runtime::allocate_endpoint`]),
    /// verifies the server resolved `expected_codex_home`, and subscribes to `thread_id` if
    /// given — or, if `None` (a fresh launch, before the interactive client has created a
    /// thread), waits up to [`ATTACH_TIMEOUT`] for the first `thread/started` broadcast (received
    /// by every connected client regardless of subscription, live-verified by agent-relay#15) and
    /// subscribes to that thread instead.
    ///
    /// **Known limitation of the `None` path, live-confirmed against `codex-cli 0.155.0`:**
    /// `thread/resume` answers `"no rollout found for thread id …"` for a thread that has not yet
    /// completed a single turn (no rollout file has been persisted for it yet) — which is exactly
    /// the state a thread is in the instant its own `thread/started` broadcast fires, before the
    /// interactive client has sent its first message. This makes the `None` path fragile for a
    /// genuinely fresh launch and it is **not** currently used by `relay-cli`'s production wiring
    /// for that reason — only the `Some(thread_id)` path (a resume of an already-populated
    /// thread, which never hits this) is. Fixing this properly needs a bounded retry against the
    /// specific "no rollout found" response, tolerant of however long a real user takes to send
    /// their first message; left for a follow-up rather than shipping an under-tested retry loop
    /// here. The `None` path's tests below use a scripted fake server that does not reproduce this
    /// specific real-server behavior, so they verify the broadcast-learning mechanism itself, not
    /// this timing edge case.
    pub fn attach(
        endpoint: &Path,
        expected_codex_home: &Path,
        thread_id: Option<&str>,
    ) -> Result<Self, ObserverError> {
        let socket = Self::connect_and_handshake(endpoint, expected_codex_home)?;
        Self::finish_attach(socket, thread_id)
    }

    /// GitHub #18 (fresh-attach determinism follow-up): the connect-plus-handshake half of
    /// [`Self::attach`], split out so a caller (`codex_runtime::EventDrivenRuntime::start`) can
    /// establish this connection — and have it actively listening — *before* the interactive
    /// Codex client is ever spawned for a fresh launch. `thread/started` broadcasts exactly once,
    /// system-wide, to whoever is already connected at that moment (live-confirmed, see this
    /// module's own doc comment); a caller that only opens this connection *after* launching the
    /// interactive client is racing it, and a reconnect attempt that opens a brand-new connection
    /// after a failed one closes can miss the one and only broadcast forever. Establishing the
    /// connection first and handing the same already-open socket to [`Self::finish_attach`] is
    /// what closes that race — never a guess, never a longer timeout.
    pub fn connect_and_handshake(
        endpoint: &Path,
        expected_codex_home: &Path,
    ) -> Result<WebSocket<UnixStream>, ObserverError> {
        let stream = connect_unix(endpoint)?;
        // A blocking `UnixStream` read has no timeout by default, so without this, every bounded
        // wait below (`Instant::now() < deadline`, re-checked only *between* calls to
        // `socket.read()`) is an illusion: one `read()` that never returns blocks past its own
        // deadline forever. Cleared once attach succeeds (see [`Self::finish_attach`]) so the
        // long-lived background reader can block indefinitely without busy-polling.
        stream
            .set_read_timeout(Some(ATTACH_POLL_INTERVAL))
            .map_err(|_| ObserverError::Connect)?;
        handshake(stream, expected_codex_home)
    }

    /// GitHub #18 (fresh-attach determinism follow-up): the remainder of [`Self::attach`] given
    /// an already-connected, already-handshaken socket (see [`Self::connect_and_handshake`]'s own
    /// doc comment for why these are split) — waits for a thread id if none is given, then
    /// subscribes via `thread/resume`.
    pub fn finish_attach(
        mut socket: WebSocket<UnixStream>,
        thread_id: Option<&str>,
    ) -> Result<Self, ObserverError> {
        let shutdown_handle = socket
            .get_ref()
            .try_clone()
            .map_err(|_| ObserverError::Connect)?;
        let resolved_thread_id = match thread_id {
            Some(id) => id.to_owned(),
            None => wait_for_first_thread(&mut socket)?,
        };
        // A resume failure here is reported with the id still attached — never collapsed into a
        // single generic error — specifically so a retrying caller (GitHub #18) can tell apart the
        // one confirmed-benign signature (`ResumeNotYetReady`, safe to retry the same id forever)
        // from an affirmative rejection (`ResumeRejected`, a cue to consider re-learning the active
        // thread) and a bare timeout (`ResumeTimedOut`, informative but not proof of anything about
        // the id itself).
        if let Err(failure) = resume(&mut socket, &resolved_thread_id) {
            return Err(match failure {
                ResumeFailure::Rejected(message) if is_no_rollout_yet(&message) => {
                    ObserverError::ResumeNotYetReady(resolved_thread_id)
                }
                ResumeFailure::Rejected(_) => ObserverError::ResumeRejected(resolved_thread_id),
                ResumeFailure::TimedOut => ObserverError::ResumeTimedOut(resolved_thread_id),
                ResumeFailure::TargetEnded => ObserverError::ResumeTargetEnded(resolved_thread_id),
            });
        }
        // GitHub #18 (mid-session thread-replacement follow-up — live-reproduced under heavy
        // parallel test load): clearing the attach-time read timeout can itself fail (observed
        // `EINVAL`) if the peer closes at essentially the same instant `resume` above succeeds —
        // a real possibility, not just a test artifact, since a daemon can legitimately tear down
        // a subscription right after confirming it. Failing attach outright over this would be
        // wrong: the resume already succeeded, so this is a live, attached connection that happens
        // to be closing — exactly what [`read_loop`] (tolerant of its own read timeout remaining
        // set — see that function's own doc comment) and [`Observer::drain_events`] already exist
        // to report, via an ordinary `Disconnected`, not a confusing attach failure. Best-effort
        // only: never fails attach itself.
        let _ = socket.get_ref().set_read_timeout(None);

        let (sender, events) = mpsc::channel();
        let thread_id = Arc::new(Mutex::new(resolved_thread_id));
        let reader_thread_id = Arc::clone(&thread_id);
        let reader = thread::spawn(move || read_loop(socket, sender, reader_thread_id));
        Ok(Self {
            events,
            shutdown: shutdown_handle,
            reader: Some(reader),
            disconnect_reported: Cell::new(false),
            thread_id,
        })
    }

    /// The thread id this observer is subscribed to *right now* — GitHub #18 (mid-session
    /// thread-replacement follow-up): after a same-connection [`ObserverEvent::ThreadReplaced`],
    /// this reflects the replacement, never the one originally attached to. A method rather than a
    /// plain field precisely because it can change out from under a caller holding `&Observer`
    /// without that caller needing a `&mut`.
    #[must_use]
    pub fn thread_id(&self) -> String {
        self.thread_id.lock().expect("thread_id mutex").clone()
    }

    /// Non-blocking drain — the caller's own tick loop (GitHub #13's existing 300ms cadence,
    /// unaffected) calls this; it must never wait for a new event. Once the background connection
    /// is gone, this yields exactly one [`ObserverEvent::Disconnected`] (whichever comes first:
    /// the reader thread's own explicit sentinel, already queued ahead of the channel closing, or
    /// — if the thread panicked before it could send one — a synthesized
    /// `Disconnected(DisconnectReason::ReadError)` from the channel disconnection itself) and
    /// never repeats it on a later call, even though the underlying channel would happily keep
    /// answering `Disconnected` forever.
    pub fn drain_events(&self) -> Vec<ObserverEvent> {
        let mut drained = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => drained.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !drained
                        .iter()
                        .any(|event| matches!(event, ObserverEvent::Disconnected(_)))
                    {
                        drained.push(ObserverEvent::Disconnected(DisconnectReason::ReadError));
                    }
                    break;
                }
            }
        }
        let has_disconnect = drained
            .iter()
            .any(|event| matches!(event, ObserverEvent::Disconnected(_)));
        if has_disconnect && self.disconnect_reported.replace(true) {
            // Already reported on an earlier call — this call learned nothing new.
            return Vec::new();
        }
        drained
    }

    /// Shuts the socket down (unblocking the reader thread's pending `read()`) and joins it.
    /// Idempotent-ish in effect: a second call after the reader has already exited is harmless.
    pub fn stop(mut self) {
        let _ignored = self.shutdown.shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ignored = reader.join();
        }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        let _ignored = self.shutdown.shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ignored = reader.join();
        }
    }
}

fn connect_unix(endpoint: &Path) -> Result<UnixStream, ObserverError> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    loop {
        match UnixStream::connect(endpoint) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Err(ObserverError::Connect),
        }
    }
}

/// The WebSocket opening handshake plus the app-server's own `initialize`/`initialized`
/// handshake, with the same `codexHome` verification `crate::app_server::Session::handshake`
/// already performs for the ephemeral one-shot reads.
fn handshake(
    stream: UnixStream,
    expected_codex_home: &Path,
) -> Result<WebSocket<UnixStream>, ObserverError> {
    // Unix-socket connections have no real host; tungstenite only needs a syntactically valid
    // request URL to build the HTTP Upgrade headers — the server itself does not validate `Host`
    // for this private, filesystem-permissioned endpoint.
    let mut socket = tungstenite::client("ws://localhost/", stream)
        .map(|(socket, _response)| socket)
        .map_err(|_| ObserverError::Handshake)?;
    let initialize_id = "relay-observer-initialize";
    send_json(
        &mut socket,
        &json!({
            "jsonrpc": "2.0",
            "id": initialize_id,
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "agent-relay-observer", "title": "Agent Relay (observer)", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": false},
            },
        }),
    )
    .map_err(|_| ObserverError::Handshake)?;
    let result = read_matching_result(&mut socket, initialize_id)?;
    let reported = result
        .get("codexHome")
        .and_then(Value::as_str)
        .ok_or(ObserverError::Handshake)?;
    if !same_directory(Path::new(reported), expected_codex_home) {
        return Err(ObserverError::HomeMismatch);
    }
    send_json(
        &mut socket,
        &json!({"jsonrpc": "2.0", "method": "initialized"}),
    )
    .map_err(|_| ObserverError::Handshake)?;
    Ok(socket)
}

fn same_directory(a: &Path, b: &Path) -> bool {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(a) == canonical(b)
}

/// `true` for the underlying socket's own read-timeout error (the read timeout `Observer::attach`
/// sets before this loop starts) — the *expected*, retryable outcome of "nothing arrived yet,"
/// never treated the same as a real protocol/connection failure.
fn is_timeout(error: &tungstenite::Error) -> bool {
    matches!(
        error,
        tungstenite::Error::Io(io_error)
            if matches!(io_error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
    )
}

/// Waits for the first `thread/started` broadcast (live-verified by agent-relay#15 to reach every
/// connected client, subscribed or not) and returns its thread id. Bounded by `ATTACH_TIMEOUT`
/// for real this time: the underlying socket has its own short read timeout (set by
/// `Observer::attach` before this runs), so the outer deadline is actually re-checked on every
/// timed-out `read()`, not only between whole messages.
fn wait_for_first_thread(socket: &mut WebSocket<UnixStream>) -> Result<String, ObserverError> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    while Instant::now() < deadline {
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) if is_timeout(&error) => continue,
            Err(_) => return Err(ObserverError::NoThread),
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("method").and_then(Value::as_str) == Some("thread/started")
            && let Some(id) = value
                .get("params")
                .and_then(|params| params.get("thread"))
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
        {
            return Ok(id.to_owned());
        }
    }
    Err(ObserverError::NoThread)
}

/// Why `thread/resume` did not succeed, kept distinct from [`ObserverError`] itself (which also
/// needs to classify the one known-benign message, done by [`Observer::attach`] after this
/// returns) — see [`read_matching_result_for_resume`]'s own doc comment for how each is produced.
enum ResumeFailure {
    /// A JSON-RPC `error` response matching `resume_id` arrived, carrying this message.
    Rejected(String),
    /// Nothing matching `resume_id` (a response or a transport failure) arrived within
    /// [`ATTACH_TIMEOUT`] at all.
    TimedOut,
    /// GitHub #18 follow-up: the app-server's own authoritative lifecycle signal for this exact
    /// `threadId` arrived *before* a matching `thread/resume` response did — see
    /// `crate::events::notification_reports_this_thread_ended`'s own doc comment. Strictly more
    /// authoritative than any resume outcome, so this always wins even over a `result` that might
    /// still be sitting unread behind it.
    TargetEnded,
}

fn resume(socket: &mut WebSocket<UnixStream>, thread_id: &str) -> Result<(), ResumeFailure> {
    let resume_id = "relay-observer-resume";
    send_json(
        socket,
        &json!({
            "jsonrpc": "2.0",
            "id": resume_id,
            "method": "thread/resume",
            "params": {"threadId": thread_id, "excludeTurns": true},
        }),
    )
    .map_err(|_| ResumeFailure::TimedOut)?;
    read_matching_result_for_resume(socket, resume_id, thread_id)
}

fn send_json(socket: &mut WebSocket<UnixStream>, value: &Value) -> tungstenite::Result<()> {
    socket.send(Message::Text(value.to_string().into()))?;
    // A read-mostly connection queues but never automatically flushes; without this, the
    // handshake/subscribe requests above would sit buffered until something else calls
    // `read()`, and a Pong reply to a future Ping would sit buffered indefinitely too.
    socket.flush()
}

/// Reads notifications until one carries `id == expected_id` (a JSON-RPC response, matching
/// `crate::app_server::Session::request`'s own "ignore anything else" rule), returning its
/// `result`. An `error` response, a closed connection, or a non-text message where a response
/// was expected all fail the same way — the caller cannot proceed without this one field.
fn read_matching_result(
    socket: &mut WebSocket<UnixStream>,
    expected_id: &str,
) -> Result<Value, ObserverError> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    while Instant::now() < deadline {
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) if is_timeout(&error) => continue,
            Err(_) => return Err(ObserverError::Handshake),
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("id").and_then(Value::as_str) != Some(expected_id) {
            continue;
        }
        return value.get("result").cloned().ok_or(ObserverError::Handshake);
    }
    Err(ObserverError::Handshake)
}

/// Like [`read_matching_result`], but for `thread/resume` specifically (GitHub #18 follow-up):
/// an explicit JSON-RPC `error` response matching `expected_id` carries its own `message` back
/// (classified by [`Observer::attach`] into `ResumeNotYetReady` vs `ResumeRejected`) rather than
/// being collapsed into the same generic failure a bare timeout gets. A non-matching message is
/// still skipped exactly like `read_matching_result` does, *except* one: an authoritative
/// lifecycle notification for `target_thread_id` itself (`thread/closed` et al. — see
/// [`crate::events::notification_reports_this_thread_ended`]) short-circuits to
/// [`ResumeFailure::TargetEnded`] immediately, never waiting for a resume response that may now
/// never usefully arrive.
fn read_matching_result_for_resume(
    socket: &mut WebSocket<UnixStream>,
    expected_id: &str,
    target_thread_id: &str,
) -> Result<(), ResumeFailure> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    while Instant::now() < deadline {
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) if is_timeout(&error) => continue,
            Err(_) => return Err(ResumeFailure::TimedOut),
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(method) = value.get("method").and_then(Value::as_str)
            && let Some(params) = value.get("params")
            && events::notification_reports_this_thread_ended(method, params, target_thread_id)
        {
            return Err(ResumeFailure::TargetEnded);
        }
        if value.get("id").and_then(Value::as_str) != Some(expected_id) {
            continue;
        }
        if value.get("result").is_some() {
            return Ok(());
        }
        let message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        return Err(ResumeFailure::Rejected(message));
    }
    Err(ResumeFailure::TimedOut)
}

/// How long [`wait_for_replacement_and_resume`] pauses between repeated `thread/resume` attempts
/// against a just-learned replacement thread id — mirrors `relay_cli::codex_runtime::KnownThread`'s
/// own reasoning (a self-learned id is retried, never discarded), but here the retry is a plain
/// `thread/resume` call on the *same* already-open connection rather than a fresh reconnect, so a
/// short fixed pause (rather than the caller's own exponential backoff) is enough to avoid
/// hammering a transiently-busy daemon while the thread's first turn is still starting.
const REPLACEMENT_RESUME_RETRY_PAUSE: Duration = Duration::from_millis(500);

/// GitHub #18 (mid-session thread-replacement follow-up): waits — unbounded, since there is no way
/// to know how long a real user takes to start their next thread — on `socket` for the next
/// `thread/started` broadcast, exactly like [`wait_for_first_thread`] but without that function's
/// [`ATTACH_TIMEOUT`] deadline (which exists only to bound a *fresh launch's* best-effort attach,
/// never this already-attached, already-healthy connection). Returns `None` only once the
/// connection itself fails.
fn wait_for_next_thread_started(socket: &mut WebSocket<UnixStream>) -> Option<String> {
    loop {
        let message = match socket.read() {
            Ok(message) => message,
            // Same reasoning as `read_loop`'s own tolerance: the attach-time read timeout may
            // still be set (clearing it is best-effort — see `Observer::finish_attach`), and an
            // ordinary timeout here must never be mistaken for the connection having died.
            Err(error) if is_timeout(&error) => continue,
            Err(_) => return None,
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("method").and_then(Value::as_str) == Some("thread/started")
            && let Some(id) = value
                .get("params")
                .and_then(|params| params.get("thread"))
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
        {
            return Some(id.to_owned());
        }
    }
}

/// GitHub #18 (mid-session thread-replacement follow-up): after an authoritative
/// [`ObserverEvent::ThreadEnded`] for the previously-subscribed thread, waits for and resumes the
/// next thread on this exact same already-live connection — never a reconnect, which would risk
/// missing the one-shot broadcast exactly as a fresh launch's own prelisten exists to prevent (see
/// this module's own doc comment and `relay_cli::codex_runtime::KnownThread`). A resume failure
/// against the newly-learned id is never evidence the id itself is wrong (mirrors
/// `KnownThread::SelfLearned`'s reasoning exactly): retried on the same id after a short pause. If
/// the newly-learned thread *itself* turns out to already be authoritatively ended
/// (`ResumeFailure::TargetEnded` — a pathological but possible back-to-back case), this waits for
/// yet another broadcast rather than giving up, however many times that repeats. Returns `None`
/// only once the connection itself becomes unusable while no replacement has been resumed yet —
/// the caller must then reconnect and *relearn* (the old id is confirmed gone; there is nothing
/// left on this connection to resume).
fn wait_for_replacement_and_resume(socket: &mut WebSocket<UnixStream>) -> Option<String> {
    loop {
        let candidate_id = wait_for_next_thread_started(socket)?;
        loop {
            match resume(socket, &candidate_id) {
                Ok(()) => return Some(candidate_id),
                Err(ResumeFailure::TargetEnded) => break,
                Err(_) => thread::sleep(REPLACEMENT_RESUME_RETRY_PAUSE),
            }
        }
    }
}

/// The background reader: blocks on `socket.read()` until the socket is shut down (from
/// [`Observer::stop`]/[`Observer::drop`]) or the server closes it, decoding only the two
/// notification shapes `crate::events` knows about and dropping everything else — including any
/// notification shape a future Codex version might add, which must stay inert here exactly as
/// `crate::events`'s own doc comment requires. Always sends exactly one terminal
/// [`ObserverEvent::Disconnected`] as its last act before returning (GitHub #17/#18's identified
/// gap: without this, a dead observer was indistinguishable from "nothing happened yet").
///
/// GitHub #18 (mid-session thread-replacement follow-up): an authoritative
/// [`ObserverEvent::ThreadEnded`] for the currently-subscribed thread no longer ends this loop.
/// Instead, [`wait_for_replacement_and_resume`] keeps this exact connection alive and waiting on
/// the caller's behalf; on success, `thread_id` (shared with [`Observer::thread_id`]) is updated
/// in place, [`ObserverEvent::ThreadReplaced`] is sent, and dispatch resumes against the new id —
/// never tearing down and reconnecting, which is the whole point (see this module's own doc
/// comment for why a reconnect here would risk missing the one-shot broadcast). If the connection
/// instead fails while no replacement has been resumed yet, a plain `Disconnected` is sent, same
/// as any other connection failure — the caller (`relay_cli::codex_runtime::EventDrivenRuntime`)
/// distinguishes "disconnected with a still-valid known thread" from "disconnected with the old
/// thread confirmed gone and no replacement learned" by tracking whether a `ThreadReplaced` has
/// been seen since the last `ThreadEnded`, and relearns (rather than resumes the confirmed-gone id)
/// in the latter case.
fn read_loop(
    mut socket: WebSocket<UnixStream>,
    sender: mpsc::Sender<ObserverEvent>,
    thread_id: Arc<Mutex<String>>,
) {
    loop {
        let message = match socket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                let _ignored = sender.send(ObserverEvent::Disconnected(DisconnectReason::Closed));
                return;
            }
            // GitHub #18 (mid-session thread-replacement follow-up): [`Observer::finish_attach`]
            // clearing the attach-time read timeout is best-effort — if it failed (observed
            // `EINVAL` under a peer-closes-at-the-same-instant race), this socket may still be
            // using the short attach-time timeout rather than blocking indefinitely. Without this
            // branch, every ordinary idle period longer than that timeout would misreport a
            // perfectly healthy, merely-quiet connection as `Disconnected`. A genuine timeout is
            // retried exactly like the attach-time code already does elsewhere in this module.
            Err(error) if is_timeout(&error) => continue,
            Err(_) => {
                let _ignored =
                    sender.send(ObserverEvent::Disconnected(DisconnectReason::ReadError));
                return;
            }
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            continue;
        };
        let Some(params) = value.get("params") else {
            continue;
        };
        let current_thread_id = thread_id.lock().expect("thread_id mutex").clone();
        // GitHub #18 follow-up: an authoritative "this exact subscribed thread has ended" signal
        // always wins over any other decoding.
        if events::notification_reports_this_thread_ended(method, params, &current_thread_id) {
            if sender.send(ObserverEvent::ThreadEnded).is_err() {
                return; // the caller dropped the receiver; nothing left to report to.
            }
            match wait_for_replacement_and_resume(&mut socket) {
                Some(new_thread_id) => {
                    *thread_id.lock().expect("thread_id mutex") = new_thread_id.clone();
                    if sender
                        .send(ObserverEvent::ThreadReplaced(new_thread_id))
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                None => {
                    let _ignored =
                        sender.send(ObserverEvent::Disconnected(DisconnectReason::ReadError));
                    return;
                }
            }
        }
        let event = match method {
            "error" if events::error_notification_is_usage_limit_exceeded(params) => {
                Some(ObserverEvent::UsageLimitExceeded)
            }
            "account/rateLimits/updated" => Some(ObserverEvent::RateLimitsHint(
                events::max_used_percent_from_rate_limits_updated(params),
            )),
            _ => None,
        };
        if let Some(event) = event
            && sender.send(event).is_err()
        {
            return; // the caller dropped the receiver; nothing left to report to.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A minimal scripted WebSocket app-server stand-in: accepts one unix connection, performs
    /// the WS handshake via `tungstenite::accept`, answers `initialize` with a fixed `codexHome`,
    /// answers `initialized` (no response expected), broadcasts a `thread/started` immediately
    /// (so the no-thread-id-given attach path has something to wait for), answers `thread/resume`
    /// with an empty result, then emits the two scripted notifications this test cares about.
    fn install_fake_server(listener: UnixListener, codex_home: &Path, thread_id: &str) {
        let codex_home = codex_home.to_path_buf();
        let thread_id = thread_id.to_owned();
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        let _ = socket.send(Message::Text(
                            json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    Some("initialized") => {
                        let _ = socket.send(Message::Text(
                            json!({"method": "thread/started", "params": {"thread": {"id": thread_id}}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        let _ = socket.send(Message::Text(
                            json!({"id": id, "result": {}}).to_string().into(),
                        ));
                        let _ = socket.send(Message::Text(
                            json!({
                                "method": "error",
                                "params": {
                                    "error": {"codexErrorInfo": "usageLimitExceeded", "message": "quota"},
                                    "threadId": thread_id,
                                    "turnId": "u1",
                                    "willRetry": false,
                                }
                            })
                            .to_string()
                            .into(),
                        ));
                        let _ = socket.send(Message::Text(
                            json!({
                                "method": "account/rateLimits/updated",
                                "params": {"rateLimits": {"primary": {"usedPercent": 77}}}
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
    }

    #[test]
    fn attach_with_a_known_thread_id_decodes_both_signals() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server(listener, &codex_home, "thread-1");

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        assert_eq!(observer.thread_id(), "thread-1");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_usage_limit = false;
        let mut saw_hint = None;
        while Instant::now() < deadline && (!saw_usage_limit || saw_hint.is_none()) {
            for event in observer.drain_events() {
                match event {
                    ObserverEvent::UsageLimitExceeded => saw_usage_limit = true,
                    ObserverEvent::RateLimitsHint(hint) => saw_hint = Some(hint),
                    ObserverEvent::Disconnected(_)
                    | ObserverEvent::ThreadEnded
                    | ObserverEvent::ThreadReplaced(_) => {}
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_usage_limit, "must decode the usageLimitExceeded error");
        assert_eq!(saw_hint, Some(Some(77)), "must decode the rate-limit hint");
        observer.stop();
    }

    #[test]
    fn attach_with_no_thread_id_learns_it_from_the_broadcast() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t2.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server(listener, &codex_home, "learned-thread");

        let observer = Observer::attach(&socket_path, &codex_home, None).expect("attach");
        assert_eq!(observer.thread_id(), "learned-thread");
        observer.stop();
    }

    /// A fake server that always answers `thread/resume` with a JSON-RPC error, mirroring the
    /// real server's live-confirmed (agent-relay#17) `"no rollout found for thread id …"` for a
    /// thread whose first turn has not started yet.
    fn install_fake_server_that_never_resumes(listener: UnixListener, codex_home: &Path) {
        let codex_home = codex_home.to_path_buf();
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        let _ = socket.send(Message::Text(
                            json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        let _ = socket.send(Message::Text(
                            json!({"id": id, "error": {"code": -32600, "message": "no rollout found for thread id t1"}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    _ => {}
                }
            }
        });
    }

    #[test]
    fn a_resume_failure_carries_the_thread_id_for_a_retrying_caller() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t5.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server_that_never_resumes(listener, &codex_home);

        let result = Observer::attach(&socket_path, &codex_home, Some("t1"));
        assert_eq!(
            result.err(),
            Some(ObserverError::ResumeNotYetReady("t1".to_owned()))
        );
    }

    #[test]
    fn a_resume_failure_after_learning_the_thread_id_from_the_broadcast_still_carries_it() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t5b.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        // Reuses the resume-always-fails server, but it never sends `thread/started` either, so
        // this specifically exercises the `Some(id)`-given path failing, not the `None`-learning
        // path (covered above) — both must carry the id forward identically.
        install_fake_server_that_never_resumes(listener, &codex_home);

        let result = Observer::attach(&socket_path, &codex_home, Some("known-id"));
        assert_eq!(
            result.err(),
            Some(ObserverError::ResumeNotYetReady("known-id".to_owned()))
        );
    }

    /// A fake server that always answers `thread/resume` with an explicit JSON-RPC error whose
    /// message does **not** match the known "no rollout found" signature — GitHub #18 follow-up:
    /// a real `~/repos` natural exhaustion showed the previous, undifferentiated handling of this
    /// case (collapsed into the exact same `ResumeNotYetReady` the benign case gets) leaving the
    /// attach worker retrying a thread id forever with no way to tell the two apart.
    fn install_fake_server_that_rejects_resume(listener: UnixListener, codex_home: &Path) {
        let codex_home = codex_home.to_path_buf();
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        let _ = socket.send(Message::Text(
                            json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        let _ = socket.send(Message::Text(
                            json!({"id": id, "error": {"code": -32602, "message": "thread not found"}})
                                .to_string()
                                .into(),
                        ));
                        let _ = socket.flush();
                    }
                    _ => {}
                }
            }
        });
    }

    #[test]
    fn a_resume_rejection_with_a_different_message_is_classified_distinctly_from_not_yet_ready() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t5c.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server_that_rejects_resume(listener, &codex_home);

        let result = Observer::attach(&socket_path, &codex_home, Some("t1"));
        assert_eq!(
            result.err(),
            Some(ObserverError::ResumeRejected("t1".to_owned())),
            "an explicit rejection unrelated to \"no rollout found\" must never be mistaken for \
             the confirmed-benign not-yet-ready case"
        );
    }

    /// A fake server that answers `initialize` but then never answers `thread/resume` at all —
    /// the one case `read_matching_result_for_resume` must classify as a bare timeout, distinct
    /// from any explicit response.
    fn install_fake_server_silent_on_resume(listener: UnixListener, codex_home: &Path) {
        let codex_home = codex_home.to_path_buf();
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if value.get("method").and_then(Value::as_str) == Some("initialize") {
                    let id = value["id"].clone();
                    let _ = socket.send(Message::Text(
                        json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}})
                            .to_string()
                            .into(),
                    ));
                    let _ = socket.flush();
                }
                // `thread/resume` (and anything else) is deliberately never answered.
            }
        });
    }

    #[test]
    fn a_resume_timeout_is_classified_distinctly_from_an_explicit_response() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t5d.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server_silent_on_resume(listener, &codex_home);

        let result = Observer::attach(&socket_path, &codex_home, Some("t1"));
        assert_eq!(
            result.err(),
            Some(ObserverError::ResumeTimedOut("t1".to_owned())),
            "a bare timeout waiting for any response must never be conflated with an explicit \
             rejection or the confirmed-benign not-yet-ready case"
        );
    }

    // --- GitHub #17/#18: observer disconnect must be a deterministic, exactly-once signal ---

    #[test]
    fn an_abrupt_local_shutdown_is_surfaced_as_a_read_error_disconnect() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t6.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server(listener, &codex_home, "thread-1");

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        // Shutting down our own end at the OS level (without going through `stop()`, which would
        // join first) — no WebSocket close handshake ever completes, so this is the same shape as
        // a genuine dropped connection, not a graceful close.
        let _ = observer_shutdown_handle_for_test(&observer).shutdown(std::net::Shutdown::Both);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reason = None;
        while Instant::now() < deadline && reason.is_none() {
            for event in observer.drain_events() {
                if let ObserverEvent::Disconnected(r) = event {
                    reason = Some(r);
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            reason,
            Some(DisconnectReason::ReadError),
            "an abrupt local shutdown must surface as a read error, not a graceful close"
        );
    }

    #[test]
    fn no_events_yet_is_never_mistaken_for_disconnect() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t7.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server(listener, &codex_home, "thread-1");

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        // Drain immediately, before the fake server's scripted notifications necessarily arrive —
        // an empty drain (or one with only the two real signals) must never itself contain
        // `Disconnected` while the connection is still healthy.
        for event in observer.drain_events() {
            assert!(
                !matches!(event, ObserverEvent::Disconnected(_)),
                "a live connection with merely no NEW events yet must not report Disconnected"
            );
        }
        observer.stop();
    }

    #[test]
    fn duplicate_disconnect_signals_are_coalesced_to_exactly_one() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t8.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server(listener, &codex_home, "thread-1");

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        let _ = observer_shutdown_handle_for_test(&observer).shutdown(std::net::Shutdown::Both);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut disconnect_count = 0usize;
        while Instant::now() < deadline {
            for event in observer.drain_events() {
                if matches!(event, ObserverEvent::Disconnected(_)) {
                    disconnect_count += 1;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            disconnect_count, 1,
            "repeated drains after disconnect must report it exactly once, not on every tick"
        );
    }

    // --- GitHub #18 (mid-session thread-replacement follow-up): the same connection survives a
    // thread ending and resumes its replacement without ever reconnecting ---

    fn send_notification(socket: &mut WebSocket<UnixStream>, value: Value) {
        let _ = socket.send(Message::Text(value.to_string().into()));
        let _ = socket.flush();
    }

    fn respond_to_resume(socket: &mut WebSocket<UnixStream>, id: &Value) {
        send_notification(socket, json!({"id": id, "result": {}}));
    }

    /// A scripted fake server driving the exact T → T2 transition this follow-up targets: attaches
    /// to `first_thread_id`, then (once a signal arrives on `advance_rx`, so the test controls
    /// timing precisely) emits an authoritative `thread/closed` for it, optionally sends one or
    /// more *unrelated* historical-thread notifications first (never mistaken for the replacement),
    /// then broadcasts `thread/started` for `second_thread_id` and answers its `thread/resume`.
    /// `resume_rejections` lets a test make the replacement's first N `thread/resume` calls fail
    /// with the confirmed-benign "no rollout found" signature before succeeding, exercising the
    /// same-connection retry path.
    fn install_fake_server_with_thread_replacement(
        listener: UnixListener,
        codex_home: &Path,
        first_thread_id: &str,
        second_thread_id: &str,
        advance_rx: Receiver<()>,
        unrelated_historical_thread_id: Option<&str>,
        mut resume_rejections: u32,
    ) {
        let codex_home = codex_home.to_path_buf();
        let first_thread_id = first_thread_id.to_owned();
        let second_thread_id = second_thread_id.to_owned();
        let unrelated_historical_thread_id = unrelated_historical_thread_id.map(str::to_owned);
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        send_notification(
                            &mut socket,
                            json!({"id": id, "result": {"codexHome": codex_home.to_string_lossy()}}),
                        );
                    }
                    Some("initialized") => {
                        send_notification(
                            &mut socket,
                            json!({"method": "thread/started", "params": {"thread": {"id": first_thread_id}}}),
                        );
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        let requested = value
                            .get("params")
                            .and_then(|p| p.get("threadId"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if requested == second_thread_id && resume_rejections > 0 {
                            resume_rejections -= 1;
                            send_notification(
                                &mut socket,
                                json!({"id": id, "error": {"code": -32600, "message": "no rollout found for thread id t2"}}),
                            );
                            continue;
                        }
                        respond_to_resume(&mut socket, &id);
                        if requested == first_thread_id {
                            // Wait for the test's own go-ahead before ending this thread, so the
                            // test can deterministically control exactly when the transition
                            // starts relative to the observer's own state.
                            let _ = advance_rx.recv();
                            if let Some(ref unrelated) = unrelated_historical_thread_id {
                                // An unrelated, already-existing thread's own lifecycle
                                // notification must never be mistaken for the live replacement —
                                // sent with a *different* method/id than anything this observer
                                // is waiting on.
                                send_notification(
                                    &mut socket,
                                    json!({
                                        "method": "thread/status/changed",
                                        "params": {"threadId": unrelated, "status": {"type": "idle"}}
                                    }),
                                );
                            }
                            send_notification(
                                &mut socket,
                                json!({"method": "thread/closed", "params": {"threadId": first_thread_id}}),
                            );
                            send_notification(
                                &mut socket,
                                json!({"method": "thread/started", "params": {"thread": {"id": second_thread_id}}}),
                            );
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    #[test]
    fn an_authoritative_thread_ended_resumes_its_replacement_on_the_same_connection() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("tr1.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let (advance_tx, advance_rx) = mpsc::channel();
        install_fake_server_with_thread_replacement(
            listener,
            &codex_home,
            "thread-1",
            "thread-2",
            advance_rx,
            None,
            0,
        );

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        assert_eq!(observer.thread_id(), "thread-1");
        advance_tx
            .send(())
            .expect("tell the fake server to end thread-1");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_thread_ended = false;
        let mut replaced_with = None;
        while Instant::now() < deadline && replaced_with.is_none() {
            for event in observer.drain_events() {
                match event {
                    ObserverEvent::ThreadEnded => saw_thread_ended = true,
                    ObserverEvent::ThreadReplaced(id) => replaced_with = Some(id),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            saw_thread_ended,
            "must report the authoritative ThreadEnded first"
        );
        assert_eq!(
            replaced_with,
            Some("thread-2".to_owned()),
            "must resume the replacement thread without ever reconnecting"
        );
        assert_eq!(
            observer.thread_id(),
            "thread-2",
            "thread_id() must reflect the replacement from this point on"
        );
        observer.stop();
    }

    #[test]
    fn a_replacement_resume_rejection_is_retried_on_the_same_id_and_connection() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("tr2.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let (advance_tx, advance_rx) = mpsc::channel();
        install_fake_server_with_thread_replacement(
            listener,
            &codex_home,
            "thread-1",
            "thread-2",
            advance_rx,
            None,
            2, // first two thread/resume attempts against thread-2 are rejected
        );

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        advance_tx
            .send(())
            .expect("tell the fake server to end thread-1");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut replaced_with = None;
        while Instant::now() < deadline && replaced_with.is_none() {
            for event in observer.drain_events() {
                if let ObserverEvent::ThreadReplaced(id) = event {
                    replaced_with = Some(id);
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            replaced_with,
            Some("thread-2".to_owned()),
            "a benign resume rejection against the newly-learned id must be retried on the same \
             connection, never discarded and never a reconnect"
        );
        observer.stop();
    }

    #[test]
    fn an_unrelated_historical_threads_notification_is_never_mistaken_for_the_replacement() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("tr3.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let (advance_tx, advance_rx) = mpsc::channel();
        install_fake_server_with_thread_replacement(
            listener,
            &codex_home,
            "thread-1",
            "thread-2",
            advance_rx,
            Some("some-old-historical-thread"),
            0,
        );

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        advance_tx
            .send(())
            .expect("tell the fake server to end thread-1");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut replaced_with = None;
        while Instant::now() < deadline && replaced_with.is_none() {
            for event in observer.drain_events() {
                if let ObserverEvent::ThreadReplaced(id) = event {
                    replaced_with = Some(id);
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            replaced_with,
            Some("thread-2".to_owned()),
            "an unrelated historical thread's own notification must never be mistaken for, or \
             block, the live replacement"
        );
        observer.stop();
    }

    #[test]
    fn a_connection_that_dies_while_awaiting_a_replacement_still_reports_disconnected() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("tr4.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        // A fake server that ends thread-1 and then, instead of ever broadcasting a replacement,
        // simply closes the connection — simulating a daemon replacement/crash that happens while
        // the observer is waiting for the next thread/started broadcast.
        let codex_home_clone = codex_home.clone();
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        send_notification(
                            &mut socket,
                            json!({"id": id, "result": {"codexHome": codex_home_clone.to_string_lossy()}}),
                        );
                    }
                    Some("initialized") => {
                        send_notification(
                            &mut socket,
                            json!({"method": "thread/started", "params": {"thread": {"id": "thread-1"}}}),
                        );
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        respond_to_resume(&mut socket, &id);
                        send_notification(
                            &mut socket,
                            json!({"method": "thread/closed", "params": {"threadId": "thread-1"}}),
                        );
                        // Deliberately never broadcasts a replacement — the connection just ends,
                        // immediately, deliberately racing `Observer::finish_attach`'s own
                        // post-resume bookkeeping (see that function's own doc comment for why
                        // this exact race is real, not just a test artifact).
                        return;
                    }
                    _ => {}
                }
            }
        });

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_thread_ended = false;
        let mut disconnected = false;
        while Instant::now() < deadline && !disconnected {
            for event in observer.drain_events() {
                match event {
                    ObserverEvent::ThreadEnded => saw_thread_ended = true,
                    ObserverEvent::Disconnected(_) => disconnected = true,
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_thread_ended);
        assert!(
            disconnected,
            "a connection that dies while awaiting a replacement must still surface Disconnected, \
             never hang"
        );
    }

    #[test]
    fn repeated_thread_replacements_all_succeed_on_the_same_connection() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("tr5.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let codex_home_clone = codex_home.clone();
        thread::spawn(move || {
            let Ok((stream, _addr)) = listener.accept() else {
                return;
            };
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            let chain = ["thread-1", "thread-2", "thread-3"];
            loop {
                let Ok(Message::Text(text)) = socket.read() else {
                    return;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let id = value["id"].clone();
                        send_notification(
                            &mut socket,
                            json!({"id": id, "result": {"codexHome": codex_home_clone.to_string_lossy()}}),
                        );
                    }
                    Some("initialized") => {
                        send_notification(
                            &mut socket,
                            json!({"method": "thread/started", "params": {"thread": {"id": chain[0]}}}),
                        );
                    }
                    Some("thread/resume") => {
                        let id = value["id"].clone();
                        let requested = value
                            .get("params")
                            .and_then(|p| p.get("threadId"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        respond_to_resume(&mut socket, &id);
                        if let Some(position) = chain.iter().position(|id| *id == requested)
                            && position + 1 < chain.len()
                        {
                            send_notification(
                                &mut socket,
                                json!({"method": "thread/closed", "params": {"threadId": requested}}),
                            );
                            send_notification(
                                &mut socket,
                                json!({"method": "thread/started", "params": {"thread": {"id": chain[position + 1]}}}),
                            );
                        }
                    }
                    _ => {}
                }
            }
        });

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut replacements = Vec::new();
        while Instant::now() < deadline && replacements.len() < 2 {
            for event in observer.drain_events() {
                if let ObserverEvent::ThreadReplaced(id) = event {
                    replacements.push(id);
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            replacements,
            vec!["thread-2".to_owned(), "thread-3".to_owned()],
            "repeated T -> T2 -> T3 transitions must each succeed in order, all on one connection"
        );
        observer.stop();
    }

    /// Test-only accessor: a second handle to the same underlying socket as `observer`'s own
    /// `shutdown` field, so a test can end the connection without going through `stop()` (which
    /// would join the reader thread before the test gets to call `drain_events` itself).
    fn observer_shutdown_handle_for_test(observer: &Observer) -> UnixStream {
        observer
            .shutdown
            .try_clone()
            .expect("clone shutdown handle")
    }

    #[test]
    fn a_codex_home_mismatch_fails_closed() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t3.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let real_home = scratch.path().join("real_home");
        let wrong_home = scratch.path().join("wrong_home");
        std::fs::create_dir_all(&real_home).expect("real home");
        std::fs::create_dir_all(&wrong_home).expect("wrong home");
        install_fake_server(listener, &real_home, "thread-1");

        let result = Observer::attach(&socket_path, &wrong_home, Some("thread-1"));
        assert_eq!(result.err(), Some(ObserverError::HomeMismatch));
    }

    #[test]
    fn stop_cleanly_joins_the_reader_thread_even_with_no_traffic() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("t4.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let codex_home = scratch.path().join("codex_home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        install_fake_server(listener, &codex_home, "thread-1");

        let observer =
            Observer::attach(&socket_path, &codex_home, Some("thread-1")).expect("attach");
        // No sleep: stop() must not hang even if the reader thread is mid-blocking-read.
        observer.stop();
    }

    /// Live smoke test against the REAL `codex app-server` binary (not the scripted fake-server
    /// stand-in every other test in this module uses) — the one risk fake-server tests cannot
    /// rule out: that this hand-written `tungstenite` client and the real server's own WebSocket
    /// implementation actually interoperate. Exercises exactly the production shape
    /// `EventDrivenRuntime::start` uses (a *known* thread id, i.e. a resume — see this crate's own
    /// `Observer::attach` doc comment for why a fresh/unknown thread id is deliberately not
    /// exercised here): a driver connection creates a thread and completes one real seed turn
    /// (so the thread has real, persisted rollout content, exactly like a thread being resumed
    /// after a prior session), *then* the real `Observer` attaches to that already-populated
    /// thread by id, then the driver runs a second real turn and the test confirms the `Observer`
    /// decodes the live `account/rateLimits/updated` notification it produces. Spends real
    /// (trivial) model usage on the given profile — never run in the default `cargo test` gate.
    ///
    /// `RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<name>/codex cargo test --release -p
    /// relay-provider-codex --lib observer::tests::live_smoke_test_against_the_real_app_server --
    /// --ignored --nocapture`
    #[test]
    #[ignore = "spends real (trivial) Codex usage against a real, already-authenticated profile — see the doc comment"]
    fn live_smoke_test_against_the_real_app_server() {
        use crate::runtime::{AppServerHandle, allocate_endpoint};

        let Ok(codex_home) = std::env::var("RELAY_BENCH_CODEX_HOME") else {
            eprintln!("skipped: set RELAY_BENCH_CODEX_HOME to a real, authenticated CODEX_HOME");
            return;
        };
        let executable =
            std::env::var("RELAY_BENCH_CODEX_EXE").unwrap_or_else(|_| "codex".to_owned());
        let codex_home = std::path::PathBuf::from(codex_home);
        let executable = std::path::PathBuf::from(executable);
        let endpoint = allocate_endpoint().expect("endpoint");

        let handle = AppServerHandle::spawn(&executable, &codex_home, &endpoint)
            .expect("spawn real app-server");

        let thread_id = seed_one_real_thread(&endpoint);
        println!("seeded a real, already-populated thread: {thread_id}");

        let observer = Observer::attach(&endpoint, &codex_home, Some(&thread_id))
            .expect("attach observer to the already-populated thread");
        assert_eq!(observer.thread_id(), thread_id);

        drive_second_real_turn(&endpoint, &thread_id);

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_hint = false;
        while Instant::now() < deadline && !saw_hint {
            for event in observer.drain_events() {
                if let ObserverEvent::RateLimitsHint(hint) = event {
                    println!("observer decoded a real account/rateLimits/updated hint: {hint:?}");
                    saw_hint = true;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(
            saw_hint,
            "the real app-server must have emitted account/rateLimits/updated for the real turn"
        );
        observer.stop();
        handle.terminate();
    }

    /// GitHub #18 live validation (A/B): a real observer attached to a real app-server, killed
    /// abruptly mid-turn (simulating a genuine crash/disconnect, not a clean `stop()`), then
    /// reconnected via a second, independent `Observer::attach` call against the *same* real
    /// runtime and thread — proving the reconnect mechanism itself (not just the underlying
    /// protocol behavior agent-relay#17's Python research already proved) against the real
    /// binary. Confirms the reconnected observer receives the remainder of the turn it missed the
    /// start of, including the completion signal that would carry a real `usageLimitExceeded` had
    /// this been a genuine exhaustion. Spends real (trivial) model usage — never run by default.
    ///
    /// `RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<name>/codex cargo test --release -p
    /// relay-provider-codex --lib observer::tests::live_reconnect_after_disconnect_against_the_real_app_server
    /// -- --ignored --nocapture`
    #[test]
    #[ignore = "spends real (trivial) Codex usage against a real, already-authenticated profile — see the doc comment"]
    fn live_reconnect_after_disconnect_against_the_real_app_server() {
        use crate::runtime::{AppServerHandle, allocate_endpoint};

        let Ok(codex_home) = std::env::var("RELAY_BENCH_CODEX_HOME") else {
            eprintln!("skipped: set RELAY_BENCH_CODEX_HOME to a real, authenticated CODEX_HOME");
            return;
        };
        let executable =
            std::env::var("RELAY_BENCH_CODEX_EXE").unwrap_or_else(|_| "codex".to_owned());
        let codex_home = std::path::PathBuf::from(codex_home);
        let executable = std::path::PathBuf::from(executable);
        let endpoint = allocate_endpoint().expect("endpoint");

        let handle = AppServerHandle::spawn(&executable, &codex_home, &endpoint)
            .expect("spawn real app-server");
        let thread_id = seed_one_real_thread(&endpoint);
        println!("seeded a real, already-populated thread: {thread_id}");

        // First attach, then an abrupt disconnect (not `stop()`) — simulating a genuine crash,
        // not a deliberate shutdown.
        let first = Observer::attach(&endpoint, &codex_home, Some(&thread_id)).expect("attach 1");
        let _ = observer_shutdown_handle_for_test(&first).shutdown(std::net::Shutdown::Both);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut disconnected = false;
        while Instant::now() < deadline && !disconnected {
            for event in first.drain_events() {
                if matches!(event, ObserverEvent::Disconnected(_)) {
                    disconnected = true;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            disconnected,
            "the real app-server side of a killed connection must surface Disconnected"
        );

        // Start a second real turn on the same thread *before* reconnecting, deliberately racing
        // the reconnect against it (agent-relay#17 live-verified this exact ordering).
        let endpoint_for_driver = endpoint.clone();
        let thread_for_driver = thread_id.clone();
        let driver = thread::spawn(move || {
            drive_second_real_turn(&endpoint_for_driver, &thread_for_driver);
        });
        thread::sleep(Duration::from_millis(150));

        let reconnected = Observer::attach(&endpoint, &codex_home, Some(&thread_id))
            .expect("reconnect against the same real runtime and thread");
        assert_eq!(reconnected.thread_id(), thread_id);

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_completion = false;
        while Instant::now() < deadline && !saw_completion {
            for event in reconnected.drain_events() {
                if let ObserverEvent::RateLimitsHint(hint) = event {
                    println!("reconnected observer decoded a real rateLimits hint: {hint:?}");
                    saw_completion = true;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(
            saw_completion,
            "a reconnected observer must still receive the remainder of an in-flight turn"
        );
        driver.join().expect("driver thread");
        reconnected.stop();
        handle.terminate();
    }

    /// GitHub #18 live validation (C): activates the fresh-launch path — no known thread id — for
    /// real, against a real, freshly-started app-server (so nothing else has ever created a
    /// thread on it), and confirms `Observer::attach(None)` learns the thread from the real
    /// `thread/started` broadcast and then successfully resumes it once a real driver starts (not
    /// completes) the very first turn — the exact sequencing agent-relay#17's Python research
    /// proved and this crate's `attach_worker_loop` retries around. Spends real (trivial) model
    /// usage — never run by default.
    ///
    /// `RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<name>/codex cargo test --release -p
    /// relay-provider-codex --lib observer::tests::live_fresh_thread_activation_against_the_real_app_server
    /// -- --ignored --nocapture`
    #[test]
    #[ignore = "spends real (trivial) Codex usage against a real, already-authenticated profile — see the doc comment"]
    fn live_fresh_thread_activation_against_the_real_app_server() {
        use crate::runtime::{AppServerHandle, allocate_endpoint};

        let Ok(codex_home) = std::env::var("RELAY_BENCH_CODEX_HOME") else {
            eprintln!("skipped: set RELAY_BENCH_CODEX_HOME to a real, authenticated CODEX_HOME");
            return;
        };
        let executable =
            std::env::var("RELAY_BENCH_CODEX_EXE").unwrap_or_else(|_| "codex".to_owned());
        let codex_home = std::path::PathBuf::from(codex_home);
        let executable = std::path::PathBuf::from(executable);
        let endpoint = allocate_endpoint().expect("endpoint");

        let handle = AppServerHandle::spawn(&executable, &codex_home, &endpoint)
            .expect("spawn real app-server");

        // A real driver connection that creates a brand-new thread (no prior turns anywhere on
        // this fresh app-server) and, after a short lead-in so `attach(None)` is already
        // connected and waiting for the broadcast, starts its first turn.
        let endpoint_for_driver = endpoint.clone();
        let driver = thread::spawn(move || {
            let stream = UnixStream::connect(&endpoint_for_driver).expect("connect driver");
            stream
                .set_read_timeout(Some(ATTACH_POLL_INTERVAL))
                .expect("set read timeout");
            let mut socket = tungstenite::client("ws://localhost/", stream)
                .expect("driver handshake")
                .0;
            driver_handshake(&mut socket, "driver-init-fresh");
            thread::sleep(Duration::from_millis(300));
            let thread_id = driver_start_thread(&mut socket, "driver-thread-fresh");
            driver_run_turn(&mut socket, &thread_id, "driver-turn-fresh", "fresh");
            thread_id
        });

        // `Observer::attach` itself makes exactly one attempt (the retry loop is
        // `relay-cli::codex_runtime::attach_worker_loop`, one crate up, already unit-tested
        // against a fake server) — this test retries the same way, against the real binary, to
        // prove the underlying primitive (`attach`, `ResumeNotYetReady` carrying the learned id)
        // behaves correctly when retried for real, not just against a fake.
        let mut known_thread_id: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        let observer = loop {
            match Observer::attach(&endpoint, &codex_home, known_thread_id.as_deref()) {
                Ok(observer) => break observer,
                Err(ObserverError::ResumeNotYetReady(id)) => known_thread_id = Some(id),
                Err(error) if Instant::now() < deadline => {
                    eprintln!("fresh-thread attach attempt failed, retrying: {error:?}");
                }
                Err(error) => panic!("attach with no known thread id (fresh launch): {error:?}"),
            }
            thread::sleep(Duration::from_millis(300));
        };
        let driven_thread_id = driver.join().expect("driver thread");
        assert_eq!(
            observer.thread_id(),
            driven_thread_id,
            "the observer must have learned and attached to the exact thread the driver created"
        );

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_completion = false;
        while Instant::now() < deadline && !saw_completion {
            for event in observer.drain_events() {
                if let ObserverEvent::RateLimitsHint(hint) = event {
                    println!("fresh-thread observer decoded a real rateLimits hint: {hint:?}");
                    saw_completion = true;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(
            saw_completion,
            "the fresh-thread observer must receive the first turn's own completion signal"
        );
        observer.stop();
        handle.terminate();
    }

    /// GitHub #18 research question: does a REAL, currently-EXHAUSTED Codex account's real
    /// app-server turn actually emit the structured `usageLimitExceeded` notification this crate's
    /// `Observer` decodes as [`ObserverEvent::UsageLimitExceeded`]? Every other test in this module
    /// only proves that decoding against a scripted fake server (`install_fake_server`) — this is
    /// the one thing only a genuinely exhausted real account can answer, and #16/#17/#18's whole
    /// event-driven design assumes the answer is yes without ever having live-verified it. This
    /// test observes whatever actually happens and reports it, including a negative result, which
    /// is itself important research data — it never substitutes a rate-limit percentage,
    /// `ordinaryUsageAllowed=false` from a separate read, terminal text, or a synthetic
    /// notification for the real thing.
    ///
    /// **This spends/attempts real Codex usage** against whatever account `RELAY_BENCH_CODEX_HOME`
    /// points at — it is intended only for a deliberate, explicit run against an account already
    /// known to be exhausted (never run in the default `cargo test` gate, and never run
    /// automatically by CI). **It validates provider-protocol emission only** — the real backend
    /// really does emit the real structured event this crate's `Observer` expects — and proves
    /// nothing about `CodexPollScheduler`, trigger provenance, the durable auto-handoff trace, or
    /// an actual Codex → Claude handoff, all of which live one layer up in `relay-cli` and still
    /// require their own real natural-exhaustion validation (see GitHub #18's own standing
    /// acceptance criteria). Live-proved 2026-09-27 against `codex-cli 0.155.0`: `turn/start` was
    /// accepted and `ObserverEvent::UsageLimitExceeded` arrived ~4.35s later via the async
    /// notification stream (see GitHub #18 comment 5860754508 for the full result).
    ///
    /// Mirrors [`live_fresh_thread_activation_against_the_real_app_server`]'s exact fresh-thread
    /// attach/retry sequencing (already live-verified against a healthy account) — the only
    /// difference here is the account's own quota state and that the turn's own JSON-RPC response
    /// is captured and reported rather than asserted to succeed, in case the account is rejected at
    /// that step instead of via an asynchronous notification. Sends exactly one trivial turn;
    /// never retried, never repeated. Never prints raw provider payloads, account ids, or
    /// credentials — only decoded event variants, a JSON-RPC error `code` if one occurs, and local
    /// timing.
    ///
    /// `RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<name>/codex cargo test --release -p
    /// relay-provider-codex --lib
    /// observer::tests::live_exhausted_turn_emits_usage_limit_event -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a real, currently-EXHAUSTED, already-authenticated Codex profile — see the doc comment"]
    fn live_exhausted_turn_emits_usage_limit_event() {
        use crate::runtime::{AppServerHandle, allocate_endpoint};

        let Ok(codex_home) = std::env::var("RELAY_BENCH_CODEX_HOME") else {
            eprintln!(
                "skipped: set RELAY_BENCH_CODEX_HOME to a real, authenticated, exhausted CODEX_HOME"
            );
            return;
        };
        let executable =
            std::env::var("RELAY_BENCH_CODEX_EXE").unwrap_or_else(|_| "codex".to_owned());
        let codex_home = std::path::PathBuf::from(codex_home);
        let executable = std::path::PathBuf::from(executable);
        let endpoint = allocate_endpoint().expect("endpoint");

        let handle = AppServerHandle::spawn(&executable, &codex_home, &endpoint)
            .expect("spawn real app-server");

        // Same fresh-thread driver shape as `live_fresh_thread_activation_against_the_real_app_server`:
        // a brand-new thread on an app-server that has never had any other thread, so the
        // subsequent `Observer::attach(None)` path is exercised identically to that already-proven
        // test — the only variable this test changes is the account's own quota state.
        let endpoint_for_driver = endpoint.clone();
        let driver = thread::spawn(move || {
            let stream = UnixStream::connect(&endpoint_for_driver).expect("connect driver");
            stream
                .set_read_timeout(Some(ATTACH_POLL_INTERVAL))
                .expect("set read timeout");
            let mut socket = tungstenite::client("ws://localhost/", stream)
                .expect("driver handshake")
                .0;
            driver_handshake(&mut socket, "driver-init-exhausted");
            thread::sleep(Duration::from_millis(300));
            let thread_id = driver_start_thread(&mut socket, "driver-thread-exhausted");
            let turn_response = send_one_turn_capturing_response(
                &mut socket,
                &thread_id,
                "driver-turn-exhausted",
                "Reply with exactly: OK",
            );
            (thread_id, turn_response)
        });

        let mut known_thread_id: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        let observer = loop {
            match Observer::attach(&endpoint, &codex_home, known_thread_id.as_deref()) {
                Ok(observer) => break observer,
                Err(ObserverError::ResumeNotYetReady(id)) => known_thread_id = Some(id),
                Err(error) if Instant::now() < deadline => {
                    eprintln!("exhausted-turn attach attempt failed, retrying: {error:?}");
                }
                Err(error) => {
                    panic!("attach with no known thread id (exhausted-turn test): {error:?}")
                }
            }
            thread::sleep(Duration::from_millis(300));
        };
        let turn_attempted_at = Instant::now();
        let (driven_thread_id, turn_response) = driver.join().expect("driver thread");
        assert_eq!(
            observer.thread_id(),
            driven_thread_id,
            "the observer must have learned and attached to the exact thread the driver created"
        );

        if let Some(error) = turn_response.get("error") {
            println!(
                "LIVE FINDING: turn/start itself returned a JSON-RPC error (code={:?}) instead of \
                 an accepted turn — the account may be rejected before any structured \
                 usageLimitExceeded notification stream begins.",
                error.get("code")
            );
        } else {
            println!("turn/start was accepted; waiting for the async notification stream.");
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_usage_limit_event = false;
        let mut saw_rate_limits_hint = false;
        let mut usage_limit_event_after: Option<Duration> = None;
        while Instant::now() < deadline && !saw_usage_limit_event {
            for event in observer.drain_events() {
                match event {
                    ObserverEvent::UsageLimitExceeded => {
                        saw_usage_limit_event = true;
                        usage_limit_event_after = Some(turn_attempted_at.elapsed());
                    }
                    ObserverEvent::RateLimitsHint(_) => saw_rate_limits_hint = true,
                    ObserverEvent::Disconnected(reason) => {
                        println!("observer disconnected while waiting: {reason:?}");
                    }
                    ObserverEvent::ThreadEnded => {
                        println!("observer's thread authoritatively ended while waiting");
                    }
                    ObserverEvent::ThreadReplaced(new_id) => {
                        println!("observer resumed a replacement thread while waiting: {new_id}");
                    }
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        println!("saw_rate_limits_hint={saw_rate_limits_hint}");
        if saw_usage_limit_event {
            println!(
                "LIVE PROOF: ObserverEvent::UsageLimitExceeded received ({:?} after the turn attempt)",
                usage_limit_event_after.unwrap_or_default()
            );
        } else {
            println!(
                "LIVE FINDING: no ObserverEvent::UsageLimitExceeded arrived within 30s of a real \
                 attempted turn against an account expected to be exhausted."
            );
        }

        observer.stop();
        handle.terminate();

        assert!(
            saw_usage_limit_event,
            "expected the real exhausted account's turn to produce \
             ObserverEvent::UsageLimitExceeded; see stdout (rerun with --nocapture) for what was \
             actually observed"
        );
    }

    /// Connects a minimal raw driver client (deliberately not `Observer` itself), creates a fresh
    /// thread, runs one trivial turn to full completion, and disconnects — leaving a real,
    /// persisted, already-populated thread behind, exactly the state a thread being *resumed* is
    /// always in (never the fresh, zero-turns state `Observer::attach`'s doc comment flags as
    /// unsupported). Returns the thread id.
    #[cfg(test)]
    fn seed_one_real_thread(endpoint: &Path) -> String {
        let stream = UnixStream::connect(endpoint).expect("connect driver");
        stream
            .set_read_timeout(Some(ATTACH_POLL_INTERVAL))
            .expect("set read timeout");
        let mut socket = tungstenite::client("ws://localhost/", stream)
            .expect("driver handshake")
            .0;
        driver_handshake(&mut socket, "driver-init-seed");
        let thread_id = driver_start_thread(&mut socket, "driver-thread-seed");
        driver_run_turn(&mut socket, &thread_id, "driver-turn-seed", "seed");
        wait_for_notification(&mut socket, "turn/completed");
        thread_id
    }

    /// A second, independent raw connection (mirroring the real target architecture's remote
    /// TUI, which is a separate connection from Relay's own observer) that runs one more real
    /// turn on an already-existing thread — the observer, already resumed onto that same thread,
    /// must see this turn's `account/rateLimits/updated` without ever driving it itself.
    #[cfg(test)]
    fn drive_second_real_turn(endpoint: &Path, thread_id: &str) {
        let stream = UnixStream::connect(endpoint).expect("connect second driver");
        let mut socket = tungstenite::client("ws://localhost/", stream)
            .expect("second driver handshake")
            .0;
        driver_handshake(&mut socket, "driver-init-2");
        driver_run_turn(&mut socket, thread_id, "driver-turn-2", "again");
    }

    #[cfg(test)]
    fn driver_handshake(socket: &mut WebSocket<UnixStream>, id: &str) {
        send_json(
            socket,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "initialize",
                "params": {"clientInfo": {"name": "relay-observer-test-driver", "title": "driver", "version": "0"}, "capabilities": {"experimentalApi": false}},
            }),
        )
        .expect("send initialize");
        read_matching_result(socket, id).expect("initialize result");
        send_json(socket, &json!({"jsonrpc": "2.0", "method": "initialized"}))
            .expect("initialized");
    }

    #[cfg(test)]
    fn driver_start_thread(socket: &mut WebSocket<UnixStream>, id: &str) -> String {
        send_json(
            socket,
            &json!({"jsonrpc": "2.0", "id": id, "method": "thread/start", "params": {"cwd": "/tmp"}}),
        )
        .expect("send thread/start");
        let result = read_matching_result(socket, id).expect("thread/start result");
        result["thread"]["id"]
            .as_str()
            .expect("thread id")
            .to_owned()
    }

    #[cfg(test)]
    fn driver_run_turn(socket: &mut WebSocket<UnixStream>, thread_id: &str, id: &str, word: &str) {
        send_json(
            socket,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "turn/start",
                "params": {"threadId": thread_id, "input": [{"type": "text", "text": format!("Reply with exactly the single word: {word}")}]},
            }),
        )
        .expect("send turn/start");
        read_matching_result(socket, id).expect("turn/start result");
    }

    /// Like [`driver_run_turn`], but for `live_exhausted_turn_emits_usage_limit_event`: an already-
    /// exhausted real account might reject the turn immediately as a JSON-RPC error rather than
    /// accepting it and rejecting it later via the async notification stream — an outcome that
    /// specific test must observe and report, not treat as a panic. Returns the full raw JSON-RPC
    /// response object (`{"id":..,"result":..}` or `{"id":..,"error":..}`) so the caller can
    /// inspect which shape actually occurred.
    #[cfg(test)]
    fn send_one_turn_capturing_response(
        socket: &mut WebSocket<UnixStream>,
        thread_id: &str,
        id: &str,
        message: &str,
    ) -> Value {
        send_json(
            socket,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "turn/start",
                "params": {"threadId": thread_id, "input": [{"type": "text", "text": message}]},
            }),
        )
        .expect("send turn/start");
        let deadline = Instant::now() + ATTACH_TIMEOUT;
        while Instant::now() < deadline {
            let msg = match socket.read() {
                Ok(msg) => msg,
                Err(error) if is_timeout(&error) => continue,
                Err(error) => {
                    panic!("socket read failed waiting for turn/start response: {error:?}")
                }
            };
            let Message::Text(text) = msg else { continue };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if value.get("id").and_then(Value::as_str) != Some(id) {
                continue;
            }
            return value;
        }
        panic!("no JSON-RPC response for turn/start within the attach timeout");
    }

    /// Reads notifications (tolerating the socket's own read timeout, exactly like
    /// `read_matching_result`) until one carries `method == expected_method`.
    #[cfg(test)]
    fn wait_for_notification(socket: &mut WebSocket<UnixStream>, expected_method: &str) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let message = match socket.read() {
                Ok(message) => message,
                Err(error) if is_timeout(&error) => continue,
                Err(error) => panic!("wait_for_notification({expected_method}): {error:?}"),
            };
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if value.get("method").and_then(Value::as_str) == Some(expected_method) {
                return;
            }
        }
        panic!("wait_for_notification({expected_method}): timed out");
    }
}
