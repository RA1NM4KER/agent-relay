//! Deterministic provider behavior for tests and the pre-alpha M1 CLI.

use std::path::Path;

use relay_core::{
    AtomicWrite, AuthenticationState, Availability, AvailabilityObservation, Error, FsAtomicWriter,
    IdentityMetadata, ProfileSetupMode, ProfileSetupRequest, Provider, ProviderKind,
    ProviderObservation, Result,
};
use serde::{Deserialize, Serialize};

const MARKER_NAME: &str = ".relay-fake-profile.toml";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FakeSetupBehavior {
    Normal,
    AuthenticationRequired,
    InspectionFailed,
    Identity(String),
}

#[derive(Clone, Debug)]
pub struct FakeProvider {
    setup_behavior: FakeSetupBehavior,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self {
            setup_behavior: FakeSetupBehavior::Normal,
        }
    }
}

impl FakeProvider {
    #[must_use]
    pub const fn with_setup_behavior(setup_behavior: FakeSetupBehavior) -> Self {
        Self { setup_behavior }
    }

    pub fn overwrite_identity(config_dir: &Path, stable_id: &str) -> Result<()> {
        let mut marker = read_marker(config_dir)?;
        marker.stable_id = stable_id.to_owned();
        write_marker(config_dir, &marker)
    }

    pub fn overwrite_authentication(
        config_dir: &Path,
        authentication: AuthenticationState,
    ) -> Result<()> {
        let mut marker = read_marker(config_dir)?;
        marker.authentication = authentication;
        write_marker(config_dir, &marker)
    }
}

impl Provider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Fake
    }

    fn setup_profile(&self, request: &ProfileSetupRequest) -> Result<ProviderObservation> {
        if request.mode == ProfileSetupMode::AdoptExisting {
            return self.inspect_profile(&request.config_dir);
        }
        let (authentication, stable_id) = match &self.setup_behavior {
            FakeSetupBehavior::Normal => (
                AuthenticationState::Authenticated,
                format!("fake:{}", request.name),
            ),
            FakeSetupBehavior::AuthenticationRequired => (
                AuthenticationState::Required,
                format!("fake:{}", request.name),
            ),
            FakeSetupBehavior::InspectionFailed => (
                AuthenticationState::InspectionFailed,
                format!("fake:{}", request.name),
            ),
            FakeSetupBehavior::Identity(stable_id) => {
                (AuthenticationState::Authenticated, stable_id.clone())
            }
        };
        let marker = FakeMarker {
            version: 1,
            authentication,
            stable_id,
        };
        write_marker(&request.config_dir, &marker)?;
        observation(&marker)
    }

    fn inspect_profile(&self, config_dir: &Path) -> Result<ProviderObservation> {
        observation(&read_marker(config_dir)?)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FakeMarker {
    version: u32,
    authentication: AuthenticationState,
    stable_id: String,
}

fn observation(marker: &FakeMarker) -> Result<ProviderObservation> {
    if marker.version != 1 {
        return Err(Error::ProviderStateCorrupted);
    }
    let state = match marker.authentication {
        AuthenticationState::Authenticated => Availability::Available,
        AuthenticationState::Required => Availability::AuthRequired,
        AuthenticationState::InspectionFailed => Availability::Unknown,
    };
    Ok(ProviderObservation {
        authentication: marker.authentication,
        identity: (marker.authentication == AuthenticationState::Authenticated).then(|| {
            IdentityMetadata {
                stable_id: marker.stable_id.clone(),
                display_label: None,
            }
        }),
        availability: AvailabilityObservation {
            state,
            source: "fake_provider".to_owned(),
            observed_unix_ms: 0,
            reset_unix_ms: None,
        },
    })
}

fn read_marker(config_dir: &Path) -> Result<FakeMarker> {
    let path = config_dir.join(MARKER_NAME);
    let contents = std::fs::read_to_string(&path).map_err(|_| Error::ProviderUnavailable)?;
    toml::from_str(&contents).map_err(|_| Error::ProviderStateCorrupted)
}

fn write_marker(config_dir: &Path, marker: &FakeMarker) -> Result<()> {
    let path = config_dir.join(MARKER_NAME);
    let contents = toml::to_string_pretty(marker).map_err(|_| Error::SerializationFailed)?;
    FsAtomicWriter.write_atomic(&path, contents.as_bytes())
}
