use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{Error, ProfileName, RelayPaths, Result};

use super::{
    FailedPhase, HandoffJournal, HandoffState, JournalStore, LeaseStore, OrchestrationLock,
    ProcessIdentity, ProjectId, TransactionId, WriterLease,
    journal::{ArtifactRecord, VerificationRecord, checkpoint_project},
};

/// The result of checking whether a profile's writer is still alive. `active` is the primary
/// decision (block or proceed); `untracked_session_ids` is a secondary, purely informational
/// finding — other Claude sessions visible for this profile that are neither absent nor the one
/// being handed off, i.e. a manually-started or otherwise Relay-untracked session. M2B.5 treats
/// both as blocking (see [`Error::UntrackedWriterDetected`]) rather than silently proceeding.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct LivenessVerdict {
    pub active: bool,
    pub untracked_session_ids: Vec<String>,
}

/// Read-only: is the source profile's Claude process currently running? `relay-core` does not
/// know how to launch or inspect Claude; `relay-provider-claude` supplies the real check.
///
/// `expected_session_id` is the session the coordinator expects to be handing off.
/// `recorded_owner` is the process identity from the project's current [`WriterLease`], when one
/// exists for this profile — implementations should treat it as the strongest available signal
/// (an exact pid + start-time fingerprint check) and use provider-specific session listings only
/// as corroboration/bootstrap, never as the sole source of truth (see M2B.5's design notes: a
/// provider's own session bookkeeping can lag a hard crash).
pub trait SourceLiveness: Send + Sync {
    fn check(
        &self,
        source_config_dir: &Path,
        project_dir: &Path,
        expected_session_id: &str,
        recorded_owner: Option<&ProcessIdentity>,
    ) -> Result<LivenessVerdict>;
}

/// M2B.75: issues an authoritative provider-level stop for one specific session and does not
/// return `Ok` until quiescence has been verified with multiple consecutive observations — a
/// single transient "not running" reading is never sufficient (see docs/architecture.md's
/// "never trigger a destructive transition from display text alone" and M2B.5's live finding
/// that a provider's own session bookkeeping can report a killed process as still running).
/// `recorded_owner`, when present, is used only as corroborating evidence, never as the sole
/// signal — the provider implementation must never stop a different profile's or a different
/// session's process.
pub trait SessionStopper: Send + Sync {
    fn stop_and_verify(
        &self,
        source_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
        recorded_owner: Option<&ProcessIdentity>,
    ) -> Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferredArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct TransferOutcome {
    pub artifacts: Vec<TransferredArtifact>,
}

/// Stages the session's artifacts from source to target. `relay-provider-claude` implements this
/// over the M2A `stage_transfer` function, so a project-level handoff and a bare
/// `relay session stage-transfer` share one hash-verified, divergence-checked implementation.
pub trait SessionStager: Send + Sync {
    fn stage(
        &self,
        source_config_dir: &Path,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<TransferOutcome>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetVerification {
    pub target_session_id: String,
    pub started_successfully: bool,
}

/// Launches the target under its own profile and verifies it actually resumed the expected
/// session. `relay-core` never launches processes itself.
pub trait TargetLauncher: Send + Sync {
    fn launch_and_verify(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<TargetVerification>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchOutcome {
    pub verification: TargetVerification,
}

pub struct HandoffRequest {
    pub project_dir: PathBuf,
    pub source_profile: ProfileName,
    pub source_config_dir: PathBuf,
    pub target_profile: ProfileName,
    pub target_config_dir: PathBuf,
    pub session_id: String,
}

pub struct HandoffCoordinator<'a> {
    pub paths: &'a RelayPaths,
    pub liveness: &'a dyn SourceLiveness,
    pub stopper: &'a dyn SessionStopper,
    pub stager: &'a dyn SessionStager,
    pub launcher: &'a dyn TargetLauncher,
}

impl HandoffCoordinator<'_> {
    /// Runs one complete transaction end to end, inside a single orchestration-lock acquisition
    /// so there is never an unlock/relock gap between source verification and target
    /// verification. Returns the final journal (which may describe a `Failed` transaction —
    /// that is a normal, fully-reported outcome, not a panic).
    pub fn run(&self, request: HandoffRequest) -> Result<HandoffJournal> {
        if request.source_profile == request.target_profile {
            return Err(Error::ProviderMismatch {
                expected: "distinct source and target profiles".to_owned(),
                observed: request.source_profile.to_string(),
            });
        }
        // Canonicalize before deriving the project id: two spellings (a symlink, `..`, a
        // trailing slash) of the same project must never produce two different ids, and a
        // symlink must never be able to alias one project's lock/lease onto another's.
        require_absolute(&request.project_dir)?;
        let project_dir = fs::canonicalize(&request.project_dir).map_err(|source| Error::Io {
            path: request.project_dir.clone(),
            source,
        })?;
        let project_id = ProjectId::for_canonical_path(&project_dir)?;
        let project_state_dir = self.paths.project_state_dir(&project_id);
        fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
            path: project_state_dir.clone(),
            source,
        })?;
        let handoffs_dir = project_state_dir.join("handoffs");
        fs::create_dir_all(&handoffs_dir).map_err(|source| Error::Io {
            path: handoffs_dir.clone(),
            source,
        })?;

        let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
        let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));
        let transaction_id = TransactionId::generate();
        let journal_store =
            JournalStore::at_path(handoffs_dir.join(format!("{transaction_id}.json")));
        let current_pointer = project_state_dir.join("current_transaction.json");

        lock.try_with(|| {
            self.run_locked(
                &request,
                project_id,
                &project_dir,
                transaction_id,
                &journal_store,
                &lease_store,
                &current_pointer,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn run_locked(
        &self,
        request: &HandoffRequest,
        project_id: ProjectId,
        project_dir: &Path,
        transaction_id: TransactionId,
        journal_store: &JournalStore,
        lease_store: &LeaseStore,
        current_pointer: &Path,
    ) -> Result<HandoffJournal> {
        let existing_lease = lease_store.load()?;
        if let Some(existing) = &existing_lease
            && existing.owner_profile != request.source_profile
        {
            return Err(Error::WriterLeaseOwnedByAnotherProfile(
                existing.owner_profile.to_string(),
            ));
        }
        // Only trust the lease's recorded process identity when it actually belongs to the
        // source profile we are about to check — a lease for a different (or no) profile gives
        // the liveness check nothing to corroborate against.
        let recorded_owner = existing_lease
            .as_ref()
            .filter(|lease| lease.owner_profile == request.source_profile)
            .map(|lease| lease.owner_process.clone());

        let mut journal = HandoffJournal::new(
            transaction_id,
            project_id.clone(),
            project_dir.to_path_buf(),
            request.source_profile.clone(),
            request.target_profile.clone(),
            request.session_id.clone(),
        );
        journal_store.save(&journal)?;
        write_current_pointer(current_pointer, journal.transaction_id.as_str())?;

        macro_rules! fail_and_return {
            ($phase:expr, $reason:expr, $error:expr) => {{
                let reason: String = $reason;
                journal
                    .advance(
                        HandoffState::Failed {
                            phase: $phase,
                            reason: reason.clone(),
                        },
                        reason,
                    )
                    .expect("Failed is always reachable from an in-progress state");
                journal_store.save(&journal)?;
                return Err($error);
            }};
        }

        let checkpoint = match checkpoint_project(project_dir) {
            Ok(checkpoint) => checkpoint,
            Err(error) => fail_and_return!(
                FailedPhase::Prepare,
                format!("checkpoint failed: {error}"),
                error
            ),
        };
        journal.checkpoint = Some(checkpoint);
        journal.advance(HandoffState::Checkpointed, "captured git checkpoint")?;
        journal_store.save(&journal)?;

        journal.advance(
            HandoffState::SourceStopping,
            "checking for untracked sessions and issuing an authoritative stop",
        )?;
        journal_store.save(&journal)?;
        // Untracked-session detection (M2B.5): any OTHER active session for this profile and
        // project must block, since Relay cannot safely reason about work it never launched.
        match self.liveness.check(
            &request.source_config_dir,
            project_dir,
            &request.session_id,
            recorded_owner.as_ref(),
        ) {
            Ok(verdict) if !verdict.untracked_session_ids.is_empty() => fail_and_return!(
                FailedPhase::Stop,
                format!(
                    "untracked Claude session(s) detected for source profile: {:?}",
                    verdict.untracked_session_ids
                ),
                Error::UntrackedWriterDetected(verdict.untracked_session_ids.join(", "))
            ),
            Ok(_) => {}
            Err(error) => fail_and_return!(
                FailedPhase::Stop,
                format!("liveness check failed: {error}"),
                error
            ),
        }
        // M2B.75: authoritative stop of our own session, verified quiescent across multiple
        // consecutive observations — not a single check, and not a raw kill.
        match self.stopper.stop_and_verify(
            &request.source_config_dir,
            project_dir,
            &request.session_id,
            recorded_owner.as_ref(),
        ) {
            Ok(()) => {}
            Err(error) => fail_and_return!(
                FailedPhase::Stop,
                format!("authoritative stop/verification failed: {error}"),
                error
            ),
        }
        journal.advance(
            HandoffState::SourceStopped,
            "source authoritatively stopped and verified quiescent",
        )?;
        journal_store.save(&journal)?;

        journal.advance(
            HandoffState::SessionTransferring,
            "staging session artifacts",
        )?;
        journal_store.save(&journal)?;
        let transfer = match self.stager.stage(
            &request.source_config_dir,
            &request.target_config_dir,
            project_dir,
            &request.session_id,
        ) {
            Ok(transfer) => transfer,
            Err(error) => fail_and_return!(
                FailedPhase::Transfer,
                format!("transfer failed: {error}"),
                error
            ),
        };
        journal.transferred_artifacts = transfer
            .artifacts
            .iter()
            .map(|artifact| ArtifactRecord {
                relative_path: artifact.relative_path.clone(),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
            })
            .collect();
        journal.advance(
            HandoffState::SessionTransferred,
            "artifacts staged and hash-verified",
        )?;
        journal_store.save(&journal)?;

        journal.advance(HandoffState::TargetStarting, "launching target")?;
        journal_store.save(&journal)?;
        let verification = match self.launcher.launch_and_verify(
            &request.target_config_dir,
            project_dir,
            &request.session_id,
        ) {
            Ok(verification) => verification,
            Err(error) => fail_and_return!(
                FailedPhase::TargetStart,
                format!("target failed to start: {error}"),
                error
            ),
        };

        if !verification.started_successfully
            || verification.target_session_id != request.session_id
        {
            fail_and_return!(
                FailedPhase::Verify,
                format!(
                    "target verification mismatch: started={}, session_id={}",
                    verification.started_successfully, verification.target_session_id
                ),
                Error::TargetVerificationMismatch
            );
        }
        journal.verification = Some(VerificationRecord {
            target_profile: request.target_profile.clone(),
            target_session_id: verification.target_session_id.clone(),
            target_config_dir: request.target_config_dir.clone(),
            started_successfully: true,
        });
        journal.advance(HandoffState::TargetVerified, "target verified")?;
        journal_store.save(&journal)?;

        let lease = WriterLease::new(
            project_id,
            request.target_profile.clone(),
            ProcessIdentity::current(),
            request.session_id.clone(),
            journal.transaction_id.clone(),
            now_unix_ms(),
        );
        lease_store.save(&lease)?;
        journal.advance(HandoffState::Complete, "ownership moved to target profile")?;
        journal_store.save(&journal)?;

        Ok(journal)
    }

    /// Re-acquires the same orchestration lock a running transaction would hold. If that
    /// succeeds, the original process is provably gone (the OS released its `flock`), so it is
    /// safe to decide what to do next; if it fails, a transaction is genuinely still in
    /// progress and recovery correctly refuses rather than racing it.
    pub fn recover(
        &self,
        project_state_dir: &Path,
        transaction_id: &TransactionId,
    ) -> Result<HandoffJournal> {
        let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
        let journal_store = JournalStore::at_path(
            project_state_dir
                .join("handoffs")
                .join(format!("{transaction_id}.json")),
        );
        let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));

        lock.try_with(|| {
            let mut journal = journal_store.load()?;
            match journal.state.clone() {
                HandoffState::Complete | HandoffState::Failed { .. } => Ok(journal),
                HandoffState::RecoveryRequired { .. } => Ok(journal),
                HandoffState::Preparing | HandoffState::Checkpointed => {
                    journal.advance(
                        HandoffState::Failed {
                            phase: FailedPhase::Prepare,
                            reason: "recovered before any external mutation".to_owned(),
                        },
                        "recovery: safe to abandon, nothing was mutated yet",
                    )?;
                    journal_store.save(&journal)?;
                    Ok(journal)
                }
                HandoffState::SourceStopping | HandoffState::SourceStopped => {
                    journal.advance(
                        HandoffState::Failed {
                            phase: FailedPhase::Stop,
                            reason: "recovered before transfer began".to_owned(),
                        },
                        "recovery: safe to abandon, no target artifacts were written",
                    )?;
                    journal_store.save(&journal)?;
                    Ok(journal)
                }
                HandoffState::SessionTransferring => {
                    journal.advance(
                        HandoffState::RecoveryRequired {
                            reason: "interrupted while staging the transcript; target artifacts \
                                     are hash-verified and a retry is divergence-checked, but this \
                                     requires an explicit new `relay handoff` attempt"
                                .to_owned(),
                        },
                        "recovery: requires an explicit retry",
                    )?;
                    journal_store.save(&journal)?;
                    Ok(journal)
                }
                HandoffState::SessionTransferred | HandoffState::TargetStarting => {
                    journal.advance(
                        HandoffState::RecoveryRequired {
                            reason: "interrupted before target verification completed; the \
                                     target process may or may not have started — it must not be \
                                     re-launched automatically"
                                .to_owned(),
                        },
                        "recovery: requires human confirmation before any further action",
                    )?;
                    journal_store.save(&journal)?;
                    Ok(journal)
                }
                HandoffState::TargetVerified => {
                    let already_reflects_target = lease_store
                        .load()?
                        .is_some_and(|lease| lease.owner_profile == journal.target_profile);
                    if !already_reflects_target {
                        let lease = WriterLease::new(
                            journal.project_id.clone(),
                            journal.target_profile.clone(),
                            ProcessIdentity::current(),
                            journal.session_id.clone(),
                            journal.transaction_id.clone(),
                            now_unix_ms(),
                        );
                        lease_store.save(&lease)?;
                    }
                    journal.advance(
                        HandoffState::Complete,
                        "recovery: target was already verified before the interruption",
                    )?;
                    journal_store.save(&journal)?;
                    Ok(journal)
                }
            }
        })
    }
}

fn require_absolute(path: &Path) -> Result<&Path> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Error::PathNotAbsolute(path.to_path_buf()))
    }
}

fn write_current_pointer(path: &Path, transaction_id: &str) -> Result<()> {
    use crate::{AtomicWrite, FsAtomicWriter};
    FsAtomicWriter.write_atomic(path, transaction_id.as_bytes())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
