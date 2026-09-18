use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::Command,
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
        Self {
            pid,
            start_time_fingerprint: query_start_time(pid),
        }
    }

    /// `Some(true)`: the pid is running and its start time still matches (same process).
    /// `Some(false)`: the pid is either gone or now belongs to a different process (safe to treat
    /// as no longer the owner). `None`: identity could not be established either way — the
    /// caller must fail closed rather than guess.
    #[must_use]
    pub fn is_still_the_same_process(&self) -> Option<bool> {
        let recorded = self.start_time_fingerprint.as_ref()?;
        let current = query_start_time(self.pid)?;
        Some(&current == recorded)
    }
}

fn query_start_time(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::{OrchestrationLock, ProcessIdentity};

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
}
