use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{Error, ProfileName, ProviderKind, RelayPaths, Result};

use super::{
    ContinuationBundle, ContinuityType, FailedPhase, HandoffJournal, HandoffState, JournalStore,
    LeaseStore, OrchestrationLock, ProcessIdentity, ProjectId, TransactionId, WriterLease,
    journal::{
        ArtifactRecord, BundleSummary, TargetLaunchRecord, VerificationRecord, checkpoint_project,
    },
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

const ORPHAN_NOT_STOPPED_NOTE: &str = "recovery: orphan target not confirmed stopped";

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

    /// M2C: authoritatively stops a Relay-spawned *target* process that may have outlived its
    /// orchestrator (`claude -p --resume`, which `claude stop` cannot address because it is not a
    /// background session). `orphan` is the identity persisted at spawn time. Implementations may
    /// signal that exact process, but only after re-confirming its pid + start-time fingerprint
    /// still match — never a different process that reused the pid. The default falls back to the
    /// provider-level stop, which is all a provider without foreground children needs.
    fn stop_orphan_target(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
        orphan: &ProcessIdentity,
    ) -> Result<()> {
        self.stop_and_verify(target_config_dir, project_dir, session_id, Some(orphan))
    }

    /// M2C: closes the window between a target process being spawned and its identity reaching
    /// the durable journal. With no recorded identity, the provider must discover any target
    /// process still resuming this exact session under this exact profile, stop it, and confirm
    /// quiescence — or fail. Returning `Ok` means no such process remains.
    fn stop_unrecorded_targets(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
    ) -> Result<()> {
        self.stop_and_verify(target_config_dir, project_dir, session_id, None)
    }
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
///
/// Only used for [`ContinuityType::SessionContinuation`] — `STATE_CONTINUATION` never copies
/// provider-native session artifacts between providers (see [`ContextCapturer`] instead).
pub trait SessionStager: Send + Sync {
    /// `source_mode`/`target_mode` are only meaningful for Claude (see
    /// [`crate::ClaudeConfigMode`]); a stager for any other provider ignores them.
    fn stage(
        &self,
        source_config_dir: &Path,
        target_config_dir: &Path,
        project_dir: &Path,
        session_id: &str,
        source_mode: crate::ClaudeConfigMode,
        target_mode: crate::ClaudeConfigMode,
    ) -> Result<TransferOutcome>;

    /// Everything [`stage`](Self::stage) will require that can be known without writing anything,
    /// checked while the source is still running so a predictable refusal (a conflicting writer, a
    /// missing session) never costs the user their live session. `recorded_owner` is the exact
    /// source process the transaction is about to stop; it is allowed. The authoritative checks in
    /// `stage` still run after the stop.
    fn preflight(
        &self,
        _source_config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
        _recorded_owner: Option<&ProcessIdentity>,
        _source_mode: crate::ClaudeConfigMode,
    ) -> Result<()> {
        Ok(())
    }
}

/// M6: builds a provider-neutral [`ContinuationBundle`] from the SOURCE's own local state, for a
/// [`ContinuityType::StateContinuation`] transaction. Implemented per source provider
/// (`relay-provider-claude`/`relay-provider-codex`) — `relay-core` never reads provider-native
/// transcript formats itself. Runs while the source profile is still the confirmed writer (during
/// `Checkpointed`, before `SourceStopping`), so the source's own local state is read while it is
/// still known-quiescent-or-owned rather than raced against a stop.
pub trait ContextCapturer: Send + Sync {
    fn capture(
        &self,
        source_config_dir: &Path,
        project_dir: &Path,
        source_session_id: &str,
        source_profile: &ProfileName,
        source_provider: ProviderKind,
        target_provider: ProviderKind,
    ) -> Result<ContinuationBundle>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetVerification {
    pub target_session_id: String,
    pub started_successfully: bool,
}

/// What the target should do on its first turn. `relay-core` decides which variant applies (from
/// the transaction's [`ContinuityType`]); it never inspects provider identity itself.
pub enum LaunchDirective<'a> {
    /// `SESSION_CONTINUATION`: resume this exact session id, the same way a same-profile resume
    /// would.
    ResumeSession { session_id: &'a str },
    /// `STATE_CONTINUATION`: start a genuinely new session/thread, bootstrapped from this bundle.
    Bootstrap { bundle: &'a ContinuationBundle },
}

/// Launches the target under its own profile and verifies it actually started the expected
/// session (an exact resume for [`LaunchDirective::ResumeSession`], any valid new session/thread
/// id for [`LaunchDirective::Bootstrap`]). `relay-core` never launches processes itself.
///
/// `on_started` must be invoked exactly once, immediately after the target process is spawned
/// and before any long blocking wait for it to finish — the coordinator uses it to persist a
/// [`super::TargetLaunchRecord`] to the durable journal atomically with (never after) the actual
/// spawn. This is M2C's fix for the orphan-target risk the M2B.75 soak test found: if the
/// coordinator's own process is killed while waiting for the target, the journal already proves a
/// real process may exist, so `relay recover` knows to find and authoritatively stop it rather
/// than risk a second target being launched while the first may still be alive. An implementation
/// that cannot determine a pid should still call `on_started` with `None` rather than skip it.
pub trait TargetLauncher: Send + Sync {
    fn launch_and_verify(
        &self,
        target_config_dir: &Path,
        project_dir: &Path,
        directive: &LaunchDirective<'_>,
        on_started: &mut dyn FnMut(Option<ProcessIdentity>) -> Result<()>,
    ) -> Result<TargetVerification>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchOutcome {
    pub verification: TargetVerification,
}

pub struct HandoffRequest {
    pub project_dir: PathBuf,
    pub source_profile: ProfileName,
    pub source_provider: ProviderKind,
    pub source_config_dir: PathBuf,
    pub target_profile: ProfileName,
    pub target_provider: ProviderKind,
    pub target_config_dir: PathBuf,
    pub session_id: String,
    pub continuity_type: ContinuityType,
    /// The Relay Session's own state directory (lease, journals, lock). `None` keeps the legacy
    /// project-level directory (used only by low-level tests).
    pub state_dir: Option<PathBuf>,
    /// Only meaningful when the respective side is Claude; see [`crate::ClaudeConfigMode`].
    /// Defaults preserved for every existing construction site via `..Default::default()`-style
    /// literal fields would be nicer, but `HandoffRequest` has no `Default`; every constructor
    /// simply states `relay_core::ClaudeConfigMode::Explicit` when the mode is not yet threaded
    /// through that call site.
    pub source_claude_mode: crate::ClaudeConfigMode,
    pub target_claude_mode: crate::ClaudeConfigMode,
}

pub struct HandoffCoordinator<'a> {
    pub paths: &'a RelayPaths,
    pub liveness: &'a dyn SourceLiveness,
    /// Stops the SOURCE's writer. For a same-provider transaction this is the same provider as
    /// `target_stopper`; for a cross-provider one it is NOT — never assume one `SessionStopper`
    /// implementation can serve both roles.
    pub source_stopper: &'a dyn SessionStopper,
    /// Stops an orphaned TARGET process during crash recovery
    /// ([`Self::recover`]'s `supervise_orphan_target`) — a target process may exist under a
    /// different provider than the source, so this must be the TARGET's provider's stopper, not
    /// necessarily the same instance as `source_stopper`. Unused on the ordinary (non-crash)
    /// path, which never stops a target.
    pub target_stopper: &'a dyn SessionStopper,
    /// Required when `continuity_type` is [`ContinuityType::SessionContinuation`]; unused
    /// otherwise. `run` fails closed with [`Error::MissingHandoffPort`] if the wrong one is
    /// absent for the requested continuity type.
    pub stager: Option<&'a dyn SessionStager>,
    /// Required when `continuity_type` is [`ContinuityType::StateContinuation`]; unused
    /// otherwise.
    pub context_capturer: Option<&'a dyn ContextCapturer>,
    pub launcher: &'a dyn TargetLauncher,
}

/// A failed handoff together with the durable journal written before the transaction lock was
/// released. Automation uses the journal to distinguish a safely-dead launch target from an
/// ambiguous failure; ordinary callers can continue using [`HandoffCoordinator::run`] and see
/// the original [`Error`].
pub struct HandoffFailure {
    pub error: Error,
    pub journal: Option<Box<HandoffJournal>>,
}

impl HandoffCoordinator<'_> {
    /// Runs one complete transaction end to end, inside a single orchestration-lock acquisition
    /// so there is never an unlock/relock gap between source verification and target
    /// verification. Returns the final journal (which may describe a `Failed` transaction —
    /// that is a normal, fully-reported outcome, not a panic).
    pub fn run(&self, request: HandoffRequest) -> Result<HandoffJournal> {
        self.run_detailed(request).map_err(|failure| failure.error)
    }

    /// Runs the same transaction as [`Self::run`], but preserves the failed journal while the
    /// orchestration lock is still held. This makes a bounded automatic fallback cascade safe:
    /// the caller never has to race another transaction while rediscovering how far this one got.
    pub fn run_detailed(
        &self,
        request: HandoffRequest,
    ) -> std::result::Result<HandoffJournal, HandoffFailure> {
        if request.source_profile == request.target_profile {
            return Err(HandoffFailure {
                error: Error::ProviderMismatch {
                    expected: "distinct source and target profiles".to_owned(),
                    observed: request.source_profile.to_string(),
                },
                journal: None,
            });
        }
        // Canonicalize before deriving the project id: two spellings (a symlink, `..`, a
        // trailing slash) of the same project must never produce two different ids, and a
        // symlink must never be able to alias one project's lock/lease onto another's.
        require_absolute(&request.project_dir).map_err(|error| HandoffFailure {
            error,
            journal: None,
        })?;
        let project_dir = fs::canonicalize(&request.project_dir)
            .map_err(|source| Error::Io {
                path: request.project_dir.clone(),
                source,
            })
            .map_err(|error| HandoffFailure {
                error,
                journal: None,
            })?;
        let project_id =
            ProjectId::for_canonical_path(&project_dir).map_err(|error| HandoffFailure {
                error,
                journal: None,
            })?;
        let project_state_dir = request
            .state_dir
            .clone()
            .unwrap_or_else(|| self.paths.project_state_dir(&project_id));
        fs::create_dir_all(&project_state_dir)
            .map_err(|source| Error::Io {
                path: project_state_dir.clone(),
                source,
            })
            .map_err(|error| HandoffFailure {
                error,
                journal: None,
            })?;
        let handoffs_dir = project_state_dir.join("handoffs");
        fs::create_dir_all(&handoffs_dir)
            .map_err(|source| Error::Io {
                path: handoffs_dir.clone(),
                source,
            })
            .map_err(|error| HandoffFailure {
                error,
                journal: None,
            })?;

        let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
        let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));
        let transaction_id = TransactionId::generate();
        let journal_store =
            JournalStore::at_path(handoffs_dir.join(format!("{transaction_id}.json")));
        let current_pointer = project_state_dir.join("current_transaction.json");

        match lock.try_with(|| {
            let result = self.run_locked(
                &request,
                project_id,
                &project_dir,
                transaction_id,
                &journal_store,
                &lease_store,
                &current_pointer,
            );
            Ok(match result {
                Ok(journal) => Ok(journal),
                Err(error) => Err(HandoffFailure {
                    error,
                    journal: journal_store.load().ok().map(Box::new),
                }),
            })
        }) {
            Ok(result) => result,
            Err(error) => Err(HandoffFailure {
                error,
                journal: None,
            }),
        }
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
        // M2C: never start a fresh transaction while a prior one for this project is stuck in
        // RecoveryRequired — that state means a target process may still exist from an earlier
        // interrupted TARGET_STARTING, and starting a new transaction could spawn a second one
        // while the first is still alive. An explicit `relay recover` must resolve it first.
        if let Ok(existing_id) = fs::read_to_string(current_pointer)
            && let Ok(parsed_id) = TransactionId::parse(existing_id.trim())
        {
            let prior_store = JournalStore::at_path(
                journal_store
                    .path()
                    .parent()
                    .expect("handoffs dir")
                    .join(format!("{parsed_id}.json")),
            );
            if let Ok(prior_journal) = prior_store.load()
                && matches!(prior_journal.state, HandoffState::RecoveryRequired { .. })
            {
                return Err(Error::PendingRecoveryRequired(parsed_id.to_string()));
            }
        }

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
            request.target_config_dir.clone(),
            request.session_id.clone(),
            request.continuity_type,
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

        // M6: for STATE_CONTINUATION, the provider-neutral bundle is captured now — while the
        // source is still the confirmed writer, before it is stopped — never after, so the
        // source's own local state is read at a known-owned moment rather than raced against
        // the stop below. Only a hash/size summary is persisted to the journal; the bundle
        // itself lives only in this process's memory until the target is launched.
        let captured_bundle = match request.continuity_type {
            ContinuityType::SessionContinuation | ContinuityType::NativeResume => None,
            ContinuityType::StateContinuation => {
                let Some(capturer) = self.context_capturer else {
                    fail_and_return!(
                        FailedPhase::Prepare,
                        "no context capturer configured for a state-continuation transaction"
                            .to_owned(),
                        Error::MissingHandoffPort("context_capturer".to_owned())
                    );
                };
                let bundle = match capturer.capture(
                    &request.source_config_dir,
                    project_dir,
                    &request.session_id,
                    &request.source_profile,
                    request.source_provider,
                    request.target_provider,
                ) {
                    Ok(bundle) => bundle,
                    Err(error) => fail_and_return!(
                        FailedPhase::Prepare,
                        format!("context capture failed: {error}"),
                        error
                    ),
                };
                let serialized = match serde_json::to_vec(&bundle) {
                    Ok(bytes) => bytes,
                    Err(_) => fail_and_return!(
                        FailedPhase::Prepare,
                        "context bundle could not be serialized".to_owned(),
                        Error::SerializationFailed
                    ),
                };
                let mut hasher = Sha256::new();
                hasher.update(&serialized);
                journal.bundle_summary = Some(BundleSummary {
                    sha256: format!("{:x}", hasher.finalize()),
                    size_bytes: serialized.len() as u64,
                });
                journal_store.save(&journal)?;
                Some(bundle)
            }
        };

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
        // Everything staging will demand is checked NOW, while the source still runs: a
        // predictable refusal must not cost the user their live session. (`stage` re-checks after
        // the stop; this only avoids a destructive failure that could have been foreseen.)
        if let (ContinuityType::SessionContinuation, Some(stager)) =
            (&request.continuity_type, self.stager)
            && let Err(error) = stager.preflight(
                &request.source_config_dir,
                project_dir,
                &request.session_id,
                recorded_owner.as_ref(),
                request.source_claude_mode,
            )
        {
            fail_and_return!(
                FailedPhase::Stop,
                format!("preflight refused before the source was stopped: {error}"),
                error
            );
        }
        // M2B.75: authoritative stop of our own session, verified quiescent across multiple
        // consecutive observations — not a single check, and not a raw kill.
        match self.source_stopper.stop_and_verify(
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
            match request.continuity_type {
                ContinuityType::StateContinuation => "context bundle already captured",
                _ => "staging session artifacts",
            },
        )?;
        journal_store.save(&journal)?;
        match request.continuity_type {
            ContinuityType::SessionContinuation => {
                let Some(stager) = self.stager else {
                    fail_and_return!(
                        FailedPhase::Transfer,
                        "no session stager configured for a session-continuation transaction"
                            .to_owned(),
                        Error::MissingHandoffPort("stager".to_owned())
                    );
                };
                let transfer = match stager.stage(
                    &request.source_config_dir,
                    &request.target_config_dir,
                    project_dir,
                    &request.session_id,
                    request.source_claude_mode,
                    request.target_claude_mode,
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
            }
            ContinuityType::StateContinuation | ContinuityType::NativeResume => {}
        }
        journal.advance(
            HandoffState::SessionTransferred,
            match request.continuity_type {
                ContinuityType::StateContinuation => "context bundle ready for the target",
                _ => "artifacts staged and hash-verified",
            },
        )?;
        journal_store.save(&journal)?;

        journal.advance(HandoffState::TargetStarting, "launching target")?;
        journal_store.save(&journal)?;
        let mut on_started = |process: Option<ProcessIdentity>| -> Result<()> {
            if let Some(process) = process {
                journal.target_launch = Some(TargetLaunchRecord {
                    process,
                    spawned_unix_ms: now_unix_ms(),
                });
                journal_store.save(&journal)?;
            }
            Ok(())
        };
        let directive = match (&request.continuity_type, &captured_bundle) {
            (ContinuityType::StateContinuation, Some(bundle)) => {
                LaunchDirective::Bootstrap { bundle }
            }
            _ => LaunchDirective::ResumeSession {
                session_id: &request.session_id,
            },
        };
        let verification = match self.launcher.launch_and_verify(
            &request.target_config_dir,
            project_dir,
            &directive,
            &mut on_started,
        ) {
            Ok(verification) => verification,
            Err(error) => fail_and_return!(
                FailedPhase::TargetStart,
                format!("target failed to start: {error}"),
                error
            ),
        };

        let verification_ok = match request.continuity_type {
            ContinuityType::StateContinuation => {
                verification.started_successfully && !verification.target_session_id.is_empty()
            }
            ContinuityType::SessionContinuation | ContinuityType::NativeResume => {
                verification.started_successfully
                    && verification.target_session_id == request.session_id
            }
        };
        if !verification_ok {
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

        // The lease always records the TARGET's actual session/thread id — for
        // `SESSION_CONTINUATION` that is provably equal to `request.session_id` (checked above);
        // for `STATE_CONTINUATION` it is the new id the target itself reported, never the
        // source's.
        let lease = WriterLease::new(
            project_id,
            request.target_profile.clone(),
            ProcessIdentity::current(),
            verification.target_session_id.clone(),
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
                HandoffState::RecoveryRequired { .. }
                    if !journal
                        .notes
                        .iter()
                        .any(|note| note == ORPHAN_NOT_STOPPED_NOTE) =>
                {
                    Ok(journal)
                }
                HandoffState::RecoveryRequired { .. } => {
                    // The earlier attempt to stop the target process did not confirm success,
                    // possibly transiently. Stopping is idempotent; try again.
                    journal.advance(
                        HandoffState::TargetStarting,
                        "recovery: retrying orphan target supervision",
                    )?;
                    self.supervise_orphan_target(journal, &journal_store, &lease_store)
                }
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
                    // Staging writes each artifact atomically and hash-verifies it, so an
                    // interruption leaves either nothing or a complete file; a retry is
                    // divergence-checked. Leaving this in RecoveryRequired would only block the
                    // project for no safety benefit.
                    journal.advance(
                        HandoffState::Failed {
                            phase: FailedPhase::Transfer,
                            reason: "interrupted while staging the transcript; staged artifacts \
                                     are atomic and hash-verified, and a retry is \
                                     divergence-checked"
                                .to_owned(),
                        },
                        "recovery: safe to retry the handoff",
                    )?;
                    journal_store.save(&journal)?;
                    Ok(journal)
                }
                HandoffState::SessionTransferred | HandoffState::TargetStarting => {
                    self.supervise_orphan_target(journal, &journal_store, &lease_store)
                }
                HandoffState::TargetVerified => {
                    let already_reflects_target = lease_store
                        .load()?
                        .is_some_and(|lease| lease.owner_profile == journal.target_profile);
                    if !already_reflects_target {
                        // `verification` is always `Some` once `TargetVerified` is reached (set
                        // immediately before that transition) and carries the TARGET's actual
                        // session/thread id, which for STATE_CONTINUATION differs from
                        // `journal.session_id` (the source's).
                        let target_session_id = journal.verification.as_ref().map_or_else(
                            || journal.session_id.clone(),
                            |v| v.target_session_id.clone(),
                        );
                        let lease = WriterLease::new(
                            journal.project_id.clone(),
                            journal.target_profile.clone(),
                            ProcessIdentity::current(),
                            target_session_id,
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

impl HandoffCoordinator<'_> {
    /// Recovery for a transaction interrupted at or after transfer but before target
    /// verification. Runs under the orchestration lock held by [`Self::recover`].
    fn supervise_orphan_target(
        &self,
        mut journal: HandoffJournal,
        journal_store: &JournalStore,
        lease_store: &LeaseStore,
    ) -> Result<HandoffJournal> {
        // M2C: if a target process was spawned before the interruption its identity
        // was persisted at spawn time (`TargetLauncher`'s `on_started` contract), so
        // it is discovered from the journal, never guessed at. It is authoritatively
        // stopped and confirmed gone first; only then is the target re-verified. The
        // target transcript is never rewritten here, so every turn the orphan wrote
        // is preserved and simply resumed from.
        let stop_result = match journal.target_launch.clone() {
            Some(record) => self
                .target_stopper
                .stop_orphan_target(
                    &journal.target_config_dir,
                    &journal.project_dir,
                    &journal.session_id,
                    &record.process,
                )
                .map_err(|error| (format!("pid {}", record.process.pid), error)),
            None => self
                .target_stopper
                .stop_unrecorded_targets(
                    &journal.target_config_dir,
                    &journal.project_dir,
                    &journal.session_id,
                )
                .map_err(|error| ("unrecorded".to_owned(), error)),
        };
        if let Err((which, error)) = stop_result {
            journal.advance(
                HandoffState::RecoveryRequired {
                    reason: format!(
                        "interrupted during target startup; the target process ({which}) could \
                         not be confirmed stopped ({error}) — no second target was started; \
                         retry `relay recover`, or after confirming no claude process is \
                         resuming this session run `relay recover --acknowledge`"
                    ),
                },
                ORPHAN_NOT_STOPPED_NOTE,
            )?;
            journal_store.save(&journal)?;
            return Ok(journal);
        }
        journal.notes.push(
            "recovery: no target process remains (stopped and confirmed quiescent)".to_owned(),
        );

        // M6: a STATE_CONTINUATION's ContinuationBundle is deliberately never persisted (see
        // docs/security.md) — it lived only in the memory of the process that crashed. This
        // recovery process cannot reconstruct it (the source may since have changed, or may
        // already be stopped/gone), so it must not fabricate a fresh capture and must not guess.
        // The orphan is already confirmed stopped above, so nothing unsafe is left running; the
        // operator resolves this explicitly (confirm, then retry `relay switch` as a new
        // transaction) rather than Relay silently retrying with different content than what was
        // verified never happened.
        if journal.continuity_type == ContinuityType::StateContinuation {
            journal.advance(
                HandoffState::RecoveryRequired {
                    reason: "interrupted during a state-continuation target launch; the orphan \
                             target was stopped and confirmed gone, but the continuation bundle \
                             cannot be durably reconstructed, so the target was not relaunched \
                             automatically — run `relay recover --acknowledge` after confirming \
                             the target is not running, then retry `relay switch`"
                        .to_owned(),
                },
                "recovery: state-continuation bundle not durably recoverable",
            )?;
            journal_store.save(&journal)?;
            return Ok(journal);
        }

        if journal.state == HandoffState::SessionTransferred {
            journal.advance(
                HandoffState::TargetStarting,
                "recovery: re-verifying target",
            )?;
        }
        journal_store.save(&journal)?;
        let target_config_dir = journal.target_config_dir.clone();
        let project_dir = journal.project_dir.clone();
        let session_id = journal.session_id.clone();
        let mut on_started = |process: Option<ProcessIdentity>| -> Result<()> {
            if let Some(process) = process {
                journal.target_launch = Some(TargetLaunchRecord {
                    process,
                    spawned_unix_ms: now_unix_ms(),
                });
                journal_store.save(&journal)?;
            }
            Ok(())
        };
        let directive = LaunchDirective::ResumeSession {
            session_id: &session_id,
        };
        let verification = self.launcher.launch_and_verify(
            &target_config_dir,
            &project_dir,
            &directive,
            &mut on_started,
        );
        match verification {
            Ok(verification)
                if verification.started_successfully
                    && verification.target_session_id == journal.session_id =>
            {
                journal.verification = Some(VerificationRecord {
                    target_profile: journal.target_profile.clone(),
                    target_session_id: verification.target_session_id.clone(),
                    target_config_dir: journal.target_config_dir.clone(),
                    started_successfully: true,
                });
                journal.advance(
                    HandoffState::TargetVerified,
                    "recovery: target re-verified after stopping the orphan",
                )?;
                journal_store.save(&journal)?;
                let lease = WriterLease::new(
                    journal.project_id.clone(),
                    journal.target_profile.clone(),
                    ProcessIdentity::current(),
                    verification.target_session_id,
                    journal.transaction_id.clone(),
                    now_unix_ms(),
                );
                lease_store.save(&lease)?;
                journal.advance(
                    HandoffState::Complete,
                    "recovery: ownership moved to target profile; orphan turns preserved",
                )?;
                journal_store.save(&journal)?;
                Ok(journal)
            }
            other => {
                let detail = match other {
                    Ok(_) => "target verification mismatch".to_owned(),
                    Err(error) => error.to_string(),
                };
                journal.advance(
                    HandoffState::Failed {
                        phase: FailedPhase::TargetStart,
                        reason: format!(
                            "orphan target stopped, but re-verification failed: \
                             {detail}; the writer lease was not moved"
                        ),
                    },
                    "recovery: re-verification failed",
                )?;
                journal_store.save(&journal)?;
                Ok(journal)
            }
        }
    }
}

impl HandoffCoordinator<'_> {
    /// Explicit operator resolution of a `RecoveryRequired` transaction: records that the operator
    /// has confirmed no target process is still running, and moves the transaction to `Failed` so
    /// the project is no longer blocked from starting a new handoff. Refuses any other state.
    pub fn acknowledge_recovery(
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
        lock.try_with(|| {
            let mut journal = journal_store.load()?;
            let HandoffState::RecoveryRequired { reason } = journal.state.clone() else {
                return Err(Error::IllegalStateTransition {
                    from: format!("{:?}", journal.state),
                    to: "acknowledged recovery".to_owned(),
                });
            };
            journal.advance(
                HandoffState::Failed {
                    phase: FailedPhase::TargetStart,
                    reason: format!("operator acknowledged recovery: {reason}"),
                },
                "recovery acknowledged by operator",
            )?;
            journal_store.save(&journal)?;
            Ok(journal)
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
