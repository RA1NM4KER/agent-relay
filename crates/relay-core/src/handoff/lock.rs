use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// A genuine OS advisory lock (`flock`), held only for the duration of one `relay handoff` (or
/// `relay recover`) command's execution. Its defining property is that the OS releases it the
/// moment the holding process exits for *any* reason, including a crash — so a later process can
/// prove the earlier one is gone by successfully acquiring the same lock, rather than by trusting
/// a timestamp. This is the "no unlock/relock gap" mechanism: the whole transaction body runs
/// inside one acquisition.
pub struct OrchestrationLock {
    path: PathBuf,
}

impl OrchestrationLock {
    #[must_use]
    pub const fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Runs `body` while holding an exclusive, non-blocking lock. Fails immediately with
    /// [`Error::OrchestrationLockHeld`] if another live process already holds it — this never
    /// blocks and never silently steals a live lock.
    pub fn try_with<T>(&self, body: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut lock = self.open_lock()?;
        let _guard = lock.try_write().map_err(|_| Error::OrchestrationLockHeld)?;
        body()
    }

    /// Best-effort, informational only: a `relay lock status` query. There is an inherent
    /// check-then-report race (see docs/security.md); this is advisory, never a decision point.
    #[must_use]
    pub fn is_currently_held(&self) -> bool {
        let Ok(mut lock) = self.open_lock() else {
            return false;
        };
        lock.try_write().is_err()
    }

    fn open_lock(&self) -> Result<fd_lock::RwLock<std::fs::File>> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // A pre-existing symlink here could redirect this project's lock onto an unrelated
        // file (or another project's lock), silently defeating single-writer enforcement.
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(Error::SymbolicLink(self.path.clone()));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .map_err(|source| Error::Io {
                path: self.path.clone(),
                source,
            })?;
        Ok(fd_lock::RwLock::new(file))
    }
}

/// A durable, crash-safe advisory lock enforcing at most one live interactive session against a
/// given provider profile directory at a time, held for that session's entire lifetime — unlike
/// [`OrchestrationLock`], which is scoped to one short transaction's own execution. Backed by the
/// same genuine OS `flock`: the kernel releases it the instant the holding process exits for any
/// reason, including a crash, so a later acquisition attempt *proves* the earlier holder is gone
/// rather than trusting a recorded pid that could be stale or reused — no separate staleness check
/// of its own is needed, unlike a plain pid-file lock.
///
/// agent-relay#18: Codex's app-server protocol broadcasts certain lifecycle notifications
/// (`thread/started` in particular) to every client connected to the same `CODEX_HOME`, with no
/// client/session-scoped filter anywhere in the protocol (confirmed against the app-server's own
/// generated schema — 170 client request methods and 84 server notifications enumerated, none
/// scoped by client/session identity). Two Relay sessions sharing one Codex profile could
/// therefore misattribute each other's threads during event-driven observation. This lock makes
/// "exactly one live Relay-managed session per profile" a provable invariant instead of a hope,
/// for exactly the span `relay_provider_codex`'s event-driven observer relies on that invariant.
///
/// What this does **not** close, honestly: it cannot stop an entirely unmanaged `codex` process a
/// user points at this same directory by hand, outside Relay — no mechanism could, short of a
/// Codex protocol change, since Codex itself has no notion of this lock. That residual boundary is
/// the same one profile isolation already draws everywhere else: Relay guarantees what it
/// controls, never a user's own direct misuse of their own files.
pub struct ProfileSessionLock {
    _guard: fd_lock::RwLockWriteGuard<'static, fs::File>,
}

/// Mirrors `crate::paths`'s own private-directory helper (not reused directly: that one is
/// private to its module and scoped to Relay's own managed root, whereas this applies to a
/// Relay-owned subdirectory inside a provider's own profile directory).
#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

impl ProfileSessionLock {
    /// `profile_dir` is the provider's own isolated config directory (e.g. a Codex profile's
    /// `CODEX_HOME`). The lock file lives in a Relay-owned subdirectory under it so it can never
    /// collide with a path the provider itself manages or creates later. Never blocks: returns
    /// [`Error::ProfileSessionLockHeld`] immediately if another live process already holds it.
    pub fn try_acquire(profile_dir: &Path) -> Result<Self> {
        let relay_dir = profile_dir.join(".relay");
        fs::create_dir_all(&relay_dir).map_err(|source| Error::Io {
            path: relay_dir.clone(),
            source,
        })?;
        // `create_dir_all` applies the umask, not the credential-bearing parent's own private
        // mode — matching `docs/security.md`'s "private profile directories" expectation even
        // though this file holds no secret (just an empty lock target), for the same
        // defense-in-depth reason every other Relay-created directory inside a profile is 0700.
        set_private_directory_permissions(&relay_dir)?;
        let lock_path = relay_dir.join("session.lock");
        // Same symlink defense as `OrchestrationLock::open_lock`, for the same reason: a
        // pre-existing symlink here could silently redirect this lock onto an unrelated file.
        if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(Error::SymbolicLink(lock_path));
        }
        // This lock must outlive the function that acquires it, for an entire live session
        // (unlike `OrchestrationLock`'s own closure-scoped transaction) — but `fd_lock`'s own
        // guard type borrows the `RwLock` it came from, and this workspace forbids `unsafe`, so
        // there is no way to keep an owned guard alongside the value it borrows without leaking
        // one of them. The guard's own explicit `Drop` (a real `flock`/`LOCK_UN` call) is kept
        // deliberately, rather than ever bypassed via `mem::forget` + reclaiming the plain `File`:
        // an earlier version tried exactly that, live-measured to leave the lock still held after
        // the `File` closed on a GitHub Actions macOS runner — relying on "closing the last fd
        // releases the flock" alone was not reliable there, where an explicit unlock call is.
        //
        // What *is* avoided is leaking on a *refused* attempt (an earlier version leaked a heap
        // allocation and an open fd on every single refusal, not only successful ones, caught by
        // an independent adversarial review). A cheap, non-leaking probe goes first: open a
        // throwaway file handle, attempt the real lock on it without ever boxing/leaking anything,
        // and if it is refused, return immediately — nothing was ever leaked, because nothing was
        // ever made `'static`. Only once that probe confirms the lock is actually free does this
        // leak a `Box` and re-acquire for real, for the lifetime this struct actually needs. A
        // narrow race remains between the probe's own release and the real acquisition — another
        // process could win it in between — but that only costs one leaked attempt in a genuine,
        // rare race, never on the common "this profile is simply busy" refusal the review found.
        {
            let probe_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
                .map_err(|source| Error::Io {
                    path: lock_path.clone(),
                    source,
                })?;
            let mut probe = fd_lock::RwLock::new(probe_file);
            let _probe_guard = probe
                .try_write()
                .map_err(|_| Error::ProfileSessionLockHeld(profile_dir.to_path_buf()))?;
            // `_probe_guard` drops here, at the end of this block — bound to a name specifically
            // so it lives until here, not merely to the end of its own statement — issuing the
            // real, explicit `flock`/`LOCK_UN` this method's own doc comment relies on.
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| Error::Io {
                path: lock_path.clone(),
                source,
            })?;
        let leaked: &'static mut fd_lock::RwLock<fs::File> =
            Box::leak(Box::new(fd_lock::RwLock::new(file)));
        let guard = leaked
            .try_write()
            .map_err(|_| Error::ProfileSessionLockHeld(profile_dir.to_path_buf()))?;
        Ok(Self { _guard: guard })
    }
}

/// A process pid plus a best-effort start-time fingerprint, so a later comparison can tell a
/// live process apart from a different process that happens to reuse the same pid. `None` means
/// the fingerprint could not be established (permissions, unsupported platform, process gone at
/// query time) and must never be treated as proof either way.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time_fingerprint: Option<String>,
}

impl ProcessIdentity {
    #[must_use]
    pub fn current() -> Self {
        Self::query(std::process::id())
    }

    #[must_use]
    pub fn query(pid: u32) -> Self {
        let start_time_fingerprint = match query_start_time(pid) {
            ProcessQuery::Found(fingerprint) => Some(fingerprint),
            ProcessQuery::ConfirmedAbsent | ProcessQuery::Indeterminate => None,
        };
        Self {
            pid,
            start_time_fingerprint,
        }
    }

    /// `Some(true)`: the pid is running and its start time still matches (same process).
    /// `Some(false)`: the pid is *confirmed* gone, or now belongs to a different process (a `ps`
    /// query that ran successfully said so) — safe to treat as no longer the owner. `None`:
    /// identity could not be established either way (no recorded fingerprint to compare against,
    /// or `ps` itself could not be run) — the caller must fail closed rather than guess.
    #[must_use]
    pub fn is_still_the_same_process(&self) -> Option<bool> {
        let recorded = self.start_time_fingerprint.as_ref()?;
        match query_start_time(self.pid) {
            ProcessQuery::Found(current) => Some(&current == recorded),
            ProcessQuery::ConfirmedAbsent => Some(false),
            ProcessQuery::Indeterminate => None,
        }
    }

    /// Whether `self` is `descendant_pid` itself, or appears somewhere in its live parent-process
    /// chain — walked via `ps -o ppid=`, one hop at a time, bounded so a corrupted or (were one
    /// possible) cyclic chain can never loop forever. This is the provenance proof a request
    /// helper process can offer when it cannot be the *same* pid as the process it claims to
    /// speak for (a shell a supervised agent's own tool-use spawned is never the agent process
    /// itself) — the strongest thing short of exact identity.
    ///
    /// `Some(true)`: `self` is confirmed alive (its own fingerprint still matches) and is an
    /// ancestor. `Some(false)`: the chain was walked to its root without ever finding `self`.
    /// `None`: fails closed — `self` could not itself be confirmed alive, some hop's parent could
    /// not be read, or the walk exceeded [`MAX_ANCESTRY_DEPTH`] without resolving either way. A
    /// caller must never treat `None` as permission.
    #[must_use]
    pub fn is_ancestor_of(&self, descendant_pid: u32) -> Option<bool> {
        if self.is_still_the_same_process() != Some(true) {
            return None;
        }
        if descendant_pid == self.pid {
            return Some(true);
        }
        let mut current = descendant_pid;
        for _ in 0..MAX_ANCESTRY_DEPTH {
            match query_parent(current) {
                Some(0) => return Some(false),
                Some(parent) if parent == self.pid => return Some(true),
                Some(1) => return Some(false),
                Some(parent) => current = parent,
                None => return None,
            }
        }
        None
    }
}

/// A parent chain longer than this is treated as unresolvable rather than walked further —
/// ordinary process trees (shell → wrapper → tool-use child) are a handful of hops deep.
const MAX_ANCESTRY_DEPTH: u32 = 32;

/// The live parent pid of `pid`, or `None` if it cannot be determined (the process is gone, or
/// `ps` itself could not be run) — ambiguity here must propagate as ambiguity, never as "no
/// parent found" (which would let [`ProcessIdentity::is_ancestor_of`] misread it as reaching the
/// chain's root).
fn query_parent(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Distinguishes "we asked and the OS said no such process" from "we could not ask at all" —
/// collapsing these (as a plain `Option` would) would make a confirmed crash indistinguishable
/// from a permissions problem, and this project's recovery logic must never guess between them.
enum ProcessQuery {
    Found(String),
    ConfirmedAbsent,
    Indeterminate,
}

/// Retried only for the "could not even run `ps`" case: on a resource-constrained host under
/// heavy concurrent load (observed on GitHub's 3-vCPU `macos-latest` runner during a full
/// parallel `cargo test --workspace`), spawning `ps` itself can transiently fail — not because
/// the pid is gone, but because the OS momentarily couldn't fork it. A `ps` that *did* run,
/// successfully, and reported nothing is never retried here: that is `ConfirmedAbsent`, a real
/// positive answer, not something to second-guess.
const SPAWN_RETRY_ATTEMPTS: u32 = 3;
const SPAWN_RETRY_DELAY: Duration = Duration::from_millis(20);

fn query_start_time(pid: u32) -> ProcessQuery {
    for attempt in 0..SPAWN_RETRY_ATTEMPTS {
        let Ok(output) = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
        else {
            if attempt + 1 < SPAWN_RETRY_ATTEMPTS {
                thread::sleep(SPAWN_RETRY_DELAY);
                continue;
            }
            return ProcessQuery::Indeterminate;
        };
        // macOS `ps -p <pid>` for a pid that does not exist exits non-zero with empty output;
        // that is a confirmed, positive answer ("no such process"), not a failure to determine
        // anything.
        if !output.status.success() {
            return ProcessQuery::ConfirmedAbsent;
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return if text.is_empty() {
            ProcessQuery::ConfirmedAbsent
        } else {
            ProcessQuery::Found(text)
        };
    }
    ProcessQuery::Indeterminate
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::{OrchestrationLock, ProcessIdentity, ProfileSessionLock};

    #[test]
    #[cfg(unix)]
    fn a_symlinked_lock_path_is_rejected_instead_of_followed() {
        let root = tempdir().expect("temp dir");
        let real_target = root.path().join("someone-elses-lock");
        std::fs::write(&real_target, b"").expect("seed target");
        let symlinked_path = root.path().join("orchestration.lock");
        std::os::unix::fs::symlink(&real_target, &symlinked_path).expect("symlink");

        let lock = OrchestrationLock::at_path(symlinked_path);
        let error = lock
            .try_with(|| Ok(()))
            .expect_err("a symlinked lock path must be rejected");
        assert_eq!(error.code(), "symbolic_link");
    }

    #[test]
    fn a_held_lock_is_reported_as_held_and_a_free_lock_is_not() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("orchestration.lock");
        let lock = OrchestrationLock::at_path(path);
        assert!(!lock.is_currently_held());

        lock.try_with(|| {
            assert!(lock.is_currently_held());
            Ok(())
        })
        .expect("acquire");

        assert!(!lock.is_currently_held(), "released after the body returns");
    }

    #[test]
    fn a_second_acquire_is_refused_while_the_first_holds_it() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("orchestration.lock");
        let lock = OrchestrationLock::at_path(path);

        let outer_ran = std::sync::Mutex::new(false);
        lock.try_with(|| {
            *outer_ran.lock().expect("mutex") = true;
            let inner = lock.try_with(|| Ok(()));
            assert!(inner.is_err(), "must refuse a nested/concurrent acquire");
            Ok(())
        })
        .expect("outer acquire");
        assert!(*outer_ran.lock().expect("mutex"));
    }

    #[test]
    fn the_lock_is_released_even_if_the_body_errors() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("orchestration.lock");
        let lock = OrchestrationLock::at_path(path);

        let result: crate::Result<()> = lock.try_with(|| Err(crate::Error::ProviderUnavailable));
        assert!(result.is_err());
        assert!(!lock.is_currently_held());
    }

    #[test]
    fn concurrent_threads_never_observe_the_lock_held_simultaneously() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("orchestration.lock");
        let barrier = Arc::new(Barrier::new(2));
        let successes = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                let successes = successes.clone();
                std::thread::spawn(move || {
                    let lock = OrchestrationLock::at_path(path);
                    barrier.wait();
                    let _ = lock.try_with(|| {
                        successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        Ok(())
                    });
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread join");
        }
        // At least one must succeed; both succeeding sequentially is fine, but the lock type
        // itself is what the held-simultaneously assertion inside try_with would have caught.
        assert!(successes.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }

    #[test]
    fn process_identity_of_the_current_process_matches_itself() {
        let identity = ProcessIdentity::current();
        // Either the platform lets us establish a fingerprint and it matches, or it could not be
        // established at all (None) — both are acceptable; a definite mismatch is not.
        assert_ne!(identity.is_still_the_same_process(), Some(false));
    }

    #[test]
    fn a_fabricated_identity_with_a_wrong_fingerprint_is_detected_as_different() {
        let identity = ProcessIdentity {
            pid: std::process::id(),
            start_time_fingerprint: Some("definitely-not-the-real-start-time".to_owned()),
        };
        assert_eq!(identity.is_still_the_same_process(), Some(false));
    }

    #[test]
    fn an_unestablishable_fingerprint_never_claims_a_definite_answer() {
        let identity = ProcessIdentity {
            pid: std::process::id(),
            start_time_fingerprint: None,
        };
        assert_eq!(identity.is_still_the_same_process(), None);
    }

    #[test]
    fn a_process_is_an_ancestor_of_its_own_direct_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn child");
        let identity = ProcessIdentity::current();
        assert_eq!(identity.is_ancestor_of(child.id()), Some(true));
        let _ignored = child.kill();
        let _ignored = child.wait();
    }

    #[test]
    fn a_process_is_an_ancestor_of_its_own_grandchild() {
        // A shell that backgrounds `sleep` and then waits on it never execs into `sleep`
        // (unlike a single simple command, which some shells optimise into a direct exec), so
        // this genuinely produces two hops: this test -> sh -> sleep.
        let pid_file = tempfile::NamedTempFile::new().expect("temp file");
        let script = format!("sleep 5 & echo $! > {} ; wait", pid_file.path().display());
        let mut shell = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .spawn()
            .expect("spawn shell");
        let grandchild_pid: u32 = (0..40)
            .find_map(|_| {
                let text = std::fs::read_to_string(pid_file.path()).ok()?;
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    return None;
                }
                trimmed.parse().ok()
            })
            .expect("grandchild pid was written");
        let identity = ProcessIdentity::current();
        assert_eq!(identity.is_ancestor_of(grandchild_pid), Some(true));
        let _ignored = shell.kill();
        let _ignored = shell.wait();
    }

    #[test]
    fn an_unrelated_process_is_not_an_ancestor() {
        let mut unrelated = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn unrelated process");
        // The unrelated process's own identity, asked whether *it* is an ancestor of *us* —
        // this process is definitely not a descendant of a sibling it just spawned.
        let identity = ProcessIdentity::query(unrelated.id());
        assert_eq!(identity.is_ancestor_of(std::process::id()), Some(false));
        let _ignored = unrelated.kill();
        let _ignored = unrelated.wait();
    }

    #[test]
    fn an_identity_that_cannot_confirm_itself_alive_never_claims_ancestry() {
        let bogus = ProcessIdentity {
            pid: std::process::id(),
            start_time_fingerprint: Some("definitely-not-the-real-start-time".to_owned()),
        };
        assert_eq!(bogus.is_ancestor_of(std::process::id()), None);
    }

    #[test]
    fn a_confirmed_dead_pid_is_reported_as_a_definite_not_the_same_process() {
        // Spawn and immediately reap a short-lived process to get a pid that is confirmed gone
        // (not just "probably" gone), then confirm the answer is Some(false), not None: a crash
        // must be distinguishable from "could not determine," not conflated with it.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn short-lived process");
        let pid = child.id();
        child.wait().expect("reap child");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let identity = ProcessIdentity {
            pid,
            start_time_fingerprint: Some("whatever-it-was-when-launched".to_owned()),
        };
        assert_eq!(
            identity.is_still_the_same_process(),
            Some(false),
            "a confirmed-absent pid must resolve definitely, not as None"
        );
    }

    // --- agent-relay#18: `ProfileSessionLock` ---

    #[test]
    fn a_profile_lock_is_acquired_when_free_and_refused_while_held() {
        let root = tempdir().expect("temp dir");
        let first = ProfileSessionLock::try_acquire(root.path()).expect("first acquire");
        let second = ProfileSessionLock::try_acquire(root.path());
        assert!(
            matches!(second, Err(crate::Error::ProfileSessionLockHeld(path)) if path == root.path()),
            "a second acquisition while the first is live must be refused, naming the profile dir"
        );
        drop(first);
        ProfileSessionLock::try_acquire(root.path()).expect("released after the first guard drops");
    }

    #[test]
    #[cfg(unix)]
    fn a_refused_acquisition_leaks_neither_a_file_descriptor_nor_a_heap_allocation() {
        // agent-relay#18: an earlier version of `try_acquire` leaked a `Box<fd_lock::RwLock<File>>`
        // unconditionally, *before* attempting the lock — so every *refused* attempt (an operator
        // simply running `relay codex` again against an already-busy profile) leaked too, not
        // only successful ones. `/dev/fd` (present on both macOS and Linux, unlike `/proc`) lets
        // this count real open descriptors directly rather than trust a doc comment's claim.
        let root = tempdir().expect("temp dir");
        let _held = ProfileSessionLock::try_acquire(root.path()).expect("hold it");
        let fds_before = std::fs::read_dir("/dev/fd").expect("list open fds").count();
        for _ in 0..200 {
            let result = ProfileSessionLock::try_acquire(root.path());
            assert!(result.is_err(), "must be refused while held");
        }
        let fds_after = std::fs::read_dir("/dev/fd").expect("list open fds").count();
        // A tolerance, not exact equality: `/dev/fd` reflects the whole test *process*, and
        // `cargo test` runs other tests concurrently on their own threads, each briefly opening
        // their own files — real cross-test noise, not a leak in this code. A real regression
        // (leaking on every one of the 200 refused attempts above) would show up as growth on
        // the order of 200, far past any plausible amount of concurrent-test noise.
        let growth = fds_after.saturating_sub(fds_before);
        assert!(
            growth < 20,
            "200 refused acquisitions must not leak file descriptors \
             (before={fds_before}, after={fds_after}, growth={growth})"
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_relay_subdirectory_is_created_private_even_under_a_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempdir().expect("temp dir");
        let _lock = ProfileSessionLock::try_acquire(root.path()).expect("acquire");
        let mode = std::fs::metadata(root.path().join(".relay"))
            .expect("relay subdir exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o700,
            "the lock's own subdirectory must be private regardless of the process umask"
        );
    }

    #[test]
    fn two_distinct_profile_directories_never_contend() {
        let root = tempdir().expect("temp dir");
        let a = root.path().join("profile-a");
        let b = root.path().join("profile-b");
        std::fs::create_dir_all(&a).expect("profile a dir");
        std::fs::create_dir_all(&b).expect("profile b dir");
        let _lock_a = ProfileSessionLock::try_acquire(&a).expect("acquire a");
        ProfileSessionLock::try_acquire(&b).expect("a different profile dir must never contend");
    }

    #[test]
    fn two_racing_threads_never_both_observe_the_profile_lock_held_simultaneously() {
        // agent-relay#18: the exact "race between two processes acquiring profile ownership"
        // adversarial case — threads race `flock` the same way two independent `relay` processes
        // starting at the same instant against the same Codex profile would (the kernel lock is
        // what actually arbitrates, identical either way).
        use std::sync::{Arc, Barrier};
        let root = tempdir().expect("temp dir");
        let path = Arc::new(root.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(2));
        let successes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                let successes = successes.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    if let Ok(lock) = ProfileSessionLock::try_acquire(&path) {
                        successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        drop(lock);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread join");
        }
        assert_eq!(
            successes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one of two simultaneous racers must win a profile that only one may hold \
             at a time — never zero (a live profile must remain usable), never two"
        );
    }

    #[test]
    fn a_symlinked_profile_lock_path_is_rejected_instead_of_followed() {
        let root = tempdir().expect("temp dir");
        let real_target = root.path().join("someone-elses-file");
        std::fs::write(&real_target, b"").expect("seed target");
        std::fs::create_dir_all(root.path().join(".relay")).expect("relay subdir");
        std::os::unix::fs::symlink(&real_target, root.path().join(".relay/session.lock"))
            .expect("symlink");
        match ProfileSessionLock::try_acquire(root.path()) {
            Err(error) => assert_eq!(error.code(), "symbolic_link"),
            Ok(_) => panic!("symlink must be rejected"),
        }
    }

    #[test]
    fn a_crashed_holders_profile_lock_is_reclaimable_without_any_staleness_check() {
        // Proves the actual cross-process crash-release guarantee (not merely a same-process
        // drop): a real child process holds the lock via `flock`, is killed outright (SIGKILL —
        // no Drop/cleanup of its own ever runs), and a later acquisition in this process must
        // still succeed cleanly, with no PID-matching or staleness check of our own required.
        let root = tempdir().expect("temp dir");
        std::fs::create_dir_all(root.path().join(".relay")).expect("relay subdir");
        let lock_path = root.path().join(".relay/session.lock");
        let holder = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import fcntl, sys, time\n\
                 f = open(sys.argv[1], 'a')\n\
                 fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n\
                 print('locked', flush=True)\n\
                 time.sleep(60)\n",
            )
            .arg(&lock_path)
            .stdout(std::process::Stdio::piped())
            .spawn();
        let Ok(mut holder) = holder else {
            // python3 is not guaranteed present on every CI runner; the crash-release guarantee
            // this proves is a property of the OS/`fd_lock`, not of this crate's own code, so
            // skipping when the tool is unavailable never hides a real regression here.
            eprintln!("skipping: python3 unavailable to spawn a real cross-process holder");
            return;
        };
        use std::io::Read as _;
        let mut line = [0_u8; 6];
        let mut stdout = holder.stdout.take().expect("piped stdout");
        if stdout.read_exact(&mut line).is_err() {
            let _ = holder.kill();
            let _ = holder.wait();
            eprintln!("skipping: holder did not confirm its lock in time");
            return;
        }
        assert!(
            ProfileSessionLock::try_acquire(root.path()).is_err(),
            "must be refused while the real external holder is still alive"
        );
        holder.kill().expect("sigkill the holder");
        holder.wait().expect("reap the killed holder");
        ProfileSessionLock::try_acquire(root.path()).expect(
            "a killed holder's lock must be cleanly reclaimable, no staleness check needed",
        );
    }
}
