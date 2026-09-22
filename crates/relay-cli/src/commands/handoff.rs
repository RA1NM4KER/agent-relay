//! `relay handoff <subcommand>`: advanced, manual single-shot Claude-to-Claude handoff
//! (`run`/`status`) used for hand debugging — `relay switch` is the provider-aware, normal entry
//! point and uses the same `HandoffCoordinator` through its own transactional path.

use relay_core::{
    Error, ProfileService, RelayPaths,
    handoff::{HandoffCoordinator, HandoffRequest, JournalStore},
};
use relay_provider_claude::{
    ClaudeSessionStager, ClaudeSessionStopper, ClaudeSourceLiveness, ClaudeTargetLauncher,
};

use crate::{
    cli::{HandoffArgs, HandoffCommand},
    output::{CommandOutput, success},
    sessions,
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    handoff: &HandoffArgs,
) -> Result<CommandOutput, Error> {
    match &handoff.command {
        HandoffCommand::Run {
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
            let source_mode = source.effective_claude_config_mode();
            let target_mode = target.effective_claude_config_mode();
            let liveness = ClaudeSourceLiveness::new(claude_executable.clone(), source_mode);
            let source_stopper = ClaudeSessionStopper::new(claude_executable.clone(), source_mode);
            let target_stopper = ClaudeSessionStopper::new(claude_executable.clone(), target_mode);
            let stager = ClaudeSessionStager;
            let launcher = ClaudeTargetLauncher::new(claude_executable.clone(), target_mode);
            let coordinator = HandoffCoordinator {
                paths,
                liveness: &liveness,
                source_stopper: &source_stopper,
                target_stopper: &target_stopper,
                stager: Some(&stager),
                context_capturer: None,
                launcher: &launcher,
            };
            // `relay handoff run` is the M2B low-level debugging entry point and predates
            // multi-provider profiles; it stays Claude-only (SESSION_CONTINUATION), exactly
            // as before M6. `relay switch` is the provider-aware M6 entry point.
            let journal = coordinator.run(HandoffRequest {
                project_dir: project_dir.clone(),
                source_profile: source.name.clone(),
                source_provider: relay_core::ProviderKind::Claude,
                source_config_dir: source.config_dir.clone(),
                target_profile: target.name.clone(),
                target_provider: relay_core::ProviderKind::Claude,
                target_config_dir: target.config_dir.clone(),
                session_id: session_id.clone(),
                continuity_type: relay_core::handoff::ContinuityType::SessionContinuation,
                source_claude_mode: source_mode,
                target_claude_mode: target_mode,
                state_dir: {
                    let canonical =
                        std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                            path: project_dir.clone(),
                            source,
                        })?;
                    sessions::session_dir_for_native(paths, &canonical, session_id)?
                },
            })?;
            let human = format!(
                "Handoff {} ({} -> {}): {:?}\nSession: {}\nTransaction: {}",
                journal.transaction_id,
                source_profile,
                target_profile,
                journal.state,
                journal.session_id,
                journal.transaction_id
            );
            success("handoff.run", human, journal)
        }
        HandoffCommand::Status {
            transaction_id,
            project_dir,
        } => {
            let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                path: project_dir.clone(),
                source,
            })?;
            let parsed = relay_core::handoff::TransactionId::parse(transaction_id)?;
            let project_state_dir = find_transaction_dir(paths, &canonical, &parsed)?;
            let journal_store = JournalStore::at_path(
                project_state_dir
                    .join("handoffs")
                    .join(format!("{parsed}.json")),
            );
            let journal = journal_store.load()?;
            let human = format!(
                "Transaction {}: {:?}\nRevision: {}",
                journal.transaction_id, journal.state, journal.revision
            );
            success("handoff.status", human, journal)
        }
    }
}

/// The state directory of whichever Relay session of the project owns a transaction.
pub(crate) fn find_transaction_dir(
    paths: &RelayPaths,
    canonical_project: &std::path::Path,
    transaction: &relay_core::handoff::TransactionId,
) -> Result<std::path::PathBuf, Error> {
    let store = sessions::open_store(paths, canonical_project)?;
    store
        .list()?
        .iter()
        .map(|view| store.session_dir(&view.record.relay_session_id))
        .find(|dir| {
            dir.join("handoffs")
                .join(format!("{transaction}.json"))
                .exists()
        })
        .ok_or_else(|| Error::InvalidTransactionId(transaction.to_string()))
}
