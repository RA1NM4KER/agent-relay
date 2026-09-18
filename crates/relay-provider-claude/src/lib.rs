//! Read-only profile inspection and non-executing process plans for Claude Code.

mod adoption;
mod inspection;
mod session_transfer;

pub use adoption::ClaudeAdoptionProvider;
pub use inspection::{
    ClaudeIdentityPin, ClaudeInspectionReport, ClaudeInspector, CommandRunner,
    EnvironmentOverrideStatus, EnvironmentVariableStatus, ProcessResult, ProcessSpec,
    SystemCommandRunner, inspect_environment, inspect_environment_with,
};
pub use session_transfer::{
    ProcessLister, SessionTransferReport, StagedArtifact, SystemProcessLister, discover_session,
    ensure_supported_claude_version, escape_project_path, stage_transfer, validate_session_id,
};

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use relay_core::{IdentityMetadata, ProfileName, Provider, Result};
use serde::Serialize;

/// Native cross-profile continuation is not a stable provider API.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTransferSupport {
    BestEffortVersionGated,
}

pub const SESSION_TRANSFER_SUPPORT: SessionTransferSupport =
    SessionTransferSupport::BestEffortVersionGated;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateClaudeProfileRequest {
    pub name: ProfileName,
    pub config_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectClaudeProfileRequest {
    pub config_dir: PathBuf,
}

/// Adoption is reference-only. The directory and its credentials remain provider-owned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdoptClaudeProfileRequest {
    pub name: ProfileName,
    pub config_dir: PathBuf,
    pub expected_identity: IdentityMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyClaudeIdentityRequest {
    pub config_dir: PathBuf,
    pub expected_identity: IdentityMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeLaunchRequest {
    pub config_dir: PathBuf,
    pub project_dir: PathBuf,
    pub arguments: Vec<OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeProcessPlan {
    pub executable: PathBuf,
    pub current_dir: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub remove_environment: Vec<OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCommandPlanner {
    executable: PathBuf,
}

impl ClaudeCommandPlanner {
    #[must_use]
    pub const fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    #[must_use]
    pub fn plan_auth_login(&self, request: &CreateClaudeProfileRequest) -> ClaudeProcessPlan {
        self.plan(
            &request.config_dir,
            &request.config_dir,
            vec![OsString::from("auth"), OsString::from("login")],
        )
    }

    #[must_use]
    pub fn plan_auth_status(&self, request: &InspectClaudeProfileRequest) -> ClaudeProcessPlan {
        self.plan(
            &request.config_dir,
            &request.config_dir,
            vec![
                OsString::from("auth"),
                OsString::from("status"),
                OsString::from("--json"),
            ],
        )
    }

    #[must_use]
    pub fn plan_launch(&self, request: &ClaudeLaunchRequest) -> ClaudeProcessPlan {
        self.plan(
            &request.config_dir,
            &request.project_dir,
            request.arguments.clone(),
        )
    }

    fn plan(
        &self,
        config_dir: &Path,
        current_dir: &Path,
        arguments: Vec<OsString>,
    ) -> ClaudeProcessPlan {
        let environment = BTreeMap::from([(
            OsString::from("CLAUDE_CONFIG_DIR"),
            config_dir.as_os_str().to_os_string(),
        )]);
        let remove_environment = AUTHENTICATION_OVERRIDE_VARIABLES
            .iter()
            .map(OsString::from)
            .collect();
        ClaudeProcessPlan {
            executable: self.executable.clone(),
            current_dir: current_dir.to_path_buf(),
            arguments,
            environment,
            remove_environment,
        }
    }
}

/// Values are removed, never inspected or logged.
pub const AUTHENTICATION_OVERRIDE_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_AWS_API_KEY",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    "ANTHROPIC_IDENTITY_TOKEN_FILE",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "AWS_ACCESS_KEY_ID",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_PROFILE",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AZURE_CLIENT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_TENANT_ID",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "CLAUDE_CODE_CLIENT_KEY",
    "CLAUDE_CODE_CLIENT_KEY_PASSPHRASE",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SKIP_ANTHROPIC_AWS_AUTH",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "CLAUDE_CODE_SKIP_MANTLE_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
];

impl ClaudeProcessPlan {
    #[must_use]
    pub fn references_config_dir(&self, expected: &Path) -> bool {
        self.environment
            .get(&OsString::from("CLAUDE_CONFIG_DIR"))
            .is_some_and(|value| Path::new(value) == expected)
    }
}

/// Execution is deliberately absent in M1. A later adapter must implement this interface
/// without returning or persisting credential material.
pub trait ClaudeProfileBackend: Provider {
    fn plan_create_profile(
        &self,
        request: &CreateClaudeProfileRequest,
    ) -> Result<ClaudeProcessPlan>;

    fn inspect_auth_status(
        &self,
        request: &InspectClaudeProfileRequest,
    ) -> Result<relay_core::ProviderObservation>;

    fn inspect_adoption_candidate(
        &self,
        request: &InspectClaudeProfileRequest,
    ) -> Result<relay_core::ProviderObservation>;

    /// Validate an adoption candidate without copying credentials or changing its directory.
    fn validate_adoption(
        &self,
        request: &AdoptClaudeProfileRequest,
    ) -> Result<relay_core::ProviderObservation>;

    fn verify_identity(&self, request: &VerifyClaudeIdentityRequest) -> Result<bool>;

    fn plan_launch(&self, request: &ClaudeLaunchRequest) -> Result<ClaudeProcessPlan>;
}
