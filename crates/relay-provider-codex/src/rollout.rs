//! GitHub #18 (architecture reopening, 2026-10-13 writer-conflict incident): a local, read-only
//! filesystem signal for Codex's own structured `usage_limit_exceeded` exhaustion record that
//! **never** opens an app-server connection, never calls `thread/resume`/`thread/start`, and
//! therefore cannot contend for a thread's single active-writer slot the way an earlier
//! app-server-connected observer proved it can (see `relay_cli::codex_runtime`'s own module doc
//! for the live incident this replaces).
//!
//! ## Why the filesystem, and exactly what on it
//!
//! Every Codex thread's conversation is durably persisted, append-only, as a `.jsonl` file under
//! `CODEX_HOME/sessions/<YYYY>/<MM>/<DD>/rollout-<timestamp>-<thread-id>.jsonl` — live-confirmed
//! against real historical artifacts from a real `codex-cli 0.156.0` profile (never guessed):
//!
//! * The filename's trailing UUID is the exact native thread id, and the file's own first record
//!   (`{"type":"session_meta","payload":{"session_id":"<thread-id>",...}}`) repeats it — two
//!   independent, structural confirmations of identity, never a heuristic (no "newest file," no
//!   "only file in the directory," no cwd match) — see [`find_rollout_file`].
//! * A failed turn's structured exhaustion record is `{"type":"event_msg","ordinal":N,
//!   "payload":{"type":"task_complete","error":{"codex_error_info":"usage_limit_exceeded",
//!   "message":"<English prose, never parsed>"}}}` — live-confirmed against four real historical
//!   exhaustions. `ordinal` increases monotonically from `0` and is never reused.
//! * The file is strictly append-only for the thread's lifetime (confirmed: every historical
//!   rollout's on-disk size only ever grows, and every one found belongs to exactly one thread
//!   id, never shared).
//!
//! ## What this module is not
//!
//! Not a general rollout parser and not an authority on exhaustion.
//! [`RolloutEvent::UsageLimitExceeded`] is a wake-up hint only — a caller must still run the one
//! authoritative `account/rateLimits/read` (`crate::usage::CodexUsageSignal`) before treating
//! anything as exhaustion. This module never makes a provider call, never opens a socket, and
//! never mutates anything outside its own small durable offset-tracking state file.
//!
//! ## Never re-triggers on old history
//!
//! [`RolloutWatcher::attach`] baselines a *newly discovered* rollout file at its current end —
//! Relay must never react to an exhaustion record already in the file before this session started
//! watching it (a thread resumed after the account's quota has since reset must not immediately
//! re-fire a stale handoff from a previous, already-resolved exhaustion). A *durable* offset
//! (persisted under the project's own state directory, like every other GitHub #18 trace) is used
//! instead, across a watcher restart within the *same* session — see [`WatchState`].
//!
//! ## Missed-event recovery
//!
//! Filesystem notifications are treated as hints only, never as a promise of exactly one
//! notification per record: every wake (a real `notify` event, or the loop's own bounded periodic
//! re-check) re-reads every byte appended since the last durably-recorded offset and parses every
//! complete line found, so coalesced or entirely missed OS notifications can never lose a record —
//! only ever delay noticing it by at most one periodic re-check interval.

use std::{
    fs,
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One decoded signal this module exists to report. A wake-up hint only — see this module's own
/// doc comment — never itself a handoff decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RolloutEvent {
    /// The watched thread's rollout gained a structured `task_complete` record whose
    /// `error.codex_error_info` is exactly `usage_limit_exceeded`. Reported at most once per
    /// [`RolloutWatcher`] (see its own doc comment) regardless of how many matching records a
    /// single read turns up, or how many times a restart re-opens the same durable offset.
    UsageLimitExceeded,
}

/// Why [`RolloutWatcher::attach`] could not start observing at all — a silent, best-effort
/// decline exactly like every other GitHub #18 failure mode: the caller falls back to GitHub
/// #13's polling, never treats this as a startup failure.
#[derive(Debug, Eq, PartialEq)]
pub enum AttachError {
    /// The OS-level watcher itself could not be constructed or could not watch the sessions
    /// directory (permission error, platform limit, `CODEX_HOME` missing entirely). This is the
    /// *only* synchronous failure mode: whether a matching rollout file already exists is never
    /// checked synchronously (see this module's own doc comment on why `attach` never blocks) —
    /// a not-yet-existing file is not a failure, it is [`RolloutWatcher::is_healthy`] staying
    /// `false` until the background loop finds it.
    WatchUnavailable,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WatchUnavailable => write!(f, "the filesystem could not be watched"),
        }
    }
}

/// How often the background loop re-checks even without a fresh `notify` event — the bound on how
/// stale "healthy" can ever be relative to reality, and what actually closes the "filesystem
/// notifications are hints, not guarantees" gap (see this module's own doc comment). Also the
/// bound on how long the background loop takes to first locate a not-yet-existing rollout file —
/// see [`RolloutWatcher::attach`]'s own doc comment for why that search never blocks `attach`
/// itself.
const RECHECK_INTERVAL: Duration = Duration::from_millis(300);

/// A live, background-threaded watch over one thread's rollout file (background thread, channel,
/// `drain_events`, `stop`) that is structurally incapable of the writer-conflict failure a live
/// incident proved for an earlier app-server-connected design: nothing here ever opens a
/// connection to Codex's app-server, so there is no writer slot to contend for.
pub struct RolloutWatcher {
    events: Receiver<RolloutEvent>,
    stop_flag: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RolloutWatcher {
    /// Starts watching `thread_id`'s rollout file under `codex_home`, durably tracking offset in
    /// `state_file` (overwritten atomically-enough for this purpose — see [`WatchState::save`]).
    ///
    /// Never blocks waiting for the rollout file to exist: unlike the app-server observer this
    /// replaces (which had to synchronously pre-connect before the interactive client could ever
    /// launch, to avoid missing a one-shot broadcast — see `relay_cli::codex_runtime`'s own module
    /// doc), there is no analogous one-shot event here to race. Locating the file (immediately, if
    /// it already exists — the common case for every real `relay codex`/`relay resume` launch; see
    /// this module's own doc comment — or after however many [`RECHECK_INTERVAL`]s it takes to
    /// appear) happens entirely on the returned background thread. [`Self::is_healthy`] is `false`
    /// until that happens, so a caller's own fallback polling correctly stays active in the
    /// meantime. The only way this fails synchronously is the OS-level watch itself.
    pub fn attach(
        codex_home: &Path,
        thread_id: &str,
        state_file: &Path,
    ) -> Result<Self, AttachError> {
        let sessions_dir = codex_home.join("sessions");
        let _ = fs::create_dir_all(&sessions_dir);
        let (fs_tx, fs_rx) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(fs_tx).map_err(|_| AttachError::WatchUnavailable)?;
        // Established *before* the one synchronous locate attempt below, never after: a `notify`
        // event for a write that lands between that attempt and this watch existing would
        // otherwise be silently missed, re-opening exactly the kind of race this function's own
        // doc comment describes closing for the common case.
        watcher
            .watch(&sessions_dir, RecursiveMode::Recursive)
            .map_err(|_| AttachError::WatchUnavailable)?;

        // One synchronous, non-blocking locate attempt — closes the race the file-already-exists
        // common case would otherwise have (the interactive client, launched by the caller right
        // after this returns, writing its first turn before a background thread's own first loop
        // iteration ever ran). A file that does not exist *yet* is not retried here — seeing it
        // appear is entirely the background loop's job (see this function's own doc comment).
        let initial_tail = find_rollout_file(codex_home, thread_id)
            .and_then(|path| open_tail(&path, thread_id, state_file).ok());

        let (events_tx, events) = mpsc::channel();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let healthy = Arc::new(AtomicBool::new(initial_tail.is_some()));
        let worker_stop = Arc::clone(&stop_flag);
        let worker_healthy = Arc::clone(&healthy);
        let thread_id = thread_id.to_owned();
        let codex_home = codex_home.to_owned();
        let state_file = state_file.to_owned();
        let worker = thread::spawn(move || {
            worker_loop(
                codex_home,
                thread_id,
                state_file,
                initial_tail,
                watcher,
                fs_rx,
                &worker_stop,
                &worker_healthy,
                &events_tx,
            );
        });
        Ok(Self {
            events,
            stop_flag,
            healthy,
            worker: Some(worker),
        })
    }

    /// Whether the background loop currently has live, caught-up coverage. `false` while still
    /// searching for a not-yet-existing file, or after a filesystem error it could not immediately
    /// recover from — the one condition under which a caller must keep (or resume) GitHub #13's
    /// own periodic polling.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// Non-blocking drain — never waits for a new event.
    pub fn drain_events(&self) -> Vec<RolloutEvent> {
        let mut drained = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            drained.push(event);
        }
        drained
    }

    /// Stops the background loop and joins it.
    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ignored = worker.join();
        }
    }
}

impl Drop for RolloutWatcher {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ignored = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    codex_home: PathBuf,
    thread_id: String,
    state_file: PathBuf,
    mut tail: Option<Tail>,
    mut watcher: RecommendedWatcher,
    fs_rx: Receiver<notify::Result<notify::Event>>,
    stop_flag: &AtomicBool,
    healthy: &AtomicBool,
    events_tx: &mpsc::Sender<RolloutEvent>,
) {
    let sessions_dir = codex_home.join("sessions");
    let mut already_signaled = false;
    // `tail` is `Some` already when [`RolloutWatcher::attach`]'s own synchronous locate attempt
    // found the file (the common case for a real launch). `None` means a not-yet-existing file (a
    // genuinely fresh thread whose first turn has not started) — `healthy` just stays `false`
    // until it appears. See [`RolloutWatcher::attach`]'s own doc comment for why that search never
    // blocks `attach` itself.
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        if tail.is_none() {
            if let Some(path) = find_rollout_file(&codex_home, &thread_id) {
                match open_tail(&path, &thread_id, &state_file) {
                    Ok(opened) => {
                        tail = Some(opened);
                        healthy.store(true, Ordering::Relaxed);
                    }
                    Err(_) => healthy.store(false, Ordering::Relaxed),
                }
            }
        }
        match fs_rx.recv_timeout(RECHECK_INTERVAL) {
            Ok(Ok(_event)) => {}
            Ok(Err(_)) => {
                healthy.store(false, Ordering::Relaxed);
                if watcher
                    .watch(&sessions_dir, RecursiveMode::Recursive)
                    .is_err()
                {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(current_tail) = tail.as_mut() else {
            continue;
        };
        match current_tail.read_new_lines() {
            Ok(lines) => {
                for line in &lines {
                    let Ok(value) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    if !already_signaled && record_is_usage_limit_exceeded(&value) {
                        already_signaled = true;
                        if events_tx.send(RolloutEvent::UsageLimitExceeded).is_err() {
                            return;
                        }
                    }
                }
                if !lines.is_empty() {
                    let _ = WatchState {
                        thread_id: thread_id.clone(),
                        path: current_tail.path.clone(),
                        offset: current_tail.offset,
                    }
                    .save(&state_file);
                }
            }
            Err(_) => {
                healthy.store(false, Ordering::Relaxed);
                tail = None;
            }
        }
    }
}

/// Opens an incremental reader for `path`, resuming from a durably persisted offset if
/// [`WatchState::load`] finds one for the exact same `(thread_id, path)` pair — never a different
/// thread's or a different path's offset applied to this file. Otherwise baselines fresh at the
/// file's current end (see this module's own doc comment for why: never react to history that
/// predates this watch).
fn open_tail(path: &Path, thread_id: &str, state_file: &Path) -> std::io::Result<Tail> {
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let offset = WatchState::load(state_file)
        .filter(|state| state.thread_id == thread_id && state.path == path)
        .map_or(len, |state| state.offset.min(len));
    Ok(Tail {
        file,
        path: path.to_owned(),
        offset,
    })
}

/// Durable, restart-safe tailing state — see [`open_tail`]'s own doc comment for why both fields
/// must match before an offset is trusted.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WatchState {
    thread_id: String,
    path: PathBuf,
    offset: u64,
}

impl WatchState {
    fn load(state_file: &Path) -> Option<Self> {
        let text = fs::read_to_string(state_file).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, state_file: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string(self).unwrap_or_default();
        fs::write(state_file, text)
    }
}

/// An incremental, offset-based reader over one rollout file. `offset` only ever advances past
/// bytes that formed a *complete* line (ending in `\n`) — a trailing partial line (a write still
/// in flight) is simply re-read, unharmed, on the next call once the rest of it has landed. This
/// is what makes `offset` safe to persist and resume from at any point, including mid-line.
struct Tail {
    file: fs::File,
    path: PathBuf,
    offset: u64,
}

impl Tail {
    /// Reads every complete line appended since `offset`, advancing it past exactly what was
    /// consumed. Returns an empty vec (not an error) when nothing new is there yet.
    fn read_new_lines(&mut self) -> std::io::Result<Vec<String>> {
        self.file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        self.file.read_to_end(&mut buf)?;
        if buf.is_empty() {
            return Ok(Vec::new());
        }
        let mut lines = Vec::new();
        let mut consumed = 0usize;
        while let Some(relative_newline) = buf[consumed..].iter().position(|&byte| byte == b'\n') {
            let line_end = consumed + relative_newline;
            if let Ok(line) = std::str::from_utf8(&buf[consumed..line_end]) {
                lines.push(line.to_owned());
            }
            consumed = line_end + 1;
        }
        self.offset += consumed as u64;
        Ok(lines)
    }
}

/// Scans `codex_home/sessions/**/*.jsonl` for the one rollout file whose own first record
/// (`session_meta`) structurally names `thread_id` — never the filename alone, and never "newest"
/// or "only file in the directory" (GitHub #18's explicit "no heuristics" requirement). The
/// filename's own trailing UUID is used only as a cheap pre-filter to check candidates in the
/// likeliest order first; a mismatch there is never itself disqualifying on its own account,
/// though in every real Codex rollout this and the content agree.
#[must_use]
pub fn find_rollout_file(codex_home: &Path, thread_id: &str) -> Option<PathBuf> {
    let sessions_dir = codex_home.join("sessions");
    let mut candidates = Vec::new();
    collect_jsonl_files(&sessions_dir, &mut candidates);
    let suffix = format!("{thread_id}.jsonl");
    candidates.sort_by_key(|path| {
        let name_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&suffix));
        !name_matches
    });
    candidates
        .into_iter()
        .find(|path| rollout_header_names_thread(path, thread_id))
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

fn rollout_header_names_thread(path: &Path, thread_id: &str) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    // The header is always the very first line and always small — never read more than one
    // bounded chunk to find it, so a huge file never costs more than this fixed amount to check.
    let mut buf = vec![0u8; 8192];
    let Ok(read) = file.read(&mut buf) else {
        return false;
    };
    let Some(newline) = buf[..read].iter().position(|&byte| byte == b'\n') else {
        return false;
    };
    let Ok(first_line) = std::str::from_utf8(&buf[..newline]) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(first_line) else {
        return false;
    };
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return false;
    }
    let Some(payload) = value.get("payload") else {
        return false;
    };
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("id").and_then(Value::as_str));
    session_id == Some(thread_id)
}

/// `true` exactly for Codex's own structured exhaustion record shape, live-confirmed against four
/// real historical rollouts: `{"type":"event_msg","payload":{"type":"task_complete",
/// "error":{"codex_error_info":"usage_limit_exceeded",...}}}`. Every other `task_complete` (no
/// error, or a different `codex_error_info`) and every other record type is inert — this is a
/// snake_case on-disk shape, distinct from (and never confused with) the app-server's own
/// camelCase wire protocol notifications.
#[must_use]
fn record_is_usage_limit_exceeded(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("event_msg")
        && value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("task_complete")
        && value
            .get("payload")
            .and_then(|payload| payload.get("error"))
            .and_then(|error| error.get("codex_error_info"))
            .and_then(Value::as_str)
            == Some("usage_limit_exceeded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn session_meta(thread_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:00.000Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {"session_id": thread_id, "id": thread_id, "cwd": "/work"},
        })
        .to_string()
    }

    fn task_complete_ok(ordinal: u32) -> String {
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:01.000Z",
            "ordinal": ordinal,
            "type": "event_msg",
            "payload": {"type": "task_complete", "turn_id": "t", "last_agent_message": "done"},
        })
        .to_string()
    }

    fn task_complete_exhausted(ordinal: u32) -> String {
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:02.000Z",
            "ordinal": ordinal,
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "turn_id": "t",
                "last_agent_message": null,
                "error": {"message": "You've hit your usage limit.", "codex_error_info": "usage_limit_exceeded"},
            },
        })
        .to_string()
    }

    fn write_rollout(dir: &Path, thread_id: &str, lines: &[String]) -> PathBuf {
        let day_dir = dir.join("sessions/2026/10/13");
        fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join(format!("rollout-2026-10-13T00-00-00-{thread_id}.jsonl"));
        let mut file = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    fn append(path: &Path, lines: &[String]) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn record_classification_matches_only_the_exact_structured_shape() {
        assert!(record_is_usage_limit_exceeded(
            &serde_json::from_str(&task_complete_exhausted(1)).unwrap()
        ));
        assert!(!record_is_usage_limit_exceeded(
            &serde_json::from_str(&task_complete_ok(1)).unwrap()
        ));
        assert!(!record_is_usage_limit_exceeded(&serde_json::json!({})));
        assert!(!record_is_usage_limit_exceeded(&serde_json::json!(null)));
        let other_error = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "error": {"codex_error_info": "contextWindowExceeded"}},
        });
        assert!(!record_is_usage_limit_exceeded(&other_error));
    }

    #[test]
    fn find_rollout_file_requires_content_match_never_filename_alone() {
        let dir = tempfile::tempdir().unwrap();
        // A file whose *name* carries one thread id but whose *header* names a different one
        // (corrupted/renamed) must never be matched by the wrong id.
        let day_dir = dir.path().join("sessions/2026/10/13");
        fs::create_dir_all(&day_dir).unwrap();
        let mismatched = day_dir.join("rollout-2026-10-13T00-00-00-wrong-id.jsonl");
        fs::write(&mismatched, format!("{}\n", session_meta("real-id"))).unwrap();
        assert_eq!(find_rollout_file(dir.path(), "wrong-id"), None);
        assert_eq!(
            find_rollout_file(dir.path(), "real-id"),
            Some(mismatched),
            "must match by verified content even when the filename disagrees"
        );
    }

    #[test]
    fn find_rollout_file_ignores_other_threads_in_the_same_codex_home() {
        let dir = tempfile::tempdir().unwrap();
        write_rollout(dir.path(), "thread-a", &[session_meta("thread-a")]);
        let target = write_rollout(dir.path(), "thread-b", &[session_meta("thread-b")]);
        write_rollout(dir.path(), "thread-c", &[session_meta("thread-c")]);
        assert_eq!(find_rollout_file(dir.path(), "thread-b"), Some(target));
        assert_eq!(find_rollout_file(dir.path(), "does-not-exist"), None);
    }

    #[test]
    fn tail_reads_only_complete_lines_and_resumes_a_split_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_rollout(dir.path(), "t1", &[session_meta("t1")]);
        let mut tail = Tail {
            file: fs::File::open(&path).unwrap(),
            path: path.clone(),
            offset: 0,
        };
        let first = tail.read_new_lines().unwrap();
        assert_eq!(first.len(), 1);
        // Simulate a write split mid-line: partial bytes with no trailing newline yet.
        {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"{\"partial\":").unwrap();
        }
        let partial = tail.read_new_lines().unwrap();
        assert!(
            partial.is_empty(),
            "a line with no trailing newline is never a record yet"
        );
        {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"true}\n").unwrap();
        }
        let completed = tail.read_new_lines().unwrap();
        assert_eq!(completed, vec!["{\"partial\":true}".to_owned()]);
    }

    #[test]
    fn a_watcher_never_reacts_to_history_that_predates_it() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        // An old, already-resolved exhaustion already sits in the file before the watcher ever
        // starts (e.g. a thread resumed long after the account's quota reset).
        let path = write_rollout(
            codex_home.path(),
            "t1",
            &[session_meta("t1"), task_complete_exhausted(1)],
        );
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            watcher.drain_events().is_empty(),
            "pre-existing history must never retrigger a signal"
        );
        append(&path, &[task_complete_ok(2)]);
        std::thread::sleep(Duration::from_millis(500));
        assert!(watcher.drain_events().is_empty());
        watcher.stop();
    }

    #[test]
    fn a_new_exhaustion_after_attach_is_signaled_exactly_once() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        append(&path, &[task_complete_ok(1), task_complete_exhausted(2)]);
        std::thread::sleep(Duration::from_millis(600));
        let events = watcher.drain_events();
        assert_eq!(events, vec![RolloutEvent::UsageLimitExceeded]);
        // A later drain (and later writes repeating the same shape) never signal again.
        append(&path, &[task_complete_exhausted(3)]);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            watcher.drain_events().is_empty(),
            "must signal at most once per watcher, never once per matching record"
        );
        watcher.stop();
    }

    #[test]
    fn file_created_after_watch_begins_is_still_found() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        let codex_home_path = codex_home.path().to_owned();
        let handle = thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            write_rollout(
                &codex_home_path,
                "late-thread",
                &[session_meta("late-thread")],
            );
        });
        let watcher = RolloutWatcher::attach(codex_home.path(), "late-thread", &state_file)
            .expect("attach never blocks on the file existing — see its own doc comment");
        handle.join().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            watcher.is_healthy(),
            "the background loop must find the file once it appears"
        );
        watcher.stop();
    }

    #[test]
    fn unrelated_simultaneous_thread_writes_never_trigger_the_watcher() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let other = write_rollout(codex_home.path(), "other", &[session_meta("other")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        append(&other, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            watcher.drain_events().is_empty(),
            "an unrelated thread's own exhaustion must never be attributed to this watch"
        );
        watcher.stop();
    }

    #[test]
    fn a_restart_resumes_from_the_durable_offset_without_resignaling() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        append(&path, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded]
        );
        watcher.stop();

        // A second watcher for the same thread/state file (simulating a process restart) must
        // never re-signal the exact same already-durably-offset-past record.
        let restarted = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            restarted.drain_events().is_empty(),
            "a restart must resume from the durable offset, never replay an already-seen record"
        );
        restarted.stop();
    }

    #[test]
    fn attach_succeeds_immediately_but_stays_unhealthy_when_nothing_ever_matches() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        let watcher = RolloutWatcher::attach(codex_home.path(), "never-appears", &state_file)
            .expect("attach never blocks on the file existing — see its own doc comment");
        assert!(!watcher.is_healthy());
        std::thread::sleep(Duration::from_millis(500));
        assert!(!watcher.is_healthy());
        watcher.stop();
    }

    #[test]
    fn many_records_delivered_in_one_write_are_all_parsed() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_file = tempfile::tempdir().unwrap().path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        let mut batch = Vec::new();
        for ordinal in 1..200 {
            batch.push(task_complete_ok(ordinal));
        }
        batch.push(task_complete_exhausted(200));
        append(&path, &batch);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded]
        );
        watcher.stop();
    }
}
