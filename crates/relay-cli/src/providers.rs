//! M6: picks the concrete provider adapters for a [`relay_core::ProviderKind`], so command
//! handlers (`relay switch`, `relay watch run`) never `match` on provider identity themselves —
//! they ask this module once per profile and get back trait objects `relay-core`'s
//! provider-neutral ports already know how to use.

use std::path::{Path, PathBuf};

use relay_core::{
    ProviderKind, Result,
    handoff::{ContextCapturer, SessionStager, SessionStopper, SourceLiveness, TargetLauncher},
    usage::UsageSignal,
};

use relay_provider_claude::{
    ClaudeContextCapturer, ClaudeSessionStager, ClaudeSessionStopper, ClaudeSourceLiveness,
    ClaudeTargetLauncher, ClaudeUsageSignal,
};
use relay_provider_codex::{
    CodexContextCapturer, CodexSessionStopper, CodexSourceLiveness, CodexTargetLauncher,
    CodexUsageSignal,
};

/// Explicit executables for controlled validation/testing, mirroring the existing
/// `--claude-executable` flags. `None` resolves from `PATH`.
#[derive(Clone, Debug, Default)]
pub struct ExecutableOverrides {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
}

/// One provider's full set of handoff-port adapters. Held as `Box<dyn Trait>` (not `&dyn Trait`)
/// because the concrete type differs per provider and callers need one uniform value to build a
/// [`relay_core::handoff::HandoffCoordinator`] from regardless of which provider was requested.
pub struct ProviderPorts {
    pub liveness: Box<dyn SourceLiveness>,
    pub stopper: Box<dyn SessionStopper>,
    /// `Some` only for Claude — the only provider with a `SessionStager` (used solely for
    /// `SESSION_CONTINUATION`, which only ever happens Claude-to-Claude; see
    /// `relay_provider_codex::PROVIDER_CAPABILITIES.native_session_transfer`).
    pub stager: Option<Box<dyn SessionStager>>,
    pub launcher: Box<dyn TargetLauncher>,
    pub context_capturer: Box<dyn ContextCapturer>,
}

#[must_use]
pub fn ports_for(provider: ProviderKind, executables: &ExecutableOverrides) -> ProviderPorts {
    match provider {
        ProviderKind::Codex => ProviderPorts {
            liveness: Box::new(CodexSourceLiveness),
            stopper: Box::new(CodexSessionStopper),
            stager: None,
            launcher: Box::new(CodexTargetLauncher::new(executables.codex.clone())),
            context_capturer: Box::new(CodexContextCapturer),
        },
        ProviderKind::Claude | ProviderKind::Fake => ProviderPorts {
            liveness: Box::new(ClaudeSourceLiveness::new(executables.claude.clone())),
            stopper: Box::new(ClaudeSessionStopper::new(executables.claude.clone())),
            stager: Some(Box::new(ClaudeSessionStager)),
            launcher: Box::new(ClaudeTargetLauncher::new(executables.claude.clone())),
            context_capturer: Box::new(ClaudeContextCapturer),
        },
    }
}

/// A provider's usage/exhaustion signal. `probe` mirrors `relay watch run`'s existing
/// `--probe`/`ledger.is_known_exhausted` gating for Claude's real-API-spend probe; Codex's signal
/// is a read-only structured rate-limit query that never spends anything (see
/// `relay_provider_codex::usage`), so the flag is accepted for interface uniformity but has no
/// effect there.
#[must_use]
pub fn usage_signal_for(
    provider: ProviderKind,
    executables: &ExecutableOverrides,
    probe: bool,
    workload_model: Option<String>,
) -> Box<dyn UsageSignal> {
    match provider {
        ProviderKind::Codex => Box::new(CodexUsageSignal::new(executables.codex.clone())),
        ProviderKind::Claude | ProviderKind::Fake => Box::new(
            ClaudeUsageSignal::new(executables.claude.clone(), probe)
                .with_workload_model(workload_model),
        ),
    }
}

/// Resolves the provider executable to use for launching/inspecting a profile whose provider is
/// already known, from the pair of possibly-set CLI override flags.
#[must_use]
pub fn executable_override(
    provider: ProviderKind,
    executables: &ExecutableOverrides,
) -> Option<&Path> {
    match provider {
        ProviderKind::Codex => executables.codex.as_deref(),
        ProviderKind::Claude | ProviderKind::Fake => executables.claude.as_deref(),
    }
}

/// Constructs a [`relay_core::Provider`] trait object for the given kind — the port
/// `ProfileService::add`/`status`/`doctor` need. Returns an error only if the provider's CLI
/// cannot be discovered (mirrors each backend's own `discover`).
pub fn provider_backend(
    provider: ProviderKind,
    executables: &ExecutableOverrides,
) -> Result<Box<dyn relay_core::Provider>> {
    match provider {
        ProviderKind::Codex => Ok(Box::new(relay_provider_codex::CodexBackend::discover(
            executables.codex.as_deref(),
        )?)),
        ProviderKind::Claude | ProviderKind::Fake => Ok(Box::new(
            relay_provider_claude::ClaudeAdoptionProvider::discover(executables.claude.as_deref())?,
        )),
    }
}
