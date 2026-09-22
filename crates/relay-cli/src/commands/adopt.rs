//! `relay adopt --session <id>`: bring an already-running Claude conversation under Relay from
//! outside it (see the function doc comment below for why this exists alongside the in-session
//! `/relay adopt`).

use relay_core::{Error, Profile, ProfileService, ProviderKind, RelayPaths};
use serde_json::json;

use crate::{
    cli::AdoptArgs,
    live,
    output::{CommandOutput, success},
    providers,
    util::project_display_name,
};

/// `relay adopt --session <id>`: brings an already-running Claude conversation under Relay from
/// OUTSIDE it — no in-session `/relay adopt` required. This exists because a Claude process
/// started before Relay's `/relay` command was installed never sees it (Claude only loads custom
/// commands at session start), so the in-session hook path is structurally unreachable for a
/// conversation that predates the install. Searches every registered Claude profile's own live
/// session registry (NativeDefault included, exactly like any other registered profile) for the
/// one that structurally owns `--session`, unless `--profile` pins the search — never guessing
/// from a profile name. Never restarts or stops the Claude process; the exact-session safety
/// invariants `adopt_claude` already enforces (one live owner per native conversation, atomic
/// lease creation) apply unchanged.
pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &AdoptArgs,
) -> Result<CommandOutput, Error> {
    if !live::is_session_uuid(&args.session) {
        return Err(Error::AdoptionRefused(format!(
            "'{}' is not a Claude session id",
            args.session
        )));
    }
    let registered = service.list()?;
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: None,
    };

    if let Some(name) = &args.profile {
        let pinned = registered
            .iter()
            .find(|profile| &profile.name == name)
            .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
        if pinned.provider != ProviderKind::Claude {
            return Err(Error::ProfileProviderMismatch {
                profile: name.to_string(),
                expected: "Claude",
                actual: pinned.provider.to_string(),
            });
        }
    }
    let all_claude: Vec<&Profile> = registered
        .iter()
        .filter(|profile| profile.provider == ProviderKind::Claude)
        .collect();
    let checked = all_claude.len();

    // Every registered Claude profile is checked structurally regardless of `--profile`: an
    // explicit-but-wrong pin still gets told which profile actually owns the live conversation,
    // exactly like the exact-uuid discovery in `relay claude --resume` — never a bare "not found".
    let mut live_owner: Option<(&Profile, live::LiveSession)> = None;
    for profile in &all_claude {
        match live::identify_external(
            &profile.config_dir,
            profile.effective_claude_config_mode(),
            args.claude_executable.as_deref(),
            &args.session,
        ) {
            Ok(live_session) => {
                if live_owner.is_some() {
                    return Err(Error::AdoptionRefused(format!(
                        "conversation '{}' appears live under more than one registered profile; \
                         pass `--profile <name>` to choose",
                        args.session
                    )));
                }
                live_owner = Some((profile, live_session));
            }
            Err(_) => continue,
        }
    }
    let (profile, live_session) = match (live_owner, &args.profile) {
        (Some((profile, _live_session)), Some(pinned)) if &profile.name != pinned => {
            return Err(Error::AdoptionRefused(format!(
                "'{pinned}' has no live process for session '{}'; it is live under profile '{}' \
                 instead — run `relay adopt --session {} --profile {}`",
                args.session, profile.name, args.session, profile.name
            )));
        }
        (Some(found), _) => found,
        (None, _) => {
            return Err(Error::AdoptionRefused(format!(
                "no registered Claude profile ({checked} checked) has a live process for \
                 session '{}'; register the profile that owns it first (`relay profile adopt \
                 --provider claude --native-default`, or `--config-dir <dir>` for an isolated \
                 one)",
                args.session
            )));
        }
    };

    let outcome = live::adopt_claude(
        service,
        paths,
        &live_session,
        args.profile.as_ref(),
        &executables,
        None,
        Vec::new(),
    )?;

    let (verb, relay_session_id, automatic_handoff) = match &outcome {
        live::AdoptionOutcome::Adopted {
            relay_session_id,
            automatic_handoff,
            ..
        } => ("adopted", relay_session_id.clone(), *automatic_handoff),
        live::AdoptionOutcome::Reactivated {
            relay_session_id,
            automatic_handoff,
            ..
        } => ("reactivated", relay_session_id.clone(), *automatic_handoff),
        live::AdoptionOutcome::AlreadyManaged {
            relay_session_id, ..
        } => ("already managed", relay_session_id.clone(), false),
    };
    let human = format!(
        "Session {} {verb} under profile '{}' as Relay session {}.\nProject: {}\nAutomatic \
         handoff: {}\n\n`relay status` now shows it ACTIVE; `relay switch --session {} <target>` \
         moves it.",
        args.session,
        profile.name,
        relay_session_id.short(),
        project_display_name(&live_session.project),
        if automatic_handoff {
            "enabled"
        } else {
            "not installed for this profile"
        },
        relay_session_id.short(),
    );
    success(
        "adopt",
        human,
        json!({
            "profile": profile.name.as_str(),
            "relay_session_id": relay_session_id.as_str(),
            "session_id": args.session,
            "project_dir": live_session.project,
            "outcome": verb,
        }),
    )
}
