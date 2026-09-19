//! Maps a Herdr pane to a registered Relay profile without ever guessing.
//!
//! See the module doc on `lib.rs` and `docs/herdr-integration.md` for why this cannot be derived
//! from `CLAUDE_CONFIG_DIR`: Herdr's `PaneInfo`/`WorkspaceInfo` expose no such field. Instead the
//! mapping is an explicit `relay_profile` token cached in Herdr's own pane/workspace `tokens`
//! map, re-validated against Relay's live profile registry on every use.

use serde::Deserialize;

use crate::client::{CommandRunner, RelayClient};
use crate::error::HerdrIntegrationError;
use crate::{
    HerdrPaneContext, RELAY_FALLBACK_TOKEN_KEY, RELAY_PROFILE_TOKEN_KEY, RELAY_SESSION_TOKEN_KEY,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProfile {
    pub name: String,
    pub config_dir: std::path::PathBuf,
}

/// The subset of `relay profile status <name> --json`'s `data` payload this crate relies on.
/// Deliberately not `deny_unknown_fields`: Relay may add fields later and this is a
/// forward-compatible client, not a schema mirror.
#[derive(Debug, Deserialize)]
struct ProfileStatusView {
    profile: ProfileView,
    authentication: String,
    identity_matches: bool,
}

#[derive(Debug, Deserialize)]
struct ProfileView {
    enabled: bool,
    config_dir: std::path::PathBuf,
}

/// Resolves the pane's `relay_profile` token to a currently-valid Relay profile.
///
/// Order of checks (each one fails closed, never guesses):
/// 1. The pane must be a recognized Claude pane (`HerdrPaneContext::is_claude_pane`).
/// 2. The pane-level and workspace-level `relay_profile` tokens, if both present, must agree.
/// 3. A token must be present at all (pane-level takes priority, workspace-level is the
///    fallback default for every pane in that workspace).
/// 4. The named profile must currently exist, be enabled, be authenticated, and have its
///    observed identity match Relay's registered pin for it — i.e. `relay profile status`
///    must report exactly the healthy state `relay profile doctor` would call safe.
pub fn resolve_profile<R: CommandRunner>(
    pane: &HerdrPaneContext,
    client: &RelayClient<R>,
) -> Result<ResolvedProfile, HerdrIntegrationError> {
    if !pane.is_claude_pane() {
        return Err(HerdrIntegrationError::NonClaudePane);
    }

    let pane_token = pane.pane_tokens.get(RELAY_PROFILE_TOKEN_KEY);
    let workspace_token = pane.workspace_tokens.get(RELAY_PROFILE_TOKEN_KEY);
    let name = match (pane_token, workspace_token) {
        (Some(pane_value), Some(workspace_value)) => {
            if pane_value == workspace_value {
                pane_value
            } else {
                return Err(HerdrIntegrationError::ProfileMappingAmbiguous {
                    candidates: vec![pane_value.clone(), workspace_value.clone()],
                });
            }
        }
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return Err(HerdrIntegrationError::ProfileMappingUnknown),
    };

    let status: ProfileStatusView = match client.run_json(&["profile", "status", name]) {
        Ok(status) => status,
        Err(HerdrIntegrationError::RelayRefused { code, .. }) if code == "profile_not_found" => {
            return Err(HerdrIntegrationError::ProfileMappingUnknown);
        }
        Err(other) => return Err(other),
    };

    if !status.profile.enabled
        || status.authentication != "authenticated"
        || !status.identity_matches
    {
        return Err(HerdrIntegrationError::ProfileMappingUnknown);
    }

    Ok(ResolvedProfile {
        name: name.clone(),
        config_dir: status.profile.config_dir,
    })
}

/// Reads the pane's (or, failing that, its workspace's) `relay_profile_fallback` token: a
/// comma-separated priority list of fallback profile names, in the same shape `relay watch run
/// --fallback` already accepts one profile at a time. This exists so a Herdr action can invoke
/// automatic evaluation or manual handoff with **no action parameters**, matching every current
/// Herdr plugin's convention (`docs/herdr-integration.md`): the fallback order is configured once
/// via `herdr pane|workspace report-metadata --token relay_profile_fallback=bob,carol`, not typed
/// in on every invocation. An absent or empty token yields an empty list; callers that require at
/// least one fallback (see `actions::watch_evaluate`) fail closed on that, same as any other
/// missing mapping.
#[must_use]
pub fn resolve_fallback_profiles(pane: &HerdrPaneContext) -> Vec<String> {
    let raw = pane
        .pane_tokens
        .get(RELAY_FALLBACK_TOKEN_KEY)
        .or_else(|| pane.workspace_tokens.get(RELAY_FALLBACK_TOKEN_KEY));
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

/// The session id an action should actually use: Herdr's own detected `agent_session_id` when
/// present, otherwise the pane's (or workspace's) explicit `relay_session_id` token fallback.
///
/// This fallback exists for a **confirmed real limitation**, not a hypothetical one (M3.2,
/// `docs/herdr-integration.md`): Herdr's built-in Claude integration only watches the default
/// `~/.claude`, so a pane running Claude under an isolated Relay profile's own `CLAUDE_CONFIG_DIR`
/// never gets an `agent_session` from Herdr at all. Rather than silently treating that pane as
/// having no session (which would make every session-requiring action permanently unusable for
/// isolated profiles), a pane may carry an explicit, one-time-set token instead — still
/// deterministic, still never guessed, just supplied by the operator once instead of derived.
/// Pane-level and workspace-level values disagreeing is ambiguous, exactly like profile mapping;
/// this never silently prefers one.
pub fn resolve_session_id(
    pane: &HerdrPaneContext,
) -> Result<Option<String>, HerdrIntegrationError> {
    if let Some(id) = &pane.agent_session_id {
        return Ok(Some(id.clone()));
    }
    let pane_token = pane.pane_tokens.get(RELAY_SESSION_TOKEN_KEY);
    let workspace_token = pane.workspace_tokens.get(RELAY_SESSION_TOKEN_KEY);
    match (pane_token, workspace_token) {
        (Some(pane_value), Some(workspace_value)) => {
            if pane_value == workspace_value {
                Ok(Some(pane_value.clone()))
            } else {
                Err(HerdrIntegrationError::ProfileMappingAmbiguous {
                    candidates: vec![pane_value.clone(), workspace_value.clone()],
                })
            }
        }
        (Some(value), None) | (None, Some(value)) => Ok(Some(value.clone())),
        (None, None) => Ok(None),
    }
}
