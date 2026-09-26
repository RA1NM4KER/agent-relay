//! Read-only profile inspection and non-executing process plans for the Codex CLI — the M6
//! Codex adapter, structured to mirror `relay-provider-claude`'s shape wherever Codex actually
//! has an equivalent (see each module's doc comment for where it genuinely doesn't).

pub mod app_server;
mod context_capture;
mod handoff_adapters;
mod inspection;
pub mod polling;
mod usage;

pub use context_capture::CodexContextCapturer;
pub use handoff_adapters::{
    CodexSessionStopper, CodexSourceLiveness, CodexTargetLauncher, LaunchedThread,
    launch_new_thread,
};
pub use inspection::{
    CodexAuthStatus, CodexInspector, VERIFIED_VERSIONS, VersionStatus, assess_version,
};
pub use usage::{CodexUsageReading, CodexUsageSignal};

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use relay_core::{
    AuthenticationState, Availability, AvailabilityObservation, IdentityMetadata,
    ProfileSetupRequest, Provider, ProviderCapabilities, ProviderKind, ProviderObservation, Result,
};
use sha2::{Digest, Sha256};

/// M6: Codex's declared capabilities — see [`ProviderCapabilities`]'s field docs.
///
/// `native_session_transfer` is `false`: unlike Claude, Codex has no documented way to resume a
/// thread created under a different `CODEX_HOME` — thread history is local to the home that
/// created it (`thread_history_*.sqlite` under `CODEX_HOME`, observed directly, not documented
/// upstream). A cross-profile Codex handoff is therefore always `STATE_CONTINUATION`, never
/// `SESSION_CONTINUATION`, even though same-profile resume (`native_session_resume`) works.
///
/// `usage_detection` is `true` (M7): `codex app-server`'s typed `account/rateLimits/read` gives an
/// account-validated `ordinaryUsageAllowed` verdict plus usage windows — see `usage.rs`. Anything
/// unavailable or ambiguous maps to `UNKNOWN`, which routing never acts on.
pub const PROVIDER_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    native_session_resume: true,
    native_session_transfer: false,
    state_export: false,
    state_import: true,
    usage_detection: true,
    process_control: true,
    auth_status: true,
};

/// Values are removed, never inspected or logged — mirrors
/// `relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES`. Codex's official login flow
/// (`codex login`) is file-based (`CODEX_HOME/auth.json`); these are ambient-environment
/// override vectors the underlying OpenAI-compatible client stack may honor, and Relay must
/// never let one leak an ambient credential into an isolated profile's process.
pub const AUTHENTICATION_OVERRIDE_VARIABLES: &[&str] = &[
    "OPENAI_API_KEY",
    "OPENAI_ORGANIZATION",
    "OPENAI_ORG_ID",
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "CODEX_AUTH_TOKEN",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexProcessPlan {
    pub executable: PathBuf,
    pub current_dir: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub remove_environment: Vec<OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexCommandPlanner {
    executable: PathBuf,
}

impl CodexCommandPlanner {
    #[must_use]
    pub const fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    /// `codex login` under the profile's isolated `CODEX_HOME` — the official login flow, never
    /// a credential Relay itself reads or writes. If browser/device interaction is required,
    /// that happens inside this exact process; Relay only launches and waits.
    #[must_use]
    pub fn plan_login(&self, config_dir: &Path) -> CodexProcessPlan {
        self.plan(config_dir, config_dir, vec![OsString::from("login")])
    }

    #[must_use]
    pub fn plan_logout(&self, config_dir: &Path) -> CodexProcessPlan {
        self.plan(config_dir, config_dir, vec![OsString::from("logout")])
    }

    /// Interactive `codex resume <thread-id>` under this exact profile — `NATIVE_RESUME`. Never
    /// crosses profiles: the thread must have been created under this same `CODEX_HOME`.
    #[must_use]
    pub fn plan_resume(
        &self,
        config_dir: &Path,
        project_dir: &Path,
        thread_id: &str,
    ) -> CodexProcessPlan {
        self.plan(
            config_dir,
            project_dir,
            vec![OsString::from("resume"), OsString::from(thread_id)],
        )
    }

    fn plan(
        &self,
        config_dir: &Path,
        current_dir: &Path,
        arguments: Vec<OsString>,
    ) -> CodexProcessPlan {
        let environment = BTreeMap::from([(
            OsString::from("CODEX_HOME"),
            config_dir.as_os_str().to_os_string(),
        )]);
        let remove_environment = AUTHENTICATION_OVERRIDE_VARIABLES
            .iter()
            .map(OsString::from)
            .collect();
        CodexProcessPlan {
            executable: self.executable.clone(),
            current_dir: current_dir.to_path_buf(),
            arguments,
            environment,
            remove_environment,
        }
    }
}

impl CodexProcessPlan {
    #[must_use]
    pub fn references_config_dir(&self, expected: &Path) -> bool {
        self.environment
            .get(&OsString::from("CODEX_HOME"))
            .is_some_and(|value| Path::new(value) == expected)
    }
}

/// Wires [`CodexInspector`] into the provider-neutral [`Provider`] port `relay-core` uses for
/// setup/adoption/status. Codex has no separate "adopt an existing profile" flow distinct from
/// "point an isolated `CODEX_HOME` at it and check `codex doctor --json`" — both `setup_profile`
/// modes end up doing the same read-only inspection, since Relay never writes credentials
/// either way.
#[derive(Clone, Debug)]
pub struct CodexBackend {
    inspector: CodexInspector,
}

impl CodexBackend {
    pub fn discover(codex_executable: Option<&Path>) -> Result<Self> {
        Ok(Self {
            inspector: CodexInspector::discover(codex_executable)?,
        })
    }
}

impl Provider for CodexBackend {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn setup_profile(&self, request: &ProfileSetupRequest) -> Result<ProviderObservation> {
        let _ = request.mode; // Create and AdoptExisting both reduce to inspection; see doc comment.
        self.inspect_profile(&request.config_dir, request.claude_config_mode)
    }

    fn inspect_profile(
        &self,
        config_dir: &Path,
        _claude_config_mode: Option<relay_core::ClaudeConfigMode>,
    ) -> Result<ProviderObservation> {
        let auth = self.inspector.inspect_auth_status(config_dir)?;
        let observed_unix_ms = now_unix_ms();
        let (authentication, availability_state) = if auth.authenticated {
            (AuthenticationState::Authenticated, Availability::Available)
        } else {
            (AuthenticationState::Required, Availability::AuthRequired)
        };
        // Codex exposes no account identity in redacted output (see inspection.rs's module
        // doc). The stable id is therefore derived from the isolated CODEX_HOME path itself,
        // which IS the real isolation boundary Relay enforces — not a substitute for a real
        // account pin, and documented as a known M6 limitation in M6_FINAL_REPORT.md.
        let identity = auth.authenticated.then(|| IdentityMetadata {
            stable_id: config_dir_stable_id(config_dir),
            display_label: (!auth.summary.is_empty()).then_some(auth.summary),
        });
        Ok(ProviderObservation {
            authentication,
            identity,
            availability: AvailabilityObservation {
                state: availability_state,
                source: "codex doctor --json: checks.auth.credentials".to_owned(),
                observed_unix_ms,
                reset_unix_ms: None,
            },
        })
    }
}

/// Codex's supported structured APIs do not expose a redacted account identity. Relay's pinned
/// identity for a Codex profile is consequently its validated isolated `CODEX_HOME` path.
#[must_use]
pub fn config_dir_stable_id(config_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(config_dir.as_os_str().as_encoded_bytes());
    format!("codex:v1:home:{:x}", hasher.finalize())
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{CodexCommandPlanner, config_dir_stable_id};
    use std::path::{Path, PathBuf};

    #[test]
    fn plans_set_codex_home_and_strip_override_variables() {
        let planner = CodexCommandPlanner::new(PathBuf::from("/usr/local/bin/codex"));
        let config_dir = Path::new("/config/codex-main");
        let plan = planner.plan_login(config_dir);
        assert!(plan.references_config_dir(config_dir));
        assert!(
            plan.remove_environment
                .iter()
                .any(|v| v == "OPENAI_API_KEY")
        );
        assert_eq!(plan.arguments, vec!["login"]);
    }

    #[test]
    fn resume_plan_uses_the_project_dir_as_cwd_and_targets_one_thread() {
        let planner = CodexCommandPlanner::new(PathBuf::from("/usr/local/bin/codex"));
        let plan = planner.plan_resume(
            Path::new("/config/codex-main"),
            Path::new("/work/proj"),
            "01a0bf34-8d81-7062-bc7e-f5e053d2f7de",
        );
        assert_eq!(plan.current_dir, Path::new("/work/proj"));
        assert_eq!(
            plan.arguments,
            vec!["resume", "01a0bf34-8d81-7062-bc7e-f5e053d2f7de"]
        );
    }

    #[test]
    fn stable_id_is_deterministic_and_distinguishes_different_homes() {
        let a = config_dir_stable_id(Path::new("/config/codex-main"));
        let b = config_dir_stable_id(Path::new("/config/codex-backup"));
        assert_ne!(a, b);
        assert_eq!(a, config_dir_stable_id(Path::new("/config/codex-main")));
    }
}
