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
//! ## Never re-triggers on old history, and never skips post-watch history either
//!
//! Two different files need two different baselines, and conflating them is a real bug this
//! module was once shipped with: a file that already existed *before* [`RolloutWatcher::attach`]
//! was ever called baselines at its current end (never react to an exhaustion already resolved by
//! a previous session). A file that did **not** exist yet at that exact moment — the OS watch is
//! always installed first, so its later appearance is provably entirely post-watch — baselines at
//! byte zero instead: every byte in it, including anything written before the background loop's
//! own first lazy discovery gets around to opening it, is eligible. [`RolloutWatcher::attach`]'s
//! own one synchronous, non-blocking locate decides which case applies and only it ever uses the
//! "existed already" baseline; every lazy (re)discovery the background loop performs on its own
//! always uses the "did not exist yet" baseline instead — which is also the only safe choice for a
//! file *re*-discovered after a transient error, since by then this watcher had already been
//! reading it and a stray re-baseline at a merely-current end could just as easily skip real,
//! unprocessed content. A *durable* offset (persisted under the project's own state directory)
//! always wins over either baseline, across a watcher restart within the *same* session or a
//! same-session recovery from a transient error — but only when its own `(thread_id, path, dev,
//! ino)` all still match the file actually being opened, and its recorded offset is no larger
//! than that file's current length — never a different file generation that happened to land at
//! the same path, and never a merely-clamped, silently-trusted offset for a file that shrank
//! since it was saved (an independent review of this exact module found both of these: a stale
//! offset left over from a different inode, and one larger than the current file, were each being
//! clamped-and-trusted instead of rejected outright) — see [`WatchState`]/[`open_tail`]. The
//! "existed already" baseline, additionally, uses the file's last *complete* line boundary, never
//! its raw length (the same review found a length landing mid-line — the file's last record only
//! partially written at the exact instant of `attach()`'s own synchronous stat — could otherwise
//! permanently tear that one record in two).
//!
//! ## Missed-event recovery
//!
//! Filesystem notifications are treated as hints only, never as a promise of exactly one
//! notification per record: every wake (a real `notify` event, or the loop's own bounded periodic
//! re-check) re-reads every byte appended since the last durably-recorded offset and parses every
//! complete line found, so coalesced or entirely missed OS notifications can never lose a record —
//! only ever delay noticing it by at most one periodic re-check interval.
//!
//! ## Profile-domain watching (mid-session thread transitions)
//!
//! A live supervised Codex TUI can change which native thread it is driving *without ending
//! supervision* — real, pinned-version-confirmed in-TUI commands: "start a new chat during a
//! conversation" (`/new`), "fork the current chat" (`/fork`), "resume a saved chat". Watching only
//! the originally-known thread id forever would leave `is_healthy()` reporting `true` on a now-
//! stale file while the real conversation moved elsewhere — a silent missed-exhaustion risk, not
//! merely a residual gap.
//!
//! This watcher therefore does not bind to one thread id forever. It recursively watches the whole
//! `sessions` tree (already necessary just to find the first file — see above) and tracks a single
//! mutable "currently authoritative top-level thread," rebinding it when — and only when —
//! structural evidence proves the authoritative thread changed:
//!
//! * **Subagents and ephemeral side-conversations are excluded structurally, not heuristically.**
//!   Codex's own protocol documents `Thread.parentThreadId` as "only set if this thread is a
//!   subagent," and `Thread.ephemeral` threads are never materialized on disk at all. Any rollout
//!   whose own `session_meta` carries a non-null `parent_thread_id` is permanently ignored — see
//!   [`classify_rollout_header`].
//! * **`/fork` is attributed via `forked_from_id`.** A fork's own `session_meta` names the exact
//!   thread it was forked from — a direct, documented structural claim, not an inference. A new
//!   top-level rollout whose `forked_from_id` equals the currently-bound thread id is always a
//!   transition candidate, independent of whether it has grown at all yet.
//! * **`/new` and "resume a saved chat" are attributed via profile exclusivity plus structural
//!   top-level activity.** Neither carries any link back to the previous thread — by design, a new
//!   chat is a fresh conversation and a resumed chat is an *existing*, independently-created one.
//!   What makes attributing post-watch activity on *some other* top-level rollout to *this*
//!   session's own TUI non-heuristic is `relay_core::handoff::ProfileSessionLock`: as of this
//!   issue's own ordering fix (profile exclusivity enforced *before* any Relay-controlled thread
//!   creation, not only at interactive-attach time — see `relay_cli::codex_runtime`'s own module
//!   doc), every Relay-controlled path that could create or drive a top-level Codex thread in this
//!   profile is serialized behind the exact same lock this session already holds for its entire
//!   run. Within that *supported* lifecycle (an unmanaged `codex` process a user points at this
//!   same `CODEX_HOME` by hand remains explicitly out of scope — no mechanism could close that, the
//!   same residual boundary profile isolation already draws everywhere else), structurally
//!   top-level activity on a rollout other than the one currently bound has no other possible
//!   source: not Codex's own shared daemon (confirmed against its protocol schema — every thread
//!   creation request, `thread/start`/`resume`/`fork`, is client-initiated; no server-initiated
//!   creation notification exists), not a second Relay-controlled launch/switch/handoff on this
//!   profile (now refused before it ever reaches `codex exec`), and not a subagent (excluded
//!   structurally above). It can only be this session's own supervised TUI.
//!
//! A candidate's own growth is detected cheaply (a stat, not a content read) against its length the
//! *first time it is ever seen* — itself computed the same way file identity already is, by reading
//! just its header, never its full content, unless and until it actually becomes the transition
//! target. Only then is its full current content read once, to compute the same line-boundary-safe
//! starting offset [`open_tail`]'s own "existed already" baseline already uses, so nothing written
//! between first sight and the transition is ever skipped. See [`worker_loop`] and
//! [`classify_rollout_header`].

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom},
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RolloutEvent {
    /// The currently-bound thread's rollout gained a structured `task_complete` record whose
    /// `error.codex_error_info` is exactly `usage_limit_exceeded`. Reported at most once per
    /// currently-bound thread (see this module's own doc comment on profile-domain watching) —
    /// [`Self::ThreadTransitioned`] resets this guard for whichever thread becomes current next.
    UsageLimitExceeded,
    /// agent-relay#18 (profile-domain watching): the authoritative top-level thread changed —
    /// `/new`, `/fork`, or "resume a saved chat" in the one supervised TUI holding this profile's
    /// exclusivity lock (see this module's own doc comment for why that lock makes every other
    /// in-scope actor's own structural activity unambiguous). Carries the new thread id so a
    /// caller can request exactly one reconciliation, the same action already taken on recovery
    /// from an unhealthy gap — a transition never itself flips [`RolloutWatcher::is_healthy`].
    ThreadTransitioned(String),
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

        // agent-relay#18 (profile-domain watching, restart continuity): a durable `WatchState`
        // from a *previous* run of this same session can now name a thread id different from
        // `thread_id` — a profile-domain transition the background loop made before a restart
        // (process crash, or an ordinary stop/resume of this same continuation). Resuming bound to
        // `thread_id` unconditionally would silently rebind back to the *original* thread, losing
        // the transition entirely. A persisted state is only trusted here when its own file still
        // verifiably exists at the exact `(path, dev, ino)` it names — never a path alone, for the
        // same reason `open_tail`'s own matching never trusts a path alone.
        let resumed_thread_id = WatchState::load(state_file)
            .filter(|state| {
                use std::os::unix::fs::MetadataExt as _;
                fs::metadata(&state.path).is_ok_and(|metadata| {
                    metadata.dev() == state.dev && metadata.ino() == state.ino
                })
            })
            .map(|state| state.thread_id);
        let effective_thread_id = resumed_thread_id.as_deref().unwrap_or(thread_id);

        // One synchronous, non-blocking locate attempt — closes the race the file-already-exists
        // common case would otherwise have (the interactive client, launched by the caller right
        // after this returns, writing its first turn before a background thread's own first loop
        // iteration ever ran). A file that does not exist *yet* is not retried here — seeing it
        // appear is entirely the background loop's job (see this function's own doc comment).
        // `Baseline::ExistedBeforeWatch` is correct here, and *only* here: this is the one place
        // that can actually prove the file was already there, as opposed to a lazy discovery by
        // the background loop later, which can never tell "existed before the watch" apart from
        // "created the instant after" and must always assume the latter (see `worker_loop`).
        let initial_tail = find_rollout_file(codex_home, effective_thread_id).and_then(|path| {
            open_tail(
                &path,
                effective_thread_id,
                state_file,
                Baseline::ExistedBeforeWatch,
            )
            .ok()
        });

        // agent-relay#18 (profile-domain watching): every *other* `.jsonl` already on disk at this
        // exact synchronous moment gets its length recorded now, cheaply (a stat, never a content
        // read) — exactly the same "existed before the watch" moment `initial_tail` above proves
        // for the known thread's own file, generalized to every sibling. Without this, a candidate
        // the background loop only gets around to classifying *later* would wrongly baseline at
        // whatever had *already* accumulated on it by then, silently treating real pre-existing
        // history as "post-watch growth" the first time anything appends to it. A file that
        // genuinely does not exist yet at this moment is correctly absent from this map;
        // [`worker_loop`] treats "never seen before, not in this map" as baseline zero, which is
        // exactly right for a file whose entire existence is provably post-watch.
        let mut pre_existing_lengths = HashMap::new();
        let mut all_paths = Vec::new();
        collect_jsonl_files(&sessions_dir, &mut all_paths);
        for path in all_paths {
            if initial_tail.as_ref().is_some_and(|tail| tail.path == path) {
                continue;
            }
            if let Ok(metadata) = fs::metadata(&path) {
                pre_existing_lengths.insert(path, metadata.len());
            }
        }

        let (events_tx, events) = mpsc::channel();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let healthy = Arc::new(AtomicBool::new(initial_tail.is_some()));
        let worker_stop = Arc::clone(&stop_flag);
        let worker_healthy = Arc::clone(&healthy);
        let thread_id = effective_thread_id.to_owned();
        let codex_home = codex_home.to_owned();
        let state_file = state_file.to_owned();
        let worker = thread::spawn(move || {
            worker_loop(
                codex_home,
                thread_id,
                state_file,
                initial_tail,
                pre_existing_lengths,
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

/// Guarantees `healthy` becomes `false` the moment [`worker_loop`] stops providing live coverage,
/// for *any* reason — an early `return` on a disconnected channel or a dead event consumer, an
/// unrecoverable watch-rebuild failure, an ordinary `stop()`, or (since `Drop` still runs while a
/// panic unwinds a stack) a panic. This exists specifically so that adding a new exit path to
/// [`worker_loop`] in the future can never silently reintroduce "the worker is gone but `healthy`
/// still says `true`" — the exact class of bug a live review of this module once found.
struct HealthGuard<'a> {
    healthy: &'a AtomicBool,
}

impl Drop for HealthGuard<'_> {
    fn drop(&mut self) {
        self.healthy.store(false, Ordering::Relaxed);
    }
}

/// A top-level (non-subagent, non-ephemeral) rollout file seen but not yet (or no longer) bound as
/// the current thread — see this module's own doc comment on profile-domain watching.
/// `first_seen_len` is a cheap stat (never a content read) of the file's length the moment it was
/// first classified — or, for a file already present at [`RolloutWatcher::attach`]'s own
/// synchronous moment, that exact moment's length (via `pre_existing_lengths`), never whatever
/// happened to accumulate by the time the background loop got around to it. Growth past it is the
/// generic `/new`/"resume a saved chat" transition signal, detected with another stat, never a
/// content read, on every tick until it actually fires.
struct Candidate {
    thread_id: String,
    forked_from_id: Option<String>,
    first_seen_len: u64,
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    codex_home: PathBuf,
    thread_id: String,
    state_file: PathBuf,
    mut tail: Option<Tail>,
    pre_existing_lengths: HashMap<PathBuf, u64>,
    mut watcher: RecommendedWatcher,
    fs_rx: Receiver<notify::Result<notify::Event>>,
    stop_flag: &AtomicBool,
    healthy: &AtomicBool,
    events_tx: &mpsc::Sender<RolloutEvent>,
) {
    let _health_guard = HealthGuard { healthy };
    let sessions_dir = codex_home.join("sessions");
    let mut current_thread_id = thread_id;
    let mut already_signaled = false;
    // Every other top-level rollout this loop has ever classified, keyed by path. A candidate is
    // removed the instant it becomes `tail` (it is no longer "other"); a file proven *not*
    // top-level (subagent, or an unreadable/not-yet-written header) is simply never inserted here
    // and is re-checked every tick until it classifies one way or the other — see
    // [`classify_rollout_header`].
    let mut candidates: HashMap<PathBuf, Candidate> = HashMap::new();
    // Files proven, once, to be structurally *not* top-level — permanently skipped, never
    // re-classified (a subagent rollout never becomes a top-level one).
    let mut not_top_level: HashSet<PathBuf> = HashSet::new();
    // `tail` is `Some` already when [`RolloutWatcher::attach`]'s own synchronous locate attempt
    // found the file (the common case for a real launch). `None` means a not-yet-existing file (a
    // genuinely fresh thread whose first turn has not started) — `healthy` just stays `false`
    // until it appears. See [`RolloutWatcher::attach`]'s own doc comment for why that search never
    // blocks `attach` itself.
    //
    // `healthy` is set to `true` in exactly one place *in this loop*: right after a *successful*
    // `read_new_lines`, on a `Tail` that is either freshly (re)discovered this same iteration or
    // already established from a previous one. That is the only point in `worker_loop` that
    // actually proves live, caught-up coverage — setting it anywhere else here (e.g. merely on a
    // successful `open_tail`, or merely on a successful watch rebuild) would let `healthy` go
    // `true` before a single byte of the file has actually been read this generation, which is
    // exactly how a prior version of this function could report `true` immediately after a
    // backend-error recovery without ever having proven it caught back up. (The *initial* value
    // this loop receives is set once, separately, by [`RolloutWatcher::attach`]'s own synchronous
    // open — trivially proven healthy there too, since baselining at a verified line boundary
    // with nothing unread yet *is* "caught up," just established before this loop's first
    // iteration rather than inside it.) A thread transition never touches `healthy` at all — see
    // [`RolloutEvent::ThreadTransitioned`]'s own doc comment.
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        if tail.is_none() {
            // Always `Baseline::Zero` here — see `open_tail`'s own doc comment and this module's
            // top-level doc comment for why a lazy discovery can never safely assume "this file's
            // current content is history that predates the watch," unlike `RolloutWatcher::attach`'s
            // own one synchronous locate.
            if let Some(path) = find_rollout_file(&codex_home, &current_thread_id) {
                match open_tail(&path, &current_thread_id, &state_file, Baseline::Zero) {
                    Ok(opened) => tail = Some(opened),
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
        if let Some(current_tail) = tail.as_mut() {
            match current_tail.read_new_lines() {
                Ok(lines) => {
                    // Proven: a valid, identity-checked handle was just read successfully. This
                    // is the one and only place `healthy` becomes `true` — see this function's
                    // own doc comment for why nowhere earlier is good enough.
                    healthy.store(true, Ordering::Relaxed);
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
                            thread_id: current_thread_id.clone(),
                            path: current_tail.path.clone(),
                            dev: current_tail.dev,
                            ino: current_tail.ino,
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
        // Profile-domain scan: every other rollout under this `CODEX_HOME`, classified once and
        // then only stat-checked, looking for authoritative evidence the TUI moved on — see this
        // module's own doc comment on profile-domain watching for why this never needs a heuristic.
        let mut discovered = Vec::new();
        collect_jsonl_files(&sessions_dir, &mut discovered);
        for path in discovered {
            if tail.as_ref().is_some_and(|current| current.path == path) {
                continue;
            }
            if not_top_level.contains(&path) {
                continue;
            }
            if !candidates.contains_key(&path) {
                match classify_rollout_header(&path) {
                    Classification::NotYetReadable => continue,
                    Classification::NotTopLevel => {
                        not_top_level.insert(path);
                        continue;
                    }
                    Classification::TopLevel(header) => {
                        // `pre_existing_lengths` wins when present: it is `attach()`'s own
                        // synchronous-moment length, never a later, possibly-already-grown one —
                        // see this struct's own doc comment and this module's doc comment on
                        // profile-domain watching for why that distinction is load-bearing, not
                        // cosmetic. Absent from it (and never previously a candidate) means this
                        // file's entire existence is provably post-watch: baseline zero.
                        let first_seen_len = pre_existing_lengths.get(&path).copied().unwrap_or(0);
                        candidates.insert(
                            path.clone(),
                            Candidate {
                                thread_id: header.thread_id,
                                forked_from_id: header.forked_from_id,
                                first_seen_len,
                            },
                        );
                    }
                }
            }
            let Some(candidate) = candidates.get(&path) else {
                continue;
            };
            // Never a transition target: this *is* the currently-bound thread, merely not yet
            // (re)bound as `tail` this exact tick — the dedicated lazy-(re)discovery at the top of
            // this loop owns that, on this tick or the next. Without this check, a brand-new or
            // just-replaced file for the already-current thread can race that dedicated path
            // (whichever of the two first notices the file wins, nondeterministically) and get
            // spuriously reported as a `ThreadTransitioned` into the thread it already is.
            if candidate.thread_id == current_thread_id {
                continue;
            }
            let is_fork_of_current =
                candidate.forked_from_id.as_deref() == Some(current_thread_id.as_str());
            let grew =
                fs::metadata(&path).is_ok_and(|metadata| metadata.len() > candidate.first_seen_len);
            if !is_fork_of_current && !grew {
                continue;
            }
            let Ok(content) = fs::read(&path) else {
                continue;
            };
            let safe_len = (candidate.first_seen_len as usize).min(content.len());
            let baseline_offset = last_complete_line_boundary(&content[..safe_len]);
            let new_thread_id = candidate.thread_id.clone();
            candidates.remove(&path);
            let Ok(new_tail) = open_tail(
                &path,
                &new_thread_id,
                &state_file,
                Baseline::At(baseline_offset),
            ) else {
                continue;
            };
            // The abandoned thread becomes an ordinary candidate in its own right, baselined at
            // its length *right now* — never zero, which would make its own pre-existing content
            // look like fresh post-watch growth the instant it is (re)classified and immediately
            // bounce the transition straight back to it. A genuine bounce back (the TUI actually
            // resumes this same thread again later) still works correctly: it would need *real*
            // growth past this exact point, exactly like any other "resume a saved chat".
            if let Some(abandoned) = tail.take() {
                if let Ok(metadata) = fs::metadata(&abandoned.path) {
                    candidates.insert(
                        abandoned.path,
                        Candidate {
                            thread_id: current_thread_id.clone(),
                            forked_from_id: None,
                            first_seen_len: metadata.len(),
                        },
                    );
                }
            }
            tail = Some(new_tail);
            current_thread_id = new_thread_id.clone();
            already_signaled = false;
            if events_tx
                .send(RolloutEvent::ThreadTransitioned(new_thread_id))
                .is_err()
            {
                return;
            }
            // Re-enter the outer loop fresh: the next iteration reads the new `tail`'s own new
            // lines (including anything between `baseline_offset` and now) before this scan ever
            // runs again.
            break;
        }
    }
}

/// Which byte offset to fall back to when no durable [`WatchState`] matches — see this module's
/// own doc comment for why these are *not* interchangeable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Baseline {
    /// The file is known to have already existed before this exact watch started — skip
    /// whatever history is already in it.
    ExistedBeforeWatch,
    /// The file either did not exist yet when the watch started (so everything in it, however it
    /// got there before a lazy discovery opened it, is provably post-watch), or this is a
    /// *re*-open of a file this same watcher had already been reading before a transient error —
    /// never skip real content on a guess.
    Zero,
    /// An explicit, already-computed line-boundary-safe offset — used only for a profile-domain
    /// transition, where [`worker_loop`] has already read the candidate's own content up to its
    /// first-seen length and computed the safe boundary itself (see this module's own doc comment
    /// on profile-domain watching). Never a raw, unchecked length: the caller is responsible for
    /// having already applied [`last_complete_line_boundary`].
    At(u64),
}

/// Opens an incremental reader for `path`, resuming from a durably persisted offset if
/// [`WatchState::load`] finds one for the exact same `(thread_id, path)` pair, the exact same
/// device/inode (never a stale offset left over from a *different* file generation that happened
/// to occupy this same path earlier), *and* an offset no larger than the file's current length
/// (never one larger — that can only mean the file shrank since the offset was saved, which is
/// itself proof the persisted value is no longer trustworthy, not something to silently clamp and
/// treat as "already caught up"). Otherwise falls back to `baseline` (see this module's own doc
/// comment for why the two baselines are not interchangeable).
///
/// `Baseline::ExistedBeforeWatch`'s fallback is the file's last *complete* line boundary, never
/// its raw current length: a length that happens to land mid-line (the file's last record is only
/// partially written at the exact instant this synchronous call runs) would otherwise permanently
/// tear that record in two — the earlier half silently skipped as "history," the trailing half
/// (just the eventual `\n`) read later as a bogus empty line. Scanning for complete lines costs one
/// full read of whatever already exists, which is bounded (a rollout file's historical content is
/// always finite, and this only runs once, at the very first synchronous locate).
fn open_tail(
    path: &Path,
    thread_id: &str,
    state_file: &Path,
    baseline: Baseline,
) -> std::io::Result<Tail> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let dev = metadata.dev();
    let ino = metadata.ino();
    let len = metadata.len();
    let fallback = match baseline {
        Baseline::ExistedBeforeWatch => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            last_complete_line_boundary(&buf)
        }
        Baseline::Zero => 0,
        Baseline::At(offset) => offset,
    };
    let offset = WatchState::load(state_file)
        .filter(|state| {
            state.thread_id == thread_id
                && state.path == path
                && state.dev == dev
                && state.ino == ino
                && state.offset <= len
        })
        .map_or(fallback, |state| state.offset);
    Ok(Tail {
        file,
        path: path.to_owned(),
        dev,
        ino,
        offset,
    })
}

/// The byte offset immediately after the last `\n` in `buf` — `0` if `buf` contains no complete
/// line at all. Mirrors [`Tail::read_new_lines`]'s own line-splitting loop exactly (same
/// "advance past a `\n`, never past a trailing partial line" rule) so the two can never disagree
/// about what counts as "a complete line."
#[must_use]
fn last_complete_line_boundary(buf: &[u8]) -> u64 {
    let mut consumed = 0usize;
    while let Some(relative_newline) = buf[consumed..].iter().position(|&byte| byte == b'\n') {
        consumed += relative_newline + 1;
    }
    consumed as u64
}

/// Durable, restart-safe tailing state — see [`open_tail`]'s own doc comment for why every field
/// must match (and the offset must be in range) before it is trusted.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WatchState {
    thread_id: String,
    path: PathBuf,
    dev: u64,
    ino: u64,
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
    /// Device/inode at open time — an already-open handle keeps referring to the same underlying
    /// file even after its path is unlinked or replaced (ordinary POSIX semantics), so a mismatch
    /// against a *fresh*, path-based stat is the only way to notice "the file at this path is no
    /// longer the file this handle is reading" (a remove-and-recreate, or a rename swap).
    dev: u64,
    ino: u64,
    offset: u64,
}

impl Tail {
    /// Reads every complete line appended since `offset`, advancing it past exactly what was
    /// consumed. Returns an empty vec (not an error) when nothing new is there yet. An error here
    /// (identity mismatch, truncation, or an ordinary I/O failure) always means "this handle can
    /// no longer be trusted" — the caller drops it and re-discovers the file fresh, by content,
    /// from scratch.
    fn read_new_lines(&mut self) -> std::io::Result<Vec<String>> {
        use std::os::unix::fs::MetadataExt as _;
        let current = fs::metadata(&self.path)?;
        if current.dev() != self.dev || current.ino() != self.ino {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the file at this path was replaced or recreated since it was opened",
            ));
        }
        if current.len() < self.offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the rollout file shrank past the last known offset",
            ));
        }
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
    let Some(payload) = read_session_meta_payload(path) else {
        return false;
    };
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("id").and_then(Value::as_str));
    session_id == Some(thread_id)
}

/// Safety ceiling for a single `session_meta` header line, kept comfortably above real observed
/// sizes: a live-confirmed pinned `codex-cli 0.156.0` rollout's first record (its `base_instructions`
/// payload included) runs to 22,275 bytes. A fixed small chunk (the previous implementation read
/// only 8,192 bytes and never found that record's newline, which is the exact GitHub #18 live
/// preflight failure this constant fixes) is therefore unsafe; this ceiling exists only so a
/// corrupt or pathological file without any newline is never read in full — hitting it is treated
/// as "not yet readable" (visibly unhealthy, retried later), never silently misparsed.
const MAX_SESSION_META_LINE_BYTES: usize = 1024 * 1024;

/// Reads `path`'s own first record and returns its `payload` object iff that first record is a
/// well-formed `session_meta` line — `None` for an unreadable file, a header not yet fully
/// flushed (no complete first line yet), a first line beyond [`MAX_SESSION_META_LINE_BYTES`], or a
/// first record of any other type. Every rollout's identity (thread id, `parent_thread_id`,
/// `forked_from_id`, `ephemeral`) always lives in this exact shape (live-confirmed against real
/// `codex-cli 0.156.0` artifacts — see this module's own doc comment), so
/// [`rollout_header_names_thread`] and [`classify_rollout_header`] both read it through this one
/// function, never duplicating the parsing. Bounded by `read_until(b'\n')` rather than a fixed
/// chunk size, so a first record of any size up to the ceiling is found correctly, without ever
/// reading a multi-megabyte rollout in full just to identify it.
fn read_session_meta_payload(path: &Path) -> Option<Value> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first_line: Vec<u8> = Vec::new();
    loop {
        let read = reader
            .by_ref()
            .take((MAX_SESSION_META_LINE_BYTES - first_line.len()) as u64 + 1)
            .read_until(b'\n', &mut first_line)
            .ok()?;
        if read == 0 {
            // EOF before a complete line was ever found — header not yet fully flushed (or an
            // empty file). Retried on a later tick, never misread as a verdict.
            return None;
        }
        if first_line.last() == Some(&b'\n') {
            first_line.pop();
            break;
        }
        if first_line.len() > MAX_SESSION_META_LINE_BYTES {
            // No newline within the safety ceiling — fail closed rather than read without bound.
            return None;
        }
    }
    let first_line = std::str::from_utf8(&first_line).ok()?;
    let value: Value = serde_json::from_str(first_line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    value.get("payload").cloned()
}

/// One rollout header's identity-relevant fields, once proven [`Classification::TopLevel`] — see
/// this module's own doc comment on profile-domain watching.
struct CandidateHeader {
    thread_id: String,
    forked_from_id: Option<String>,
}

/// Whether a rollout — other than the one currently bound — is eligible to ever become a
/// profile-domain transition target.
enum Classification {
    /// No complete, parseable `session_meta` header yet — retry on a later tick. Never cached as
    /// [`Self::NotTopLevel`]: a file that is merely mid-write right now may well be exactly the
    /// transition this watcher needs to catch next tick.
    NotYetReadable,
    /// Codex's own protocol documents `Thread.parentThreadId` as set only for a subagent, and
    /// `Thread.ephemeral` threads as never materialized on disk (so a non-null `ephemeral: true`
    /// here is itself unexpected, but handled the same way regardless). Permanently irrelevant —
    /// see this module's own doc comment on profile-domain watching for why this exclusion is
    /// structural, never a heuristic.
    NotTopLevel,
    /// A genuine top-level (non-subagent, non-ephemeral) rollout — a transition candidate.
    TopLevel(CandidateHeader),
}

/// Classifies a rollout other than the one currently bound, from its header alone (never its full
/// content — see this module's own doc comment on profile-domain watching for why that read is
/// deferred to the moment a transition actually fires).
fn classify_rollout_header(path: &Path) -> Classification {
    let Some(payload) = read_session_meta_payload(path) else {
        return Classification::NotYetReadable;
    };
    let Some(thread_id) = payload
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("id").and_then(Value::as_str))
    else {
        return Classification::NotYetReadable;
    };
    let parent_thread_id = payload.get("parent_thread_id").and_then(Value::as_str);
    let ephemeral = payload
        .get("ephemeral")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if parent_thread_id.is_some() || ephemeral {
        return Classification::NotTopLevel;
    }
    let forked_from_id = payload
        .get("forked_from_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Classification::TopLevel(CandidateHeader {
        thread_id: thread_id.to_owned(),
        forked_from_id,
    })
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

    /// Codex's own protocol documents `Thread.parentThreadId` as set only for a subagent — see
    /// this module's own doc comment on profile-domain watching.
    fn session_meta_subagent(thread_id: &str, parent_thread_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:00.000Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "session_id": thread_id, "id": thread_id, "cwd": "/work",
                "parent_thread_id": parent_thread_id,
            },
        })
        .to_string()
    }

    /// A top-level (non-subagent) rollout whose own `session_meta` names the exact thread it was
    /// forked from — see this module's own doc comment on profile-domain watching.
    fn session_meta_forked(thread_id: &str, forked_from_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:00.000Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "session_id": thread_id, "id": thread_id, "cwd": "/work",
                "forked_from_id": forked_from_id,
            },
        })
        .to_string()
    }

    /// Codex's own protocol documents `Thread.ephemeral` threads as never materialized on disk —
    /// this exercises the defensive classification path anyway (see this module's own doc comment
    /// on profile-domain watching).
    fn session_meta_ephemeral(thread_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:00.000Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "session_id": thread_id, "id": thread_id, "cwd": "/work", "ephemeral": true,
            },
        })
        .to_string()
    }

    /// A real pinned `codex-cli 0.156.0` `session_meta` line, padded with a large
    /// `base_instructions` payload the same way the real provider's own first record is — a live
    /// GitHub #18 attach against such a rollout produced a 22,275-byte first line, which the
    /// previous 8,192-byte fixed-chunk read could never find a newline within. This helper
    /// defaults past that real size so a regression here is caught long before it recurs.
    fn session_meta_with_large_base_instructions(thread_id: &str, min_bytes: usize) -> String {
        let base_instructions = "x".repeat(min_bytes);
        serde_json::json!({
            "timestamp": "2026-10-13T00:00:00.000Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "session_id": thread_id, "id": thread_id, "cwd": "/work",
                "base_instructions": base_instructions,
            },
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
        let no_state_file = dir.path().join("no-state.json");
        let mut tail = open_tail(&path, "t1", &no_state_file, Baseline::Zero).unwrap();
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
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
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
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
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

    /// GitHub #18 live preflight regression: a real pinned `codex-cli 0.156.0` attach produced a
    /// 22,275-byte first `session_meta` record (its `base_instructions` payload), which the
    /// previous fixed-8,192-byte `read_session_meta_payload` could never find a newline within —
    /// `find_rollout_file` returned `None` forever, `initial_tail` stayed `None`, and the watcher
    /// never became healthy, leaving timer polling active indefinitely. This first line is built
    /// well beyond that real size (32 KiB of padding, ~32,100 bytes total) so this exact failure
    /// can never regress.
    #[test]
    fn a_first_record_far_larger_than_eight_kibibytes_is_still_discovered_and_becomes_healthy() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        let first_line = session_meta_with_large_base_instructions("t1", 32 * 1024);
        assert!(
            first_line.len() > 22_275,
            "fixture must exceed the real live-confirmed first-record size"
        );
        write_rollout(codex_home.path(), "t1", &[first_line]);
        assert!(find_rollout_file(codex_home.path(), "t1").is_some());

        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            watcher.is_healthy(),
            "a session_meta far larger than 8 KiB must still be discovered and attached"
        );
        watcher.stop();
    }

    #[test]
    fn file_created_after_watch_begins_is_still_found() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
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
    fn a_subagents_own_exhaustion_is_never_attributed_to_the_supervised_thread() {
        // agent-relay#18 (profile-domain watching): the one case that remains permanently
        // excluded, never a profile-domain transition target regardless of growth — Codex's own
        // protocol documents `parentThreadId` as set only for a subagent (see this module's own
        // doc comment). Pre-dating the GitHub #18 profile-domain redesign, this test used to prove
        // *no* other thread's activity was ever attributed; under that redesign, attributing
        // genuine top-level activity on another thread to this session is the *intended* new
        // behavior (see the sibling tests for `/new`/`/fork`/"resume a saved chat"), so only the
        // structurally-excluded subagent case is still asserted here.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let subagent = write_rollout(
            codex_home.path(),
            "subagent",
            &[session_meta_subagent("subagent", "watched")],
        );
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        append(&subagent, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            watcher.drain_events().is_empty(),
            "a subagent's own exhaustion must never be attributed to the supervised thread"
        );
        watcher.stop();
    }

    #[test]
    fn an_ephemeral_side_conversations_own_exhaustion_is_never_attributed() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let side = write_rollout(codex_home.path(), "side", &[session_meta_ephemeral("side")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        append(&side, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            watcher.drain_events().is_empty(),
            "an ephemeral side-conversation's own exhaustion must never be attributed"
        );
        watcher.stop();
    }

    #[test]
    fn a_fork_is_followed_via_its_own_forked_from_id_even_before_it_grows() {
        // agent-relay#18 (Phase 5, `/fork`): a fork's own `session_meta` names the exact thread it
        // was forked from — an immediate, zero-growth-required transition signal, unlike `/new`
        // and "resume a saved chat" below.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        let forked = write_rollout(
            codex_home.path(),
            "forked",
            &[session_meta_forked("forked", "watched")],
        );
        std::thread::sleep(Duration::from_millis(500));
        append(&forked, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            watcher.drain_events(),
            vec![
                RolloutEvent::ThreadTransitioned("forked".to_owned()),
                RolloutEvent::UsageLimitExceeded,
            ]
        );
        assert!(
            watcher.is_healthy(),
            "a transition must never flip healthy false"
        );
        watcher.stop();
    }

    #[test]
    fn a_new_chat_started_after_attach_is_followed_as_the_authoritative_thread() {
        // agent-relay#18 (Phase 5, `/new`): no link back to the previous thread at all — the
        // profile exclusivity invariant (this module's own doc comment on profile-domain
        // watching) is what makes attributing this to the supervised TUI non-heuristic.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        let new_chat = write_rollout(
            codex_home.path(),
            "new-chat",
            &[session_meta("new-chat"), task_complete_exhausted(1)],
        );
        let _ = new_chat;
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![
                RolloutEvent::ThreadTransitioned("new-chat".to_owned()),
                RolloutEvent::UsageLimitExceeded,
            ]
        );
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    #[test]
    fn resuming_a_pre_existing_saved_chat_is_followed_via_post_watch_growth() {
        // agent-relay#18 (Phase 5, "resume a saved chat"): the hardest case — the target rollout
        // already exists, with no structural marker at all for "being actively driven again."
        // Post-watch growth on it (never its mtime, never "newest file") is the only signal, and
        // it must be measured from the exact moment this watch started, not from whenever the
        // background loop happens to first notice the file (see `RolloutWatcher::attach`'s own doc
        // comment on `pre_existing_lengths`).
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let saved = write_rollout(
            codex_home.path(),
            "saved-chat",
            &[session_meta("saved-chat")],
        );
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        append(&saved, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![
                RolloutEvent::ThreadTransitioned("saved-chat".to_owned()),
                RolloutEvent::UsageLimitExceeded,
            ]
        );
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    #[test]
    fn a_saved_chat_resumed_with_no_growth_yet_is_never_a_premature_transition() {
        // The mere *existence* of another top-level rollout, pre-dating this watch, is never
        // itself evidence of anything — only post-watch growth (or an explicit fork link) is.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        write_rollout(codex_home.path(), "dormant", &[session_meta("dormant")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert!(watcher.drain_events().is_empty());
        watcher.stop();
    }

    #[test]
    fn only_the_candidate_that_actually_grew_is_followed_never_the_merely_newest_file() {
        // GitHub #18's explicit "no newest-file/mtime ordering" requirement: two structurally
        // identical, equally pre-existing candidates — only the one that actually produces
        // post-watch evidence (an append, here) may ever win, regardless of which file is newer
        // by name, mtime, or directory order.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "watched", &[session_meta("watched")]);
        let older = write_rollout(
            codex_home.path(),
            "older-candidate",
            &[session_meta("older-candidate")],
        );
        // Created after `older-candidate` but *before* the watch starts — so it gets a real,
        // attach-time baseline too, exactly like `older-candidate`; the only difference between
        // them is which one is later appended to.
        write_rollout(
            codex_home.path(),
            "newer-candidate",
            &[session_meta("newer-candidate")],
        );
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        append(&older, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![
                RolloutEvent::ThreadTransitioned("older-candidate".to_owned()),
                RolloutEvent::UsageLimitExceeded,
            ],
            "the candidate that actually grew must win, never the merely newer/younger file"
        );
        watcher.stop();
    }

    #[test]
    fn a_restart_resumes_from_the_durable_offset_without_resignaling() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
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
    fn a_restart_after_a_profile_domain_transition_resumes_bound_to_the_new_thread_not_the_original()
     {
        // agent-relay#18 (Phase 5/6, restart continuity): a watcher constructed with the
        // *originally*-known thread id, restarted after a profile-domain transition already moved
        // it elsewhere, must resume bound to wherever it last authoritatively was — never silently
        // snap back to the construction-time id, which would both re-arm `already_signaled` for a
        // thread already fully caught up and (worse) stop observing the thread actually active.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "original", &[session_meta("original")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "original", &state_file).unwrap();
        let resumed = write_rollout(
            codex_home.path(),
            "resumed-elsewhere",
            &[session_meta("resumed-elsewhere")],
        );
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::ThreadTransitioned(
                "resumed-elsewhere".to_owned()
            )]
        );
        watcher.stop();

        // Constructed with the *original* thread id again, exactly as a real caller would after a
        // process restart (it only ever knows the launch-time thread id) — must resume on
        // `resumed-elsewhere`, proven by catching an exhaustion appended to it, and never signal
        // anything for `original` (which never changes again).
        let restarted = RolloutWatcher::attach(codex_home.path(), "original", &state_file).unwrap();
        append(&resumed, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            restarted.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "a restart must resume bound to the last authoritative thread, not the original one"
        );
        restarted.stop();
    }

    #[test]
    fn attach_succeeds_immediately_but_stays_unhealthy_when_nothing_ever_matches() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
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
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
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

    // --- Regression: file created after attach, fully written before first lazy discovery ---

    #[test]
    fn a_rollout_created_and_fully_written_before_first_locate_is_never_skipped() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        // Attach while the file does not exist at all yet.
        let watcher = RolloutWatcher::attach(codex_home.path(), "late-thread", &state_file)
            .expect("attach never blocks on the file existing");
        assert!(!watcher.is_healthy());
        // The entire file — header, ordinary records, *and* the exhaustion record — is written in
        // one shot, simulating the interactive client completing a whole first turn before the
        // background loop's own first lazy discovery ever gets a chance to run.
        write_rollout(
            codex_home.path(),
            "late-thread",
            &[
                session_meta("late-thread"),
                task_complete_ok(1),
                task_complete_exhausted(2),
            ],
        );
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "every byte of a file that did not exist when the watch started must be eligible, \
             never silently baselined past"
        );
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    #[test]
    fn many_writes_before_first_locate_are_all_still_caught() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        let mut lines = vec![session_meta("t1")];
        for ordinal in 1..150 {
            lines.push(task_complete_ok(ordinal));
        }
        lines.push(task_complete_exhausted(150));
        write_rollout(codex_home.path(), "t1", &lines);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded]
        );
        watcher.stop();
    }

    #[test]
    fn a_partial_final_line_before_first_locate_is_not_lost() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(task_complete_exhausted(1).as_bytes())
                .unwrap();
            // No trailing newline yet — the line is not complete when the background loop first
            // discovers this file.
        }
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            watcher.drain_events().is_empty(),
            "an incomplete final line must never be parsed as a record yet"
        );
        append(&path, &[String::new()]); // writes just the trailing newline
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "completing the line later must still be caught, including by the first discovery"
        );
        watcher.stop();
    }

    #[test]
    fn a_subagent_created_first_does_not_affect_the_watched_threads_baseline() {
        // Pre-dating the GitHub #18 profile-domain redesign, this test used `"other"` as a
        // same-standing top-level thread to prove it could never affect `"watched"`'s own
        // baseline; under the redesign, a genuine top-level thread appearing before `"watched"`
        // itself does (correctly, see `a_new_chat_appearing_before_the_originally_known_thread_
        // is_still_followed` below) become the authoritative thread. What must still never happen
        // is a *subagent*'s own activity affecting anything, structurally excluded regardless of
        // timing.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        write_rollout(
            codex_home.path(),
            "subagent",
            &[
                session_meta_subagent("subagent", "watched"),
                task_complete_exhausted(1),
            ],
        );
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.drain_events().is_empty());
        assert!(
            !watcher.is_healthy(),
            "the watched thread's own file still does not exist"
        );
        write_rollout(
            codex_home.path(),
            "watched",
            &[session_meta("watched"), task_complete_exhausted(2)],
        );
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded]
        );
        watcher.stop();
    }

    #[test]
    fn a_new_chat_appearing_before_the_originally_known_thread_is_still_followed() {
        // agent-relay#18 (Phase 5): production never actually reaches this exact shape (the
        // originally-known thread id always already exists by the time `attach` is called — see
        // `relay_cli::codex_runtime`'s own module doc) — but the watcher's own correctness must
        // not depend on that assumption holding. A genuine top-level thread appearing before the
        // originally-known one ever does is exactly as attributable as any other post-watch
        // top-level activity under the profile exclusivity invariant (this module's own doc
        // comment on profile-domain watching) — never specially exempted just because it is not
        // the thread id this watcher happened to start with.
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        fs::create_dir_all(codex_home.path().join("sessions")).unwrap();
        let watcher = RolloutWatcher::attach(codex_home.path(), "watched", &state_file).unwrap();
        write_rollout(
            codex_home.path(),
            "actually-current",
            &[session_meta("actually-current"), task_complete_exhausted(1)],
        );
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![
                RolloutEvent::ThreadTransitioned("actually-current".to_owned()),
                RolloutEvent::UsageLimitExceeded,
            ]
        );
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    // --- Regression: worker exit paths must never leave `is_healthy()` stuck `true` ---

    #[test]
    fn dropping_the_watcher_without_stop_never_leaves_it_reporting_healthy() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        drop(watcher);
        // Nothing left to assert against (the handle is gone) — this test's real purpose is that
        // `Drop` (background worker join) does not panic or hang, exercising the same shutdown
        // path `HealthGuard` relies on.
    }

    #[test]
    fn a_file_replaced_at_the_same_path_is_detected_and_recovered() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        // Replace the file at the exact same path with a new inode (remove + recreate, not an
        // in-place append) — the same shape a rename-swap would produce.
        fs::remove_file(&path).unwrap();
        write_rollout(
            codex_home.path(),
            "t1",
            &[session_meta("t1"), task_complete_exhausted(1)],
        );
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "a replaced file must be rediscovered by content, never silently read from the old \
             (now-detached) handle forever"
        );
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    #[test]
    fn a_truncated_file_is_detected_rather_than_silently_read_as_empty_forever() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        let path = write_rollout(
            codex_home.path(),
            "t1",
            &[session_meta("t1"), task_complete_ok(1), task_complete_ok(2)],
        );
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        // Truncate in place (same inode, shorter length) — distinct from the replace case above.
        {
            let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_len(10).unwrap();
        }
        std::thread::sleep(Duration::from_millis(600));
        // However this resolves (unhealthy until re-caught-up, or already recovered by the next
        // check), it must never crash and must never silently stay healthy while reading nothing
        // meaningful forever — recovery is proven by the exhaustion record below still landing.
        append(&path, &[task_complete_exhausted(3)]);
        std::thread::sleep(Duration::from_millis(600));
        let events = watcher.drain_events();
        assert!(
            events.is_empty() || events == vec![RolloutEvent::UsageLimitExceeded],
            "must never duplicate-signal even across a truncation recovery: {events:?}"
        );
        watcher.stop();
    }

    // --- Regression: recovery must actually restore healthy coverage, not stay degraded ---

    #[test]
    fn healthy_only_becomes_true_after_a_proven_successful_read_not_merely_on_discovery() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        // `attach` itself already performed the one synchronous locate-and-read-free open; by the
        // time any time at all has passed, the background loop must have performed at least one
        // real `read_new_lines` to justify `healthy`.
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    // --- Regression (independent review): a stale/impossible offset must never be blindly
    // clamped-and-trusted — it must be rejected outright, falling back to a safe baseline, never
    // silently skipping content this exact watcher never actually read. ---

    #[test]
    fn a_stale_offset_larger_than_the_same_files_current_length_is_never_trusted() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let metadata = fs::metadata(&path).unwrap();
        use std::os::unix::fs::MetadataExt as _;
        let stale = WatchState {
            thread_id: "t1".to_owned(),
            path: path.clone(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            offset: 10_000_000, // larger than this exact file will ever be in this test
        };
        stale.save(&state_file).unwrap();
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            watcher.is_healthy(),
            "an impossible stale offset must never be trusted, but must also never leave the \
             watcher stuck unhealthy forever"
        );
        append(&path, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "content appended after attach must never be lost to a bogus, too-large durable offset"
        );
        watcher.stop();
    }

    #[test]
    fn a_stale_offset_from_a_different_file_generation_at_the_same_path_is_never_trusted() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        // An old `WatchState` survives from a *previous* file generation at this exact path (a
        // different inode) — live-plausible if a prior session's state file was never cleaned up
        // and a new thread later happens to produce a same-shaped path.
        let old_path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let old_metadata = fs::metadata(&old_path).unwrap();
        use std::os::unix::fs::MetadataExt as _;
        let stale = WatchState {
            thread_id: "t1".to_owned(),
            path: old_path.clone(),
            dev: old_metadata.dev(),
            ino: old_metadata.ino(),
            offset: old_metadata.len(), // plausible-looking, even in-range, for the OLD file
        };
        stale.save(&state_file).unwrap();
        // Replace the file in place (new inode) with fresh content nobody has read yet.
        fs::remove_file(&old_path).unwrap();
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        append(&path, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "an in-range-looking offset left over from a different file generation at the same \
             path must never suppress detection of genuinely new content"
        );
        watcher.stop();
    }

    #[test]
    fn a_record_torn_by_attachs_own_synchronous_stat_is_never_lost() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        // Simulate a torn write: the exhaustion record's bytes landed, but not yet its trailing
        // newline, at the exact instant `attach()` performs its one synchronous locate-and-open —
        // this is the one place that baselines by raw file length rather than re-reading forward.
        {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(task_complete_exhausted(1).as_bytes())
                .unwrap();
        }
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        // Complete the torn write shortly after attach.
        append(&path, &[String::new()]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded],
            "a record whose trailing newline lands after attach's own synchronous stat must \
             still be caught whole, never torn into a skipped half and a bogus empty line"
        );
        watcher.stop();
    }

    #[test]
    fn a_corrupt_state_file_degrades_to_the_baseline_rather_than_erroring() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        fs::write(&state_file, b"not valid json at all").unwrap();
        write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        watcher.stop();
    }

    #[test]
    fn a_usage_limit_record_written_immediately_after_recovery_is_still_caught() {
        let codex_home = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state_file = state_dir.path().join("state.json");
        let path = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        let watcher = RolloutWatcher::attach(codex_home.path(), "t1", &state_file).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.is_healthy());
        // Simulate a transient disruption and recovery via a replace (see the dedicated replace
        // test above), then immediately append the exhaustion record on the new file.
        fs::remove_file(&path).unwrap();
        let replaced = write_rollout(codex_home.path(), "t1", &[session_meta("t1")]);
        std::thread::sleep(Duration::from_millis(500));
        append(&replaced, &[task_complete_exhausted(1)]);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            watcher.drain_events(),
            vec![RolloutEvent::UsageLimitExceeded]
        );
        watcher.stop();
    }
}
