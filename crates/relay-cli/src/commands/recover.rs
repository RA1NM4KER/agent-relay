//! `relay recover <transaction-id>`: advanced, manual recovery decision for an interrupted
//! handoff transaction (see `relay_core::handoff::HandoffCoordinator::recover`).

use std::path::PathBuf;

use relay_core::{
    ClaudeConfigMode, Error, Profile, ProfileService, RelayPaths,
    handoff::{HandoffCoordinator, JournalStore},
};
use relay_provider_claude::{
    ClaudeSessionStager, ClaudeSessionStopper, ClaudeSourceLiveness, ClaudeTargetLauncher,
};

use crate::output::{CommandOutput, success};

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    transaction_id: &str,
    project_dir: &PathBuf,
    acknowledge: &bool,
    claude_executable: &Option<PathBuf>,
) -> Result<CommandOutput, Error> {
    let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;
    let parsed = relay_core::handoff::TransactionId::parse(transaction_id)?;
    let project_state_dir =
        crate::commands::handoff::find_transaction_dir(paths, &canonical, &parsed)?;
    // Peek the journal (independent of the coordinator below) to learn which registered
    // profiles this transaction actually named, so recovery uses their real
    // NativeDefault/Explicit modes rather than assuming Explicit.
    let peeked_journal = JournalStore::at_path(
        project_state_dir
            .join("handoffs")
            .join(format!("{parsed}.json")),
    )
    .load()?;
    let registered = service.list()?;
    let mode_for = |name: &relay_core::ProfileName| {
        registered
            .iter()
            .find(|profile| &profile.name == name)
            .map_or_else(
                ClaudeConfigMode::default,
                Profile::effective_claude_config_mode,
            )
    };
    let source_mode = mode_for(&peeked_journal.source_profile);
    let target_mode = mode_for(&peeked_journal.target_profile);
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
    let journal = if *acknowledge {
        coordinator.acknowledge_recovery(&project_state_dir, &parsed)?
    } else {
        coordinator.recover(&project_state_dir, &parsed)?
    };
    let human = format!(
        "Recovery decision for {}: {:?}",
        journal.transaction_id, journal.state
    );
    success("recover", human, journal)
}
