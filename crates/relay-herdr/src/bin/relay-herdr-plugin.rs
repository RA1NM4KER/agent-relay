//! The argv-invoked program a `herdr-plugin.toml` action actually runs (`plugins/herdr/`).
//!
//! **Not live-verified.** `docs/herdr-integration.md` documents `HERDR_PLUGIN_CONTEXT_JSON`'s
//! *general* shape (workspace/tab/pane/worktree/agent/selection context, varying by where the
//! action was invoked from) from Herdr's own docs and its `api schema --json` dump, but the exact
//! byte-for-byte field layout for a `pane`-context action was not captured into this repository
//! (the M3 safety boundary explicitly forbids linking this plugin into a live Herdr server or
//! running `herdr api snapshot` against real session data). The parsing below is written
//! defensively — it accepts the documented field names, is tolerant of extra/missing optional
//! fields, and fails closed (a clear stderr message, non-zero exit) rather than guessing — but it
//! must be checked against a real `HERDR_PLUGIN_CONTEXT_JSON` payload (`herdr plugin link`, a
//! disposable workspace, no real account) before this plugin is trusted. See
//! `M3_OVERNIGHT_REPORT.md` for the exact steps.
//!
//! Every action shares this same entry point; the action name is `argv[1]`, matching
//! `HERDR_PLUGIN_ACTION_ID` (both are set by Herdr for consistency, only one is actually read
//! here). Output is a small JSON object on stdout; exit code 0 for success, 1 for a refusal Relay
//! or the mapping layer reported, 2 for a usage/environment error in the plugin invocation itself.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use relay_herdr::actions::{self, WatchEvaluateRequest};
use relay_herdr::client::RelayClient;
use relay_herdr::mapping;
use relay_herdr::{HerdrIntegrationError, HerdrPaneContext};
use serde::Deserialize;
use serde_json::json;

fn main() -> ExitCode {
    let action = match env::args().nth(1) {
        Some(action) => action,
        None => {
            eprintln!("usage: relay-herdr-plugin <status|doctor|handoff|watch|recovery>");
            return ExitCode::from(2);
        }
    };

    let pane = match load_pane_context() {
        Ok(pane) => pane,
        Err(message) => {
            eprintln!("relay-herdr-plugin: {message}");
            return ExitCode::from(2);
        }
    };

    let client = match RelayClient::discover(relay_executable_override().as_deref()) {
        Ok(client) => client,
        Err(error) => return report_error(&error),
    };

    let outcome = match action.as_str() {
        "status" => actions::status(&pane, &client).map(|status| {
            json!({
                "profile": status.profile.name,
                "authentication": status.profile_status.authentication,
                "identity_matches": status.profile_status.identity_matches,
                "usage_state": status.profile_status.availability_state,
                "writer_locked": status.lock.locked,
                "writer_lease_owner": status.lock.lease_owner,
                "current_transaction": status.lock.current_transaction,
            })
        }),
        "doctor" => actions::doctor(&pane, &client).map(|report| {
            json!({
                "healthy": report.healthy,
                "checks": report.checks,
            })
        }),
        "recovery" => actions::recovery_status(&pane, &client).map(|recovery| {
            json!({
                "locked": recovery.locked,
                "lease_owner": recovery.lease_owner,
                "transaction_id": recovery.transaction_id,
                "transaction_state": recovery.transaction_state,
                "transaction_reason": recovery.transaction_reason,
                "is_terminal": recovery.is_terminal,
            })
        }),
        "watch" => {
            let fallback = mapping::resolve_fallback_profiles(&pane);
            actions::watch_evaluate(
                &pane,
                &client,
                &WatchEvaluateRequest {
                    fallback_profiles: &fallback,
                    dry_run: false,
                    workload_model: None,
                },
            )
            .map(|outcome| json!({ "outcome": format!("{outcome:?}") }))
        }
        "handoff" => {
            let fallback = mapping::resolve_fallback_profiles(&pane);
            match fallback.first() {
                Some(target) => actions::handoff_manual(&pane, &client, target)
                    .map(|journal| json!({ "transaction_id": journal.transaction_id, "state": journal.state.state })),
                None => Err(HerdrIntegrationError::ProfileMappingUnknown),
            }
        }
        other => {
            eprintln!("relay-herdr-plugin: unknown action '{other}'");
            return ExitCode::from(2);
        }
    };

    match outcome {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error),
    }
}

fn report_error(error: &HerdrIntegrationError) -> ExitCode {
    eprintln!(
        "{}",
        json!({ "error": error.to_string(), "kind": format!("{error:?}") })
    );
    ExitCode::FAILURE
}

/// An explicit override for controlled testing, deliberately undocumented in the manifest: real
/// use always relies on `RelayClient::discover`'s normal `PATH` search.
fn relay_executable_override() -> Option<PathBuf> {
    env::var_os("RELAY_HERDR_PLUGIN_RELAY_EXECUTABLE").map(PathBuf::from)
}

// ---------------------------------------------------------------------------------------------
// HERDR_PLUGIN_CONTEXT_JSON parsing — see the module-level warning above.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PluginContext {
    pane: Option<PaneInfo>,
    workspace: Option<WorkspaceInfo>,
}

#[derive(Debug, Deserialize)]
struct PaneInfo {
    cwd: Option<PathBuf>,
    foreground_cwd: Option<PathBuf>,
    agent: Option<String>,
    agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AgentSessionInfo {
    kind: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

fn load_pane_context() -> Result<HerdrPaneContext, String> {
    let raw = env::var("HERDR_PLUGIN_CONTEXT_JSON").map_err(|_| {
        "HERDR_PLUGIN_CONTEXT_JSON is not set (Herdr metadata unavailable)".to_owned()
    })?;
    let context: PluginContext = serde_json::from_str(&raw)
        .map_err(|error| format!("HERDR_PLUGIN_CONTEXT_JSON did not parse: {error}"))?;
    let pane = context
        .pane
        .ok_or_else(|| "this action has no pane context (run it from a focused pane)".to_owned())?;
    let working_directory = pane
        .foreground_cwd
        .or(pane.cwd)
        .ok_or_else(|| "pane context has no cwd".to_owned())?;
    let agent_session_id = pane
        .agent_session
        .filter(|session| session.kind == "id")
        .map(|session| session.value);
    let workspace_tokens = context
        .workspace
        .map(|workspace| workspace.tokens)
        .unwrap_or_default();

    Ok(HerdrPaneContext {
        pane_id: env::var("HERDR_PANE_ID").unwrap_or_default(),
        working_directory,
        agent: pane.agent,
        agent_session_id,
        pane_tokens: pane.tokens,
        workspace_tokens,
    })
}
