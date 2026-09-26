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
    os::unix::net::UnixStream,
    path::Path,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::events;

/// One decoded, sanitized signal from the observed runtime. Never raw provider output, never an
/// account id, never conversation content — only what the two functions in `crate::events`
/// already decided.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserverEvent {
    /// A turn on the observed thread failed with the structured `usageLimitExceeded` error.
    /// Callers must treat this exactly like GitHub #13's own trigger: start the existing
    /// authoritative `account/rateLimits/read` evaluation, never hand off directly on this alone.
    UsageLimitExceeded,
    /// `account/rateLimits/updated` fired with this sanitized highest-window `usedPercent` (never
    /// `ordinaryUsageAllowed`, never an account id — the type itself cannot carry either). Feeds
    /// `crate::polling::poll_interval_secs`'s existing cadence policy only.
    RateLimitsHint(Option<u32>),
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
}

impl std::fmt::Display for ObserverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect => write!(f, "could not connect to the runtime's private endpoint"),
            Self::Handshake => write!(f, "the runtime did not answer the protocol handshake"),
            Self::HomeMismatch => write!(f, "the runtime resolved a different CODEX_HOME"),
            Self::NoThread => write!(f, "no thread became known within the bounded wait"),
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
    pub thread_id: String,
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
        let stream = connect_unix(endpoint)?;
        let shutdown_handle = stream.try_clone().map_err(|_| ObserverError::Connect)?;
        // A blocking `UnixStream` read has no timeout by default, so without this, every bounded
        // wait below (`Instant::now() < deadline`, re-checked only *between* calls to
        // `socket.read()`) is an illusion: one `read()` that never returns blocks past its own
        // deadline forever. Cleared once attach succeeds (see below) so the long-lived background
        // reader can block indefinitely without busy-polling.
        stream
            .set_read_timeout(Some(ATTACH_POLL_INTERVAL))
            .map_err(|_| ObserverError::Connect)?;
        let mut socket = handshake(stream, expected_codex_home)?;

        let resolved_thread_id = match thread_id {
            Some(id) => id.to_owned(),
            None => wait_for_first_thread(&mut socket)?,
        };
        resume(&mut socket, &resolved_thread_id)?;
        socket
            .get_ref()
            .set_read_timeout(None)
            .map_err(|_| ObserverError::Handshake)?;

        let (sender, events) = mpsc::channel();
        let reader = thread::spawn(move || read_loop(socket, sender));
        Ok(Self {
            events,
            shutdown: shutdown_handle,
            reader: Some(reader),
            thread_id: resolved_thread_id,
        })
    }

    /// Non-blocking drain — the caller's own tick loop (GitHub #13's existing 300ms cadence,
    /// unaffected) calls this; it must never wait for a new event.
    pub fn drain_events(&self) -> Vec<ObserverEvent> {
        let mut drained = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            drained.push(event);
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

fn resume(socket: &mut WebSocket<UnixStream>, thread_id: &str) -> Result<(), ObserverError> {
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
    .map_err(|_| ObserverError::Handshake)?;
    read_matching_result(socket, resume_id)?;
    Ok(())
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

/// The background reader: blocks on `socket.read()` until the socket is shut down (from
/// [`Observer::stop`]/[`Observer::drop`]) or the server closes it, decoding only the two
/// notification shapes `crate::events` knows about and dropping everything else — including any
/// notification shape a future Codex version might add, which must stay inert here exactly as
/// `crate::events`'s own doc comment requires.
fn read_loop(mut socket: WebSocket<UnixStream>, sender: mpsc::Sender<ObserverEvent>) {
    loop {
        let message = match socket.read() {
            Ok(message) => message,
            Err(_) => return,
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
        assert_eq!(observer.thread_id, "thread-1");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_usage_limit = false;
        let mut saw_hint = None;
        while Instant::now() < deadline && (!saw_usage_limit || saw_hint.is_none()) {
            for event in observer.drain_events() {
                match event {
                    ObserverEvent::UsageLimitExceeded => saw_usage_limit = true,
                    ObserverEvent::RateLimitsHint(hint) => saw_hint = Some(hint),
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
        assert_eq!(observer.thread_id, "learned-thread");
        observer.stop();
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
        assert_eq!(observer.thread_id, thread_id);

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
