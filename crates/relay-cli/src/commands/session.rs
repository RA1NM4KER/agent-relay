//! `relay session <subcommand>`: advanced, manual session-transfer and conflict-resolution
//! commands (`stage-transfer`, `conflict inspect/resolve/rollback`) used for hand debugging —
//! `relay switch`/automatic handoff use the same underlying machinery through the normal
//! transactional path, not this module.

use std::path::Path;

use relay_core::{
    Error, Profile, ProfileService, RelayPaths,
    handoff::{LeaseStore, ProjectId},
};
use relay_provider_claude::{SystemProcessLister, stage_transfer};
use serde_json::json;

use crate::{
    cli::{ConflictCommand, SessionArgs, SessionCommand},
    output::{CommandOutput, success},
    providers,
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    session: &SessionArgs,
) -> Result<CommandOutput, Error> {
    match &session.command {
        SessionCommand::StageTransfer {
            source_profile,
            target_profile,
            project_dir,
            session_id,
        } => {
            let registered = service.list()?;
            let source = registered
                .iter()
                .find(|profile| &profile.name == source_profile)
                .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
            let target = registered
                .iter()
                .find(|profile| &profile.name == target_profile)
                .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
            if source.name == target.name {
                return Err(Error::ProviderMismatch {
                    expected: "distinct source and target profiles".to_owned(),
                    observed: source.name.to_string(),
                });
            }
            let report = stage_transfer(
                &SystemProcessLister,
                &source.config_dir,
                &target.config_dir,
                project_dir,
                session_id,
                source.effective_claude_config_mode(),
            )?;
            let human = format!(
                "Staged session {} from '{}' to '{}'\nProject key: {}\nArtifacts: {}",
                report.session_id,
                source_profile,
                target_profile,
                report.project_key,
                report
                    .artifacts
                    .iter()
                    .map(|artifact| format!(
                        "{} (sha256={}, {} bytes{})",
                        artifact.relative_path,
                        artifact.sha256,
                        artifact.size_bytes,
                        if artifact.already_present_and_identical {
                            ", already staged"
                        } else {
                            ""
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            success("session.stage_transfer", human, report)
        }
        SessionCommand::Conflict(conflict) => match &conflict.command {
            ConflictCommand::Inspect {
                source_profile,
                target_profile,
                project_dir,
                session_id,
                claude_executable,
            } => {
                let registered = service.list()?;
                let source = registered
                    .iter()
                    .find(|profile| &profile.name == source_profile)
                    .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
                let target = registered
                    .iter()
                    .find(|profile| &profile.name == target_profile)
                    .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                let target_active = target_is_active(
                    paths,
                    target,
                    project_dir,
                    session_id,
                    claude_executable.as_deref(),
                )?;
                let report = relay_provider_claude::inspect_conflict(
                    &source.config_dir,
                    &target.config_dir,
                    project_dir,
                    session_id,
                    target_active,
                )?;
                let human = format!(
                    "Session {session_id}: target ({target_profile}) is {:?}",
                    report.classification
                );
                success("session.conflict.inspect", human, report)
            }
            ConflictCommand::Resolve {
                source_profile,
                target_profile,
                project_dir,
                session_id,
                claude_executable,
                dry_run,
                yes,
                force_discard_divergent,
            } => {
                let registered = service.list()?;
                let source = registered
                    .iter()
                    .find(|profile| &profile.name == source_profile)
                    .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
                let target = registered
                    .iter()
                    .find(|profile| &profile.name == target_profile)
                    .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                let target_active = target_is_active(
                    paths,
                    target,
                    project_dir,
                    session_id,
                    claude_executable.as_deref(),
                )?;
                let decision = if *dry_run {
                    relay_provider_claude::ResolveDecision::Preview
                } else if *force_discard_divergent {
                    relay_provider_claude::ResolveDecision::ForceDiscardDivergent
                } else if *yes {
                    relay_provider_claude::ResolveDecision::Confirm
                } else {
                    relay_provider_claude::ResolveDecision::Preview
                };
                let resolution = relay_provider_claude::resolve_conflict(
                    &source.config_dir,
                    &target.config_dir,
                    project_dir,
                    session_id,
                    target_active,
                    decision,
                    source.effective_claude_config_mode(),
                )?;
                let human = format!(
                    "Session {session_id}: {} (dry_run={}){}",
                    resolution.action,
                    resolution.dry_run,
                    resolution
                        .backup_path
                        .as_ref()
                        .map(|path| format!("\nBackup: {}", path.display()))
                        .unwrap_or_default()
                );
                success("session.conflict.resolve", human, resolution)
            }
            ConflictCommand::Rollback {
                target_profile,
                project_dir,
                session_id,
                claude_executable,
            } => {
                let registered = service.list()?;
                let target = registered
                    .iter()
                    .find(|profile| &profile.name == target_profile)
                    .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                let target_active = target_is_active(
                    paths,
                    target,
                    project_dir,
                    session_id,
                    claude_executable.as_deref(),
                )?;
                let restored_path = relay_provider_claude::rollback_conflict(
                    &target.config_dir,
                    project_dir,
                    session_id,
                    target_active,
                )?;
                success(
                    "session.conflict.rollback",
                    format!("Restored backup to {}", restored_path.display()),
                    json!({ "restored_path": restored_path }),
                )
            }
        },
    }
}

/// Whether `owner`'s session is live, judged by **that profile's own provider** (Claude: session
/// registry + pid fingerprint; Codex: recorded pid + processes under its `CODEX_HOME`). The one
/// place status/session-conflict paths dispatch on provider, so none of them can quietly ask
/// Claude about a Codex lease (or the reverse).
fn owner_is_live(
    owner: &Profile,
    project_dir: &Path,
    session_id: &str,
    recorded_owner: Option<&relay_core::handoff::ProcessIdentity>,
    executables: &providers::ExecutableOverrides,
) -> Result<bool, Error> {
    let ports = providers::ports_for(
        owner.provider,
        executables,
        owner.effective_claude_config_mode(),
    );
    Ok(ports
        .liveness
        .check(&owner.config_dir, project_dir, session_id, recorded_owner)?
        .active)
}

/// Checks whether a session is currently active for `target` using the same liveness mechanism the
/// handoff coordinator uses for that profile's provider, so `session conflict` commands never touch
/// a genuinely in-use target.
pub(crate) fn target_is_active(
    paths: &RelayPaths,
    target: &Profile,
    project_dir: &Path,
    session_id: &str,
    claude_executable: Option<&Path>,
) -> Result<bool, Error> {
    let canonical_project = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.to_path_buf(),
        source,
    })?;
    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let lease =
        LeaseStore::at_path(paths.project_state_dir(&project_id).join("lease.json")).load()?;
    let recorded_owner = lease
        .filter(|lease| lease.owner_profile == target.name)
        .map(|lease| lease.owner_process);
    owner_is_live(
        target,
        &canonical_project,
        session_id,
        recorded_owner.as_ref(),
        &providers::ExecutableOverrides {
            claude: claude_executable.map(Path::to_path_buf),
            codex: None,
        },
    )
}
