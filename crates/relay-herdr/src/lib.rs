//! Herdr adapter boundary for Agent Relay (M3).
//!
//! Herdr is never required for core profile storage or safety decisions: everything in this
//! crate is a thin, optional layer that maps Herdr pane/workspace metadata onto a *registered*
//! Relay profile and then invokes Relay's existing, already-tested `relay --json` CLI surface.
//! It never re-implements profile identity, usage policy, writer-lease authority, transactional
//! handoff, source-stop verification, session transfer, target verification, conflict
//! resolution, or recovery — see `docs/herdr-integration.md` for the full design record and
//! `docs/herdr-integration.md#responsibilities-we-deliberately-do-not-duplicate` in particular.
//!
//! Design note (M3.0/M3.1): Herdr's own `PaneInfo`/`WorkspaceInfo` carry no `CLAUDE_CONFIG_DIR`,
//! pid, or provider-identity field of any kind — confirmed live against Herdr 0.9.0's socket
//! schema. Mapping a pane to a Relay profile therefore uses Herdr's own `tokens` extension point
//! (`herdr pane|workspace report-metadata --token relay_profile=<name>`) rather than guessing
//! from environment or process state. [`mapping::resolve_profile`] never trusts a cached token
//! blindly: it re-confirms the named profile is still registered, enabled, authenticated, and
//! identity-matched before any action proceeds, and fails closed on anything ambiguous or stale.

pub mod actions;
pub mod client;
pub mod error;
pub mod herdr_client;
pub mod install;
pub mod mapping;
pub mod usage_interop;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use error::HerdrIntegrationError;

/// The Herdr `tokens` map key Relay's plugin reads/writes to cache a resolved profile mapping.
pub const RELAY_PROFILE_TOKEN_KEY: &str = "relay_profile";

/// The Herdr `tokens` map key holding a comma-separated fallback-profile priority list, so
/// actions that need a fallback/target (manual handoff, automatic watch/evaluate) can stay
/// parameterless, matching current Herdr plugin conventions. See
/// [`mapping::resolve_fallback_profiles`].
pub const RELAY_FALLBACK_TOKEN_KEY: &str = "relay_profile_fallback";

/// The Herdr `tokens` map key holding an explicit Claude session id, used only as a fallback when
/// Herdr's own `agent_session` detection is unavailable. **Live-confirmed genuine upstream
/// limitation** (M3.2, `docs/herdr-integration.md`): Herdr's built-in Claude integration
/// (`herdr integration install claude`) hard-codes the default `~/.claude` config directory —
/// there is no per-profile/`--config-dir` variant — so a Claude session running under an isolated
/// Relay profile's own `CLAUDE_CONFIG_DIR` is invisible to Herdr's session detection today. This
/// token is the "smallest explicit binding mechanism" fallback: set once
/// (`herdr pane report-metadata --token relay_session_id=<uuid>`) and reused by every subsequent
/// action against that pane, never re-typed per invocation. See
/// [`mapping::resolve_session_id`].
pub const RELAY_SESSION_TOKEN_KEY: &str = "relay_session_id";

/// Everything the adapter needs about "the pane Herdr says is focused/current," gathered from
/// Herdr's socket/CLI API. Construction of this type (talking to Herdr) is deliberately outside
/// this crate's scope for M3.1 — see [`HerdrAdapter`] — so every field here is exactly what
/// Herdr's own `PaneInfo`/`WorkspaceInfo` schema documents, nothing inferred or guessed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HerdrPaneContext {
    pub pane_id: String,
    pub working_directory: PathBuf,
    /// Herdr's `PaneInfo.agent` field: `Some("claude")` for a detected Claude pane, `Some(other)`
    /// for a different agent kind, `None` if Herdr has not detected any agent in this pane.
    pub agent: Option<String>,
    /// Herdr's `PaneInfo.agent_session.value` when `agent_session.kind == "id"` — the Claude
    /// session id Herdr's own `SessionStart`-hook-driven integration recorded for this pane.
    pub agent_session_id: Option<String>,
    /// Herdr's `PaneInfo.tokens` map (pane-scoped `report-metadata` tokens).
    pub pane_tokens: BTreeMap<String, String>,
    /// Herdr's `WorkspaceInfo.tokens` map for the workspace this pane belongs to
    /// (workspace-scoped `report-metadata` tokens, used as a fallback default).
    pub workspace_tokens: BTreeMap<String, String>,
}

impl HerdrPaneContext {
    #[must_use]
    pub fn is_claude_pane(&self) -> bool {
        self.agent.as_deref() == Some("claude")
    }
}

/// Fetches Herdr's own view of the focused pane. Implementations talk to the Herdr socket/CLI;
/// this crate never assumes which transport is used. A fake implementation for tests lives
/// alongside `client::ScriptedCommandRunner` in each module's test suite.
pub trait HerdrAdapter: Send + Sync {
    fn focused_pane(&self) -> Result<HerdrPaneContext, HerdrIntegrationError>;
}
