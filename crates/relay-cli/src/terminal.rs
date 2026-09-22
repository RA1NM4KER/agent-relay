//! Supervised interactive terminal sessions.
//!
//! Before this module, `relay claude` / `relay resume` `exec`'d straight into the provider's
//! interactive command, so nothing of Relay remained in the terminal once the session started.
//! When the real usage limit was hit and Relay handed the conversation to a fallback profile,
//! the user was left staring at a dead session and had to discover `relay resume` themselves
//! (found live, M6: the first genuine exhaustion test).
//!
//! Here the interactive command runs as a *child* instead, and Relay stays alive as a thin
//! foreground wrapper — not a daemon, not a poller of provider state. It only:
//!
//! 1. watches its own project's `lease.json` (one cheap local read a second, only while the
//!    user's own terminal session is running) so that if the lease owner *moves to a different
//!    profile* — i.e. a handoff completed — the now-stopped source session is closed gracefully;
//! 2. waits (bounded) for any in-flight handoff transaction to settle when the child exits;
//! 3. reports back whether the caller should continue on the new owner.
//!
//! It never starts, stops, or hands off anything itself: every ownership change still happens
//! through the existing `HandoffCoordinator` under the orchestration lock, so single-writer
//! safety is exactly as before.

use std::{
    ffi::OsString,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use relay_core::{
    ProfileName,
    handoff::{LeaseStore, OrchestrationLock, WriterLease},
};

/// One interactive command to run in the user's terminal, fully specified so a continuation on a
/// different owner can be built the same way as the first launch.
#[derive(Clone, Debug)]
pub struct TerminalCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
    pub current_dir: Option<PathBuf>,
}

impl TerminalCommand {
    fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        if let Some(dir) = &self.current_dir {
            command.current_dir(dir);
        }
        command
    }
}

/// Who the attached session belongs to. Only the *owner profile* is compared: a same-profile
/// replacement (`relay claude --new`) ends the old session but is not a handoff to continue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseOwner(pub ProfileName);

impl LeaseOwner {
    #[must_use]
    pub fn of(lease: &WriterLease) -> Self {
        Self(lease.owner_profile.clone())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TerminalEnd {
    /// The child exited on its own with this code (`1` if it was killed by a signal).
    Exited(i32),
    /// The lease moved to a different profile while the child was still running; the child was
    /// asked to stop (its session was stopped by the handoff, so it is dead by definition).
    OwnerMoved,
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How often the child is checked for exit.
    pub poll: Duration,
    /// How often the lease file is re-read while the child runs.
    pub lease_check_every: Duration,
    /// How long a graceful stop request gets before the child is killed outright.
    pub terminate_grace: Duration,
    /// Upper bound on waiting for an in-flight handoff transaction to settle.
    pub settle_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(250),
            lease_check_every: Duration::from_secs(1),
            terminate_grace: Duration::from_secs(3),
            settle_timeout: Duration::from_secs(240),
        }
    }
}

/// A periodic action run only while the user's own terminal session is in the foreground — used
/// for providers (Codex) that have no "the limit was hit" event to hang a trigger on, so the
/// only honest way to notice exhaustion in time is to ask the provider's structured usage
/// interface now and then. It never decides anything: the action just starts the same bounded,
/// one-shot evaluation the Claude hook starts, and dies with the terminal session.
pub struct Tick<'a> {
    pub every: Duration,
    pub action: &'a mut dyn FnMut(),
}

/// Runs `command` in the foreground until it exits or the lease owner moves away from
/// `expected`.
pub fn run_watching_lease(
    command: &TerminalCommand,
    lease_store: &dyn Fn() -> LeaseStore,
    expected: &LeaseOwner,
    timing: &Timing,
    mut tick: Option<Tick<'_>>,
    on_spawn: Option<&dyn Fn(u32)>,
) -> std::io::Result<TerminalEnd> {
    let mut child = command.to_command().spawn()?;
    if let Some(on_spawn) = on_spawn {
        on_spawn(child.id());
    }
    let mut last_lease_check = Instant::now();
    let mut last_tick = Instant::now();
    loop {
        if let Some(tick) = tick.as_mut()
            && last_tick.elapsed() >= tick.every
        {
            last_tick = Instant::now();
            (tick.action)();
        }
        if let Some(status) = child.try_wait()? {
            return Ok(TerminalEnd::Exited(status.code().unwrap_or(1)));
        }
        if last_lease_check.elapsed() >= timing.lease_check_every {
            last_lease_check = Instant::now();
            if owner_moved(&lease_store(), expected) {
                terminate(&mut child, timing.terminate_grace);
                return Ok(TerminalEnd::OwnerMoved);
            }
        }
        std::thread::sleep(timing.poll);
    }
}

/// True only when a lease is readable and names a *different* owner. A missing or unreadable
/// lease is never treated as a move: the safe default is to leave the user's session alone.
#[must_use]
pub fn owner_moved(lease_store: &LeaseStore, expected: &LeaseOwner) -> bool {
    matches!(lease_store.load(), Ok(Some(lease)) if LeaseOwner::of(&lease) != *expected)
}

/// Waits for any in-flight handoff/recovery holding the project's orchestration lock. Returns
/// `false` if it was still held when the timeout expired.
#[must_use]
pub fn wait_until_settled(lock: &OrchestrationLock, timeout: Duration, interval: Duration) -> bool {
    let started = Instant::now();
    while lock.is_currently_held() {
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(interval);
    }
    true
}

/// Graceful first (`SIGTERM`, so a TUI can restore the terminal), then a hard kill if the child
/// ignores it.
fn terminate(child: &mut std::process::Child, grace: Duration) {
    #[cfg(unix)]
    {
        let _ignored = Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status();
        let started = Instant::now();
        while started.elapsed() < grace {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(not(unix))]
    let _ = grace;
    let _ignored = child.kill();
    let _ignored = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core::handoff::{ProcessIdentity, ProjectId, TransactionId};

    fn lease_for(owner: &str, project: &std::path::Path) -> WriterLease {
        WriterLease::new(
            ProjectId::for_canonical_path(project).expect("project id"),
            ProfileName::new(owner).expect("profile name"),
            ProcessIdentity::current(),
            "11111111-1111-4111-8111-111111111111".to_owned(),
            TransactionId::generate(),
            1,
        )
    }

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(20),
            lease_check_every: Duration::from_millis(40),
            terminate_grace: Duration::from_millis(500),
            settle_timeout: Duration::from_secs(2),
        }
    }

    fn sh(script: &str) -> TerminalCommand {
        TerminalCommand {
            program: PathBuf::from("/bin/sh"),
            args: vec![OsString::from("-c"), OsString::from(script)],
            envs: Vec::new(),
            current_dir: None,
        }
    }

    #[test]
    fn a_child_that_exits_reports_its_own_code_and_leaves_an_unmoved_lease_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LeaseStore::at_path(dir.path().join("lease.json"));
        store
            .save(&lease_for("erika", dir.path()))
            .expect("save lease");
        let end = run_watching_lease(
            &sh("exit 7"),
            &|| LeaseStore::at_path(store.path().to_path_buf()),
            &LeaseOwner(ProfileName::new("erika").unwrap()),
            &fast(),
            None,
            None,
        )
        .expect("run");
        assert_eq!(end, TerminalEnd::Exited(7));
    }

    #[test]
    fn a_lease_that_moves_to_another_profile_closes_the_now_dead_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LeaseStore::at_path(dir.path().join("lease.json"));
        store
            .save(&lease_for("erika", dir.path()))
            .expect("save lease");
        // The lease flips to `megan` shortly after the long-running child starts.
        let flip_store = LeaseStore::at_path(dir.path().join("lease.json"));
        let flip_lease = lease_for("megan", dir.path());
        let flipper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flip_store.save(&flip_lease).expect("flip lease");
        });
        let started = Instant::now();
        let end = run_watching_lease(
            &sh("sleep 30"),
            &|| LeaseStore::at_path(store.path().to_path_buf()),
            &LeaseOwner(ProfileName::new("erika").unwrap()),
            &fast(),
            None,
            None,
        )
        .expect("run");
        flipper.join().expect("flipper");
        assert_eq!(end, TerminalEnd::OwnerMoved);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_tick_runs_periodically_while_the_child_runs_and_stops_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LeaseStore::at_path(dir.path().join("lease.json"));
        store
            .save(&lease_for("erika", dir.path()))
            .expect("save lease");
        let mut ticks = 0_u32;
        let mut action = || ticks += 1;
        let end = run_watching_lease(
            &sh("sleep 0.6"),
            &|| LeaseStore::at_path(store.path().to_path_buf()),
            &LeaseOwner(ProfileName::new("erika").unwrap()),
            &fast(),
            Some(Tick {
                every: Duration::from_millis(100),
                action: &mut action,
            }),
            None,
        )
        .expect("run");
        assert_eq!(end, TerminalEnd::Exited(0));
        assert!((3..=7).contains(&ticks), "ticked {ticks} times");
    }

    #[test]
    fn a_missing_or_unreadable_lease_is_never_a_move() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LeaseStore::at_path(dir.path().join("lease.json"));
        let expected = LeaseOwner(ProfileName::new("erika").unwrap());
        assert!(!owner_moved(&store, &expected));
        std::fs::write(dir.path().join("lease.json"), b"not json").expect("corrupt");
        assert!(!owner_moved(&store, &expected));
    }

    #[test]
    fn a_same_profile_lease_is_not_a_move() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LeaseStore::at_path(dir.path().join("lease.json"));
        store
            .save(&lease_for("erika", dir.path()))
            .expect("save lease");
        assert!(!owner_moved(
            &store,
            &LeaseOwner(ProfileName::new("erika").unwrap())
        ));
        assert!(owner_moved(
            &store,
            &LeaseOwner(ProfileName::new("megan").unwrap())
        ));
    }

    #[test]
    fn waiting_for_an_unheld_lock_returns_immediately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = OrchestrationLock::at_path(dir.path().join("orchestration.lock"));
        assert!(wait_until_settled(
            &lock,
            Duration::from_millis(200),
            Duration::from_millis(20)
        ));
    }

    #[test]
    fn waiting_gives_up_after_the_timeout_while_the_lock_stays_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("orchestration.lock");
        let holder = OrchestrationLock::at_path(lock_path.clone());
        let waiter = OrchestrationLock::at_path(lock_path);
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            holder
                .try_with(|| {
                    held_tx.send(()).expect("signal held");
                    let _ = release_rx.recv();
                    Ok(())
                })
                .expect("hold lock");
        });
        held_rx.recv().expect("lock held");
        assert!(!wait_until_settled(
            &waiter,
            Duration::from_millis(150),
            Duration::from_millis(20)
        ));
        release_tx.send(()).expect("release");
        thread.join().expect("holder thread");
        assert!(wait_until_settled(
            &waiter,
            Duration::from_millis(500),
            Duration::from_millis(20)
        ));
    }
}
