use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    AtomicWrite, Error, FsAtomicWriter, ProfileName, Result,
    handoff::{ContinuityType, HandoffState, ProcessIdentity, ProjectId, TransactionId},
};

/// M12: bumped from 2 to 3 to add monotonic handoff timing diagnostics. M6 bumped from 1 to 2 to add `continuity_type` (and, for `STATE_CONTINUATION` transactions,
/// `bundle_summary`). Journals are short-lived per-transaction records, not long-term config, so
/// — matching the existing version-mismatch-is-fatal design — a journal written by a pre-M6
/// Relay is simply never readable by this version rather than migrated; that only matters for a
/// transaction that was already interrupted across an upgrade, which `relay recover` already
/// treats as requiring explicit operator attention.
const JOURNAL_VERSION: u32 = 3;

/// One elapsed-time measurement from the handoff coordinator. Values are durations measured with
/// `Instant`, not wall-clock timestamps, so clock changes cannot produce negative or fabricated
/// latency. They deliberately name only coordinator phases and contain no provider output,
/// conversation content, or credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffTiming {
    pub phase: String,
    pub elapsed_ms: u64,
}

/// A sanitized wall-clock correlation point for a durable handoff state transition. Unlike
/// [`HandoffTiming`], these correlate the coordinator with the independently spawned hook and
/// terminal supervisor; they never drive recovery or any state-machine decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffStateTimestamp {
    pub state: String,
    pub unix_ms: u64,
}

/// Evidence that a [`super::ContinuationBundle`] was built and delivered to the target, without
/// persisting its content — matching docs/security.md's rule that continuation content never
/// lands in the journal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSummary {
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub branch: String,
    pub head: String,
    pub dirty: bool,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    pub target_profile: ProfileName,
    pub target_session_id: String,
    pub target_config_dir: PathBuf,
    pub started_successfully: bool,
}

/// M2C: persisted the moment the target process is actually spawned — before the coordinator
/// blocks waiting for it to finish — so a crash mid-`TARGET_STARTING` leaves durable evidence a
/// real process may exist. `relay recover` uses this to find and authoritatively stop an orphan
/// rather than ever risking a second target being launched while the first may still be alive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetLaunchRecord {
    pub process: ProcessIdentity,
    pub spawned_unix_ms: u64,
}

/// The durable per-transaction record. Every externally visible mutation the coordinator makes
/// is preceded by rewriting this journal, so `relay recover <id>` always has a trustworthy
/// account of exactly how far the transaction got.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffJournal {
    pub version: u32,
    pub transaction_id: TransactionId,
    pub project_id: ProjectId,
    pub project_dir: PathBuf,
    pub source_profile: ProfileName,
    pub target_profile: ProfileName,
    /// Known from the moment the transaction is created (unlike [`VerificationRecord`], which is
    /// only set once verification completes) — recovery needs this to supervise a target process
    /// interrupted before it was ever verified.
    pub target_config_dir: PathBuf,
    pub session_id: String,
    pub continuity_type: ContinuityType,
    pub state: HandoffState,
    pub revision: u64,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub checkpoint: Option<Checkpoint>,
    pub transferred_artifacts: Vec<ArtifactRecord>,
    /// Set only for `STATE_CONTINUATION` transactions, once the bundle has been built. Content
    /// is never stored here — see [`BundleSummary`].
    #[serde(default)]
    pub bundle_summary: Option<BundleSummary>,
    pub verification: Option<VerificationRecord>,
    #[serde(default)]
    pub target_launch: Option<TargetLaunchRecord>,
    /// Sanitized, monotonic elapsed timings for this attempt. They are diagnostics, not state
    /// machine inputs: an absent or partial list must never affect recovery.
    #[serde(default)]
    pub timings: Vec<HandoffTiming>,
    /// Durable, sanitized state-transition wall-clock correlation points.  These make a
    /// hook-triggered automatic handoff explainable across processes; monotonic phase durations
    /// remain in [`Self::timings`].
    #[serde(default)]
    pub state_timestamps: Vec<HandoffStateTimestamp>,
    /// Human-readable evidence trail. Never contains transcript contents, diffs, or secrets —
    /// only state transitions, reasons, and filenames, matching docs/security.md's event policy.
    pub notes: Vec<String>,
}

impl HandoffJournal {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transaction_id: TransactionId,
        project_id: ProjectId,
        project_dir: PathBuf,
        source_profile: ProfileName,
        target_profile: ProfileName,
        target_config_dir: PathBuf,
        session_id: String,
        continuity_type: ContinuityType,
    ) -> Self {
        let now = now_unix_ms();
        Self {
            version: JOURNAL_VERSION,
            transaction_id,
            project_id,
            project_dir,
            source_profile,
            target_profile,
            target_config_dir,
            session_id,
            continuity_type,
            state: HandoffState::Preparing,
            revision: 0,
            created_unix_ms: now,
            updated_unix_ms: now,
            checkpoint: None,
            transferred_artifacts: Vec::new(),
            bundle_summary: None,
            verification: None,
            target_launch: None,
            timings: Vec::new(),
            state_timestamps: vec![HandoffStateTimestamp {
                state: "PREPARING".to_owned(),
                unix_ms: now,
            }],
            notes: Vec::new(),
        }
    }

    /// Validates the transition against [`HandoffState::can_advance_to`], bumps the revision,
    /// and records a note. Refuses silently-wrong transitions rather than overwriting state.
    pub fn advance(&mut self, next: HandoffState, note: impl Into<String>) -> Result<()> {
        if !self.state.can_advance_to(&next) {
            return Err(Error::IllegalStateTransition {
                from: format!("{:?}", self.state),
                to: format!("{next:?}"),
            });
        }
        self.state = next;
        self.revision += 1;
        self.updated_unix_ms = now_unix_ms();
        self.state_timestamps.push(HandoffStateTimestamp {
            state: state_name(&self.state),
            unix_ms: self.updated_unix_ms,
        });
        self.notes.push(note.into());
        Ok(())
    }

    /// Records a completed or failed-attempt phase. Replacing a phase rather than appending a
    /// duplicate makes retries inside a future coordinator implementation unambiguous while
    /// keeping this journal compact.
    pub fn record_timing(&mut self, phase: impl Into<String>, elapsed: std::time::Duration) {
        let phase = phase.into();
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        if let Some(existing) = self.timings.iter_mut().find(|item| item.phase == phase) {
            existing.elapsed_ms = elapsed_ms;
        } else {
            self.timings.push(HandoffTiming { phase, elapsed_ms });
        }
    }
}

fn state_name(state: &HandoffState) -> String {
    match state {
        HandoffState::Preparing => "PREPARING".to_owned(),
        HandoffState::Checkpointed => "CHECKPOINTED".to_owned(),
        HandoffState::SourceStopping => "SOURCE_STOPPING".to_owned(),
        HandoffState::SourceStopped => "SOURCE_STOPPED".to_owned(),
        HandoffState::SessionTransferring => "SESSION_TRANSFERRING".to_owned(),
        HandoffState::SessionTransferred => "SESSION_TRANSFERRED".to_owned(),
        HandoffState::TargetStarting => "TARGET_STARTING".to_owned(),
        HandoffState::TargetVerified => "TARGET_VERIFIED".to_owned(),
        HandoffState::Complete => "COMPLETE".to_owned(),
        HandoffState::Failed { .. } => "FAILED".to_owned(),
        HandoffState::RecoveryRequired { .. } => "RECOVERY_REQUIRED".to_owned(),
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub struct JournalStore {
    path: PathBuf,
}

impl JournalStore {
    #[must_use]
    pub const fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<HandoffJournal> {
        let bytes = std::fs::read(&self.path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::TransactionNotFound(self.path.display().to_string())
            } else {
                Error::Io {
                    path: self.path.clone(),
                    source,
                }
            }
        })?;
        let journal: HandoffJournal =
            serde_json::from_slice(&bytes).map_err(|_| Error::CorruptedJournal)?;
        if journal.version != JOURNAL_VERSION {
            return Err(Error::CorruptedJournal);
        }
        Ok(journal)
    }

    pub fn save(&self, journal: &HandoffJournal) -> Result<()> {
        let text = serde_json::to_string_pretty(journal).map_err(|_| Error::SerializationFailed)?;
        FsAtomicWriter.write_atomic(&self.path, text.as_bytes())
    }
}

/// Reads branch, HEAD, dirty state, and changed filenames only — never file contents, matching
/// docs/security.md's checkpoint policy. Read-only; never mutates the project.
pub fn checkpoint_project(project_dir: &Path) -> Result<Checkpoint> {
    let head = run_git(project_dir, &["rev-parse", "HEAD"])?;
    let branch = run_git(project_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let status = run_git(project_dir, &["status", "--porcelain"])?;
    let changed_files: Vec<String> = status
        .lines()
        .filter_map(|line| line.get(3..).map(str::to_owned))
        .collect();
    Ok(Checkpoint {
        branch,
        head,
        dirty: !changed_files.is_empty(),
        changed_files,
    })
}

fn run_git(project_dir: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(args)
        .output()
        .map_err(|_| Error::ProviderCommandFailed)?;
    if !output.status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| Error::MalformedProviderOutput)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{HandoffJournal, HandoffState, JournalStore};
    use crate::{
        ProfileName,
        handoff::{ContinuityType, FailedPhase, ProjectId, TransactionId},
    };

    fn sample_journal() -> HandoffJournal {
        HandoffJournal::new(
            TransactionId::generate(),
            ProjectId::for_canonical_path(std::path::Path::new("/tmp/proj")).expect("id"),
            std::path::PathBuf::from("/tmp/proj"),
            ProfileName::new("erika").expect("name"),
            ProfileName::new("megan").expect("name"),
            std::path::PathBuf::from("/tmp/megan-config"),
            "8586fe71-395b-4449-b973-78011d561fed".to_owned(),
            ContinuityType::SessionContinuation,
        )
    }

    #[test]
    fn missing_journal_is_reported_as_transaction_not_found() {
        let root = tempdir().expect("temp dir");
        let store = JournalStore::at_path(root.path().join("txn.json"));
        let error = store.load().expect_err("must fail closed");
        assert_eq!(error.code(), "transaction_not_found");
    }

    #[test]
    fn round_trips_and_preserves_revision() {
        let root = tempdir().expect("temp dir");
        let store = JournalStore::at_path(root.path().join("txn.json"));
        let mut journal = sample_journal();
        journal.record_timing(
            "project_git_checkpoint",
            std::time::Duration::from_millis(42),
        );
        journal
            .advance(HandoffState::Checkpointed, "checkpointed")
            .expect("advance");
        store.save(&journal).expect("save");
        let loaded = store.load().expect("load");
        assert_eq!(loaded.state, HandoffState::Checkpointed);
        assert_eq!(loaded.revision, 1);
        assert_eq!(loaded.timings.len(), 1);
        assert_eq!(loaded.timings[0].phase, "project_git_checkpoint");
        assert_eq!(loaded.timings[0].elapsed_ms, 42);
        assert_eq!(
            loaded
                .state_timestamps
                .iter()
                .map(|point| point.state.as_str())
                .collect::<Vec<_>>(),
            ["PREPARING", "CHECKPOINTED"]
        );
    }

    #[test]
    fn corrupted_journal_fails_closed() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("txn.json");
        std::fs::write(&path, "{not json").expect("write garbage");
        let store = JournalStore::at_path(path);
        let error = store.load().expect_err("must fail closed");
        assert_eq!(error.code(), "corrupted_journal");
    }

    #[test]
    fn illegal_transitions_are_rejected_and_do_not_mutate_state() {
        let mut journal = sample_journal();
        let error = journal
            .advance(HandoffState::SourceStopped, "skip ahead")
            .expect_err("must reject skipping ahead");
        assert_eq!(error.code(), "illegal_state_transition");
        assert_eq!(journal.state, HandoffState::Preparing);
        assert_eq!(journal.revision, 0);
    }

    #[test]
    fn failure_can_be_recorded_from_mid_transaction() {
        let mut journal = sample_journal();
        journal
            .advance(HandoffState::Checkpointed, "checkpointed")
            .expect("advance");
        journal
            .advance(HandoffState::SourceStopping, "stopping source")
            .expect("advance");
        journal
            .advance(
                HandoffState::Failed {
                    phase: FailedPhase::Stop,
                    reason: "source still active".to_owned(),
                },
                "source refused to stop",
            )
            .expect("failure must be reachable from an in-progress state");
        assert!(journal.state.is_terminal());
    }
}
