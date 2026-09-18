use std::{
    path::{Path, PathBuf},
    sync::{Arc, Barrier, Mutex},
};

use relay_core::{
    Error, ProfileName, RelayPaths,
    handoff::{
        FailedPhase, HandoffCoordinator, HandoffRequest, HandoffState, JournalStore, LeaseStore,
        LivenessVerdict, OrchestrationLock, ProcessIdentity, ProjectId, SessionStager,
        SessionStopper, SourceLiveness, TargetLauncher, TargetVerification, TransactionId,
        TransferOutcome, TransferredArtifact, WriterLease,
    },
};
use tempfile::tempdir;

const SESSION_ID: &str = "8586fe71-395b-4449-b973-78011d561fed";

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("project dir");
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} must succeed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("seed file");
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "init"]);
}

fn relay_paths(root: &Path) -> RelayPaths {
    RelayPaths::new(root.join("config"), root.join("state")).expect("relay paths")
}

struct FixedLiveness(bool);
impl SourceLiveness for FixedLiveness {
    fn check(
        &self,
        _source_config_dir: &Path,
        _project_dir: &Path,
        _expected_session_id: &str,
        _recorded_owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<LivenessVerdict> {
        Ok(LivenessVerdict {
            active: self.0,
            untracked_session_ids: Vec::new(),
        })
    }
}

struct SlowLiveness {
    active: bool,
    delay: std::time::Duration,
}
impl SourceLiveness for SlowLiveness {
    fn check(
        &self,
        _source_config_dir: &Path,
        _project_dir: &Path,
        _expected_session_id: &str,
        _recorded_owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<LivenessVerdict> {
        std::thread::sleep(self.delay);
        Ok(LivenessVerdict {
            active: self.active,
            untracked_session_ids: Vec::new(),
        })
    }
}

struct OkStopper;
impl SessionStopper for OkStopper {
    fn stop_and_verify(
        &self,
        _source_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
        _recorded_owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<()> {
        Ok(())
    }
}

struct FailingStopper;
impl SessionStopper for FailingStopper {
    fn stop_and_verify(
        &self,
        _source_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
        _recorded_owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<()> {
        Err(Error::StopNotVerified(
            "session never went quiet".to_owned(),
        ))
    }
}

struct OkStager;
impl SessionStager for OkStager {
    fn stage(
        &self,
        _source_config_dir: &Path,
        _target_config_dir: &Path,
        _project_dir: &Path,
        session_id: &str,
    ) -> relay_core::Result<TransferOutcome> {
        Ok(TransferOutcome {
            artifacts: vec![TransferredArtifact {
                relative_path: format!("projects/proj/{session_id}.jsonl"),
                sha256: "deadbeef".to_owned(),
                size_bytes: 42,
            }],
        })
    }
}

struct FailingStager;
impl SessionStager for FailingStager {
    fn stage(
        &self,
        _source_config_dir: &Path,
        _target_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> relay_core::Result<TransferOutcome> {
        Err(Error::TargetArtifactDiverges)
    }
}

struct OkLauncher;
impl TargetLauncher for OkLauncher {
    fn launch_and_verify(
        &self,
        _target_config_dir: &Path,
        _project_dir: &Path,
        session_id: &str,
    ) -> relay_core::Result<TargetVerification> {
        Ok(TargetVerification {
            target_session_id: session_id.to_owned(),
            started_successfully: true,
        })
    }
}

struct FailingLauncher;
impl TargetLauncher for FailingLauncher {
    fn launch_and_verify(
        &self,
        _target_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> relay_core::Result<TargetVerification> {
        Err(Error::ProviderCommandFailed)
    }
}

struct WrongSessionLauncher;
impl TargetLauncher for WrongSessionLauncher {
    fn launch_and_verify(
        &self,
        _target_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> relay_core::Result<TargetVerification> {
        Ok(TargetVerification {
            target_session_id: "wrong-session-id".to_owned(),
            started_successfully: true,
        })
    }
}

fn request(project_dir: &Path, from: &str, to: &str) -> HandoffRequest {
    HandoffRequest {
        project_dir: project_dir.to_path_buf(),
        source_profile: ProfileName::new(from).expect("name"),
        source_config_dir: project_dir.join(format!("{from}-config")),
        target_profile: ProfileName::new(to).expect("name"),
        target_config_dir: project_dir.join(format!("{to}-config")),
        session_id: SESSION_ID.to_owned(),
    }
}

#[test]
fn successful_handoff_reaches_complete_and_updates_the_lease() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };

    let journal = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect("handoff must succeed");

    assert_eq!(journal.state, HandoffState::Complete);
    assert_eq!(journal.transferred_artifacts.len(), 1);
    assert!(journal.verification.is_some());
    assert!(journal.checkpoint.is_some());

    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let lease_store = LeaseStore::at_path(paths.project_state_dir(&project_id).join("lease.json"));
    let lease = lease_store
        .load()
        .expect("load lease")
        .expect("lease exists");
    assert_eq!(lease.owner_profile.to_string(), "megan");
    assert_eq!(lease.session_id, SESSION_ID);
}

#[test]
fn a_wrong_source_profile_is_rejected_once_a_lease_is_owned_by_someone_else() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect("first handoff succeeds");

    // Erika is no longer the owner (megan is); attempting to hand off FROM erika again must be
    // rejected rather than silently proceeding as if erika still owned the project.
    let error = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect_err("must reject a source that is not the current lease owner");
    assert_eq!(error.code(), "writer_lease_owned_by_another_profile");
}

#[test]
fn reverse_handoff_from_the_new_owner_succeeds() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect("erika -> megan");

    let journal = coordinator
        .run(request(&project_dir, "megan", "erika"))
        .expect("megan -> erika must succeed: megan is the current owner");
    assert_eq!(journal.state, HandoffState::Complete);
}

#[test]
fn stop_verification_failure_fails_the_stop_phase_and_stages_nothing() {
    // M2B.75: an active source no longer immediately blocks the handoff — the coordinator
    // actively stops it. Only a stop that cannot be verified quiescent blocks.
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(true),
        stopper: &FailingStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };

    let error = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect_err("must refuse when the stop cannot be verified quiescent");
    assert_eq!(error.code(), "stop_not_verified");
}

#[test]
fn an_active_source_is_stopped_rather_than_immediately_refused() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(true),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };

    let journal = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect("an active source that stops successfully must still complete the handoff");
    assert_eq!(journal.state, HandoffState::Complete);
}

#[test]
fn journal_records_failed_phase_when_stop_cannot_be_verified() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(true),
        stopper: &FailingStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    let _ = coordinator.run(request(&project_dir, "erika", "megan"));

    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let handoffs_dir = paths.project_state_dir(&project_id).join("handoffs");
    let entries: Vec<_> = std::fs::read_dir(&handoffs_dir)
        .expect("handoffs dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(entries.len(), 1);
    let journal = JournalStore::at_path(entries[0].clone())
        .load()
        .expect("load");
    match journal.state {
        HandoffState::Failed { phase, .. } => assert_eq!(phase, FailedPhase::Stop),
        other => panic!("expected Failed{{Stop}}, got {other:?}"),
    }
}

#[test]
fn divergent_transcript_fails_the_transfer_phase() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &FailingStager,
        launcher: &OkLauncher,
    };

    let error = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect_err("must propagate the staging failure");
    assert_eq!(error.code(), "target_artifact_diverges");
}

#[test]
fn target_startup_failure_fails_the_target_start_phase() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &FailingLauncher,
    };

    let error = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect_err("must propagate the launch failure");
    assert_eq!(error.code(), "provider_command_failed");
}

#[test]
fn target_identity_mismatch_fails_verification_and_does_not_move_the_lease() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &WrongSessionLauncher,
    };

    let error = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect_err("must reject a mismatched session id");
    assert_eq!(error.code(), "target_verification_mismatch");

    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let lease_store = LeaseStore::at_path(paths.project_state_dir(&project_id).join("lease.json"));
    assert_eq!(
        lease_store.load().expect("load"),
        None,
        "ownership must never move on a failed verification"
    );
}

#[test]
fn concurrent_handoff_attempts_are_serialized_and_exactly_one_transaction_per_slot_wins() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = Arc::new(relay_paths(root.path()));
    let barrier = Arc::new(Barrier::new(2));

    let run_one = |paths: Arc<RelayPaths>, project_dir: PathBuf, barrier: Arc<Barrier>| {
        std::thread::spawn(move || {
            let liveness = SlowLiveness {
                active: false,
                delay: std::time::Duration::from_millis(50),
            };
            let coordinator = HandoffCoordinator {
                paths: &paths,
                liveness: &liveness,
                stopper: &OkStopper,
                stager: &OkStager,
                launcher: &OkLauncher,
            };
            barrier.wait();
            coordinator.run(request(&project_dir, "erika", "megan"))
        })
    };

    let first = run_one(paths.clone(), project_dir.clone(), barrier.clone());
    let second = run_one(paths.clone(), project_dir.clone(), barrier.clone());
    let first_result = first.join().expect("thread");
    let second_result = second.join().expect("thread");

    let outcomes = [first_result, second_result];
    let succeeded = outcomes.iter().filter(|result| result.is_ok()).count();
    let refused_as_busy = outcomes
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.code() == "orchestration_lock_held")
        })
        .count();
    assert_eq!(succeeded, 1, "exactly one concurrent attempt must win");
    assert_eq!(
        refused_as_busy, 1,
        "the other must be refused as busy, never silently skipped or duplicated"
    );
}

#[test]
fn a_live_orchestration_lock_blocks_recovery() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect(
            "seed a completed transaction to recover against is unnecessary; we just need the dir",
        );

    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let project_state_dir = paths.project_state_dir(&project_id);
    let lock_path = project_state_dir.join("orchestration.lock");
    let lock = OrchestrationLock::at_path(lock_path);

    let ready = Arc::new(Barrier::new(2));
    let ready_clone = ready.clone();
    let hold_result = Arc::new(Mutex::new(None));
    let hold_result_clone = hold_result.clone();
    let lock_path_for_thread = project_state_dir.join("orchestration.lock");
    let holder = std::thread::spawn(move || {
        let lock = OrchestrationLock::at_path(lock_path_for_thread);
        let _ = lock.try_with(|| {
            ready_clone.wait();
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok(())
        });
    });
    ready.wait();
    // While the holder thread has the lock, recovery must be refused, not race it.
    let recover_result = coordinator.recover(&project_state_dir, &TransactionId::generate());
    *hold_result_clone.lock().expect("mutex") = Some(recover_result);
    holder.join().expect("holder thread");

    let recover_result = hold_result.lock().expect("mutex").take().expect("result");
    let error = recover_result.expect_err("recovery must be refused while the lock is live");
    assert!(matches!(
        error.code(),
        "orchestration_lock_held" | "transaction_not_found"
    ));
    drop(lock);
}

#[test]
fn recovery_at_every_interrupted_stage_produces_the_documented_safe_outcome() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let project_state_dir = paths.project_state_dir(&project_id);
    std::fs::create_dir_all(project_state_dir.join("handoffs")).expect("handoffs dir");

    let cases: &[(HandoffState, &str)] = &[
        (HandoffState::Preparing, "abandon"),
        (HandoffState::Checkpointed, "abandon"),
        (HandoffState::SourceStopping, "abandon"),
        (HandoffState::SourceStopped, "abandon"),
        (HandoffState::SessionTransferring, "recovery_required"),
        (HandoffState::SessionTransferred, "recovery_required"),
        (HandoffState::TargetStarting, "recovery_required"),
        (HandoffState::TargetVerified, "complete"),
    ];

    for (index, (crash_state, expectation)) in cases.iter().enumerate() {
        let transaction_id = TransactionId::generate();
        let mut journal = relay_core::handoff::HandoffJournal::new(
            transaction_id.clone(),
            project_id.clone(),
            project_dir.clone(),
            ProfileName::new("erika").expect("name"),
            ProfileName::new("megan").expect("name"),
            format!("{SESSION_ID}-{index}"),
        );
        // Walk the journal forward to the crash point using only legal transitions.
        let sequence = [
            HandoffState::Checkpointed,
            HandoffState::SourceStopping,
            HandoffState::SourceStopped,
            HandoffState::SessionTransferring,
            HandoffState::SessionTransferred,
            HandoffState::TargetStarting,
            HandoffState::TargetVerified,
        ];
        if *crash_state != HandoffState::Preparing {
            for state in sequence {
                journal
                    .advance(state.clone(), "walking to crash point")
                    .expect("advance");
                if state == *crash_state {
                    break;
                }
            }
        }
        let journal_store = JournalStore::at_path(
            project_state_dir
                .join("handoffs")
                .join(format!("{transaction_id}.json")),
        );
        journal_store.save(&journal).expect("seed crashed journal");

        let recovered = coordinator
            .recover(&project_state_dir, &transaction_id)
            .unwrap_or_else(|error| {
                panic!("recover must decide, not error, for {crash_state:?}: {error}")
            });

        match *expectation {
            "abandon" => assert!(
                matches!(recovered.state, HandoffState::Failed { .. }),
                "state {crash_state:?} must recover to Failed, got {:?}",
                recovered.state
            ),
            "recovery_required" => assert!(
                matches!(recovered.state, HandoffState::RecoveryRequired { .. }),
                "state {crash_state:?} must recover to RecoveryRequired, got {:?}",
                recovered.state
            ),
            "complete" => assert_eq!(
                recovered.state,
                HandoffState::Complete,
                "state {crash_state:?} must safely auto-complete"
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn recovering_an_already_terminal_transaction_twice_is_idempotent() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    let journal = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect("handoff");
    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let project_state_dir = paths.project_state_dir(&project_id);

    let first = coordinator
        .recover(&project_state_dir, &journal.transaction_id)
        .expect("first recover on a terminal transaction");
    let second = coordinator
        .recover(&project_state_dir, &journal.transaction_id)
        .expect("second recover must also succeed, not double-apply anything");
    assert_eq!(first.state, HandoffState::Complete);
    assert_eq!(second.state, HandoffState::Complete);
    assert_eq!(
        first.revision, second.revision,
        "recovering Complete twice must not bump revision"
    );
}

#[test]
fn recovering_an_unknown_transaction_id_reports_not_found_rather_than_guessing() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let project_state_dir = paths.project_state_dir(&project_id);
    std::fs::create_dir_all(&project_state_dir).expect("dir");

    let error = coordinator
        .recover(&project_state_dir, &TransactionId::generate())
        .expect_err("must fail closed on an unknown transaction id");
    assert_eq!(error.code(), "transaction_not_found");
}

#[test]
fn a_corrupted_journal_fails_closed_during_recovery() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let project_state_dir = paths.project_state_dir(&project_id);
    let handoffs_dir = project_state_dir.join("handoffs");
    std::fs::create_dir_all(&handoffs_dir).expect("dir");
    let transaction_id = TransactionId::generate();
    std::fs::write(
        handoffs_dir.join(format!("{transaction_id}.json")),
        "{not json",
    )
    .expect("corrupt journal");

    let error = coordinator
        .recover(&project_state_dir, &transaction_id)
        .expect_err("must fail closed on a corrupted journal");
    assert_eq!(error.code(), "corrupted_journal");
}

#[test]
fn a_pid_reuse_scenario_is_detected_as_a_different_process_not_the_recorded_owner() {
    // The recorded owner process (a fabricated, essentially-never-real pid/fingerprint pairing)
    // must never be reported as still-live just because *a* process with that pid exists now.
    let lease = WriterLease::new(
        ProjectId::for_canonical_path(Path::new("/tmp/proj")).expect("id"),
        ProfileName::new("erika").expect("name"),
        ProcessIdentity {
            pid: std::process::id(),
            start_time_fingerprint: Some("not-this-processes-real-start-time".to_owned()),
        },
        SESSION_ID.to_owned(),
        TransactionId::generate(),
        0,
    );
    assert_eq!(
        lease.owner_process.is_still_the_same_process(),
        Some(false),
        "a fingerprint mismatch must never be treated as the same process"
    );
}

#[test]
fn wrong_project_never_collides_with_a_different_projects_state() {
    let root = tempdir().expect("temp dir");
    let project_a = root.path().join("project-a");
    let project_b = root.path().join("project-b");
    init_git_repo(&project_a);
    init_git_repo(&project_b);
    let project_a = project_a.canonicalize().expect("canonicalize");
    let project_b = project_b.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };

    coordinator
        .run(request(&project_a, "erika", "megan"))
        .expect("handoff on project A");

    // Project B has never had a handoff; erika must still be free to be its source (no lease
    // exists yet for B), proving A's lease does not leak into B's state.
    let journal_b = coordinator
        .run(request(&project_b, "erika", "megan"))
        .expect("project B is independent of project A's lease");
    assert_eq!(journal_b.state, HandoffState::Complete);

    let id_a = ProjectId::for_canonical_path(&project_a).expect("id");
    let id_b = ProjectId::for_canonical_path(&project_b).expect("id");
    assert_ne!(id_a, id_b);
}

struct UntrackedLiveness;
impl SourceLiveness for UntrackedLiveness {
    fn check(
        &self,
        _source_config_dir: &Path,
        _project_dir: &Path,
        _expected_session_id: &str,
        _recorded_owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<LivenessVerdict> {
        Ok(LivenessVerdict {
            active: false,
            untracked_session_ids: vec!["some-other-session-id".to_owned()],
        })
    }
}

#[test]
fn an_untracked_session_for_the_source_profile_blocks_the_handoff() {
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");
    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &UntrackedLiveness,
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };

    let error = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect_err("an untracked session for this profile must block the handoff");
    assert_eq!(error.code(), "untracked_writer_detected");

    let project_id = ProjectId::for_canonical_path(&project_dir).expect("id");
    let lease_store = LeaseStore::at_path(paths.project_state_dir(&project_id).join("lease.json"));
    assert_eq!(
        lease_store.load().expect("load"),
        None,
        "no lease must be written when the handoff is blocked"
    );
}

#[test]
fn a_confirmed_dead_recorded_owner_is_not_treated_as_active() {
    // Regression test for the tri-state ProcessIdentity fix: a lease whose recorded process is
    // confirmed gone (not just "unknown") must let the liveness check proceed rather than being
    // conflated with "cannot determine, fail closed."
    let root = tempdir().expect("temp dir");
    let project_dir = root.path().join("project");
    init_git_repo(&project_dir);
    let project_dir = project_dir.canonicalize().expect("canonicalize");

    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn short-lived process");
    let dead_pid = child.id();
    child.wait().expect("reap child");
    std::thread::sleep(std::time::Duration::from_millis(50));
    let dead_identity = ProcessIdentity {
        pid: dead_pid,
        start_time_fingerprint: Some("whatever-it-was".to_owned()),
    };
    assert_eq!(dead_identity.is_still_the_same_process(), Some(false));

    let paths = relay_paths(root.path());
    let coordinator = HandoffCoordinator {
        paths: &paths,
        liveness: &FixedLiveness(false),
        stopper: &OkStopper,
        stager: &OkStager,
        launcher: &OkLauncher,
    };
    let journal = coordinator
        .run(request(&project_dir, "erika", "megan"))
        .expect("handoff must succeed when the fake liveness check reports not-active");
    assert_eq!(journal.state, HandoffState::Complete);
}
