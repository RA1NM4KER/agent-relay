//! M2A: minimal, hash-verified discovery and staging of a Claude session transcript from one
//! profile's `CLAUDE_CONFIG_DIR` to another's, so the target profile can `claude --resume` it.
//!
//! This never inspects, copies, or infers credentials. It only ever touches
//! `<config_dir>/projects/<project_key>/<session_id>*.jsonl`. The source artifact is always
//! retained; the target is never overwritten once written, and a divergent existing target is a
//! hard error rather than a silent overwrite.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use relay_core::{AtomicWrite, Error, FsAtomicWriter, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use relay_core::handoff::ProcessIdentity;

use crate::{
    inspection::is_supported_version,
    process_scope::{
        ClassifiedProcess, RawProcess, RegistryEntry, WriterScope, blockers, classify,
        is_claude_process,
    },
};

/// Finds the Claude processes that could be a conflicting writer for one project (see
/// [`crate::process_scope`]): a best-effort process-table check, not a lock, with the known race
/// limits of any process-table inspection (see docs/security.md).
pub trait ProcessLister: Send + Sync {
    /// Only the processes that must block the operation.
    fn blocking_claude_processes(&self, scope: &WriterScope<'_>) -> Result<Vec<ClassifiedProcess>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessLister;

/// `pid ppid <lstart: 5 tokens> command… ENV=…` from `ps -Eww`.
fn parse_ps_line(line: &str) -> Option<RawProcess> {
    let mut tokens = line.split_whitespace();
    let pid: u32 = tokens.next()?.parse().ok()?;
    let ppid: u32 = tokens.next()?.parse().ok()?;
    for _ in 0..5 {
        tokens.next()?;
    }
    let rest: Vec<&str> = tokens.collect();
    let is_env = |token: &str| {
        token.split_once('=').is_some_and(|(key, _)| {
            !key.is_empty()
                && key.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
                && key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
    };
    let split = rest
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, token)| is_env(token))
        .map_or(rest.len(), |(index, _)| index);
    let value = |key: &str| {
        rest[split..]
            .iter()
            .find_map(|token| token.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    };
    Some(RawProcess {
        pid,
        ppid,
        argv: rest[..split]
            .iter()
            .map(|token| (*token).to_owned())
            .collect(),
        config_dir: value("CLAUDE_CONFIG_DIR"),
        pwd: value("PWD").map(PathBuf::from),
        cwd: None,
    })
}

fn probe_cwd(pid: u32) -> Option<PathBuf> {
    if let Ok(path) = fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(path);
    }
    let output = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .map(PathBuf::from)
}

fn canonical_or_same(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

impl ProcessLister for SystemProcessLister {
    fn blocking_claude_processes(&self, scope: &WriterScope<'_>) -> Result<Vec<ClassifiedProcess>> {
        let output = Command::new("ps")
            .args(["-Eww", "-axo", "pid=,ppid=,lstart=,command="])
            .output()
            .map_err(|_| Error::ProviderCommandFailed)?;
        if !output.status.success() {
            return Err(Error::ProviderCommandFailed);
        }
        let config_text = scope.config_dir.to_string_lossy().into_owned();
        let canonical_config = canonical_or_same(scope.config_dir.to_path_buf());
        let mut processes: Vec<RawProcess> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_ps_line)
            .collect();
        let canonical_config_text = canonical_config.to_string_lossy().into_owned();
        for process in &mut processes {
            // Normalise the profile directory so a symlinked spelling still matches.
            if process.config_dir.as_deref() == Some(canonical_config_text.as_str()) {
                process.config_dir = Some(config_text.clone());
            }
            if process.config_dir.as_deref() == Some(config_text.as_str())
                && is_claude_process(&process.argv)
            {
                process.cwd = probe_cwd(process.pid).map(canonical_or_same);
                process.pwd = process.pwd.take().map(canonical_or_same);
            }
        }
        let registry: Vec<RegistryEntry> = read_registry(scope.config_dir)
            .into_iter()
            .filter(|entry| processes.iter().any(|process| process.pid == entry.pid))
            .collect();
        let expected = scope.expected;
        let fingerprint = |pid: u32| {
            expected
                .filter(|owner| owner.pid == pid)
                .and_then(ProcessIdentity::is_still_the_same_process)
        };
        Ok(blockers(classify(
            &processes,
            &registry,
            scope,
            &fingerprint,
        )))
    }
}

fn read_registry(config_dir: &Path) -> Vec<RegistryEntry> {
    let Ok(entries) = fs::read_dir(config_dir.join("sessions")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.path()).ok()?).ok()?;
            Some(RegistryEntry {
                pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
                session_id: value
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                cwd: value
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(|cwd| canonical_or_same(PathBuf::from(cwd))),
            })
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StagedArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// True when the target already held a byte-identical copy and nothing was written.
    pub already_present_and_identical: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionTransferReport {
    pub project_key: String,
    pub session_id: String,
    pub source_config_dir: PathBuf,
    pub target_config_dir: PathBuf,
    pub artifacts: Vec<StagedArtifact>,
}

/// Mirrors the escaping Claude Code itself uses for `projects/<key>` directory names: every
/// path separator becomes `-`. Observed directly against Claude Code 2.1.276 (e.g.
/// `/Users/x/repos/y` -> `-Users-x-repos-y`); not documented upstream, so this is a versioned
/// assumption gated the same way as the auth-status schema.
#[must_use]
pub fn escape_project_path(project_dir: &Path) -> String {
    project_dir.to_string_lossy().replace('/', "-")
}

fn require_absolute(path: &Path) -> Result<&Path> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Error::PathNotAbsolute(path.to_path_buf()))
    }
}

/// A Claude `--session-id` must be a UUID; Relay never trusts an operator-supplied string into
/// a filesystem path without validating its shape first.
pub fn validate_session_id(session_id: &str) -> Result<()> {
    let bytes = session_id.as_bytes();
    let valid = bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|&index| bytes[index] == b'-')
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => true,
            _ => byte.is_ascii_hexdigit(),
        });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidSessionId)
    }
}

/// Gates on the same supported-version policy as adoption. A session written by an
/// unsupported Claude layout must never be staged as if the escaping/format were known-good.
pub fn ensure_supported_claude_version(version: &str) -> Result<()> {
    if is_supported_version(version) {
        Ok(())
    } else {
        Err(Error::UnsupportedProviderVersion)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        Err(Error::SymbolicLink(path.to_path_buf()))
    } else {
        Ok(())
    }
}

/// Discovers the primary transcript plus any subagent sidecar transcripts (files named
/// `<session_id>-*.jsonl` in the same directory) for one project+session under one config dir.
/// Read-only; makes no filesystem changes.
pub fn discover_session(
    config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
) -> Result<Vec<PathBuf>> {
    validate_session_id(session_id)?;
    let project_dir = require_absolute(project_dir)?;
    let key = escape_project_path(project_dir);
    let project_session_dir = config_dir.join("projects").join(&key);
    let primary = project_session_dir.join(format!("{session_id}.jsonl"));
    reject_symlink(&primary)?;
    if !primary.is_file() {
        return Err(Error::SessionNotFound);
    }
    let mut artifacts = vec![primary];
    let sidecar_prefix = format!("{session_id}-");
    if let Ok(entries) = fs::read_dir(&project_session_dir) {
        let mut sidecars: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                (name.starts_with(&sidecar_prefix) && name.ends_with(".jsonl"))
                    .then(|| entry.path())
            })
            .collect();
        sidecars.sort();
        artifacts.extend(sidecars);
    }
    Ok(artifacts)
}

/// Refuses when anything other than the exact `expected` source process could write to this
/// project. Shared by the pre-stop preflight and the authoritative post-stop staging check.
pub fn require_no_conflicting_writer(
    lister: &dyn ProcessLister,
    source_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    expected: Option<&ProcessIdentity>,
    mode: relay_core::ClaudeConfigMode,
) -> Result<()> {
    let blocking = lister.blocking_claude_processes(&WriterScope {
        config_dir: source_config_dir,
        project_dir,
        session_id: Some(session_id),
        expected,
        mode,
    })?;
    if blocking.is_empty() {
        Ok(())
    } else {
        Err(Error::SourceProfileActive)
    }
}

/// Everything staging will need, checked without writing: no conflicting writer (the source
/// process itself excepted) and a discoverable source session. Run before the source is stopped
/// so a predictable refusal never costs the user their live session.
pub fn stage_preflight(
    lister: &dyn ProcessLister,
    source_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    expected: Option<&ProcessIdentity>,
    mode: relay_core::ClaudeConfigMode,
) -> Result<()> {
    let project_dir = require_absolute(project_dir)?;
    require_no_conflicting_writer(
        lister,
        source_config_dir,
        project_dir,
        session_id,
        expected,
        mode,
    )?;
    discover_session(source_config_dir, project_dir, session_id).map(|_| ())
}

/// Stages every discovered artifact from `source_config_dir` into the identical relative path
/// under `target_config_dir`. Refuses if the source profile still has a live Claude process, if
/// no matching session exists, or if the target already holds a divergent artifact. Every write
/// is staged into a same-directory temp file, synced, and atomically renamed; every written
/// artifact is re-read and re-hashed to confirm it matches the source before being reported.
/// The source is never modified.
pub fn stage_transfer(
    lister: &dyn ProcessLister,
    source_config_dir: &Path,
    target_config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    source_mode: relay_core::ClaudeConfigMode,
) -> Result<SessionTransferReport> {
    let project_dir = require_absolute(project_dir)?;
    require_no_conflicting_writer(
        lister,
        source_config_dir,
        project_dir,
        session_id,
        None,
        source_mode,
    )?;
    let key = escape_project_path(project_dir);
    let artifacts = discover_session(source_config_dir, project_dir, session_id)?;

    let mut staged = Vec::with_capacity(artifacts.len());
    for source_path in &artifacts {
        let relative = source_path
            .strip_prefix(source_config_dir)
            .map_err(|_| Error::SessionNotFound)?;
        let dest_path = target_config_dir.join(relative);
        let source_bytes = fs::read(source_path).map_err(|source| Error::Io {
            path: source_path.clone(),
            source,
        })?;
        let source_hash = sha256_hex(&source_bytes);

        reject_symlink(&dest_path)?;
        if dest_path.exists() {
            let dest_bytes = fs::read(&dest_path).map_err(|source| Error::Io {
                path: dest_path.clone(),
                source,
            })?;
            if sha256_hex(&dest_bytes) == source_hash {
                staged.push(StagedArtifact {
                    relative_path: relative.to_string_lossy().into_owned(),
                    sha256: source_hash,
                    size_bytes: source_bytes.len() as u64,
                    already_present_and_identical: true,
                });
                continue;
            }
            return Err(Error::TargetArtifactDiverges);
        }

        let dest_parent = dest_path
            .parent()
            .ok_or_else(|| Error::PathNotAbsolute(dest_path.clone()))?;
        fs::create_dir_all(dest_parent).map_err(|source| Error::Io {
            path: dest_parent.to_path_buf(),
            source,
        })?;
        // Mode 0600, temp-file-then-rename, directory fsync: identical convention to Relay's
        // own registry writes, so a staged transcript is never more exposed than the original.
        FsAtomicWriter.write_atomic(&dest_path, &source_bytes)?;

        let verify_bytes = fs::read(&dest_path).map_err(|source| Error::Io {
            path: dest_path.clone(),
            source,
        })?;
        if sha256_hex(&verify_bytes) != source_hash {
            return Err(Error::TransferVerificationFailed);
        }
        staged.push(StagedArtifact {
            relative_path: relative.to_string_lossy().into_owned(),
            sha256: source_hash,
            size_bytes: source_bytes.len() as u64,
            already_present_and_identical: false,
        });
    }

    Ok(SessionTransferReport {
        project_key: key,
        session_id: session_id.to_owned(),
        source_config_dir: source_config_dir.to_path_buf(),
        target_config_dir: target_config_dir.to_path_buf(),
        artifacts: staged,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::{
        ProcessLister, SystemProcessLister, discover_session, ensure_supported_claude_version,
        escape_project_path, stage_transfer, validate_session_id,
    };

    const SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

    fn blocker() -> crate::process_scope::ClassifiedProcess {
        crate::process_scope::ClassifiedProcess {
            pid: 1,
            role: crate::process_scope::ProcessRole::ConflictingWriter,
            evidence: "test".to_owned(),
        }
    }

    struct FixedLister(bool);

    impl super::ProcessLister for FixedLister {
        fn blocking_claude_processes(
            &self,
            _scope: &crate::process_scope::WriterScope<'_>,
        ) -> relay_core::Result<Vec<crate::process_scope::ClassifiedProcess>> {
            Ok(if self.0 { vec![blocker()] } else { Vec::new() })
        }
    }

    struct RecordingLister {
        active: bool,
        seen: Arc<Mutex<Vec<std::path::PathBuf>>>,
    }

    impl super::ProcessLister for RecordingLister {
        fn blocking_claude_processes(
            &self,
            scope: &crate::process_scope::WriterScope<'_>,
        ) -> relay_core::Result<Vec<crate::process_scope::ClassifiedProcess>> {
            self.seen
                .lock()
                .expect("seen lock")
                .push(scope.config_dir.to_path_buf());
            Ok(if self.active {
                vec![blocker()]
            } else {
                Vec::new()
            })
        }
    }

    fn seed_session(config_dir: &std::path::Path, project_dir: &std::path::Path, contents: &[u8]) {
        let key = escape_project_path(project_dir);
        let dir = config_dir.join("projects").join(key);
        std::fs::create_dir_all(&dir).expect("session dir");
        std::fs::write(dir.join(format!("{SESSION_ID}.jsonl")), contents).expect("transcript");
    }

    #[test]
    fn escaping_matches_observed_claude_convention() {
        assert_eq!(
            escape_project_path(std::path::Path::new("/Users/x/repos/y")),
            "-Users-x-repos-y"
        );
    }

    #[test]
    fn session_id_validation_rejects_malformed_input() {
        assert!(validate_session_id(SESSION_ID).is_ok());
        assert!(validate_session_id("not-a-uuid").is_err());
        assert!(validate_session_id("11111111222233334444555555555555").is_err());
        assert!(validate_session_id("../../../etc/passwd").is_err());
    }

    #[test]
    fn version_gate_matches_adoption_policy() {
        assert!(ensure_supported_claude_version("2.1.276").is_ok());
        assert!(ensure_supported_claude_version("2.2.0").is_err());
        assert!(ensure_supported_claude_version("1.0.0").is_err());
    }

    #[test]
    fn stage_transfer_copies_hash_verified_and_leaves_source_untouched() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target).expect("target dir");
        seed_session(&source, &project, b"{\"line\":1}\n");
        let source_before = std::fs::read(
            source
                .join("projects")
                .join(escape_project_path(&project))
                .join(format!("{SESSION_ID}.jsonl")),
        )
        .expect("source contents");

        let report = stage_transfer(
            &FixedLister(false),
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect("stage transfer");

        assert_eq!(report.artifacts.len(), 1);
        assert!(!report.artifacts[0].already_present_and_identical);
        let target_file = target
            .join("projects")
            .join(escape_project_path(&project))
            .join(format!("{SESSION_ID}.jsonl"));
        let target_bytes = std::fs::read(&target_file).expect("staged file");
        assert_eq!(target_bytes, source_before);

        let source_after = std::fs::read(
            source
                .join("projects")
                .join(escape_project_path(&project))
                .join(format!("{SESSION_ID}.jsonl")),
        )
        .expect("source contents unchanged");
        assert_eq!(source_after, source_before);

        // No stray temp files left behind.
        let entries: Vec<_> = std::fs::read_dir(target_file.parent().expect("parent"))
            .expect("read target dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(entries.iter().all(|name| !name.contains(".tmp.")));
    }

    #[test]
    fn stage_transfer_is_idempotent_when_target_already_matches() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target).expect("target dir");
        seed_session(&source, &project, b"identical\n");

        stage_transfer(
            &FixedLister(false),
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect("first stage");
        let second = stage_transfer(
            &FixedLister(false),
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect("second stage is a no-op, not an error");

        assert!(second.artifacts[0].already_present_and_identical);
    }

    #[test]
    fn stage_transfer_rejects_a_diverging_target() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target).expect("target dir");
        seed_session(&source, &project, b"source version\n");
        let key = escape_project_path(&project);
        let target_session_dir = target.join("projects").join(&key);
        std::fs::create_dir_all(&target_session_dir).expect("target session dir");
        std::fs::write(
            target_session_dir.join(format!("{SESSION_ID}.jsonl")),
            b"already diverged\n",
        )
        .expect("seed diverging target");

        let error = stage_transfer(
            &FixedLister(false),
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect_err("must reject a diverging target");
        assert_eq!(error.code(), "target_artifact_diverges");

        let target_bytes = std::fs::read(target_session_dir.join(format!("{SESSION_ID}.jsonl")))
            .expect("target still readable");
        assert_eq!(target_bytes, b"already diverged\n");
    }

    #[test]
    fn stage_transfer_rejects_a_live_source_profile() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target).expect("target dir");
        seed_session(&source, &project, b"content\n");

        let error = stage_transfer(
            &FixedLister(true),
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect_err("must reject a live source");
        assert_eq!(error.code(), "source_profile_active");
        assert!(
            !target
                .join("projects")
                .join(escape_project_path(&project))
                .exists(),
            "nothing must be written when the source is still active"
        );
    }

    #[test]
    fn stage_transfer_reports_missing_session() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target).expect("target dir");

        let error = stage_transfer(
            &FixedLister(false),
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect_err("missing session must be reported");
        assert_eq!(error.code(), "session_not_found");
    }

    #[test]
    fn discover_session_does_not_match_a_different_project() {
        let root = tempdir().expect("temp dir");
        let config_dir = root.path().join("config");
        let project = root.path().join("proj-a");
        let other_project = root.path().join("proj-b");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        seed_session(&config_dir, &project, b"content\n");

        let error = discover_session(&config_dir, &other_project, SESSION_ID)
            .expect_err("wrong project must not find the session");
        assert_eq!(error.code(), "session_not_found");

        // The right project still resolves.
        assert!(discover_session(&config_dir, &project, SESSION_ID).is_ok());
    }

    #[test]
    fn system_process_lister_checks_the_real_process_table_without_erroring() {
        if !crate::ps_dash_e_is_available() {
            eprintln!(
                "skipping: `ps -E` is refused in this sandbox (not a Relay defect — see crate::ps_dash_e_is_available)"
            );
            return;
        }
        let root = tempdir().expect("temp dir");
        let nobody = root.path().join("nobody");
        let result = SystemProcessLister.blocking_claude_processes(&super::WriterScope {
            config_dir: &nobody,
            project_dir: root.path(),
            session_id: None,
            expected: None,
            mode: relay_core::ClaudeConfigMode::Explicit,
        });
        assert!(result.expect("ok").is_empty());
    }

    #[test]
    fn stage_transfer_checks_the_source_profile_it_was_given() {
        let root = tempdir().expect("temp dir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        let project = root.path().join("proj");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target).expect("target dir");
        seed_session(&source, &project, b"content\n");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let lister = RecordingLister {
            active: false,
            seen: seen.clone(),
        };

        stage_transfer(
            &lister,
            &source,
            &target,
            &project,
            SESSION_ID,
            relay_core::ClaudeConfigMode::Explicit,
        )
        .expect("stage transfer");

        assert_eq!(seen.lock().expect("seen lock").as_slice(), [source]);
    }
}
