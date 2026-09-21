//! `/relay status | switch [profile] | adopt` typed inside a Claude session.
//!
//! The command is answered by Relay's own hook (`relay hook claude prompt`, a `UserPromptSubmit`
//! hook): the prompt is blocked before any model turn, so the model never sees the command, never
//! chooses an identity, and costs nothing. Everything the command acts on is proven from Claude's
//! own structured signals ([`crate::live::identify`]); the arguments are the only thing the user
//! types.
//!
//! `status` and `adopt` are answered right here (read-only / one atomic lease write). `switch` is
//! never performed by the agent's own process: it is a request to the Relay process supervising
//! this terminal ([`crate::control`]), which runs the normal switch transaction and follows the
//! conversation to its new owner.

use std::{path::Path, time::Duration};

use relay_core::{
    Error, Profile, ProfileService, ProviderKind, RelayPaths,
    handoff::{LeaseStore, ProjectId, WriterLease},
};
use serde_json::json;

use crate::{
    control::{self, ControlDir, RequestKind},
    live::{self, AdoptionOutcome, HookEnv, HookInput, LiveSession},
    preferences::Preferences,
    providers,
    target::{self, SwitchTarget},
};

/// How long the hook waits for the supervisor to accept or refuse a switch request.
const SUPERVISOR_ANSWER_TIMEOUT: Duration = Duration::from_secs(25);

/// The JSON a `UserPromptSubmit` hook prints to stop the prompt and show `text` to the user.
#[must_use]
pub fn block_output(text: &str) -> String {
    json!({"decision": "block", "reason": text}).to_string()
}

/// Splits `/relay <sub> <args…>`. `None` when the prompt is not a `/relay` command at all.
#[must_use]
pub fn parse_command(prompt: &str) -> Option<(String, Vec<String>)> {
    let trimmed = prompt.trim();
    let rest = trimmed.strip_prefix("/relay")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut words = rest.split_whitespace().map(str::to_owned);
    let sub = words.next().unwrap_or_else(|| "status".to_owned());
    Some((sub, words.collect()))
}

/// Answers a prompt if it is a `/relay` command; `None` lets every other prompt through untouched.
#[must_use]
pub fn answer(paths: &RelayPaths, config_dir: &Path, stdin: &[u8]) -> Option<String> {
    let input = HookInput::parse(stdin)?;
    let (sub, args) = parse_command(input.prompt.as_deref()?)?;
    let text = match live::identify(&input, config_dir, &HookEnv::from_process()) {
        Ok(session) => run_subcommand(paths, &session, &sub, &args),
        Err(error) => {
            format!("Agent Relay cannot verify this conversation, so it did nothing.\n{error}")
        }
    };
    Some(block_output(&text))
}

struct Context<'a> {
    paths: &'a RelayPaths,
    session: &'a LiveSession,
    service: ProfileService,
    registered: Vec<Profile>,
    preferences: Preferences,
    lease: Option<WriterLease>,
    state_dir: std::path::PathBuf,
}

impl Context<'_> {
    /// The profile that owns this very conversation *and* the lease says so.
    fn managed_owner(&self) -> Option<&Profile> {
        let lease = self.lease.as_ref()?;
        if lease.session_id != self.session.session_id {
            return None;
        }
        self.registered
            .iter()
            .find(|profile| profile.name == lease.owner_profile)
            .filter(|profile| {
                std::fs::canonicalize(&profile.config_dir).ok().as_deref()
                    == Some(self.session.config_dir.as_path())
            })
    }
}

fn run_subcommand(paths: &RelayPaths, session: &LiveSession, sub: &str, args: &[String]) -> String {
    let service = ProfileService::new(paths.clone());
    let Ok(registered) = service.list() else {
        return "Agent Relay could not read its profiles.".to_owned();
    };
    let Ok(project_id) = ProjectId::for_canonical_path(&session.project) else {
        return "Agent Relay could not identify this project.".to_owned();
    };
    let state_dir = paths.project_state_dir(&project_id);
    let lease = LeaseStore::at_path(state_dir.join("lease.json"))
        .load()
        .ok()
        .flatten();
    let context = Context {
        paths,
        session,
        service,
        registered,
        preferences: Preferences::load(paths.config_root())
            .ok()
            .flatten()
            .unwrap_or_default(),
        lease,
        state_dir,
    };
    match sub {
        "status" => status(&context),
        "adopt" => adopt(&context),
        "switch" => switch(&context, args),
        other => format!(
            "Unknown /relay command '{other}'. Try: /relay status · /relay switch [profile] · /relay adopt"
        ),
    }
}

fn project_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn rows(context: &Context<'_>, owner: &Profile) -> Vec<SwitchTarget> {
    target::build_targets(
        &context.registered,
        &context.preferences,
        &owner.name,
        &providers::ExecutableOverrides::default(),
        &context.session.project,
        false,
    )
}

fn continuity_phrase(from: ProviderKind, to: ProviderKind) -> &'static str {
    match (from, to) {
        (ProviderKind::Codex, ProviderKind::Codex) => "native resume of the same Codex thread",
        (ProviderKind::Codex, _) | (_, ProviderKind::Codex) => {
            "continues from Relay's state bundle in a new session"
        }
        _ => "the same Claude conversation continues",
    }
}

fn status(context: &Context<'_>) -> String {
    let project = project_name(&context.session.project);
    let Some(owner) = context.managed_owner() else {
        let registered = context.registered.iter().any(|profile| {
            profile.provider == ProviderKind::Claude
                && std::fs::canonicalize(&profile.config_dir).ok().as_deref()
                    == Some(context.session.config_dir.as_path())
        });
        return if registered {
            format!(
                "Agent Relay: this conversation ({project}) is not managed by Relay.\n\
                 Bring it under Relay in place with /relay adopt."
            )
        } else {
            format!(
                "Agent Relay: this conversation ({project}) is not managed, and its Claude profile \
                 is not registered with Relay.\nRegister it with `relay login <name>` first."
            )
        };
    };
    let automatic = relay_provider_claude::integration_status(&context.session.config_dir)
        .is_ok_and(|status| status.installed);
    let next = rows(context, owner)
        .into_iter()
        .find(SwitchTarget::selectable);
    let mut lines = vec![
        "Agent Relay: managed".to_owned(),
        format!("Project: {project}"),
        format!("Provider: Claude · Profile: {}", owner.name),
        format!(
            "Automatic handoff: {}",
            if automatic {
                "on"
            } else {
                "off (run `relay setup`)"
            }
        ),
    ];
    lines.push(match next {
        Some(next) => format!(
            "Next eligible: {} ({}) — {}",
            next.name,
            next.provider_label(),
            continuity_phrase(owner.provider, next.provider)
        ),
        None => "Next eligible: none".to_owned(),
    });
    let control = ControlDir::for_project(&context.state_dir);
    lines.push(match control.live_supervisor() {
        Some(record)
            if context.lease.as_ref().is_some_and(|lease| {
                record.session_id == lease.session_id
                    && record.owner_profile == lease.owner_profile.as_str()
            }) =>
        {
            "In-session switch: ready".to_owned()
        }
        _ => "In-session switch: not available here — this terminal was not started through \
              `relay claude`/`relay resume`; use `relay switch` from another terminal"
            .to_owned(),
    });
    if let Some(last) = control.last_result() {
        lines.push(format!("Last in-session action: {}", last.message));
    }
    lines.join("\n")
}

fn adopt(context: &Context<'_>) -> String {
    if let Some(owner) = context.managed_owner() {
        return format!(
            "Agent Relay: already managed (profile {}). Nothing to do.",
            owner.name
        );
    }
    let outcome = live::adopt_claude(
        &context.service,
        context.paths,
        context.session,
        None,
        &providers::ExecutableOverrides::default(),
    );
    match outcome {
        Ok(AdoptionOutcome::Adopted {
            profile,
            automatic_handoff,
        }) => format!(
            "Agent Relay: adopted this conversation in place — Claude · {} · profile {} · session {}…\n\
             {}Use /relay status, /relay switch, or `relay resume` from a terminal.",
            project_name(&context.session.project),
            profile,
            &context.session.session_id[..8],
            if automatic_handoff {
                ""
            } else {
                "Automatic handoff is off for this profile (run `relay setup`).\n"
            }
        ),
        Ok(AdoptionOutcome::AlreadyManaged { profile }) => {
            format!("Agent Relay: already managed (profile {profile}). Nothing to do.")
        }
        Err(Error::WriterAlreadyActive(owner)) => format!(
            "Agent Relay refused: this project already has a live Relay-managed conversation \
             (profile {owner}). Continue it with `relay resume`, move it with `relay switch`, or \
             replace it explicitly with `relay claude --new`. Nothing was changed."
        ),
        Err(error) => format!("Agent Relay refused: {error}"),
    }
}

fn switch(context: &Context<'_>, args: &[String]) -> String {
    let Some(owner) = context.managed_owner() else {
        return "Agent Relay: this conversation is not managed, so there is nothing to switch. \
                Use /relay adopt first."
            .to_owned();
    };
    let targets = rows(context, owner);
    let Some(name) = args.first() else {
        let listing: Vec<String> = targets
            .iter()
            .map(|target| {
                format!(
                    "  {}. {} ({}){}",
                    target.priority + 1,
                    target.name,
                    target.provider_label(),
                    if target.current {
                        " — current".to_owned()
                    } else if let Some(reason) = &target.unavailable {
                        format!(" — unavailable: {reason}")
                    } else {
                        String::new()
                    }
                )
            })
            .collect();
        return format!(
            "Agent Relay: switch this conversation to one of:\n{}\nType /relay switch <profile>.",
            listing.join("\n")
        );
    };
    let Some(chosen) = targets.iter().find(|target| target.name.as_str() == name) else {
        return format!(
            "Agent Relay: '{name}' is not a registered profile. Options: {}",
            target::describe_rows(&targets)
        );
    };
    if chosen.current {
        return format!("Agent Relay: '{name}' already holds this conversation.");
    }
    if let Some(reason) = &chosen.unavailable {
        return format!("Agent Relay: '{name}' is unavailable ({reason}).");
    }

    let control = ControlDir::for_project(&context.state_dir);
    let lease = context
        .lease
        .as_ref()
        .expect("managed_owner implies a lease");
    let supervised = control.live_supervisor().filter(|record| {
        record.session_id == lease.session_id
            && record.owner_profile == lease.owner_profile.as_str()
    });
    if supervised.is_none() {
        return format!(
            "Agent Relay: this terminal is not supervised by Relay (it was not started through \
             `relay claude` or `relay resume`), so it cannot switch itself. From another terminal, \
             run `relay switch {name}`."
        );
    }
    let id = crate::new_session_uuid().unwrap_or_else(|_| "req".to_owned());
    let request = control::request(
        id.clone(),
        RequestKind::Switch {
            target: name.clone(),
        },
        &lease.session_id,
        lease.owner_profile.as_str(),
        context.session.pid,
    );
    if control.submit(&request).is_err() {
        return "Agent Relay could not reach the process supervising this terminal.".to_owned();
    }
    match control.await_response(&id, SUPERVISOR_ANSWER_TIMEOUT) {
        Some(response) => response.message,
        None => "Agent Relay: the supervising process did not answer, so nothing was switched."
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_relay_commands_are_recognised() {
        assert_eq!(parse_command("/relay"), Some(("status".to_owned(), vec![])));
        assert_eq!(
            parse_command("  /relay switch megan "),
            Some(("switch".to_owned(), vec!["megan".to_owned()]))
        );
        assert_eq!(parse_command("/relayx status"), None);
        assert_eq!(parse_command("please /relay status"), None);
        assert_eq!(parse_command("hello"), None);
    }

    #[test]
    fn a_block_decision_carries_the_text_for_the_user() {
        let out: serde_json::Value = serde_json::from_str(&block_output("hi")).unwrap();
        assert_eq!(out["decision"], "block");
        assert_eq!(out["reason"], "hi");
    }
}
