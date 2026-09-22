use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicU32, Ordering},
    },
};

use relay_core::{
    ClaudeConfigMode, ProfileName, ProviderKind, RelayPaths,
    automation::{
        AutomationPolicy, LedgerStore, ProfileCandidate, WatchCoordinator, WatchOutcome,
        WatchRequest,
    },
    handoff::{
        ContinuityType, HandoffCoordinator, HandoffJournal, HandoffState, JournalStore,
        LaunchDirective, LeaseStore, LivenessVerdict, OrchestrationLock, ProcessIdentity,
        ProjectId, SessionStager, SessionStopper, SourceLiveness, TargetLauncher,
        TargetVerification, TransactionId, TransferOutcome, TransferredArtifact,
    },
    usage::{UsageEvidence, UsageObservation, UsageState},
};
use tempfile::tempdir;

const SESSION_ID: &str = "8586fe71-395b-4449-b973-78011d561fed";

#[derive(Default)]
struct Calls {
    stops: AtomicU32,
    stages: AtomicU32,
    launches: AtomicU32,
    fail_launch: std::sync::atomic::AtomicBool,
}

struct Ports(Arc<Calls>);

impl SourceLiveness for Ports {
    fn check(
        &self,
        _config: &Path,
        _project: &Path,
        _session: &str,
        _owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<LivenessVerdict> {
        Ok(LivenessVerdict {
            active: false,
            untracked_session_ids: Vec::new(),
        })
    }
}
impl SessionStopper for Ports {
    fn stop_and_verify(
        &self,
        _config: &Path,
        _project: &Path,
        _session: &str,
        _owner: Option<&ProcessIdentity>,
    ) -> relay_core::Result<()> {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl SessionStager for Ports {
    fn stage(
        &self,
        _source: &Path,
        _target: &Path,
        _project: &Path,
        session_id: &str,
        _source_mode: ClaudeConfigMode,
        _target_mode: ClaudeConfigMode,
    ) -> relay_core::Result<TransferOutcome> {
        self.0.stages.fetch_add(1, Ordering::SeqCst);
        Ok(TransferOutcome {
            artifacts: vec![TransferredArtifact {
                relative_path: format!("projects/proj/{session_id}.jsonl"),
                sha256: "deadbeef".to_owned(),
                size_bytes: 1,
            }],
        })
    }
}
impl TargetLauncher for Ports {
    fn launch_and_verify(
        &self,
        _target: &Path,
        _project: &Path,
        directive: &LaunchDirective<'_>,
        on_started: &mut dyn FnMut(Option<ProcessIdentity>) -> relay_core::Result<()>,
    ) -> relay_core::Result<TargetVerification> {
        self.0.launches.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_launch.load(Ordering::SeqCst) {
            return Err(relay_core::Error::ProviderCommandFailed);
        }
        on_started(Some(ProcessIdentity::current()))?;
        let session_id = match directive {
            LaunchDirective::ResumeSession { session_id } => (*session_id).to_owned(),
            LaunchDirective::Bootstrap { .. } => "bootstrap-session".to_owned(),
        };
        Ok(TargetVerification {
            target_session_id: session_id,
            started_successfully: true,
        })
    }
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("project dir");
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("seed file");
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "init"]);
}

fn name(value: &str) -> ProfileName {
    ProfileName::new(value).expect("profile name")
}

fn observation(state: UsageState, reset: Option<u64>) -> UsageObservation {
    UsageObservation {
        state,
        evidence: UsageEvidence::Simulated,
        detected_via: "test signal".to_owned(),
        observed_unix_ms: 1_000,
        reset_unix_ms: reset,
    }
}

fn candidate(profile: &str, state: UsageState, reset: Option<u64>) -> ProfileCandidate {
    ProfileCandidate {
        name: name(profile),
        provider: ProviderKind::Claude,
        config_dir: PathBuf::from(format!("/tmp/relay-watch-test/{profile}")),
        identity_stable_id: Some(format!("identity-{profile}")),
        enabled: true,
        healthy: true,
        usage: observation(state, reset),
        claude_config_mode: None,
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    paths: RelayPaths,
    calls: Arc<Calls>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().expect("temp dir");
        let project = root.path().join("project");
        init_git_repo(&project);
        let project = project.canonicalize().expect("canonicalize");
        let paths =
            RelayPaths::new(root.path().join("config"), root.path().join("state")).expect("paths");
        Self {
            _root: root,
            project,
            paths,
            calls: Arc::new(Calls::default()),
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.paths
            .project_state_dir(&ProjectId::for_canonical_path(&self.project).expect("id"))
    }

    fn evaluate(
        &self,
        source: &str,
        source_usage: UsageObservation,
        fallbacks: Vec<ProfileCandidate>,
        dry_run: bool,
        policy: AutomationPolicy,
        now: u64,
    ) -> relay_core::Result<WatchOutcome> {
        let ports = Ports(Arc::clone(&self.calls));
        let handoff = HandoffCoordinator {
            paths: &self.paths,
            liveness: &ports,
            source_stopper: &ports,
            target_stopper: &ports,
            stager: Some(&ports),
            context_capturer: None,
            launcher: &ports,
        };
        let handoff_for = |_: &ProfileName, _: &ProfileName| &handoff;
        let watch = WatchCoordinator {
            paths: &self.paths,
            handoff_for: &handoff_for,
            policy,
        };
        watch.evaluate(
            WatchRequest {
                project_dir: self.project.clone(),
                source_profile: name(source),
                source_provider: ProviderKind::Claude,
                source_config_dir: PathBuf::from(format!("/tmp/relay-watch-test/{source}")),
                source_identity_stable_id: Some(format!("identity-{source}")),
                session_id: SESSION_ID.to_owned(),
                source_usage,
                fallbacks,
                dry_run,
                state_dir: None,
                source_claude_mode: None,
            },
            now,
        )
    }

    fn total_calls(&self) -> u32 {
        self.calls.stops.load(Ordering::SeqCst)
            + self.calls.stages.load(Ordering::SeqCst)
            + self.calls.launches.load(Ordering::SeqCst)
    }

    fn lease_owner(&self) -> Option<String> {
        LeaseStore::at_path(self.state_dir().join("lease.json"))
            .load()
            .expect("lease load")
            .map(|lease| lease.owner_profile.to_string())
    }
}

fn no_cooldown(max: usize) -> AutomationPolicy {
    AutomationPolicy {
        cooldown_ms: 0,
        max_handoffs_per_window: max,
        window_ms: 3_600_000,
    }
}

#[test]
fn exhausted_source_hands_off_through_the_full_transaction_and_records_evidence() {
    let fixture = Fixture::new();
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            AutomationPolicy::default(),
            10_000,
        )
        .expect("evaluate");
    let WatchOutcome::Handoff { journal, target } = outcome else {
        panic!("expected a handoff");
    };
    assert_eq!(target, name("megan"));
    assert_eq!(journal.state, HandoffState::Complete);
    assert_eq!(fixture.calls.stops.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.stages.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.launches.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));

    let ledger = LedgerStore::at_path(fixture.state_dir().join("automation_state.json"))
        .load()
        .expect("ledger");
    assert_eq!(ledger.recent_handoffs.len(), 1);
    assert_eq!(ledger.known_exhausted.len(), 1);
    assert_eq!(ledger.known_exhausted[0].profile, name("erika"));
    assert_eq!(ledger.known_exhausted[0].evidence, UsageEvidence::Simulated);
    assert_eq!(ledger.known_exhausted[0].detected_via, "test signal");
}

#[test]
fn automation_never_acts_unless_the_source_is_exhausted() {
    for state in [
        UsageState::Available,
        UsageState::NearLimit,
        UsageState::Unknown,
    ] {
        let fixture = Fixture::new();
        let outcome = fixture
            .evaluate(
                "erika",
                observation(state, None),
                vec![candidate("megan", UsageState::Available, None)],
                false,
                AutomationPolicy::default(),
                10_000,
            )
            .expect("evaluate");
        assert!(matches!(outcome, WatchOutcome::NoActionNeeded { .. }));
        assert_eq!(fixture.total_calls(), 0, "{state:?} must cause no mutation");
        assert!(fixture.lease_owner().is_none());
    }
}

#[test]
fn dry_run_reports_the_handoff_but_mutates_nothing() {
    let fixture = Fixture::new();
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![candidate("megan", UsageState::Available, None)],
            true,
            AutomationPolicy::default(),
            10_000,
        )
        .expect("evaluate");
    assert!(matches!(
        outcome,
        WatchOutcome::DryRunWouldHandoff { target } if target == name("megan")
    ));
    assert_eq!(fixture.total_calls(), 0);
    assert!(fixture.lease_owner().is_none());
    assert!(
        !fixture.state_dir().join("automation_state.json").exists(),
        "a dry run must not persist even the automation ledger"
    );
    assert!(!fixture.state_dir().join("handoffs").exists());
}

#[test]
fn no_eligible_target_waits_for_capacity_without_touching_anything() {
    let fixture = Fixture::new();
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![candidate("megan", UsageState::Exhausted, None)],
            false,
            AutomationPolicy::default(),
            10_000,
        )
        .expect("evaluate");
    assert!(matches!(outcome, WatchOutcome::WaitingForCapacity { .. }));
    assert_eq!(fixture.total_calls(), 0);

    let mut unhealthy = candidate("megan", UsageState::Available, None);
    unhealthy.healthy = false;
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![unhealthy],
            false,
            AutomationPolicy::default(),
            10_001,
        )
        .expect("evaluate");
    assert!(matches!(outcome, WatchOutcome::WaitingForCapacity { .. }));
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn a_target_sharing_the_source_identity_is_never_used() {
    let fixture = Fixture::new();
    let mut alias = candidate("megan", UsageState::Available, None);
    alias.identity_stable_id = Some("identity-erika".to_owned());
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![alias],
            false,
            AutomationPolicy::default(),
            10_000,
        )
        .expect("evaluate");
    assert!(matches!(outcome, WatchOutcome::WaitingForCapacity { .. }));
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn cooldown_blocks_an_immediate_second_handoff_then_allows_it_later() {
    let fixture = Fixture::new();
    let policy = AutomationPolicy {
        cooldown_ms: 30_000,
        max_handoffs_per_window: 10,
        window_ms: 3_600_000,
    };
    // erika's recorded reset (10_500) has passed by the time megan is exhausted.
    fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, Some(10_500)),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            policy,
            10_000,
        )
        .expect("first handoff");
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));

    let blocked = fixture
        .evaluate(
            "megan",
            observation(UsageState::Exhausted, None),
            vec![candidate("erika", UsageState::Available, None)],
            false,
            policy,
            11_000,
        )
        .expect("evaluate");
    assert!(matches!(blocked, WatchOutcome::CooldownActive { .. }));
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));

    let allowed = fixture
        .evaluate(
            "megan",
            observation(UsageState::Exhausted, None),
            vec![candidate("erika", UsageState::Available, None)],
            false,
            policy,
            50_000,
        )
        .expect("evaluate");
    assert!(matches!(allowed, WatchOutcome::Handoff { .. }));
    assert_eq!(fixture.lease_owner().as_deref(), Some("erika"));
}

#[test]
fn a_target_recorded_exhausted_with_a_future_reset_is_never_bounced_back_to() {
    let fixture = Fixture::new();
    fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, Some(9_000_000)),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            no_cooldown(10),
            10_000,
        )
        .expect("first handoff");

    let outcome = fixture
        .evaluate(
            "megan",
            observation(UsageState::Exhausted, None),
            vec![candidate("erika", UsageState::Available, None)],
            false,
            no_cooldown(10),
            20_000,
        )
        .expect("evaluate");
    assert!(
        matches!(outcome, WatchOutcome::WaitingForCapacity { .. }),
        "erika's reset time has not passed, so she must not be selected"
    );
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));
}

#[test]
fn repeated_exhaustion_is_bounded_by_the_loop_guard() {
    let fixture = Fixture::new();
    let policy = no_cooldown(2);
    // Both profiles carry reset times already in the past so they are eligible failover targets;
    // only the bounded-handoff guard can stop the ping-pong.
    let steps = [("erika", "megan", 1_000u64), ("megan", "erika", 2_000)];
    for (from, to, now) in steps {
        let outcome = fixture
            .evaluate(
                from,
                observation(UsageState::Exhausted, Some(500)),
                vec![candidate(to, UsageState::Available, None)],
                false,
                policy,
                now,
            )
            .expect("evaluate");
        assert!(matches!(outcome, WatchOutcome::Handoff { .. }));
    }
    let stops_before = fixture.calls.stops.load(Ordering::SeqCst);
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, Some(500)),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            policy,
            3_000,
        )
        .expect("evaluate");
    assert!(matches!(outcome, WatchOutcome::LoopPrevented { .. }));
    assert_eq!(fixture.calls.stops.load(Ordering::SeqCst), stops_before);
}

#[test]
fn concurrent_automatic_triggers_perform_exactly_one_handoff() {
    let fixture = Arc::new(Fixture::new());
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                fixture.evaluate(
                    "erika",
                    observation(UsageState::Exhausted, None),
                    vec![candidate("megan", UsageState::Available, None)],
                    false,
                    AutomationPolicy::default(),
                    10_000,
                )
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect();
    let handoffs = results
        .iter()
        .filter(|result| matches!(result, Ok(WatchOutcome::Handoff { .. })))
        .count();
    assert_eq!(handoffs, 1, "exactly one trigger may perform the handoff");
    assert_eq!(fixture.calls.stages.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.launches.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));
}

#[test]
fn a_corrupted_ledger_fails_closed_before_any_mutation() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.state_dir()).expect("state dir");
    std::fs::write(
        fixture.state_dir().join("automation_state.json"),
        b"{ not json",
    )
    .expect("corrupt ledger");
    let result = fixture.evaluate(
        "erika",
        observation(UsageState::Exhausted, None),
        vec![candidate("megan", UsageState::Available, None)],
        false,
        AutomationPolicy::default(),
        10_000,
    );
    assert!(result.is_err());
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn a_failed_automatic_handoff_still_starts_the_cooldown_so_it_is_not_retried_in_a_tight_loop() {
    let fixture = Fixture::new();
    fixture.calls.fail_launch.store(true, Ordering::SeqCst);
    let policy = AutomationPolicy {
        cooldown_ms: 30_000,
        max_handoffs_per_window: 10,
        window_ms: 3_600_000,
    };
    let first = fixture.evaluate(
        "erika",
        observation(UsageState::Exhausted, None),
        vec![candidate("megan", UsageState::Available, None)],
        false,
        policy,
        10_000,
    );
    assert!(first.is_err(), "the target failed to start");
    assert_eq!(fixture.calls.launches.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.lease_owner().as_deref(), None);

    let second = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            policy,
            11_000,
        )
        .expect("evaluate");
    assert!(matches!(second, WatchOutcome::CooldownActive { .. }));
    assert_eq!(
        fixture.calls.launches.load(Ordering::SeqCst),
        1,
        "no second launch during the cooldown"
    );
}

// ---- M2C.1: startup recovery, RESET_PENDING ----

impl Fixture {
    /// Writes a journal exactly as a crashed orchestrator would have left it: advanced to `state`
    /// by the ordinary transitions, saved under handoffs/, with no orchestration lock held.
    fn crashed_journal(&self, state: HandoffState) -> String {
        let project_id = ProjectId::for_canonical_path(&self.project).expect("id");
        let id = TransactionId::generate();
        let mut journal = HandoffJournal::new(
            id.clone(),
            project_id,
            self.project.clone(),
            name("erika"),
            name("megan"),
            PathBuf::from("/tmp/relay-watch-test/megan"),
            SESSION_ID.to_owned(),
            ContinuityType::SessionContinuation,
        );
        for step in [
            HandoffState::Checkpointed,
            HandoffState::SourceStopping,
            HandoffState::SourceStopped,
            HandoffState::SessionTransferring,
            HandoffState::SessionTransferred,
            HandoffState::TargetStarting,
        ] {
            journal.advance(step, "test").expect("advance");
        }
        if !matches!(state, HandoffState::TargetStarting) {
            journal.advance(state, "test").expect("advance to final");
        }
        let dir = self.state_dir().join("handoffs");
        std::fs::create_dir_all(&dir).expect("handoffs dir");
        JournalStore::at_path(dir.join(format!("{id}.json")))
            .save(&journal)
            .expect("save journal");
        std::fs::write(
            self.state_dir().join("current_transaction.json"),
            id.as_str(),
        )
        .expect("current pointer");
        id.to_string()
    }

    fn exhausted_erika(&self, now: u64, policy: AutomationPolicy) -> WatchOutcome {
        self.evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            policy,
            now,
        )
        .expect("evaluate")
    }
}

#[test]
fn a_crashed_target_start_is_recovered_before_any_new_work_and_never_double_starts() {
    let fixture = Fixture::new();
    let id = fixture.crashed_journal(HandoffState::TargetStarting);

    let first = fixture.exhausted_erika(10_000, no_cooldown(5));
    let WatchOutcome::Recovered { transactions } = first else {
        panic!("expected startup recovery, got {first:?}");
    };
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].transaction_id, id);
    // Exactly one orphan stop and one re-verification; NO new handoff (no stage, no second
    // launch) in the same round even though the source reads exhausted.
    assert_eq!(fixture.calls.stops.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.launches.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.stages.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));

    // The recovered transaction is terminal, so the next round is ordinary and does not loop back
    // into recovery.
    let stops_before = fixture.calls.stops.load(Ordering::SeqCst);
    let second = fixture.evaluate(
        "megan",
        observation(UsageState::Available, None),
        vec![candidate("erika", UsageState::Available, None)],
        false,
        no_cooldown(5),
        20_000,
    );
    assert!(matches!(second, Ok(WatchOutcome::NoActionNeeded { .. })));
    assert_eq!(fixture.calls.stops.load(Ordering::SeqCst), stops_before);
}

#[test]
fn ambiguous_recovery_stops_with_recovery_required_and_starts_nothing() {
    let fixture = Fixture::new();
    fixture.crashed_journal(HandoffState::RecoveryRequired {
        reason: "operator must confirm".to_owned(),
    });
    let outcome = fixture.exhausted_erika(10_000, no_cooldown(5));
    let WatchOutcome::RecoveryRequired { reason, .. } = outcome else {
        panic!("expected RECOVERY_REQUIRED, got {outcome:?}");
    };
    assert!(reason.contains("operator must confirm"));
    assert_eq!(
        fixture.total_calls(),
        0,
        "nothing may be spawned or stopped"
    );
    assert_eq!(fixture.lease_owner(), None);
}

#[test]
fn an_unreadable_journal_is_ambiguous_and_blocks_new_work() {
    let fixture = Fixture::new();
    let id = TransactionId::generate();
    let dir = fixture.state_dir().join("handoffs");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join(format!("{id}.json")), b"{not a journal").expect("corrupt");
    std::fs::write(
        fixture.state_dir().join("current_transaction.json"),
        id.as_str(),
    )
    .expect("current pointer");
    let outcome = fixture.exhausted_erika(10_000, no_cooldown(5));
    assert!(matches!(outcome, WatchOutcome::RecoveryRequired { .. }));
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn a_transaction_held_by_a_live_orchestrator_is_left_alone() {
    let fixture = Fixture::new();
    fixture.crashed_journal(HandoffState::TargetStarting);
    let lock = OrchestrationLock::at_path(fixture.state_dir().join("orchestration.lock"));
    let outcome = lock
        .try_with(|| Ok(fixture.exhausted_erika(10_000, no_cooldown(5))))
        .expect("lock");
    assert!(matches!(outcome, WatchOutcome::TransactionInFlight { .. }));
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn dry_run_reports_pending_recovery_without_performing_it() {
    let fixture = Fixture::new();
    fixture.crashed_journal(HandoffState::TargetStarting);
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, None),
            vec![candidate("megan", UsageState::Available, None)],
            true,
            no_cooldown(5),
            10_000,
        )
        .expect("evaluate");
    assert!(matches!(outcome, WatchOutcome::RecoveryRequired { .. }));
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn terminal_journals_never_trigger_recovery() {
    let fixture = Fixture::new();
    fixture.crashed_journal(HandoffState::Failed {
        phase: relay_core::handoff::FailedPhase::TargetStart,
        reason: "old".to_owned(),
    });
    let outcome = fixture.exhausted_erika(10_000, no_cooldown(5));
    assert!(matches!(outcome, WatchOutcome::Handoff { .. }));
}

#[test]
fn a_previously_exhausted_source_with_a_future_reset_stays_blocking_when_the_reading_is_unknown() {
    let fixture = Fixture::new();
    // Round 1: erika observed exhausted with a reset in the future; hand off to megan.
    let first = fixture
        .evaluate(
            "erika",
            observation(UsageState::Exhausted, Some(9_000_000)),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            no_cooldown(5),
            10_000,
        )
        .expect("evaluate");
    assert!(matches!(first, WatchOutcome::Handoff { .. }));

    // Round 2: megan is now the writer and reads UNKNOWN (stale statusline); erika is UNKNOWN
    // too but still recorded RESET_PENDING, so she must not be chosen as the target.
    let second = fixture
        .evaluate(
            "megan",
            observation(UsageState::Exhausted, None),
            vec![candidate("erika", UsageState::Unknown, None)],
            false,
            no_cooldown(5),
            20_000,
        )
        .expect("evaluate");
    assert!(matches!(second, WatchOutcome::WaitingForCapacity { .. }));
    assert_eq!(fixture.lease_owner().as_deref(), Some("megan"));

    // Once the reset time passes, erika is a valid target again.
    let third = fixture
        .evaluate(
            "megan",
            observation(UsageState::Exhausted, None),
            vec![candidate("erika", UsageState::Available, None)],
            false,
            no_cooldown(5),
            9_100_000,
        )
        .expect("evaluate");
    assert!(matches!(third, WatchOutcome::Handoff { .. }));
}

#[test]
fn unknown_never_triggers_a_handoff_even_with_a_healthy_fallback() {
    let fixture = Fixture::new();
    let outcome = fixture
        .evaluate(
            "erika",
            observation(UsageState::Unknown, None),
            vec![candidate("megan", UsageState::Available, None)],
            false,
            no_cooldown(5),
            10_000,
        )
        .expect("evaluate");
    assert!(matches!(outcome, WatchOutcome::NoActionNeeded { .. }));
    assert_eq!(fixture.total_calls(), 0);
}

#[test]
fn legacy_terminal_journals_and_superseded_history_never_block_new_work() {
    let fixture = Fixture::new();
    let dir = fixture.state_dir().join("handoffs");
    std::fs::create_dir_all(&dir).expect("dir");
    // A journal from an older Relay schema (no target_config_dir) that finished long ago, and an
    // older superseded RECOVERY_REQUIRED record, both preceding the current transaction.
    let stale = TransactionId::parse("ho-1-1").expect("id");
    let legacy_done = TransactionId::parse("ho-2-1").expect("id");
    std::fs::write(
        dir.join(format!("{stale}.json")),
        br#"{"version":1,"state":{"state":"RECOVERY_REQUIRED","reason":"old"}}"#,
    )
    .expect("stale");
    std::fs::write(
        dir.join(format!("{legacy_done}.json")),
        br#"{"version":1,"state":{"state":"COMPLETE"}}"#,
    )
    .expect("legacy");
    std::fs::write(
        fixture.state_dir().join("current_transaction.json"),
        legacy_done.as_str(),
    )
    .expect("pointer");
    let outcome = fixture.exhausted_erika(10_000, no_cooldown(5));
    assert!(
        matches!(outcome, WatchOutcome::Handoff { .. }),
        "{outcome:?}"
    );
}
