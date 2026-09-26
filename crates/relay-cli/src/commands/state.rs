//! `relay state show`/`relay state update`: Issue #5's durable, provider-neutral, advisory
//! working state for a Relay Session. Agents never touch `working_state.json` directly — this
//! module owns session lookup, schema validation, bounds, locking (via
//! [`relay_core::handoff::WorkingStateStore`]), and persistence, so a coding agent only ever
//! needs to know the CLI surface, never Relay's internal state layout.

use std::io::Read as _;

use relay_core::{
    Error, ProfileService, RelayPaths,
    handoff::{RelaySessionView, SessionState, WorkingState, WorkingStateUpdate},
};
use serde_json::json;

use crate::{
    cli::StateCommand,
    output::{CommandOutput, success},
    providers, sessions,
    util::current_unix_ms,
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    command: &StateCommand,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    match command {
        StateCommand::Show {
            project_dir,
            session,
        } => show(
            service,
            paths,
            project_dir.as_deref(),
            session.as_deref(),
            json_mode,
        ),
        StateCommand::Update {
            project_dir,
            session,
            input,
        } => update(
            service,
            paths,
            project_dir.as_deref(),
            session.as_deref(),
            input,
            json_mode,
        ),
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

/// Resolves which Relay Session a bare (no-argument) `relay state` invocation means: the
/// project's one active session. An explicit `--session` selector is resolved directly instead
/// (any state, active or dormant, so `show` can inspect history after a session ends).
fn resolve_session(
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
    match selector {
        Some(selector) => store.resolve(selector),
        None => sessions::choose_session(
            &store,
            &views,
            &registered,
            sessions::Want::Active,
            None,
            None,
            json_mode,
        ),
    }
}

fn show(
    service: &ProfileService,
    paths: &RelayPaths,
    project_dir: Option<&std::path::Path>,
    session: Option<&str>,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let canonical_project = resolve_project(project_dir)?;
    let view = resolve_session(service, paths, &canonical_project, session, json_mode)?;
    let store = sessions::open_store(paths, &canonical_project)?;
    let state = store.working_state(&view.record.relay_session_id).load()?;
    success(
        "state.show",
        render_human(state.as_ref()),
        render_json(state.as_ref()),
    )
}

fn update(
    service: &ProfileService,
    paths: &RelayPaths,
    project_dir: Option<&std::path::Path>,
    session: Option<&str>,
    input: &str,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let canonical_project = resolve_project(project_dir)?;
    let view = resolve_session(service, paths, &canonical_project, session, json_mode)?;
    if view.state() != SessionState::Active {
        return Err(Error::RelaySessionDormant(
            view.record.relay_session_id.short().to_owned(),
        ));
    }
    let raw = if input == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|source| Error::Io {
                path: std::path::PathBuf::from("<stdin>"),
                source,
            })?;
        buffer
    } else {
        input.to_owned()
    };
    let parsed: WorkingStateUpdate = serde_json::from_str(&raw)
        .map_err(|error| Error::WorkingStateInvalid(format!("invalid update JSON: {error}")))?;
    let store = sessions::open_store(paths, &canonical_project)?;
    let state = store
        .working_state(&view.record.relay_session_id)
        .update(parsed, current_unix_ms())?;
    success(
        "state.update",
        render_human(Some(&state)),
        render_json(Some(&state)),
    )
}

fn render_human(state: Option<&WorkingState>) -> String {
    let Some(state) = state else {
        return "No working state recorded yet for this session.".to_owned();
    };
    let mut lines = Vec::new();
    lines.push(format!(
        "Goal: {}",
        state.goal.as_deref().unwrap_or("(none)")
    ));
    lines.push(format!(
        "Current subtask: {}",
        state.current_subtask.as_deref().unwrap_or("(none)")
    ));
    if state.decisions.is_empty() {
        lines.push("Decisions: (none)".to_owned());
    } else {
        lines.push("Decisions:".to_owned());
        for decision in &state.decisions {
            let status = match decision.status {
                relay_core::handoff::EntryStatus::Active => "active",
                relay_core::handoff::EntryStatus::Superseded => "superseded",
            };
            lines.push(format!(
                "  [{status}] {} ({})",
                decision.summary,
                decision.id.as_str()
            ));
            if let Some(rationale) = &decision.rationale {
                lines.push(format!("      rationale: {rationale}"));
            }
        }
    }
    if state.failed_attempts.is_empty() {
        lines.push("Failed attempts: (none)".to_owned());
    } else {
        lines.push("Failed attempts:".to_owned());
        for attempt in &state.failed_attempts {
            lines.push(format!("  - {} — {}", attempt.approach, attempt.reason));
        }
    }
    if state.relevant_files.is_empty() {
        lines.push("Relevant files: (none)".to_owned());
    } else {
        lines.push("Relevant files:".to_owned());
        for file in &state.relevant_files {
            match &file.role {
                Some(role) => lines.push(format!("  - {}: {role}", file.path)),
                None => lines.push(format!("  - {}", file.path)),
            }
        }
    }
    if state.next_actions.is_empty() {
        lines.push("Next actions: (none)".to_owned());
    } else {
        lines.push("Next actions:".to_owned());
        for action in &state.next_actions {
            lines.push(format!("  - {action}"));
        }
    }
    lines.push(format!("Last updated: {} (unix ms)", state.updated_unix_ms));
    lines.join("\n")
}

fn render_json(state: Option<&WorkingState>) -> serde_json::Value {
    match state {
        Some(state) => json!({ "present": true, "working_state": state }),
        None => json!({ "present": false, "working_state": null }),
    }
}
