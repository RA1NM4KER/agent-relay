use std::path::PathBuf;

use crate::{
    AuthenticationState, AvailabilityObservation, IdentityMetadata, ProfileName, ProviderKind,
    Result,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileSetupMode {
    Create,
    AdoptExisting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileSetupRequest {
    pub name: ProfileName,
    pub config_dir: PathBuf,
    pub mode: ProfileSetupMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderObservation {
    pub authentication: AuthenticationState,
    pub identity: Option<IdentityMetadata>,
    pub availability: AvailabilityObservation,
}

pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    /// Creates provider-local test/setup state. For `AdoptExisting`, implementations must only
    /// inspect the referenced directory and must never copy or rewrite credentials.
    fn setup_profile(&self, request: &ProfileSetupRequest) -> Result<ProviderObservation>;

    fn inspect_profile(&self, config_dir: &std::path::Path) -> Result<ProviderObservation>;
}

/// A declarative, data-only summary of what a provider adapter can actually do. Each
/// `relay-provider-*` crate publishes one `const` value of this shape; `relay-core` never
/// computes it and never branches on provider identity itself — callers (routing, `relay
/// switch`, `relay profiles`) read the fields to decide what is safe to attempt, keeping any
/// per-provider logic out of `relay-core`. M6: introduced so Claude and Codex can be compared
/// uniformly without a growing `match ProviderKind { .. }` spreading through the core crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCapabilities {
    /// Can this provider resume ITS OWN prior session/thread under the SAME profile
    /// (`NATIVE_RESUME`)? True for Codex (`codex resume <thread-id>`); true for Claude
    /// (`claude -p --resume <id>`) in the trivial same-profile case.
    pub native_session_resume: bool,
    /// Can a session/thread started under one profile of this provider be resumed under a
    /// DIFFERENT profile of the same provider (`SESSION_CONTINUATION` across profiles)? Only
    /// Claude has this proven (M2B); Codex's session state is local to one `CODEX_HOME` with no
    /// documented cross-home transfer, so this is `false` even though `native_session_resume`
    /// is `true`.
    pub native_session_transfer: bool,
    /// Can this provider's local, on-disk conversation state be read to build a bounded
    /// [`crate::handoff::ContinuationBundle`] when it is the SOURCE of a `STATE_CONTINUATION`?
    pub state_export: bool,
    /// Can this provider be bootstrapped with a [`crate::handoff::ContinuationBundle`] when it
    /// is the TARGET of a `STATE_CONTINUATION`?
    pub state_import: bool,
    /// Does this provider expose a trustworthy, structured (non-prose) usage/exhaustion signal
    /// Relay can act on automatically? `false` means only manual switching away from this
    /// provider is supported — see docs/automatic-handoff.md.
    pub usage_detection: bool,
    /// Can Relay discover/stop a running writer process for this provider from outside (used by
    /// `SourceLiveness`/`SessionStopper`)?
    pub process_control: bool,
    /// Does this provider expose a structured (JSON) auth/login status check?
    pub auth_status: bool,
}
