//! Non-executing M1 interface for a future Claude Code adapter.
//!
//! No function in this crate reads or mutates a Claude profile yet.

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
    "CLAUDE_CODE_OAUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
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
