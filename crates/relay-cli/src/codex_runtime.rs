//! GitHub #16: opt-in wiring for the external app-server + observer runtime that lets a
//! supervised Codex terminal wake up on a structured `usageLimitExceeded` error or
//! `account/rateLimits/updated` hint, instead of only ever finding out on GitHub #13's own next
//! poll. See `relay_provider_codex::runtime`/`observer`/`events` for the research findings and
//! the actual protocol mechanics this wires together; this module owns only the session-scoped
//! orchestration (when to start it, where its one small durable record lives, when to stop it).
//!
//! **Off by default.** [`event_driven_enabled`] gates every entry point in this module —
//! unset/not `"1"`, every Codex launch/resume/switch path is byte-for-byte what it was before
//! this issue. This is deliberate: agent-relay#15's research resolved every open protocol
//! question with live evidence, but the *topology* change this issue's target architecture
//! requires (an external app-server process Relay itself must now own the full lifecycle of, and
//! a `--remote` interactive client instead of today's single self-contained child) is large
//! enough that shipping it as the new unconditional default without a real dogfooding period
//! would risk exactly what this issue's own instructions warn against — optimizing for "issue
//! closed" over Relay's existing safety model. GitHub #13 is completely unaffected either way:
//! its own polling never depends on anything in this module, by construction (see
//! `relay_provider_codex::app_server`'s ephemeral one-shot reads, unchanged).
//!
//! Every entry point here is best-effort in the same sense the rest of Codex supervision already
//! is: any failure at any step (version unverified, spawn failed, handshake failed, socket path
//! rejected) falls back to launching the interactive command exactly as before, and is never
//! surfaced as a startup failure.

use std::path::{Path, PathBuf};

use relay_provider_codex::{
    VersionStatus, assess_version,
    observer::Observer,
    runtime::{
        AppServerHandle, CodexRuntimeRecord, StaleReconciliation, allocate_endpoint,
        reconcile_stale,
    },
};
pub const EVENT_DRIVEN_ENV: &str = "RELAY_CODEX_EVENT_DRIVEN";

const RECORD_FILE_NAME: &str = "codex_runtime.json";

#[must_use]
fn event_driven_enabled() -> bool {
    std::env::var(EVENT_DRIVEN_ENV).ok().as_deref() == Some("1")
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join(RECORD_FILE_NAME)
}

fn write_record(state_dir: &Path, record: &CodexRuntimeRecord) {
    if let Ok(text) = serde_json::to_string(record) {
        let _ignored = std::fs::write(record_path(state_dir), text);
    }
}

fn read_record(state_dir: &Path) -> Option<CodexRuntimeRecord> {
    let bytes = std::fs::read(record_path(state_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn clear_record(state_dir: &Path) {
    let _ignored = std::fs::remove_file(record_path(state_dir));
}

/// GitHub #16's stale-runtime guarantee: before starting a *new* runtime for this session, find
/// and safely reap any orphan an earlier crashed invocation of this exact session left behind.
/// Never kills anything on a bare pid/path match — only on the exact pid+start-time fingerprint
/// proof `reconcile_stale` already requires. Ambiguous cases are reported, not guessed about, and
/// never block starting a fresh runtime (the record is simply replaced once the new one starts).
fn reconcile_before_launch(state_dir: &Path, json_mode: bool) {
    let Some(record) = read_record(state_dir) else {
        return;
    };
    match reconcile_stale(&record) {
        StaleReconciliation::SafeToReap(identity) => {
            AppServerHandle::terminate_orphan(&identity, &record.endpoint);
            if !json_mode {
                eprintln!(
                    "[Relay] reaped an orphaned Codex app-server from an earlier session (pid {})",
                    identity.pid
                );
            }
        }
        StaleReconciliation::NothingToReap => {}
        StaleReconciliation::Ambiguous => {
            if !json_mode {
                eprintln!(
                    "[Relay] found a Codex event-runtime record that could not be confirmed live or gone; leaving it alone"
                );
            }
            return; // do not clear an ambiguous record — nothing here is confirmed safe to touch.
        }
    }
    clear_record(state_dir);
}

/// One started event-driven runtime: the external app-server this invocation owns end-to-end,
/// and Relay's own passive observer attached to it. `remote_arg` is what the caller appends to
/// the interactive command's own arguments so the *real* interactive client (not this process)
/// connects to the same runtime and becomes the thing driving turns on it.
pub(crate) struct EventDrivenRuntime {
    handle: AppServerHandle,
    observer: Observer,
    state_dir: PathBuf,
}

impl EventDrivenRuntime {
    /// Best-effort start — for a **resume only** (`thread_id` must be `Some`; Relay's own lease
    /// already names it). A fresh launch (no thread yet) is deliberately never attempted here:
    /// `relay_provider_codex::observer::Observer::attach`'s own doc comment records a live-
    /// confirmed limitation where the only way to learn a fresh thread's id (waiting for its
    /// `thread/started` broadcast) always races a real server-side rule that rejects `thread/
    /// resume` for a thread with no completed turns yet. Fixing that needs a genuinely open-ended
    /// retry (a real user may take any amount of time to send their first message), which is out
    /// of scope for this pass — GitHub #13 alone covers a fresh launch exactly as it always has.
    /// Returns `None` on any failure, silently — callers must always still launch the interactive
    /// command, just without `--remote`, exactly as before this issue.
    pub(crate) fn start(
        codex_executable: &Path,
        config_dir: &Path,
        state_dir: &Path,
        thread_id: Option<&str>,
        json_mode: bool,
    ) -> Option<Self> {
        let thread_id = thread_id?;
        if !event_driven_enabled() {
            return None;
        }
        if !version_verified(codex_executable) {
            return None;
        }
        reconcile_before_launch(state_dir, json_mode);

        let endpoint = allocate_endpoint().ok()?;
        let handle = AppServerHandle::spawn(codex_executable, config_dir, &endpoint).ok()?;
        write_record(
            state_dir,
            &CodexRuntimeRecord {
                app_server: handle.identity.clone(),
                endpoint: handle.endpoint.clone(),
                codex_home: config_dir.to_path_buf(),
            },
        );
        match Observer::attach(&handle.endpoint, config_dir, Some(thread_id)) {
            Ok(observer) => Some(Self {
                handle,
                observer,
                state_dir: state_dir.to_path_buf(),
            }),
            Err(_) => {
                // The app-server itself started fine but Relay's own observer could not attach —
                // never leave an unobserved, unrecorded app-server running for no reason.
                clear_record(state_dir);
                handle.terminate();
                None
            }
        }
    }

    /// The extra argument pair the interactive `TerminalCommand` must carry so it connects to
    /// this exact runtime instead of embedding its own private one.
    pub(crate) fn remote_args(&self) -> [std::ffi::OsString; 2] {
        [
            "--remote".into(),
            format!("unix://{}", self.handle.endpoint.display()).into(),
        ]
    }

    /// The thread id this runtime's observer is actually watching — for a fresh launch, this is
    /// only known *after* [`Self::start`] returns (learned from the broadcast), which is exactly
    /// why callers must read it back from here rather than assume the id they might have passed
    /// in themselves.
    pub(crate) fn thread_id(&self) -> &str {
        &self.observer.thread_id
    }

    /// Drains decoded events into `scheduler` (see
    /// `crate::codex_poll::CodexPollScheduler::notify_event`) — called on the same 300ms tick
    /// that already drives GitHub #13's own scheduler, never on its own timer.
    pub(crate) fn tick(&self, scheduler: &mut crate::codex_poll::CodexPollScheduler) {
        for event in self.observer.drain_events() {
            scheduler.notify_event(event);
        }
    }

    /// Normal/handled session end: stop the observer, terminate the app-server synchronously,
    /// and clear the durable record — GitHub #16's "normal exit must leave no zombie process and
    /// no stale record" requirement. Never called on Relay's own `SIGKILL` (nothing runs then by
    /// definition); that case is instead [`reconcile_before_launch`]'s job on the *next*
    /// invocation of this session.
    pub(crate) fn stop(self) {
        self.observer.stop();
        self.handle.terminate();
        clear_record(&self.state_dir);
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
    use relay_core::handoff::ProcessIdentity;

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

    #[test]
    fn env_var_gate_defaults_to_off() {
        // Never mutates real process env (this workspace forbids `unsafe`, which `set_var`
        // requires since Rust 2024) — matches `codex_poll::poll_mode_from_value`'s own rationale.
        // This test only documents the constant name/contract other tests and callers rely on.
        assert_eq!(EVENT_DRIVEN_ENV, "RELAY_CODEX_EVENT_DRIVEN");
    }

    #[test]
    fn a_missing_record_reconciles_to_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Must not panic, must not create anything, on a session with no prior runtime record.
        reconcile_before_launch(dir.path(), true);
        assert!(!record_path(dir.path()).exists());
    }

    #[test]
    fn a_confirmed_dead_recorded_runtime_is_cleared_without_reaping_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_record(
            dir.path(),
            &CodexRuntimeRecord {
                app_server: dead_identity(),
                endpoint: PathBuf::from("/tmp/does-not-matter.sock"),
                codex_home: PathBuf::from("/config"),
            },
        );
        assert!(record_path(dir.path()).exists());
        reconcile_before_launch(dir.path(), true);
        assert!(!record_path(dir.path()).exists());
    }

    #[test]
    fn an_ambiguous_recorded_runtime_is_left_alone_not_cleared() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_record(
            dir.path(),
            &CodexRuntimeRecord {
                app_server: ProcessIdentity {
                    pid: std::process::id(),
                    start_time_fingerprint: None,
                },
                endpoint: PathBuf::from("/tmp/does-not-matter.sock"),
                codex_home: PathBuf::from("/config"),
            },
        );
        reconcile_before_launch(dir.path(), true);
        assert!(
            record_path(dir.path()).exists(),
            "an ambiguous record must never be silently cleared"
        );
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
            true,
        );
        assert!(result.is_none());
    }

    #[test]
    fn start_is_always_a_no_op_without_a_known_thread_id() {
        // See `Observer::attach`'s own doc comment: a fresh launch's only way to learn a thread
        // id (waiting for its `thread/started` broadcast) races a live-confirmed server rule that
        // rejects `thread/resume` for a thread with no completed turns yet — out of scope for
        // this pass. A missing thread id must short-circuit before even the executable is
        // touched, regardless of whether the feature is enabled.
        let dir = tempfile::tempdir().expect("tempdir");
        let result = EventDrivenRuntime::start(
            Path::new("/definitely/not/codex"),
            dir.path(),
            dir.path(),
            None,
            true,
        );
        assert!(result.is_none());
    }
}
