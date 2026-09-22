//! `relay switch [target]`: the manual cross-profile (same provider or not) handoff entry point,
//! and the terminal-native target picker used when no target is given.

use std::path::{Path, PathBuf};

use relay_core::{
    AuthenticationState, Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
    handoff::{HandoffCoordinator, HandoffRequest},
    usage::UsageState,
};

use crate::{
    cli::SwitchArgs,
    commands::codex::codex_preflight,
    output::{CommandOutput, success},
    preferences, progress, provider_args, providers, sessions, target,
    terminal_session::{ContinuationContext, plan_codex_resume, run_managed_terminal},
    util::current_unix_ms,
};

/// M6: `relay switch <target>` — the manual cross-profile (same provider or not) handoff entry
/// point. Reads the project's current writer lease to find the source (never takes it as an
/// argument, so it can never be spoofed to claim ownership of a profile that isn't the real
/// current writer), verifies the target is authenticated, chooses SESSION_CONTINUATION or
/// STATE_CONTINUATION from the two profiles' providers, and runs one
/// [`HandoffCoordinator::run`] transaction — the exact same safety machinery `relay watch run`'s
/// automatic path uses.
pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &SwitchArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let project_dir = match &args.project_dir {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let canonical_project = std::fs::canonicalize(&project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;
    let registered = service.list()?;
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };
    // Which conversation is being moved: exactly one active session is used directly; several
    // ask (terminal) or must be named with `--session`. Other sessions are never touched.
    let store = sessions::open_store(paths, &canonical_project)?;
    let views = sessions::reconcile(paths, &canonical_project, &registered, &executables)?;
    let chosen = sessions::choose_session(
        &store,
        &views,
        &registered,
        sessions::Want::Switchable,
        args.session.as_deref(),
        None,
        json_mode,
    )?;
    let session = sessions::SessionCtx::of(&store, &chosen.record.relay_session_id);
    let project_state_dir = session.dir.clone();
    // A dormant conversation has no process to stop: right before the transaction it is given a
    // lease naming a process that provably no longer exists, so the ordinary transaction (context
    // capture or session staging, target verification, lease move) re-homes it to the target
    // profile for its next resume. Nothing is written before the target has been chosen.
    let dormant = chosen.lease.is_none();
    let source_owner = chosen.profile().clone();
    let source_native = chosen
        .native_session_id()
        .map(str::to_owned)
        .ok_or(Error::CorruptedState)?;
    let source = registered
        .iter()
        .find(|profile| profile.name == source_owner)
        .ok_or_else(|| Error::ProfileNotFound(source_owner.to_string()))?;
    let target_name = match &args.target {
        Some(name) => name.clone(),
        None => choose_switch_target(
            paths,
            &registered,
            source,
            &executables,
            &canonical_project,
            json_mode,
        )?,
    };
    let target = registered
        .iter()
        .find(|profile| profile.name == target_name)
        .ok_or_else(|| Error::ProfileNotFound(target_name.to_string()))?;
    if target.name == source.name {
        return Err(Error::AlreadyCurrentWriter(target.name.to_string()));
    }
    provider_args::validate(target.provider, &args.provider_args)?;

    let target_backend = providers::provider_backend(target.provider, &executables)?;
    let target_status = service.status(&target.name, target_backend.as_ref())?;
    if target_status.authentication != AuthenticationState::Authenticated {
        return Err(Error::AuthenticationRequired);
    }
    // Known before anything is stopped: the account behind the target must still be the one
    // registered for it.
    if !target_status.identity_matches {
        return Err(Error::IdentityMismatch);
    }
    // An explicit switch to Codex is preflighted before anything is committed: an exhausted (or
    // unverifiable) target is refused outright — manual intent is never silently rerouted.
    if target.provider == ProviderKind::Codex {
        let usage = codex_preflight(target, &executables, &canonical_project, json_mode);
        if usage.state.is_blocking() {
            return Err(Error::TargetProfileExhausted(target.name.to_string()));
        }
        if usage.state == UsageState::Unknown {
            return Err(Error::CodexUsageUnverified(target.name.to_string()));
        }
    }

    let continuity_type =
        relay_core::handoff::ContinuityType::for_transition(source.provider, target.provider);
    let source_ports = providers::ports_for(
        source.provider,
        &executables,
        source.effective_claude_config_mode(),
    );
    let target_ports = providers::ports_for(
        target.provider,
        &executables,
        target.effective_claude_config_mode(),
    );
    let coordinator = HandoffCoordinator {
        paths,
        liveness: source_ports.liveness.as_ref(),
        source_stopper: source_ports.stopper.as_ref(),
        target_stopper: target_ports.stopper.as_ref(),
        stager: match continuity_type {
            relay_core::handoff::ContinuityType::SessionContinuation => {
                source_ports.stager.as_deref()
            }
            _ => None,
        },
        context_capturer: match continuity_type {
            relay_core::handoff::ContinuityType::StateContinuation => {
                Some(source_ports.context_capturer.as_ref())
            }
            _ => None,
        },
        launcher: target_ports.launcher.as_ref(),
    };

    if !json_mode {
        println!(
            "Switching '{}' -> '{}' ({:?})...",
            source.name, target.name, continuity_type
        );
    }
    // If the switch does not complete, the conversation goes back to being dormant on its last
    // profile (this guard runs on every early return and is disarmed once the lease has moved).
    let mut restore = RunOnDrop(dormant.then_some(|| {
        sessions::release_after_exit(paths, &canonical_project, &session.id, &registered);
    }));
    if dormant {
        sessions::activate_session(
            paths,
            &canonical_project,
            &session.id,
            source,
            &source_native,
            sessions::gone_process_identity(),
            current_unix_ms(),
        )?;
    }
    let journal = coordinator.run(HandoffRequest {
        project_dir: canonical_project.clone(),
        source_profile: source.name.clone(),
        source_provider: source.provider,
        source_config_dir: source.config_dir.clone(),
        target_profile: target.name.clone(),
        target_provider: target.provider,
        target_config_dir: target.config_dir.clone(),
        session_id: source_native.clone(),
        continuity_type,
        source_claude_mode: source.effective_claude_config_mode(),
        target_claude_mode: target.effective_claude_config_mode(),
        state_dir: Some(session.dir.clone()),
    })?;

    if journal.state == relay_core::handoff::HandoffState::Complete {
        restore.0 = None;
    }
    let new_session_id = journal
        .verification
        .as_ref()
        .map(|verification| verification.target_session_id.clone())
        .unwrap_or_default();
    let human = format!(
        "Switch {} ('{}' -> '{}'): {:?}\nContinuity: {:?}\nNew session: {}",
        journal.transaction_id,
        source.name,
        target.name,
        journal.state,
        continuity_type,
        new_session_id
    );
    if args.no_attach || journal.state != relay_core::handoff::HandoffState::Complete {
        return success("switch", human, journal);
    }

    // Explicit arguments are for the TARGET's provider only and replace that provider's stored
    // ones; nothing is ever translated from the source provider's arguments.
    let mut stored_args = provider_args::ProviderArgs::load(&project_state_dir)?;
    if !args.provider_args.is_empty() {
        stored_args.set(target.provider, args.provider_args.clone());
        stored_args.save(&project_state_dir)?;
    }
    match target.provider {
        ProviderKind::Codex => {
            let command = plan_codex_resume(
                providers::executable_override(ProviderKind::Codex, &executables),
                &target.config_dir,
                &canonical_project,
                &new_session_id,
                stored_args.for_provider(ProviderKind::Codex),
                None,
            )?;
            run_managed_terminal(
                &ContinuationContext::new(
                    service,
                    paths,
                    &canonical_project,
                    &session,
                    args.claude_executable.clone(),
                    args.codex_executable.clone(),
                    json_mode,
                )?,
                command,
                target.name.clone(),
                None,
            )
        }
        ProviderKind::Claude | ProviderKind::Fake => success(
            "switch",
            format!("{human}\n\nContinue it with:\n    relay resume"),
            journal,
        ),
    }
}

/// Runs its closure when dropped (unless disarmed by setting the field to `None`).
struct RunOnDrop<F: FnMut()>(Option<F>);

impl<F: FnMut()> Drop for RunOnDrop<F> {
    fn drop(&mut self) {
        if let Some(mut action) = self.0.take() {
            action();
        }
    }
}

/// Bare `relay switch`: choose the target in a terminal-native list. The list is the shared
/// target model (global priority order; the current writer is marked and not selectable;
/// exhausted, disabled or unverifiable profiles are shown as unavailable). Cancelling changes
/// nothing, and the chosen name then takes the exact same path as `relay switch <profile>`.
fn choose_switch_target(
    paths: &RelayPaths,
    registered: &[Profile],
    source: &Profile,
    executables: &providers::ExecutableOverrides,
    project: &Path,
    json_mode: bool,
) -> Result<ProfileName, Error> {
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    if json_mode || !target::interactive() {
        let shallow = target::build_targets(
            registered,
            &preferences,
            &source.name,
            executables,
            project,
            false,
        );
        return Err(Error::SwitchPickerNeedsTerminal(target::describe_rows(
            &shallow,
        )));
    }
    let progress = progress::Progress::start("Checking profiles…", json_mode);
    let targets = target::build_targets(
        registered,
        &preferences,
        &source.name,
        executables,
        project,
        true,
    );
    progress.finish();
    if !targets.iter().any(target::SwitchTarget::selectable) {
        return Err(Error::NoEligibleProfile(source.name.to_string()));
    }
    match target::pick(&targets, &source.name) {
        Ok(Some(index)) => Ok(targets[index].name.clone()),
        Ok(None) => Err(Error::SwitchCancelled),
        Err(_) => Err(Error::SwitchPickerNeedsTerminal(target::describe_rows(
            &targets,
        ))),
    }
}
