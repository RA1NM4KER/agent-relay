//! GitHub #16/#18: the external app-server runtime an event-driven observer needs. This module
//! never decides exhaustion and never touches the interactive terminal's own I/O; see
//! `crate::events` for the structured signals it exists to let a caller observe, and `crate::usage`
//! for the one authoritative verdict those signals only ever wake up.
//!
//! ## Why this exists (agent-relay#15's live findings)
//!
//! Today's supervised Codex child (`codex resume <thread-id>`) embeds its own private app-server
//! internally (default `stdio://` transport) — nothing outside that one process can observe its
//! live notifications. The only way to observe `account/rateLimits/updated` or a turn's structured
//! `usageLimitExceeded` error from the *same* interactive runtime is to run that app-server
//! externally and have a separate, passive Relay connection attach to it too. Live-verified
//! against `codex-cli 0.155.0`: multiple independent connections to one external app-server each
//! get the full, correct notification stream for a thread they explicitly subscribed to
//! (`thread/resume`, or having created it themselves) — regardless of which connection actually
//! drives a given turn — with no cross-talk and no interference between connections.
//!
//! ## Which external app-server, and why (agent-relay#18's fix)
//!
//! An earlier version of this module spawned a *private* `codex app-server --listen unix://…`
//! and made the interactive client attach to it via `--remote <ADDR>`. Live-verified against
//! `codex-cli 0.155.0`: `--remote` unconditionally puts that client's session in Codex's own
//! `ThreadParamsMode::Remote`, and Codex's TUI explicitly refuses to carry a CLI permission
//! override (`--yolo`, `--sandbox`, `-a`, any `-c approval_policy=…`/`sandbox_mode=…`/etc.) into a
//! remote-mode resume — resuming remotely always "restores the server's saved permission
//! settings" instead, silently overriding the client's request. A dogfooded real session hit
//! exactly this: `--yolo` plus `--remote` was rejected outright with "Permission overrides are not
//! supported when resuming a remote task."; dropping `--yolo` let the resume proceed but under
//! Codex's default (non-`--yolo`) permissions, breaking MCP writes that used to need no approval.
//! This is deliberate Codex behavior, not a bug Relay can configure around from the client side.
//!
//! [`ensure_managed_daemon`] is the fix: instead of a private endpoint, it ensures Codex's own
//! shared local app-server daemon (`codex app-server daemon start`) is running for the profile's
//! `CODEX_HOME` and returns its well-known control-socket endpoint. An ordinary interactive
//! `codex resume` invocation with **no** `--remote` flag at all auto-discovers and reuses this
//! exact same daemon on its own (the same mechanism `codex agents` already relies on to list
//! every session across simultaneous local invocations) — and a connection the TUI made itself to
//! this daemon is `ThreadParamsMode::Embedded`, not `Remote`, so every permission-affecting
//! argument the caller already resolved onto `TerminalCommand.args` keeps meaning exactly what an
//! ordinary local invocation would give it. Relay's own observer connects to the identical socket
//! completely separately, using the same passive, multi-client-safe protocol #15 already proved.
//!
//! ## What this module deliberately does NOT solve
//!
//! [`ensure_managed_daemon`] never terminates the daemon it confirms running — that daemon is
//! shared and Codex-owned, potentially serving other sessions/tools that have nothing to do with
//! this Relay Session, and must keep running after this session ends exactly as if Relay had never
//! been involved. [`AppServerHandle`] (a private, Relay-owned `--listen` process this module
//! spawns, terminates, and can reap after a crash) still exists for the observer's own live-server
//! test fixtures, which need *some* disposable real app-server to exercise the protocol against —
//! it is no longer used by the production event-driven-observer path.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use relay_core::handoff::ProcessIdentity;
use serde::{Deserialize, Serialize};

use crate::AUTHENTICATION_OVERRIDE_VARIABLES;

/// Identifies one external Codex runtime this process (or an earlier invocation, for
/// [`reconcile_stale`]) may have started: the app-server child's own identity, the private socket
/// it listens on, the profile it must have resolved (checked exactly like
/// `app_server::Session::handshake` already checks the ephemeral one-shot reads), and — once
/// known — the thread the interactive client is actually using. `tui` is `None` until the
/// interactive `--remote` client has actually attached and Relay has recorded its pid; a runtime
/// with an app-server but no recorded TUI yet is not a session anyone can rely on as "the current
/// writer" (see [`RuntimeLiveness`]).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CodexRuntimeIdentity {
    pub app_server: ProcessIdentity,
    pub tui: Option<ProcessIdentity>,
    pub endpoint: PathBuf,
    pub codex_home: PathBuf,
    pub thread_id: Option<String>,
}

/// Whether `identity` still describes a runtime Relay can safely treat as the current writer.
/// Fail-closed: anything other than [`Self::Live`] must never be treated as proof of an active
/// session, mirroring every other `ProcessIdentity`-based check in this codebase
/// (`is_still_the_same_process`, `caller_is_verified`) — `None`/ambiguous is never permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeLiveness {
    /// Both the app-server and the TUI are confirmed alive with matching fingerprints.
    Live,
    /// The app-server is confirmed gone (or never confirmable). The interactive session's actual
    /// runtime is dead even if a TUI process happens to still exist — never trust a TUI pid alone.
    AppServerGone,
    /// The app-server is alive, but the recorded TUI is confirmed gone. The runtime that owns the
    /// thread may still be technically up, but nothing is driving it as this session's writer.
    TuiGone,
    /// No TUI has ever attached to this runtime yet (still starting up).
    TuiNotYetAttached,
    /// Either identity could not be confirmed either way (a `ps` query itself failed, or no
    /// fingerprint was ever recorded) — never treated as live, never treated as dead.
    Ambiguous,
}

impl RuntimeLiveness {
    /// The only state a caller may build on as "this runtime is the current writer."
    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// Evaluates [`CodexRuntimeIdentity`] against the live process table right now. Does not touch
/// the network/socket at all — pure process-identity liveness, exactly as cheap and exactly as
/// fail-closed as the rest of this codebase's `ProcessIdentity` checks. A caller that also wants
/// to know the app-server is *responsive* (not just alive) must additionally attempt a protocol
/// round trip (e.g. `account/rateLimits/read` via `crate::app_server`) and treat a failure there
/// as unproven liveness too — this function alone only proves the OS still schedules the process.
#[must_use]
pub fn evaluate_liveness(identity: &CodexRuntimeIdentity) -> RuntimeLiveness {
    match identity.app_server.is_still_the_same_process() {
        Some(false) => return RuntimeLiveness::AppServerGone,
        None => return RuntimeLiveness::Ambiguous,
        Some(true) => {}
    }
    let Some(tui) = &identity.tui else {
        return RuntimeLiveness::TuiNotYetAttached;
    };
    match tui.is_still_the_same_process() {
        Some(true) => RuntimeLiveness::Live,
        Some(false) => RuntimeLiveness::TuiGone,
        None => RuntimeLiveness::Ambiguous,
    }
}

/// Errors starting or verifying an external app-server runtime.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeError {
    Spawn,
    /// The socket never appeared / the app-server never became ready within the bounded wait.
    NotReady,
    /// The private socket directory or file failed a required permission/ownership/symlink check.
    UnsafeEndpoint,
    /// `codex app-server daemon start` exited successfully but its stdout was not the documented
    /// `{"socketPath": ..., "pid": ...}` shape — never guessed at from a different shape.
    Protocol,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn => write!(f, "codex app-server could not be started"),
            Self::NotReady => write!(f, "codex app-server did not become ready in time"),
            Self::UnsafeEndpoint => {
                write!(f, "the private runtime socket path failed a safety check")
            }
            Self::Protocol => {
                write!(
                    f,
                    "codex app-server daemon start returned an unrecognized response"
                )
            }
        }
    }
}

/// Codex's own shared local app-server daemon this invocation confirmed running: the endpoint an
/// observer connects to, and the daemon's own [`ProcessIdentity`] (never a `Child` this process
/// holds — see [`ensure_managed_daemon`]'s own doc comment for why).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedDaemon {
    pub endpoint: PathBuf,
    pub identity: ProcessIdentity,
}

/// Wall-clock budget for `codex app-server daemon start` to report readiness. A fast, local,
/// non-network command (fork-and-confirm an already-running local daemon, or start a fresh one),
/// so the same bound as [`READY_TIMEOUT`] is generous.
const DAEMON_START_TIMEOUT: Duration = READY_TIMEOUT;

/// Ensures Codex's own shared local app-server daemon is running for `codex_home` and returns its
/// control-socket endpoint plus its process identity. This is the officially supported multi-
/// client backend (`codex agents` already lists every session across simultaneous local
/// invocations through it) — an ordinary interactive `codex resume` with no `--remote` flag
/// auto-discovers and reuses this exact same daemon, which is why using it (instead of a private
/// `--listen` app-server Relay spawns itself) is what actually fixes agent-relay#18: Codex's own
/// TUI treats a connection it made itself to *this* daemon as an ordinary local session for
/// permission purposes (approval policy, sandbox, `--yolo`, everything an interactive user would
/// otherwise get), never the separate, deliberately more restrictive "remote task" mode a
/// `--remote <ADDR>` flag would put it in. See this module's own top-level doc comment for the
/// full agent-relay#15/#16/#18 background and `crate::inspection::VERIFIED_VERSIONS` for exactly
/// which versions this daemon-bootstrap mechanism was live-verified against — notably **not**
/// `0.155.0` itself: that version's `daemon start` requires a full standalone-install layout
/// under `CODEX_HOME` and fails outright against an ordinary isolated Relay profile directory.
/// `0.156.0` is the earliest version with the dedicated `packages/app-server-daemon` bootstrap
/// this function depends on.
///
/// Deliberately NOT [`AppServerHandle::spawn`]: that function owns a `Child` this process must
/// terminate. `codex app-server daemon start` is a short-lived command — it exits immediately
/// once the daemon (persistent, shared, Codex-owned, potentially already running for a
/// completely unrelated reason) is confirmed up, reporting the *daemon's* own identity, not its
/// own. There is no child of this invocation to hold or reap; the returned
/// [`ManagedDaemon::identity`] is only ever used to notice the daemon going away later, never to
/// terminate it — Relay must never stop a daemon it does not own (agent-relay#18: the whole point
/// is this must keep running for other, unrelated Codex sessions/tools exactly as if Relay had
/// never been involved). Live-verified (agent-relay#18 preflight): the daemon's pid is only ever
/// inline in this command's own JSON when it *started* the daemon this call
/// (`"status":"started"`) — the equally common "it was already running" outcome
/// (`"status":"alreadyRunning"`) omits it, so this function falls back to the pidfile Codex
/// itself durably records either way (see [`read_daemon_pid`]).
pub fn ensure_managed_daemon(
    executable: &Path,
    codex_home: &Path,
) -> Result<ManagedDaemon, RuntimeError> {
    let mut command = Command::new(executable);
    command
        .arg("app-server")
        .arg("daemon")
        .arg("start")
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let mut child = command.spawn().map_err(|_| RuntimeError::Spawn)?;
    let deadline = Instant::now() + DAEMON_START_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ignored = child.kill();
                let _ignored = child.wait();
                return Err(RuntimeError::NotReady);
            }
            Ok(None) => std::thread::sleep(READY_POLL_INTERVAL),
            Err(_) => return Err(RuntimeError::Spawn),
        }
    };
    if !status.success() {
        return Err(RuntimeError::Spawn);
    }
    let mut stdout_bytes = Vec::new();
    {
        use std::io::Read as _;
        let Some(mut stdout) = child.stdout.take() else {
            return Err(RuntimeError::Protocol);
        };
        stdout
            .read_to_end(&mut stdout_bytes)
            .map_err(|_| RuntimeError::Protocol)?;
    }
    let (socket_path, pid) = parse_managed_daemon(&stdout_bytes).ok_or(RuntimeError::Protocol)?;
    let pid = match pid {
        Some(pid) => pid,
        // agent-relay#18 preflight finding: `daemon start`'s own JSON only carries `pid` when
        // it actually started the daemon this call (`"status":"started"`). The equally
        // documented, equally common "it was already running" outcome
        // (`"status":"alreadyRunning"`) omits `pid` entirely — live-verified against a real
        // `codex-cli 0.156.0` daemon a second resume reused. `daemon.pid` under the dedicated
        // package directory is the one place that pid is durably recorded either way.
        None => read_daemon_pid(codex_home).ok_or(RuntimeError::Protocol)?,
    };
    Ok(ManagedDaemon {
        endpoint: PathBuf::from(socket_path),
        identity: ProcessIdentity::query(pid),
    })
}

/// Reads the daemon's own pid from `CODEX_HOME/app-server-daemon/daemon.pid`
/// (`{"pid": <u32>, "processStartTime": ..., ...}`) — the fallback `ensure_managed_daemon` needs
/// when `daemon start` reports `"status":"alreadyRunning"` and so never echoes `pid` itself.
fn read_daemon_pid(codex_home: &Path) -> Option<u32> {
    let bytes = fs::read(codex_home.join("app-server-daemon").join("daemon.pid")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let pid = value.get("pid")?.as_u64()?;
    u32::try_from(pid).ok()
}

/// Parses `codex app-server daemon start`'s documented JSON stdout — the socket path (always
/// present) and the pid (present only on `"status":"started"`; absent on
/// `"status":"alreadyRunning"`, see [`ensure_managed_daemon`]'s own handling of that case) —
/// live-verified against `codex-cli 0.156.0` (see this function's doc comment for why not
/// `0.155.0`). Never a different, undocumented shape guessed at from field position or ordering.
fn parse_managed_daemon(stdout: &[u8]) -> Option<(String, Option<u32>)> {
    let text = std::str::from_utf8(stdout).ok()?;
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let socket_path = value.get("socketPath")?.as_str()?.to_owned();
    let pid = value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    Some((socket_path, pid))
}

/// A short, private, per-user, per-attempt unix socket path — deliberately never nested under a
/// profile's own (long) state directory. Agent-relay#15 hit a real macOS `SUN_LEN` failure
/// (`Error: path must be shorter than SUN_LEN`) doing exactly that. Base directory: `/tmp` itself
/// (always short, unlike `$TMPDIR`, which on macOS is routinely 50+ characters on its own),
/// scoped per-user by the same "read `$HOME`'s owning uid" technique
/// `inspection::validate_executable_platform` already uses (there is no safe way to call
/// `getuid()` directly under this workspace's `unsafe_code = "forbid"`), and a random-ish
/// per-attempt suffix so concurrent Relay sessions — or a retried spawn after a failed one — can
/// never collide on the same path.
pub fn allocate_endpoint() -> Result<PathBuf, RuntimeError> {
    let directory = private_socket_directory()?;
    let suffix = unique_suffix();
    Ok(directory.join(format!("{suffix}.sock")))
}

fn private_socket_directory() -> Result<PathBuf, RuntimeError> {
    let uid = home_owner_uid().ok_or(RuntimeError::UnsafeEndpoint)?;
    let directory = PathBuf::from("/tmp").join(format!("relay-codex-{uid}"));
    ensure_private_directory(&directory)?;
    Ok(directory)
}

/// The uid that owns `$HOME` — used only to scope the socket directory name per user, exactly as
/// `relay_provider_codex::inspection::validate_executable_platform` already establishes the
/// "current user" without an `unsafe` FFI call to `getuid()`.
fn home_owner_uid() -> Option<u32> {
    let home = std::env::var_os("HOME")?;
    Some(fs::metadata(home).ok()?.uid())
}

/// Creates the private base directory if missing (owner-only, 0700), or verifies an existing one
/// is genuinely private and not a symlink — the same defence
/// `relay_core::handoff::OrchestrationLock::open_lock` already applies to its own lock file, since
/// a pre-existing symlink or a directory some other uid controls could otherwise redirect Relay's
/// socket into an attacker-controlled location.
fn ensure_private_directory(directory: &Path) -> Result<(), RuntimeError> {
    if fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(RuntimeError::UnsafeEndpoint);
    }
    match fs::create_dir(directory) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .map_err(|_| RuntimeError::UnsafeEndpoint)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_private_directory(directory)
        }
        Err(_) => Err(RuntimeError::UnsafeEndpoint),
    }
}

#[cfg(unix)]
fn verify_private_directory(directory: &Path) -> Result<(), RuntimeError> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = fs::metadata(directory).map_err(|_| RuntimeError::UnsafeEndpoint)?;
    if !metadata.is_dir() {
        return Err(RuntimeError::UnsafeEndpoint);
    }
    let owned_by_us = home_owner_uid() == Some(metadata.uid());
    let owner_only = metadata.permissions().mode() & 0o077 == 0;
    if owned_by_us && owner_only {
        Ok(())
    } else {
        Err(RuntimeError::UnsafeEndpoint)
    }
}

#[cfg(not(unix))]
fn verify_private_directory(_directory: &Path) -> Result<(), RuntimeError> {
    Err(RuntimeError::UnsafeEndpoint)
}

/// Not a security token — only a path discriminator, so retried/concurrent spawns never collide
/// on the same socket path. Safe to be predictable; the directory permissions (owner-only) are
/// what actually keeps this private, not the filename.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{:x}-{:x}", std::process::id(), nanos & 0xffff_ffff)
}

/// A live handle to one external `codex app-server --listen unix://…` this process spawned.
/// Dropping this without calling [`Self::terminate`] leaves the child running — GitHub #16's own
/// finding that nothing about the protocol or a closed pipe stops it — so callers must always
/// explicitly terminate it as part of normal session cleanup (see this module's own doc comment
/// for exactly what is, and is not, guaranteed).
pub struct AppServerHandle {
    child: Child,
    pub identity: ProcessIdentity,
    pub endpoint: PathBuf,
}

/// Wall-clock budget for the app-server to create its socket file after spawning.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Grace period for a `SIGTERM` before escalating to `SIGKILL`, matching
/// `crate::terminal::terminate`'s own existing grace window for the interactive child.
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

impl AppServerHandle {
    /// Spawns `codex app-server --listen unix://<endpoint>` under `config_dir`'s isolated
    /// `CODEX_HOME`, waits (bounded) for the socket file to appear, and returns a handle carrying
    /// the child's own `ProcessIdentity` — never assumed live from the `Child` alone; queried
    /// fresh via `ps`, exactly like every other process-identity record in this codebase.
    ///
    /// Does NOT verify the app-server actually resolved `config_dir` as its `codexHome` — that
    /// check happens on the protocol connection itself (mirroring
    /// `crate::app_server::Session::handshake`'s existing `HomeMismatch` check), since it needs an
    /// actual `initialize` round trip this function deliberately does not make (spawning and
    /// verifying protocol identity are different concerns; a caller that skips the connection
    /// step must never treat a merely-running app-server as proof of the right profile).
    pub fn spawn(
        executable: &Path,
        config_dir: &Path,
        endpoint: &Path,
    ) -> Result<Self, RuntimeError> {
        let mut command = Command::new(executable);
        command
            .arg("app-server")
            .arg("--listen")
            .arg(format!("unix://{}", endpoint.display()))
            .env("CODEX_HOME", config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
            command.env_remove(variable);
        }
        let child = command.spawn().map_err(|_| RuntimeError::Spawn)?;
        let identity = ProcessIdentity::query(child.id());
        let handle = Self {
            child,
            identity,
            endpoint: endpoint.to_path_buf(),
        };
        handle.wait_ready()?;
        Ok(handle)
    }

    fn wait_ready(&self) -> Result<(), RuntimeError> {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            if self.endpoint.exists() {
                return Ok(());
            }
            std::thread::sleep(READY_POLL_INTERVAL);
        }
        Err(RuntimeError::NotReady)
    }

    /// Graceful `SIGTERM` first (so the app-server can close its listener/socket file cleanly),
    /// then a hard `SIGKILL` if it ignores that — the same two-step `crate::terminal::terminate`
    /// already uses for the interactive child, applied here for the exact reason this module's
    /// doc comment explains: nothing about this protocol stops the process on its own.
    pub fn terminate(mut self) {
        terminate_child(&mut self.child);
        let _ignored = fs::remove_file(&self.endpoint);
    }
}

impl Drop for AppServerHandle {
    /// Best-effort safety net only — normal cleanup must call [`Self::terminate`] explicitly so
    /// the caller controls exactly when it happens relative to the rest of session teardown
    /// (releasing the lease, clearing the control record, etc.). A drop reached without an
    /// explicit `terminate()` (an early return, a panic unwinding) still must not leak the child.
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
        let _ignored = fs::remove_file(&self.endpoint);
    }
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) {
    let _ignored = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status();
    let started = Instant::now();
    while started.elapsed() < TERMINATE_GRACE {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ignored = child.kill();
    let _ignored = child.wait();
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) {
    let _ignored = child.kill();
    let _ignored = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn live_identity() -> (StdCommand, std::process::Child, ProcessIdentity) {
        let child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::query(child.id());
        (std::process::Command::new("true"), child, identity)
    }

    /// A genuinely, confirmably dead `ProcessIdentity`: captured *while the process was alive*
    /// (so `start_time_fingerprint` is `Some`, exactly as a real record always is), then reaped.
    /// Querying `ProcessIdentity::query` on an *already-dead* pid instead — the mistake this
    /// helper exists to avoid — always yields `start_time_fingerprint: None` (`query`'s own
    /// `ConfirmedAbsent | Indeterminate => None` mapping), which makes every liveness check
    /// answer `Ambiguous`, not `Gone`; `is_still_the_same_process` only ever answers `Some(false)`
    /// when a *previously recorded* fingerprint exists to compare against a now-confirmed-absent
    /// pid (see `relay_core::handoff::lock`'s own identical test pattern).
    fn dead_identity() -> ProcessIdentity {
        // Must still be alive at the moment of `query` (a `true` that has already exited by then
        // gives the same unconfirmable `None` fingerprint this helper exists to avoid).
        let mut child = std::process::Command::new("sleep")
            .arg("2")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::query(child.id());
        assert!(
            identity.start_time_fingerprint.is_some(),
            "must capture a real fingerprint while alive"
        );
        let _ = child.kill();
        child.wait().expect("reap");
        std::thread::sleep(Duration::from_millis(100));
        identity
    }

    // --- RuntimeLiveness ---

    #[test]
    fn both_alive_is_live() {
        let (_c, mut app_server, app_server_id) = live_identity();
        let (_c2, mut tui, tui_id) = live_identity();
        let identity = CodexRuntimeIdentity {
            app_server: app_server_id,
            tui: Some(tui_id),
            endpoint: PathBuf::from("/tmp/x.sock"),
            codex_home: PathBuf::from("/config"),
            thread_id: Some("t1".to_owned()),
        };
        assert_eq!(evaluate_liveness(&identity), RuntimeLiveness::Live);
        assert!(evaluate_liveness(&identity).is_live());
        let _ = app_server.kill();
        let _ = app_server.wait();
        let _ = tui.kill();
        let _ = tui.wait();
    }

    #[test]
    fn a_dead_app_server_is_reported_even_if_a_tui_identity_is_present() {
        let (_c, mut tui, tui_id) = live_identity();
        let identity = CodexRuntimeIdentity {
            app_server: dead_identity(),
            tui: Some(tui_id),
            endpoint: PathBuf::from("/tmp/x.sock"),
            codex_home: PathBuf::from("/config"),
            thread_id: None,
        };
        assert_eq!(evaluate_liveness(&identity), RuntimeLiveness::AppServerGone);
        assert!(!evaluate_liveness(&identity).is_live());
        let _ = tui.kill();
        let _ = tui.wait();
    }

    #[test]
    fn a_dead_tui_with_a_live_app_server_is_reported_distinctly() {
        let (_c, mut app_server, app_server_id) = live_identity();
        let identity = CodexRuntimeIdentity {
            app_server: app_server_id,
            tui: Some(dead_identity()),
            endpoint: PathBuf::from("/tmp/x.sock"),
            codex_home: PathBuf::from("/config"),
            thread_id: Some("t1".to_owned()),
        };
        assert_eq!(evaluate_liveness(&identity), RuntimeLiveness::TuiGone);
        assert!(!evaluate_liveness(&identity).is_live());
        let _ = app_server.kill();
        let _ = app_server.wait();
    }

    #[test]
    fn no_tui_attached_yet_is_its_own_distinct_state_not_live() {
        let (_c, mut app_server, app_server_id) = live_identity();
        let identity = CodexRuntimeIdentity {
            app_server: app_server_id,
            tui: None,
            endpoint: PathBuf::from("/tmp/x.sock"),
            codex_home: PathBuf::from("/config"),
            thread_id: None,
        };
        assert_eq!(
            evaluate_liveness(&identity),
            RuntimeLiveness::TuiNotYetAttached
        );
        assert!(!evaluate_liveness(&identity).is_live());
        let _ = app_server.kill();
        let _ = app_server.wait();
    }

    #[test]
    fn an_unconfirmable_app_server_identity_is_ambiguous_never_live_never_dead() {
        // `start_time_fingerprint: None` is the genuinely-indeterminate case (never established,
        // or `ps` itself failed) — distinct from a *wrong* fingerprint against a live pid, which
        // `ProcessIdentity::is_still_the_same_process` correctly reports as a definite `Some(false)`
        // (see `relay_core::handoff::lock`'s own `a_fabricated_identity_with_a_wrong_fingerprint_
        // is_detected_as_different`), not an ambiguity.
        let bogus = ProcessIdentity {
            pid: std::process::id(),
            start_time_fingerprint: None,
        };
        let identity = CodexRuntimeIdentity {
            app_server: bogus,
            tui: None,
            endpoint: PathBuf::from("/tmp/x.sock"),
            codex_home: PathBuf::from("/config"),
            thread_id: None,
        };
        assert_eq!(evaluate_liveness(&identity), RuntimeLiveness::Ambiguous);
        assert!(!evaluate_liveness(&identity).is_live());
    }

    // --- socket path scheme ---

    #[test]
    fn allocated_endpoints_are_short_enough_for_macos_sun_len_and_owner_private() {
        let path = allocate_endpoint().expect("allocate endpoint");
        // macOS `sockaddr_un.sun_path` is 104 bytes including the null terminator; stay well
        // under that so a realistic Relay invocation never repeats agent-relay#15's live
        // `Error: path must be shorter than SUN_LEN` failure.
        assert!(
            path.as_os_str().len() < 100,
            "endpoint path too long for SUN_LEN: {} ({} bytes)",
            path.display(),
            path.as_os_str().len()
        );
        // `Path::starts_with` compares whole components, not string prefixes — the directory
        // component is `relay-codex-<uid>`, so a plain string check is what we actually want.
        assert!(path.to_string_lossy().starts_with("/tmp/relay-codex-"));
        let directory = path.parent().expect("parent");
        let metadata = fs::metadata(directory).expect("directory exists");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn two_allocations_never_collide() {
        let a = allocate_endpoint().expect("first");
        let b = allocate_endpoint().expect("second");
        assert_ne!(a, b);
    }

    #[test]
    fn a_symlinked_base_directory_is_rejected_not_followed() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let real_target = scratch.path().join("elsewhere");
        fs::create_dir_all(&real_target).expect("real target");
        let uid = home_owner_uid().expect("uid");
        let fake_base = PathBuf::from("/tmp").join(format!("relay-codex-{uid}-test-symlink"));
        let _ = fs::remove_file(&fake_base);
        let _ = fs::remove_dir_all(&fake_base);
        std::os::unix::fs::symlink(&real_target, &fake_base).expect("symlink");
        let result = ensure_private_directory(&fake_base);
        assert_eq!(result, Err(RuntimeError::UnsafeEndpoint));
        let _ = fs::remove_file(&fake_base);
    }

    // --- spawn / ready / terminate against a scripted fake app-server ---

    /// A minimal scripted stand-in for `codex app-server --listen unix://PATH`: it only needs to
    /// create the socket path (as a plain file — real readiness detection here is "the path
    /// exists," matching `AppServerHandle::wait_ready`) and then sleep, so tests can exercise
    /// spawn/ready/terminate without a real Codex install.
    fn install_fake_app_server(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-codex-app-server");
        fs::write(
            &path,
            r#"#!/bin/sh
[ "$1" = "app-server" ] || exit 64
shift
listen=""
while [ $# -gt 0 ]; do
  case "$1" in
    --listen) listen="$2"; shift 2 ;;
    *) shift ;;
  esac
done
sockpath=$(printf '%s' "$listen" | sed 's#^unix://##')
touch "$sockpath"
trap 'rm -f "$sockpath"; exit 0' TERM
while true; do sleep 1; done
"#,
        )
        .expect("write fake app-server");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[test]
    fn spawn_waits_for_the_socket_and_records_a_confirmable_identity() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let exe = install_fake_app_server(scratch.path());
        let endpoint = allocate_endpoint().expect("endpoint");
        let handle = AppServerHandle::spawn(&exe, scratch.path(), &endpoint).expect("spawn");
        assert_eq!(handle.identity.is_still_the_same_process(), Some(true));
        assert!(
            endpoint.exists(),
            "socket path must exist once ready() returns"
        );
        handle.terminate();
        assert!(!endpoint.exists(), "terminate must remove the socket file");
    }

    #[test]
    fn a_missing_executable_fails_closed_as_spawn_error() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let endpoint = allocate_endpoint().expect("endpoint");
        let result = AppServerHandle::spawn(
            Path::new("/definitely/not/codex"),
            scratch.path(),
            &endpoint,
        );
        assert!(matches!(result, Err(RuntimeError::Spawn)));
    }

    // --- ensure_managed_daemon: agent-relay#18's fix, reusing Codex's own shared local daemon
    // instead of a private `--listen` app-server, so the interactive client never needs `--remote`
    // at all and keeps ordinary permission semantics. ---

    /// A minimal scripted stand-in for `codex app-server daemon start`: prints the documented
    /// `{"socketPath": ..., "pid": ...}` JSON and exits immediately (the real command's daemon
    /// process detaches and keeps running independently; this fixture only needs to exercise the
    /// parsing/timeout contract, never the daemon's own lifetime).
    fn install_fake_daemon_start(dir: &Path, pid: u32, socket_path: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-codex-daemon-start");
        fs::write(
            &path,
            format!(
                r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
printf '{{"status":"started","backend":"pid","pid":{pid},"socketPath":"{socket}"}}\n'
"#,
                pid = pid,
                socket = socket_path.display(),
            ),
        )
        .expect("write fake daemon-start");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn install_fake_daemon_start_failing(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-codex-daemon-start-failing");
        fs::write(
            &path,
            r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
echo "daemon failed to start" >&2
exit 1
"#,
        )
        .expect("write fake failing daemon-start");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[test]
    fn ensure_managed_daemon_parses_the_documented_socket_and_pid() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let (_c, mut app_server, identity) = live_identity();
        let socket_path = scratch.path().join("app-server-control.sock");
        let exe = install_fake_daemon_start(scratch.path(), identity.pid, &socket_path);

        let daemon = ensure_managed_daemon(&exe, scratch.path()).expect("ensure daemon");
        assert_eq!(daemon.endpoint, socket_path);
        assert_eq!(daemon.identity.pid, identity.pid);
        assert_eq!(daemon.identity.is_still_the_same_process(), Some(true));
        let _ = app_server.kill();
        let _ = app_server.wait();
    }

    #[test]
    fn ensure_managed_daemon_fails_closed_when_the_command_exits_nonzero() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let exe = install_fake_daemon_start_failing(scratch.path());
        let result = ensure_managed_daemon(&exe, scratch.path());
        assert!(matches!(result, Err(RuntimeError::Spawn)));
    }

    #[test]
    fn ensure_managed_daemon_fails_closed_on_a_missing_executable() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let result = ensure_managed_daemon(Path::new("/definitely/not/codex"), scratch.path());
        assert!(matches!(result, Err(RuntimeError::Spawn)));
    }

    #[test]
    fn parse_managed_daemon_rejects_an_undocumented_shape() {
        assert!(parse_managed_daemon(b"not json").is_none());
        assert!(
            parse_managed_daemon(br#"{"status":"started"}"#).is_none(),
            "missing socketPath entirely is never acceptable, pid or not"
        );
    }

    #[test]
    fn parse_managed_daemon_accepts_the_live_verified_started_shape() {
        let (socket_path, pid) = parse_managed_daemon(
            br#"{"status":"started","backend":"pid","pid":46643,"managedCodexPath":"/x","managedCodexVersion":"0.156.0","socketPath":"/Users/x/.codex/app-server-control/app-server-control.sock","cliVersion":"0.156.0","appServerVersion":"0.156.0"}"#,
        )
        .expect("parse");
        assert_eq!(pid, Some(46643));
        assert_eq!(
            socket_path,
            "/Users/x/.codex/app-server-control/app-server-control.sock"
        );
    }

    #[test]
    fn parse_managed_daemon_accepts_the_live_verified_already_running_shape_with_no_pid() {
        // agent-relay#18 preflight finding: a real codex-cli 0.156.0 daemon a second resume
        // reused answers exactly this shape -- `pid` is entirely absent, never null or a
        // placeholder, and a non-number `pid` (if some future version ever sent one) must be
        // treated the same as absent, never a parse failure for the whole response.
        let (socket_path, pid) = parse_managed_daemon(
            br#"{"status":"alreadyRunning","backend":"pid","managedCodexPath":"/x","managedCodexVersion":"0.156.0","socketPath":"/Users/x/.codex/app-server-control/app-server-control.sock","cliVersion":"0.156.0","appServerVersion":"0.156.0"}"#,
        )
        .expect("parse");
        assert_eq!(pid, None);
        assert_eq!(
            socket_path,
            "/Users/x/.codex/app-server-control/app-server-control.sock"
        );
    }

    #[test]
    fn read_daemon_pid_reads_the_documented_pidfile_shape() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let daemon_dir = scratch.path().join("app-server-daemon");
        fs::create_dir_all(&daemon_dir).expect("daemon dir");
        fs::write(
            daemon_dir.join("daemon.pid"),
            br#"{"pid":44173,"processStartTime":"Wed Oct  7 05:10:37 2026"}"#,
        )
        .expect("write pidfile");
        assert_eq!(read_daemon_pid(scratch.path()), Some(44173));
    }

    #[test]
    fn read_daemon_pid_is_none_when_the_pidfile_is_absent_or_malformed() {
        let scratch = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_daemon_pid(scratch.path()), None, "no pidfile at all");
        let daemon_dir = scratch.path().join("app-server-daemon");
        fs::create_dir_all(&daemon_dir).expect("daemon dir");
        fs::write(daemon_dir.join("daemon.pid"), b"not json").expect("write garbage");
        assert_eq!(read_daemon_pid(scratch.path()), None, "malformed pidfile");
    }

    /// Reproduces the exact agent-relay#18 preflight finding against the real fake-daemon-start
    /// fixture: a second `ensure_managed_daemon` call for an already-running daemon must still
    /// succeed, recovering the pid from the pidfile since `daemon start`'s own JSON omits it.
    #[test]
    fn ensure_managed_daemon_recovers_the_pid_from_the_pidfile_when_already_running() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tempfile::tempdir().expect("tempdir");
        let (_c, mut app_server, identity) = live_identity();
        let socket_path = scratch.path().join("app-server-control.sock");
        let daemon_dir = scratch.path().join("app-server-daemon");
        fs::create_dir_all(&daemon_dir).expect("daemon dir");
        fs::write(
            daemon_dir.join("daemon.pid"),
            format!(r#"{{"pid":{}}}"#, identity.pid),
        )
        .expect("write pidfile");

        let exe = scratch.path().join("fake-codex-already-running");
        fs::write(
            &exe,
            format!(
                r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
printf '{{"status":"alreadyRunning","backend":"pid","socketPath":"{socket}"}}\n'
"#,
                socket = socket_path.display(),
            ),
        )
        .expect("write fake daemon-start");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).expect("chmod");

        let daemon = ensure_managed_daemon(&exe, scratch.path()).expect("ensure daemon");
        assert_eq!(daemon.endpoint, socket_path);
        assert_eq!(daemon.identity.pid, identity.pid);
        let _ = app_server.kill();
        let _ = app_server.wait();
    }

    #[test]
    fn ensure_managed_daemon_fails_closed_when_already_running_but_the_pidfile_is_missing() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket_path = scratch.path().join("app-server-control.sock");
        let exe = scratch.path().join("fake-codex-already-running-no-pidfile");
        fs::write(
            &exe,
            format!(
                r#"#!/bin/sh
[ "$1" = "app-server" ] && [ "$2" = "daemon" ] && [ "$3" = "start" ] || exit 64
printf '{{"status":"alreadyRunning","backend":"pid","socketPath":"{socket}"}}\n'
"#,
                socket = socket_path.display(),
            ),
        )
        .expect("write fake daemon-start");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).expect("chmod");

        let result = ensure_managed_daemon(&exe, scratch.path());
        assert!(matches!(result, Err(RuntimeError::Protocol)));
    }
}
