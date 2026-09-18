//! M2B.5 part B: explicit, safe resolution for a target profile that already holds a session
//! transcript conflicting with what would be staged from the source — the exact situation M2B's
//! reverse-handoff live test hit (Erika's directory still held her own stale pre-handoff copy),
//! which at the time required a human to delete the file manually. This module lets Relay
//! classify and resolve that safely instead.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use relay_core::{AtomicWrite, Error, FsAtomicWriter, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::SystemProcessLister;
use crate::session_transfer::{escape_project_path, stage_transfer, validate_session_id};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConflictClassification {
    /// No target artifact exists yet: an ordinary stage is safe.
    TargetMissing,
    /// Target is byte-identical to source: nothing to do.
    TargetIdentical,
    /// Target's bytes are an exact prefix of source's — an earlier, superseded snapshot of the
    /// same append-only session log. Safe to replace (with a backup); no unique data is lost.
    TargetStaleAncestor {
        target_sha256: String,
        target_size_bytes: u64,
    },
    /// Target differs from source and is not a prefix of it (or source is a prefix of target,
    /// meaning target has turns source does not) — genuinely conflicting or ahead. Contains data
    /// that would be lost by a naive overwrite; never auto-resolved.
    TargetDivergent {
        target_sha256: String,
        target_size_bytes: u64,
    },
    /// The target profile currently has this session active — must not be touched at all.
    TargetActive,
}

impl ConflictClassification {
    #[must_use]
    pub const fn is_safe_to_replace(&self) -> bool {
        matches!(self, Self::TargetStaleAncestor { .. })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConflictReport {
    pub relative_path: String,
    pub source_sha256: String,
    pub source_size_bytes: u64,
    pub classification: ConflictClassification,
}

/// Read-only: classifies the target's state relative to the source. Makes no filesystem changes.
/// `target_active` must come from a real liveness check (see `ClaudeSourceLiveness`) — this
/// function does not itself check process liveness.
pub fn inspect_conflict(
    source_config_dir: &Path,
    target_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    target_active: bool,
) -> Result<ConflictReport> {
    validate_session_id(session_id)?;
    let relative_path = format!(
        "projects/{}/{session_id}.jsonl",
        escape_project_path(project_dir)
    );
    let source_path = source_config_dir.join(&relative_path);
    let source_bytes = fs::read(&source_path).map_err(|_| Error::SessionNotFound)?;
    let source_sha256 = sha256_hex(&source_bytes);
    let target_path = target_config_dir.join(&relative_path);

    let classification = if target_active {
        ConflictClassification::TargetActive
    } else {
        match fs::read(&target_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ConflictClassification::TargetMissing
            }
            Err(source) => {
                return Err(Error::Io {
                    path: target_path,
                    source,
                });
            }
            Ok(target_bytes) if target_bytes == source_bytes => {
                ConflictClassification::TargetIdentical
            }
            Ok(target_bytes) if source_bytes.starts_with(&target_bytes) => {
                ConflictClassification::TargetStaleAncestor {
                    target_sha256: sha256_hex(&target_bytes),
                    target_size_bytes: target_bytes.len() as u64,
                }
            }
            Ok(target_bytes) => ConflictClassification::TargetDivergent {
                target_sha256: sha256_hex(&target_bytes),
                target_size_bytes: target_bytes.len() as u64,
            },
        }
    };
    Ok(ConflictReport {
        relative_path,
        source_sha256,
        source_size_bytes: source_bytes.len() as u64,
        classification,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveDecision {
    /// Preview only; never writes.
    Preview,
    /// Allowed to stage a missing target or replace a provably-safe stale ancestor.
    Confirm,
    /// Additionally allowed to discard a genuinely divergent target (still backed up first).
    ForceDiscardDivergent,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConflictResolution {
    pub report: ConflictReport,
    pub action: String,
    pub backup_path: Option<PathBuf>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolutionRecord {
    version: u32,
    relative_path: String,
    classification: ConflictClassification,
    action: String,
    backup_path: Option<PathBuf>,
    resolved_unix_ms: u64,
}

const RESOLUTION_RECORD_VERSION: u32 = 1;

/// Classifies, then — only for the decision level actually granted — resolves. Never overwrites
/// a divergent target without [`ResolveDecision::ForceDiscardDivergent`]; never touches an
/// active target at all. Every actual replacement backs up the displaced file first (mode 0600,
/// atomic write, hash-verified) and writes a resolution record next to it for audit/rollback.
pub fn resolve_conflict(
    source_config_dir: &Path,
    target_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    target_active: bool,
    decision: ResolveDecision,
) -> Result<ConflictResolution> {
    let report = inspect_conflict(
        source_config_dir,
        target_config_dir,
        project_dir,
        session_id,
        target_active,
    )?;
    let dry_run = matches!(decision, ResolveDecision::Preview);
    let target_path = target_config_dir.join(&report.relative_path);

    let (action, backup_path) = match &report.classification {
        ConflictClassification::TargetActive => {
            return Err(Error::ConflictRequiresResolution(
                "target session is currently active and cannot be touched".to_owned(),
            ));
        }
        ConflictClassification::TargetMissing => {
            if dry_run {
                ("would_stage".to_owned(), None)
            } else {
                stage_transfer(
                    &SystemProcessLister,
                    source_config_dir,
                    target_config_dir,
                    project_dir,
                    session_id,
                )?;
                ("staged".to_owned(), None)
            }
        }
        ConflictClassification::TargetIdentical => ("no_op_identical".to_owned(), None),
        ConflictClassification::TargetStaleAncestor { .. } => {
            if dry_run {
                ("would_replace_stale_ancestor".to_owned(), None)
            } else if matches!(
                decision,
                ResolveDecision::Confirm | ResolveDecision::ForceDiscardDivergent
            ) {
                let backup = backup_and_replace(
                    source_config_dir,
                    target_config_dir,
                    &target_path,
                    project_dir,
                    session_id,
                )?;
                ("backed_up_and_replaced".to_owned(), Some(backup))
            } else {
                return Err(Error::ConflictRequiresResolution(
                    "target is a known-stale ancestor; pass Confirm to replace it".to_owned(),
                ));
            }
        }
        ConflictClassification::TargetDivergent { .. } => {
            if dry_run {
                ("would_require_force_discard".to_owned(), None)
            } else if matches!(decision, ResolveDecision::ForceDiscardDivergent) {
                let backup = backup_and_replace(
                    source_config_dir,
                    target_config_dir,
                    &target_path,
                    project_dir,
                    session_id,
                )?;
                ("backed_up_and_replaced_divergent".to_owned(), Some(backup))
            } else {
                return Err(Error::ConflictRequiresResolution(
                    "target contains unique turns; ForceDiscardDivergent is required".to_owned(),
                ));
            }
        }
    };

    if !dry_run {
        write_resolution_record(
            target_config_dir,
            project_dir,
            session_id,
            &report,
            &action,
            backup_path.as_deref(),
        )?;
    }

    Ok(ConflictResolution {
        report,
        action,
        backup_path,
        dry_run,
    })
}

fn backup_dir(target_config_dir: &Path, project_dir: &Path) -> PathBuf {
    target_config_dir
        .join("relay-backups")
        .join(escape_project_path(project_dir))
}

fn backup_and_replace(
    source_config_dir: &Path,
    target_config_dir: &Path,
    target_path: &Path,
    project_dir: &Path,
    session_id: &str,
) -> Result<PathBuf> {
    let relative_path = format!(
        "projects/{}/{session_id}.jsonl",
        escape_project_path(project_dir)
    );
    let source_bytes =
        fs::read(source_config_dir.join(&relative_path)).map_err(|source| Error::Io {
            path: source_config_dir.join(&relative_path),
            source,
        })?;
    let existing_target_bytes = fs::read(target_path).map_err(|source| Error::Io {
        path: target_path.to_path_buf(),
        source,
    })?;

    let dir = backup_dir(target_config_dir, project_dir);
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let backup_path = dir.join(format!("{session_id}.{timestamp}.jsonl"));

    // Preserve the displaced content before touching the target at all.
    FsAtomicWriter.write_atomic(&backup_path, &existing_target_bytes)?;
    let backed_up = fs::read(&backup_path).map_err(|source| Error::Io {
        path: backup_path.clone(),
        source,
    })?;
    if sha256_hex(&backed_up) != sha256_hex(&existing_target_bytes) {
        return Err(Error::TransferVerificationFailed);
    }

    FsAtomicWriter.write_atomic(target_path, &source_bytes)?;
    let written = fs::read(target_path).map_err(|source| Error::Io {
        path: target_path.to_path_buf(),
        source,
    })?;
    if sha256_hex(&written) != sha256_hex(&source_bytes) {
        return Err(Error::TransferVerificationFailed);
    }

    Ok(backup_path)
}

fn write_resolution_record(
    target_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    report: &ConflictReport,
    action: &str,
    backup_path: Option<&Path>,
) -> Result<()> {
    let dir = backup_dir(target_config_dir, project_dir);
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    let record = ResolutionRecord {
        version: RESOLUTION_RECORD_VERSION,
        relative_path: report.relative_path.clone(),
        classification: report.classification.clone(),
        action: action.to_owned(),
        backup_path: backup_path.map(Path::to_path_buf),
        resolved_unix_ms: timestamp,
    };
    let text = serde_json::to_string_pretty(&record).map_err(|_| Error::SerializationFailed)?;
    let path = dir.join(format!("{session_id}.{timestamp}.resolution.json"));
    FsAtomicWriter.write_atomic(&path, text.as_bytes())
}

/// Restores the most recent backup for this session back to the target path, if one exists.
/// Hash-verifies the restored content before reporting success. `target_active` must come from a
/// real liveness check (see `ClaudeSourceLiveness`), exactly as `resolve_conflict` requires:
/// a target session currently in use must never be touched, and a naive rollback overwriting a
/// live writer's transcript out from under it is exactly as destructive as a naive resolve would
/// be — this refusal is not optional cleanup, it is the same safety invariant `resolve_conflict`
/// already enforces, applied to the one write path that had been missing it.
pub fn rollback_conflict(
    target_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    target_active: bool,
) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    if target_active {
        return Err(Error::ConflictRequiresResolution(
            "target session is currently active and cannot be touched".to_owned(),
        ));
    }
    let dir = backup_dir(target_config_dir, project_dir);
    let prefix = format!("{session_id}.");
    let mut candidates: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|_| Error::NoBackupToRestore)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".jsonl"))
        })
        .collect();
    candidates.sort();
    let latest = candidates.pop().ok_or(Error::NoBackupToRestore)?;
    let backup_bytes = fs::read(&latest).map_err(|source| Error::Io {
        path: latest.clone(),
        source,
    })?;

    let relative_path = format!(
        "projects/{}/{session_id}.jsonl",
        escape_project_path(project_dir)
    );
    let target_path = target_config_dir.join(&relative_path);
    FsAtomicWriter.write_atomic(&target_path, &backup_bytes)?;
    let written = fs::read(&target_path).map_err(|source| Error::Io {
        path: target_path.clone(),
        source,
    })?;
    if sha256_hex(&written) != sha256_hex(&backup_bytes) {
        return Err(Error::TransferVerificationFailed);
    }
    Ok(target_path)
}

/// Reads back a resolution record; fails closed on anything malformed rather than guessing at
/// prior state. Exposed for tests and for a future `conflict history` inspection command.
#[cfg(test)]
fn read_resolution_record(path: &Path) -> Result<()> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let record: ResolutionRecord =
        serde_json::from_slice(&bytes).map_err(|_| Error::CorruptedConflictMetadata)?;
    if record.version != RESOLUTION_RECORD_VERSION {
        return Err(Error::CorruptedConflictMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        ConflictClassification, ResolveDecision, inspect_conflict, read_resolution_record,
        resolve_conflict, rollback_conflict,
    };
    use crate::escape_project_path;

    fn seed(
        config_dir: &std::path::Path,
        project_dir: &std::path::Path,
        session_id: &str,
        bytes: &[u8],
    ) {
        let dir = config_dir
            .join("projects")
            .join(escape_project_path(project_dir));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(format!("{session_id}.jsonl")), bytes).expect("write");
    }

    const SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn classifies_missing_target() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\n");
        std::fs::create_dir_all(&target).expect("target dir");

        let report =
            inspect_conflict(&source, &target, &project, SESSION_ID, false).expect("inspect");
        assert_eq!(report.classification, ConflictClassification::TargetMissing);
    }

    #[test]
    fn classifies_identical_target() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"line1\nline2\n");

        let report =
            inspect_conflict(&source, &target, &project, SESSION_ID, false).expect("inspect");
        assert_eq!(
            report.classification,
            ConflictClassification::TargetIdentical
        );
    }

    #[test]
    fn classifies_stale_ancestor() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\nline3\n");
        seed(&target, &project, SESSION_ID, b"line1\n");

        let report =
            inspect_conflict(&source, &target, &project, SESSION_ID, false).expect("inspect");
        assert!(matches!(
            report.classification,
            ConflictClassification::TargetStaleAncestor { .. }
        ));
    }

    #[test]
    fn classifies_divergent_when_target_has_unique_content() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"line1\nDIFFERENT\n");

        let report =
            inspect_conflict(&source, &target, &project, SESSION_ID, false).expect("inspect");
        assert!(matches!(
            report.classification,
            ConflictClassification::TargetDivergent { .. }
        ));
    }

    #[test]
    fn classifies_divergent_when_target_is_ahead_of_source() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\n");
        seed(&target, &project, SESSION_ID, b"line1\nline2\n");

        let report =
            inspect_conflict(&source, &target, &project, SESSION_ID, false).expect("inspect");
        assert!(
            matches!(
                report.classification,
                ConflictClassification::TargetDivergent { .. }
            ),
            "a target ahead of source has unique turns and must never be silently discarded"
        );
    }

    #[test]
    fn classifies_active_target_regardless_of_content() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\n");
        seed(&target, &project, SESSION_ID, b"line1\n");

        let report =
            inspect_conflict(&source, &target, &project, SESSION_ID, true).expect("inspect");
        assert_eq!(report.classification, ConflictClassification::TargetActive);
    }

    #[test]
    fn resolve_dry_run_never_writes() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"line1\n");

        let resolution = resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::Preview,
        )
        .expect("resolve");
        assert!(resolution.dry_run);
        assert_eq!(resolution.action, "would_replace_stale_ancestor");
        let target_bytes = std::fs::read(
            target
                .join("projects")
                .join(escape_project_path(&project))
                .join(format!("{SESSION_ID}.jsonl")),
        )
        .expect("target unchanged");
        assert_eq!(target_bytes, b"line1\n");
    }

    #[test]
    fn resolve_stale_ancestor_backs_up_and_replaces() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"line1\n");

        let resolution = resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::Confirm,
        )
        .expect("resolve");
        assert_eq!(resolution.action, "backed_up_and_replaced");
        let backup_path = resolution.backup_path.expect("backup path");
        assert_eq!(
            std::fs::read(&backup_path).expect("backup readable"),
            b"line1\n"
        );
        let target_bytes = std::fs::read(
            target
                .join("projects")
                .join(escape_project_path(&project))
                .join(format!("{SESSION_ID}.jsonl")),
        )
        .expect("target replaced");
        assert_eq!(target_bytes, b"line1\nline2\n");
    }

    #[test]
    fn resolve_divergent_is_refused_without_force_discard() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"DIFFERENT\n");

        let error = resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::Confirm,
        )
        .expect_err("must refuse without ForceDiscardDivergent");
        assert_eq!(error.code(), "conflict_requires_resolution");
        let target_bytes = std::fs::read(
            target
                .join("projects")
                .join(escape_project_path(&project))
                .join(format!("{SESSION_ID}.jsonl")),
        )
        .expect("target untouched");
        assert_eq!(target_bytes, b"DIFFERENT\n");
    }

    #[test]
    fn resolve_divergent_with_force_discard_backs_up_and_replaces() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"DIFFERENT\n");

        let resolution = resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::ForceDiscardDivergent,
        )
        .expect("resolve");
        assert_eq!(resolution.action, "backed_up_and_replaced_divergent");
        let backup_path = resolution.backup_path.expect("backup path");
        assert_eq!(
            std::fs::read(&backup_path).expect("backup readable"),
            b"DIFFERENT\n",
            "the discarded divergent content must be preserved in the backup"
        );
    }

    #[test]
    fn resolve_active_target_is_always_refused() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\n");
        seed(&target, &project, SESSION_ID, b"stale\n");

        let error = resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            true,
            ResolveDecision::ForceDiscardDivergent,
        )
        .expect_err("must refuse an active target regardless of decision level");
        assert_eq!(error.code(), "conflict_requires_resolution");
    }

    #[test]
    fn rollback_restores_the_most_recent_backup() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"line1\n");

        resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::Confirm,
        )
        .expect("resolve");

        let restored_path =
            rollback_conflict(&target, &project, SESSION_ID, false).expect("rollback");
        assert_eq!(std::fs::read(&restored_path).expect("restored"), b"line1\n");
    }

    #[test]
    fn rollback_without_a_backup_fails_closed() {
        let root = tempdir().expect("temp dir");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&target).expect("dir");

        let error = rollback_conflict(&target, &project, SESSION_ID, false)
            .expect_err("must fail closed with no backup");
        assert_eq!(error.code(), "no_backup_to_restore");
    }

    /// Regression: a live adversarial soak test found that rollback had no active-target guard
    /// at all, unlike `resolve_conflict` — it silently overwrote a currently-active session's
    /// transcript with an older backup, discarding turns the live writer had just appended, with
    /// no refusal and no warning. Rollback must refuse exactly like resolve does.
    #[test]
    fn rollback_refuses_an_active_target_even_with_a_backup_available() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\nline2\n");
        seed(&target, &project, SESSION_ID, b"line1\n");

        resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::Confirm,
        )
        .expect("resolve");

        // A backup now exists (from the resolve above), but the target has since gone active
        // again (e.g. someone resumed the session directly, bypassing Relay) — rollback must
        // still refuse, exactly like resolve would for the same classification.
        let error = rollback_conflict(&target, &project, SESSION_ID, true)
            .expect_err("must refuse an active target even though a backup exists");
        assert_eq!(error.code(), "conflict_requires_resolution");
        // And the live target content (as resolve last left it) must be untouched by the
        // refused rollback attempt.
        assert_eq!(
            std::fs::read(target.join(format!(
                "projects/{}/{SESSION_ID}.jsonl",
                crate::escape_project_path(&project)
            )))
            .expect("target still present"),
            b"line1\nline2\n"
        );
    }

    #[test]
    fn corrupted_resolution_metadata_fails_closed() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("bad.json");
        std::fs::write(&path, "{not json").expect("write garbage");
        let error = read_resolution_record(&path).expect_err("must fail closed");
        assert_eq!(error.code(), "corrupted_conflict_metadata");
    }

    #[test]
    fn resolve_missing_target_stages_normally() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        seed(&source, &project, SESSION_ID, b"line1\n");
        std::fs::create_dir_all(&target).expect("target dir");

        let resolution = resolve_conflict(
            &source,
            &target,
            &project,
            SESSION_ID,
            false,
            ResolveDecision::Confirm,
        )
        .expect("resolve");
        assert_eq!(resolution.action, "staged");
        let target_bytes = std::fs::read(
            target
                .join("projects")
                .join(escape_project_path(&project))
                .join(format!("{SESSION_ID}.jsonl")),
        )
        .expect("staged");
        assert_eq!(target_bytes, b"line1\n");
    }
}
