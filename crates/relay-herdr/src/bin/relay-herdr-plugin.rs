//! The argv-invoked program a `herdr-plugin.toml` action actually runs (`plugins/herdr/`).
//!
//! **Live-verified against Herdr 0.9.0** (M3.2, see `docs/herdr-integration.md`). Two real
//! findings from that verification, both incorporated below:
//!
//! 1. `HERDR_PLUGIN_CONTEXT_JSON` is a **flat** object with no `tokens` map and no
//!    `agent_session` field at all — it only names *which* pane/workspace this invocation is
//!    about (`focused_pane_id`, `workspace_id`, `focused_pane_cwd`, `focused_pane_agent`,
//!    `focused_pane_status`). Getting tokens and the Claude session id requires a real follow-up
//!    call to Herdr's own CLI (`herdr pane get <id>`, `herdr workspace get <id>`), using the
//!    `HERDR_BIN_PATH` Herdr already hands every plugin invocation — exactly the way a human
//!    operator would look it up, never guessed from the context payload.
//! 2. `herdr pane report-metadata`/`herdr workspace report-metadata` require an explicit
//!    `--source <id>` (undocumented in the M3.1-era manifest notes); this only affects the
//!    one-time setup commands an operator runs, not this binary.
//!
//! Every action shares this same entry point; the action name is `argv[1]` (equivalently
//! `HERDR_PLUGIN_ACTION_ID`, only one is read here). Output is a small JSON object on stdout;
//! exit code 0 for success, 1 for a refusal Relay/Herdr/the mapping layer reported, 2 for a
//! usage/environment error in the plugin invocation itself.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use relay_herdr::actions::{self, WatchEvaluateRequest};
use relay_herdr::client::RelayClient;
use relay_herdr::herdr_client::{AgentSessionView, HerdrCliClient};
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
        Err(error) => {
            eprintln!("relay-herdr-plugin: {error}");
            return report_error(&error);
        }
    };

    let client = match discover_relay_client() {
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
                    simulate_usage: None,
                },
            )
            .map(|outcome| json!({ "outcome": format!("{outcome:?}") }))
        }
        "handoff" => {
            let fallback = mapping::resolve_fallback_profiles(&pane);
            match fallback.first() {
                Some(target) => actions::handoff_manual(&pane, &client, target).map(|journal| {
                    json!({ "transaction_id": journal.transaction_id, "state": journal.state.state })
                }),
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
/// use always relies on the discovery order below.
fn relay_executable_override() -> Option<PathBuf> {
    env::var_os("RELAY_HERDR_PLUGIN_RELAY_EXECUTABLE").map(PathBuf::from)
}

/// Live-confirmed gotcha (M3.2, `docs/herdr-integration.md`): Herdr spawns plugin commands with
/// the **server's own PATH**, not the interactive shell's — `relay` was not found even though it
/// resolves fine from an ordinary terminal, exactly the "wrapper needed for a missing PATH entry"
/// problem the reference plugins (`herdr-claude-auto-retry`, `herdr-agent-usage`) already hit for
/// their own runtimes.
///
/// Discovery order:
/// 1. `RELAY_HERDR_PLUGIN_RELAY_EXECUTABLE` (explicit override, for tests/operators).
/// 2. `$HERDR_PLUGIN_ROOT/../../target/release/relay` — valid only for a **locally linked**
///    plugin (`herdr plugin link`, the documented dev/test path): `HERDR_PLUGIN_ROOT` is
///    `<repo>/plugins/herdr`, so this resolves to the same cargo workspace's own build output,
///    the same way `herdr-plugin.toml`'s own `command` paths do.
/// 3. A normal `PATH` search (`RelayClient::discover(None)`), for a real system install
///    (`cargo install --path crates/relay-cli`, per the main README).
///
/// **Known gap, not solved here**: a marketplace-style `herdr plugin install` (as opposed to
/// `link`) may check out only the `plugins/herdr` subdirectory, in which case step 2's relative
/// path would not exist and only step 3 (a real `relay` on `PATH`) would work. Packaging for
/// `plugin install` is explicitly deferred — see `M3_FINAL_REPORT.md`.
fn discover_relay_client() -> Result<RelayClient, HerdrIntegrationError> {
    if let Some(path) = relay_executable_override() {
        return RelayClient::with_runner(path, relay_herdr::client::SystemCommandRunner);
    }
    if let Some(plugin_root) = env::var_os("HERDR_PLUGIN_ROOT") {
        let candidate = PathBuf::from(plugin_root).join("../../target/release/relay");
        if candidate.is_file() {
            return RelayClient::with_runner(candidate, relay_herdr::client::SystemCommandRunner);
        }
    }
    RelayClient::discover(None)
}

// ---------------------------------------------------------------------------------------------
// HERDR_PLUGIN_CONTEXT_JSON + follow-up `herdr pane|workspace get` — see the module-level notes.
// ---------------------------------------------------------------------------------------------

/// The flat shape confirmed live from a real `herdr plugin action invoke`. Deliberately tolerant
/// (`#[serde(default)]` throughout) since only `focused_pane_id` is load-bearing here; every
/// other field is read straight through by Herdr's own UI/logging, not re-derived by us.
#[derive(Debug, Deserialize, Default)]
struct PluginContext {
    #[serde(default)]
    focused_pane_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

fn load_pane_context() -> Result<HerdrPaneContext, HerdrIntegrationError> {
    let raw = env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .map_err(|_| HerdrIntegrationError::HerdrMetadataUnavailable)?;
    let context: PluginContext =
        serde_json::from_str(&raw).map_err(|_| HerdrIntegrationError::HerdrMetadataUnavailable)?;
    let pane_id = context
        .focused_pane_id
        .ok_or(HerdrIntegrationError::HerdrMetadataUnavailable)?;

    let herdr_bin = env::var_os("HERDR_BIN_PATH").map(PathBuf::from);
    let herdr = HerdrCliClient::discover(herdr_bin.as_deref())?;

    let pane_info = herdr.pane_get(&pane_id)?;
    let agent_session_id = AgentSessionView::resolve_id(pane_info.agent_session.as_ref())?;
    let working_directory = pane_info
        .foreground_cwd
        .or(pane_info.cwd)
        .ok_or(HerdrIntegrationError::HerdrMetadataUnavailable)?;

    let workspace_tokens = match context.workspace_id {
        Some(workspace_id) => herdr.workspace_get(&workspace_id)?.tokens,
        None => std::collections::BTreeMap::new(),
    };

    Ok(HerdrPaneContext {
        pane_id,
        working_directory,
        agent: pane_info.agent,
        agent_session_id,
        pane_tokens: pane_info.tokens,
        workspace_tokens,
    })
}
