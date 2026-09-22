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
    Codex,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fake => formatter.write_str("fake"),
            Self::Claude => formatter.write_str("claude"),
            Self::Codex => formatter.write_str("codex"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOrigin {
    Created,
    Adopted,
}

/// Only meaningful for a Claude profile (`ProviderKind::Claude`); irrelevant for Codex. Live
/// testing found that Claude Code's own default account is NOT semantically equivalent to
/// explicitly pointing `CLAUDE_CONFIG_DIR` at the same directory: `claude auth status --json`
/// with `CLAUDE_CONFIG_DIR` unset reports the logged-in native account with
/// `configDirectory: ~/.claude`, but the identical command with `CLAUDE_CONFIG_DIR=~/.claude` set
/// explicitly reports logged OUT. The two must never be treated as interchangeable, and never
/// special-cased as `config_dir == "~/.claude"` — this field is the one place that distinction is
/// recorded, and every Claude command Relay builds must consult it (never re-derive it from the
/// path) before deciding whether to set `CLAUDE_CONFIG_DIR`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeConfigMode {
    /// Claude Code's own default account. `config_dir` is still `~/.claude` (used for every file
    /// read: session registry, transcripts, hook installation) but `CLAUDE_CONFIG_DIR` is never
    /// set when Claude is invoked.
    NativeDefault,
    /// The original isolated-profile behavior: `CLAUDE_CONFIG_DIR` is always set to `config_dir`.
    Explicit,
}

impl Default for ClaudeConfigMode {
    /// Every existing (pre-this-field) serialized profile, and every Codex profile.
    fn default() -> Self {
        Self::Explicit
    }
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
    /// `None` on every profile created before this field existed, and always for Codex/Fake:
    /// [`Profile::effective_claude_config_mode`] is the one place that ambiguity is resolved
    /// (to [`ClaudeConfigMode::Explicit`], the only mode that ever existed before).
    #[serde(default)]
    pub claude_config_mode: Option<ClaudeConfigMode>,
}

impl Profile {
    /// The mode to use when building any Claude command for this profile. Centralizes the
    /// unset-vs-explicit distinction so nothing downstream re-derives it from `config_dir`.
    #[must_use]
    pub fn effective_claude_config_mode(&self) -> ClaudeConfigMode {
        self.claude_config_mode.unwrap_or_default()
    }
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

#[cfg(test)]
mod tests {
    use super::{ClaudeConfigMode, Profile};

    /// M4: every profile record written before `claude_config_mode` existed has no such field at
    /// all in its stored JSON/TOML. It must still deserialize (`#[serde(default)]`), and
    /// `effective_claude_config_mode()` must resolve the missing value to `Explicit` — the only
    /// mode that ever existed before this field — never `NativeDefault`, which would silently
    /// reinterpret every pre-existing isolated profile as Claude's own default account.
    #[test]
    fn a_profile_record_with_no_claude_config_mode_field_defaults_to_explicit() {
        let json = r#"{
            "name": "megan",
            "provider": "claude",
            "config_dir": "/tmp/relay/megan/claude",
            "enabled": true,
            "origin": "adopted",
            "expected_identity": {"stable_id": "id-1", "display_label": null},
            "last_availability": {
                "state": "AVAILABLE",
                "source": "claude_auth_status",
                "observed_unix_ms": 0,
                "reset_unix_ms": null
            }
        }"#;
        let profile: Profile = serde_json::from_str(json).expect("old-shape profile must parse");
        assert_eq!(profile.claude_config_mode, None);
        assert_eq!(
            profile.effective_claude_config_mode(),
            ClaudeConfigMode::Explicit
        );
    }

    /// A profile explicitly recorded as `native_default` round-trips exactly, and is never
    /// resolved to anything else by `effective_claude_config_mode()`.
    #[test]
    fn a_profile_explicitly_recorded_as_native_default_round_trips() {
        let json = r#"{
            "name": "erika-default",
            "provider": "claude",
            "config_dir": "/Users/erika/.claude",
            "enabled": true,
            "origin": "adopted",
            "expected_identity": {"stable_id": "id-2", "display_label": "erika@example.com"},
            "last_availability": {
                "state": "AVAILABLE",
                "source": "claude_auth_status",
                "observed_unix_ms": 0,
                "reset_unix_ms": null
            },
            "claude_config_mode": "native_default"
        }"#;
        let profile: Profile = serde_json::from_str(json).expect("native-default profile parses");
        assert_eq!(
            profile.claude_config_mode,
            Some(ClaudeConfigMode::NativeDefault)
        );
        assert_eq!(
            profile.effective_claude_config_mode(),
            ClaudeConfigMode::NativeDefault
        );
        let round_tripped = serde_json::to_string(&profile).expect("serialize");
        assert!(round_tripped.contains("\"claude_config_mode\":\"native_default\""));
    }
}
