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
