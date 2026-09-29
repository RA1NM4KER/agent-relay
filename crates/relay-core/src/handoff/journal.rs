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
pub struct ArtifactResolutionRecord {
    pub relative_path: String,
    pub classification: String,
    pub original_target_sha256: String,
    pub replacement_sha256: String,
    pub backup_path: PathBuf,
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
    #[serde(default)]
    pub transfer_resolutions: Vec<ArtifactResolutionRecord>,
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
            transfer_resolutions: Vec::new(),
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
///
/// A Relay-managed workspace is never required to be a Git repository: a session working
/// entirely through a remote interface (e.g. MCP-driven WordPress administration) may have no
/// local repository to checkpoint at all. `Ok(None)` is exactly that — git itself confirming
/// there is nothing here to check out — never treated as a failure. Any other git failure (git
/// missing, or a real repository that genuinely cannot answer) is still `Err` and still fails the
/// handoff, exactly as before.
pub fn checkpoint_project(project_dir: &Path) -> Result<Option<Checkpoint>> {
    if !is_git_work_tree(project_dir)? {
        return Ok(None);
    }

    let head = run_git(project_dir, &["rev-parse", "HEAD"])?;
    let branch = run_git(project_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let status = run_git(project_dir, &["status", "--porcelain"])?;
    let changed_files: Vec<String> = status
        .lines()
        .filter_map(|line| line.get(3..).map(str::to_owned))
        .collect();
    Ok(Some(Checkpoint {
        branch,
        head,
        dirty: !changed_files.is_empty(),
        changed_files,
    }))
}

/// Whether `dir` is inside a Git working tree, explicitly asked of git itself
/// (`rev-parse --is-inside-work-tree`) rather than inferred from a bare `.git`-directory check —
/// the same probe [`checkpoint_project`] uses, exposed so every other Git-derived reader in Relay
/// (state-continuation repo-facts capture, in the provider crates) detects "this is not a Git
/// repository" identically rather than re-implementing it. `Ok(false)` covers both "no repository
/// here at all" and "a bare repository / inside `.git` itself" — neither has working-tree state to
/// read. Any other git failure (git missing, a real repository that cannot answer) is `Err`.
pub fn is_git_work_tree(dir: &Path) -> Result<bool> {
    let probe = run_git_raw(dir, &["rev-parse", "--is-inside-work-tree"])?;
    if !probe.success {
        if is_not_a_git_repository(&probe.stderr) {
            return Ok(false);
        }
        return Err(git_checkpoint_error(
            "rev-parse --is-inside-work-tree",
            &probe.stderr,
        ));
    }
    Ok(String::from_utf8_lossy(&probe.stdout).trim() == "true")
}

struct GitOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_git_raw(project_dir: &Path, args: &[&str]) -> Result<GitOutput> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(args)
        .output()
        .map_err(|_| {
            git_checkpoint_error(&args.join(" "), b"the git command could not be started")
        })?;
    Ok(GitOutput {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn run_git(project_dir: &Path, args: &[&str]) -> Result<String> {
    let output = run_git_raw(project_dir, args)?;
    if !output.success {
        return Err(git_checkpoint_error(&args.join(" "), &output.stderr));
    }
    String::from_utf8(output.stdout)
        // `trim_end` only: `status --porcelain`'s first line can genuinely start with a
        // meaningful leading space (e.g. `" M path"` for an unstaged-only modification) —
        // `trim()` would silently eat it and corrupt the very first parsed filename.
        .map(|text| text.trim_end().to_owned())
        .map_err(|_| Error::MalformedProviderOutput)
}

/// Distinguishes "there is no repository here" from every other git failure, matching git's own
/// stable wording for this exact case (`fatal: not a git repository (or any of the parent
/// directories): .git`) rather than inferring it from an exit code alone.
fn is_not_a_git_repository(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("not a git repository")
}

/// A sanitized, actionable [`Error::GitCheckpointFailed`]: which command failed, and (bounded, no
/// file contents, no multi-line dumps) the first line of what git said — matching
/// docs/security.md's checkpoint policy. Public so every Git-command reader across Relay (this
/// module and the provider crates' own repo-facts capture) reports the same improved error
/// vocabulary instead of the generic `Error::ProviderCommandFailed`.
pub fn git_checkpoint_error(command: &str, stderr: &[u8]) -> Error {
    const MAX_LEN: usize = 300;
    let text = String::from_utf8_lossy(stderr);
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("the command exited with a non-zero status")
        .trim();
    let detail = if first_line.chars().count() > MAX_LEN {
        first_line.chars().take(MAX_LEN).collect::<String>() + "…"
    } else {
        first_line.to_owned()
    };
    Error::GitCheckpointFailed {
        command: command.to_owned(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{HandoffJournal, HandoffState, JournalStore, checkpoint_project};
    use crate::{
        Error, ProfileName,
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

    // --- checkpoint_project ---

    fn init_git_repo(dir: &std::path::Path) {
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
        run(&["-c", "init.defaultBranch=main", "init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "hello\n").expect("seed file");
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn a_normal_git_repo_yields_a_clean_checkpoint() {
        let root = tempdir().expect("temp dir");
        let project = root.path().join("project");
        init_git_repo(&project);

        let checkpoint = checkpoint_project(&project)
            .expect("checkpoint must succeed")
            .expect("a git repository must yield a checkpoint");
        assert_eq!(checkpoint.branch, "main");
        assert_eq!(checkpoint.head.len(), 40, "a full sha1 hex digest");
        assert!(!checkpoint.dirty);
        assert!(checkpoint.changed_files.is_empty());
    }

    #[test]
    fn a_dirty_git_repo_reports_changed_files() {
        let root = tempdir().expect("temp dir");
        let project = root.path().join("project");
        init_git_repo(&project);
        std::fs::write(project.join("README.md"), "changed\n").expect("dirty the tree");
        std::fs::write(project.join("untracked.txt"), "new\n").expect("untracked file");

        let checkpoint = checkpoint_project(&project)
            .expect("checkpoint must succeed")
            .expect("a git repository must yield a checkpoint");
        assert!(checkpoint.dirty);
        assert!(checkpoint.changed_files.iter().any(|f| f == "README.md"));
        assert!(
            checkpoint
                .changed_files
                .iter()
                .any(|f| f == "untracked.txt")
        );
    }

    #[test]
    fn an_intentional_non_git_workspace_checkpoints_to_none_not_an_error() {
        // A Relay-managed workspace is never required to be a Git repository — e.g. a session
        // working entirely through a remote interface such as MCP-driven WordPress
        // administration has no local repository to checkpoint at all.
        let root = tempdir().expect("temp dir");
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");

        let checkpoint =
            checkpoint_project(&project).expect("a missing repository must not be an error");
        assert!(checkpoint.is_none());
    }

    #[test]
    fn a_genuine_git_failure_is_reported_with_the_improved_error_vocabulary_not_generic() {
        // A real repository with zero commits: `rev-parse --is-inside-work-tree` succeeds (it IS
        // a repository), but `rev-parse HEAD` genuinely fails — this must surface as an
        // actionable `GitCheckpointFailed`, never the generic `ProviderCommandFailed`, and never
        // be mistaken for "not a git repository".
        let root = tempdir().expect("temp dir");
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["-c", "init.defaultBranch=main", "init", "-q"])
            .status()
            .expect("run git init");
        assert!(status.success());

        let error = checkpoint_project(&project).expect_err("no commits means no HEAD to read");
        assert_eq!(error.code(), "git_checkpoint_failed");
        assert!(!matches!(error, Error::ProviderCommandFailed));
        match error {
            Error::GitCheckpointFailed { command, detail } => {
                assert_eq!(command, "rev-parse HEAD");
                assert!(!detail.is_empty());
            }
            other => panic!("expected GitCheckpointFailed, got {other:?}"),
        }
    }
}
