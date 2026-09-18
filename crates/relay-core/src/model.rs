use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid && !value.starts_with('-') && !value.ends_with('-') {
            Ok(Self(value))
        } else {
            Err(Error::InvalidProfileName(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ProfileName {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Fake,
    Claude,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fake => formatter.write_str("fake"),
            Self::Claude => formatter.write_str("claude"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOrigin {
    Created,
    Adopted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMetadata {
    pub stable_id: String,
    pub display_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Availability {
    Available,
    Active,
    NearingLimit,
    Limited,
    AuthRequired,
    Unavailable,
    Disabled,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityObservation {
    pub state: Availability,
    pub source: String,
    pub observed_unix_ms: u64,
    pub reset_unix_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationState {
    Authenticated,
    Required,
    InspectionFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: ProfileName,
    pub provider: ProviderKind,
    pub config_dir: PathBuf,
    pub enabled: bool,
    pub origin: ProfileOrigin,
    pub expected_identity: IdentityMetadata,
    pub last_availability: AvailabilityObservation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileStatus {
    pub profile: Profile,
    pub authentication: AuthenticationState,
    pub observed_identity: Option<IdentityMetadata>,
    pub availability: AvailabilityObservation,
    pub identity_matches: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub profile: ProfileName,
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}
