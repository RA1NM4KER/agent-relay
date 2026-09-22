//! Internal hook entry points a live Claude session invokes on its own behalf (never surfaced as
//! a normal user command; a hook must never fail, print into, or block the calling session):
//! `StopFailure`, the statusline badge, the `/relay …` prompt hook, and the `SessionStart` half
//! of `relay claude --resume`'s adoption protocol (whose environment-variable contract —
//! `RELAY_ADOPT_*` — is also owned here, since this module is what consumes it).

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use relay_core::{Error, ProfileName, ProfileService, RelayPaths};
use relay_provider_claude::{handle_statusline, handle_stop_failure, read_stdin_bounded};
use serde_json::json;

use crate::{
    agent_cmd, auto_handoff, badge,
    cli::{ClaudeHookCommand, Cli, HookArgs, HookCommand},
    live, preferences, providers,
    util::current_unix_ms,
};

/// Runs inside a live Claude Code session: never prints errors, never fails the session.
pub(crate) fn run_hook(hook: &HookArgs, cli: &Cli) -> ExitCode {
    let HookCommand::Claude(claude) = &hook.command;
    let stdin = read_stdin_bounded(std::io::stdin());
    let now = current_unix_ms();
    match &claude.command {
        ClaudeHookCommand::StopFailure { config_dir } => {
            handle_stop_failure(config_dir, &stdin, now);
            // The evidence is recorded first so the evaluation this may start can see it.
            trigger_automatic_handoff(cli, config_dir, &stdin);
            ExitCode::SUCCESS
        }
        ClaudeHookCommand::Statusline { config_dir, chain } => {
            // The badge is best-effort and additive: any doubt about the session means no badge.
            let badge = hook_paths(cli)
                .and_then(|paths| badge::badge_for(&paths, &stdin))
                .map(|plain| badge::styled(&plain));
            let code =
                handle_statusline(config_dir, &stdin, now, chain.as_deref(), badge.as_deref());
            ExitCode::from(u8::try_from(code).unwrap_or(0))
        }
        ClaudeHookCommand::Prompt { config_dir } => {
            // Any prompt that is not `/relay …` passes through untouched, silently.
            if let Some(paths) = hook_paths(cli)
                && let Some(output) = agent_cmd::answer(&paths, config_dir, &stdin)
            {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        ClaudeHookCommand::SessionStart { config_dir } => {
            if let Some(paths) = hook_paths(cli)
                && let Some(output) = resume_adoption_hook(&paths, config_dir, &stdin)
            {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// Environment the supervising `relay claude --resume` sets for the Claude it launches, so that
/// the `SessionStart` hook acts only inside *that* launch and never in an unrelated session.
pub(crate) const ADOPT_PROFILE_ENV: &str = "RELAY_ADOPT_PROFILE";
pub(crate) const ADOPT_RESULT_ENV: &str = "RELAY_ADOPT_RESULT";
pub(crate) const ADOPT_SESSION_ENV: &str = "RELAY_ADOPT_SESSION";
/// The Relay session id `relay claude --resume` pre-assigned for a conversation Relay does not
/// know yet, and the user's own provider arguments for the launch (a JSON array).
pub(crate) const ADOPT_RELAY_SESSION_ENV: &str = "RELAY_ADOPT_RELAY_SESSION";
pub(crate) const ADOPT_ARGS_ENV: &str = "RELAY_ADOPT_ARGS";

/// The `SessionStart` half of `relay claude --resume`: Claude reports which conversation it just
/// resumed (picker or explicit id); Relay proves it structurally and adopts exactly that one.
/// Records the outcome for the waiting supervisor and tells the user in Claude's own UI.
fn resume_adoption_hook(paths: &RelayPaths, config_dir: &Path, stdin: &[u8]) -> Option<String> {
    let expected = ProfileName::new(std::env::var(ADOPT_PROFILE_ENV).ok()?).ok()?;
    let result_path = PathBuf::from(std::env::var_os(ADOPT_RESULT_ENV)?);
    let input = live::HookInput::parse(stdin)?;
    // Only the resume itself: `/clear`, compaction and fresh starts are not what was requested.
    if input.source.as_deref() != Some("resume") || result_path.exists() {
        return None;
    }
    let outcome = (|| -> Result<live::AdoptionOutcome, Error> {
        if let Ok(wanted) = std::env::var(ADOPT_SESSION_ENV)
            && input.session_id.as_deref() != Some(wanted.as_str())
        {
            return Err(Error::AdoptionRefused(
                "Claude resumed a different conversation than the one requested".to_owned(),
            ));
        }
        // Claude registers the running session a moment after `SessionStart`; wait briefly.
        let mut last = None;
        for _ in 0..30 {
            match live::identify(&input, config_dir, &live::HookEnv::from_process()) {
                Ok(session) => {
                    let service = ProfileService::new(paths.clone());
                    let preassigned = std::env::var(ADOPT_RELAY_SESSION_ENV)
                        .ok()
                        .and_then(|id| relay_core::handoff::RelaySessionId::parse(&id).ok());
                    let user_args: Vec<String> = std::env::var(ADOPT_ARGS_ENV)
                        .ok()
                        .and_then(|text| serde_json::from_str(&text).ok())
                        .unwrap_or_default();
                    return live::adopt_claude(
                        &service,
                        paths,
                        &session,
                        Some(&expected),
                        &providers::ExecutableOverrides::default(),
                        preassigned,
                        user_args,
                    );
                }
                Err(error) => last = Some(error),
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(last.unwrap_or(Error::ProviderUnsupported))
    })();
    let (ok, message, relay_session) = match &outcome {
        Ok(result) => (
            true,
            format!(
                "Agent Relay: this conversation is now managed (profile {}, session {}).",
                match result {
                    live::AdoptionOutcome::Adopted { profile, .. }
                    | live::AdoptionOutcome::Reactivated { profile, .. }
                    | live::AdoptionOutcome::AlreadyManaged { profile, .. } => profile,
                },
                result.relay_session_id().short()
            ),
            Some(result.relay_session_id().to_string()),
        ),
        Err(error) => (
            false,
            format!("Agent Relay did not adopt this conversation: {error}"),
            None,
        ),
    };
    let _ignored = std::fs::write(
        &result_path,
        json!({"ok": ok, "message": message, "relay_session_id": relay_session}).to_string(),
    );
    Some(json!({"systemMessage": message}).to_string())
}

/// Starts a one-shot, detached automatic-handoff evaluation when a rate-limit `StopFailure` hook
/// fires for the session Relay manages (see [`auto_handoff`] for why this is the trigger). Best
/// effort and silent: a hook must never fail, print into, or block the Claude session it runs in.
/// The Relay roots a hook process should use: the global overrides when given, else the defaults.
fn hook_paths(cli: &Cli) -> Option<RelayPaths> {
    let discovered = RelayPaths::discover().ok()?;
    let config_root = cli
        .config_root
        .clone()
        .unwrap_or_else(|| discovered.config_root().to_path_buf());
    let state_root = cli
        .state_root
        .clone()
        .unwrap_or_else(|| discovered.state_root().to_path_buf());
    RelayPaths::new(config_root, state_root).ok()
}

fn trigger_automatic_handoff(cli: &Cli, config_dir: &Path, stdin: &[u8]) {
    let Some(paths) = hook_paths(cli) else {
        return;
    };
    let service = ProfileService::new(paths.clone());
    let Ok(Some(preferences)) = preferences::Preferences::load(paths.config_root()) else {
        return;
    };
    if let Some(plan) = auto_handoff::plan(&paths, &service, &preferences, config_dir, stdin) {
        auto_handoff::spawn_detached(&plan);
    }
}
