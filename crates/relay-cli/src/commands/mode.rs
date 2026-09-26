//! `relay mode`/`relay mode autonomous`/`relay mode interactive`: GitHub Issue #3's canonical way
//! to show or change the CURRENT Relay Session's [`ExecutionIntent`] after it was already created
//! — `relay claude --autonomous`/`relay codex --autonomous` set the initial value at launch;
//! `relay resume --autonomous`/`--interactive` change it while resuming; this command and the live
//! Claude `/relay:mode`/Codex `$relay mode` commands (see [`crate::agent_cmd`]) change it from
//! inside an already-running conversation. Every one of those converges on the same
//! [`relay_core::handoff::SessionStore::set_execution_intent`] — never a second, independent
//! mutation path — and [`render_set_message`] is the one place the human-facing "mode changed"
//! text is built, so a live in-session change and a terminal `relay mode` invocation say exactly
//! the same thing to whichever agent/human reads it.
//!
//! Execution mode is behavioral intent only — it never touches a provider permission flag, a
//! sandbox setting, or passthrough arguments. See [`relay_core::handoff::ExecutionIntent`]'s own
//! doc comment.

use relay_core::{
    Error, ProfileService, RelayPaths,
    handoff::{ExecutionIntent, RelaySessionView, SessionState, render_live_mode_notice},
};
use serde_json::json;

use crate::{
    cli::{ModeArgs, ModeValue},
    output::{CommandOutput, success},
    providers, sessions,
};

impl From<ModeValue> for ExecutionIntent {
    fn from(value: ModeValue) -> Self {
        match value {
            ModeValue::Autonomous => Self::Autonomous,
            ModeValue::Interactive => Self::Interactive,
        }
    }
}

fn intent_label(intent: ExecutionIntent) -> &'static str {
    match intent {
        ExecutionIntent::Autonomous => "autonomous",
        ExecutionIntent::Interactive => "interactive",
    }
}

fn resolve_project(project_dir: Option<&std::path::Path>) -> Result<std::path::PathBuf, Error> {
    let dir = match project_dir {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: std::path::PathBuf::from("."),
            source,
        })?,
    };
    std::fs::canonicalize(&dir).map_err(|source| Error::Io { path: dir, source })
}

/// Resolves the one Relay Session `relay mode` means: this project's currently ACTIVE session
/// (never a dormant one — there is no live agent for a live mode change to reach). An explicit
/// `--session` is resolved directly and then checked, so a dormant/unknown selector fails with the
/// same clear, specific error either way rather than silently doing nothing.
fn resolve_active_session(
    service: &ProfileService,
    paths: &RelayPaths,
    canonical_project: &std::path::Path,
    selector: Option<&str>,
    json_mode: bool,
) -> Result<RelaySessionView, Error> {
    let registered = service.list()?;
    let executables = providers::ExecutableOverrides::default();
    let store = sessions::open_store(paths, canonical_project)?;
    let views = sessions::reconcile(paths, canonical_project, &registered, &executables)?;
    let view = match selector {
        Some(selector) => store.resolve(selector)?,
        None => sessions::choose_session(
            &store,
            &views,
            &registered,
            sessions::Want::Active,
            None,
            None,
            json_mode,
        )?,
    };
    if view.state() != SessionState::Active {
        return Err(Error::RelaySessionDormant(
            view.record.relay_session_id.short().to_owned(),
        ));
    }
    Ok(view)
}

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &ModeArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let canonical_project = resolve_project(args.project_dir.as_deref())?;
    let view = resolve_active_session(
        service,
        paths,
        &canonical_project,
        args.session.as_deref(),
        json_mode,
    )?;
    match args.mode {
        None => show(view.record.execution_intent),
        Some(value) => {
            let store = sessions::open_store(paths, &canonical_project)?;
            let intent = store.set_execution_intent(&view.record.relay_session_id, value.into())?;
            set(intent)
        }
    }
}

fn show(intent: ExecutionIntent) -> Result<CommandOutput, Error> {
    success(
        "mode.show",
        format!("Execution mode: {}", intent_label(intent)),
        json!({ "execution_intent": intent, "changed": false }),
    )
}

/// The message every "mode was just set" path shares — `relay mode autonomous`/`relay mode
/// interactive`, `/relay:mode ...`, and `$relay mode ...` all render through this. Two lines: a
/// short confirmation, then [`render_live_mode_notice`] so the reader (human or the live agent
/// itself) knows exactly what changed in behavioral terms, not just which enum variant is now set.
pub(crate) fn render_set_message(intent: ExecutionIntent) -> String {
    format!(
        "Execution mode: {}\n{}",
        intent_label(intent),
        render_live_mode_notice(intent)
    )
}

fn set(intent: ExecutionIntent) -> Result<CommandOutput, Error> {
    success(
        "mode.set",
        render_set_message(intent),
        json!({ "execution_intent": intent, "changed": true }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_value_maps_onto_execution_intent_directly() {
        assert_eq!(
            ExecutionIntent::from(ModeValue::Autonomous),
            ExecutionIntent::Autonomous
        );
        assert_eq!(
            ExecutionIntent::from(ModeValue::Interactive),
            ExecutionIntent::Interactive
        );
    }

    #[test]
    fn set_message_names_the_mode_and_includes_the_behavioral_notice() {
        let autonomous = render_set_message(ExecutionIntent::Autonomous);
        assert!(autonomous.starts_with("Execution mode: autonomous\n"));
        assert!(autonomous.contains("continue"));
        assert!(!autonomous.to_lowercase().contains("permission"));

        let interactive = render_set_message(ExecutionIntent::Interactive);
        assert!(interactive.starts_with("Execution mode: interactive\n"));
    }
}
