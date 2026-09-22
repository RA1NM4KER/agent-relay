//! One shared model of "is Agent Relay actually ready to save me when my current account runs
//! out" — the question `relay doctor`, `relay setup`'s completion screen, and `relay status`'s
//! "Automatic handoff" section all answer, so they must never diverge. Every check here reuses
//! existing typed domain results (`ProfileService::status`, `IntegrationStatus`,
//! `CapabilityReport`, Codex's `VersionStatus`) rather than re-parsing provider output.

use std::path::Path;

use relay_core::{AuthenticationState, Profile, ProfileService, ProviderKind};
use relay_provider_claude::{CapabilityStatus, assess_installed, integration_status};
use relay_provider_codex::{CodexInspector, VersionStatus, assess_version};
use serde::Serialize;

use crate::{preferences::Preferences, providers};

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Warning,
    Blocking,
}

impl Level {
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "\u{2713}",       // ✓
            Self::Warning => "\u{26a0}",  // ⚠
            Self::Blocking => "\u{2717}", // ✗
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub label: String,
    pub level: Level,
    /// An extra explanatory line, shown under the label for warnings/blocking problems.
    pub detail: Option<String>,
    /// A concrete next command, when one exists.
    pub remedy: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Readiness {
    pub checks: Vec<Check>,
}

impl Readiness {
    #[must_use]
    pub fn overall(&self) -> Level {
        self.checks
            .iter()
            .map(|check| check.level)
            .max()
            .unwrap_or(Level::Ok)
    }

    /// Whether Relay is genuinely ready for automatic handoff right now — `false` only for a
    /// real blocking problem, never for a harmless warning.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.overall() != Level::Blocking
    }

    fn push(
        &mut self,
        label: impl Into<String>,
        level: Level,
        detail: Option<String>,
        remedy: Option<String>,
    ) {
        self.checks.push(Check {
            label: label.into(),
            level,
            detail,
            remedy,
        });
    }
}

/// The profiles automatic handoff actually considers, in priority order: the configured primary
/// plus fallbacks, restricted to profiles that are actually registered. Mirrors
/// `target::priority_order`'s "primary, then fallbacks" rule but stops there (doctor is about the
/// configured failover path, not every registered profile).
fn configured_profiles<'a>(
    registered: &'a [Profile],
    preferences: &Preferences,
) -> Vec<&'a Profile> {
    let mut ordered: Vec<&Profile> = Vec::new();
    for name in preferences
        .primary_profile
        .iter()
        .chain(preferences.fallback_profiles.iter())
    {
        if let Some(profile) = registered.iter().find(|candidate| &candidate.name == name)
            && !ordered.iter().any(|seen| seen.name == profile.name)
        {
            ordered.push(profile);
        }
    }
    ordered
}

fn role_label(profile: &Profile) -> String {
    let provider = match profile.provider {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude | ProviderKind::Fake => "Claude",
    };
    format!("{} ({provider})", profile.name)
}

/// Assesses whether Relay is ready to save the user automatically: every configured profile is
/// authenticated, every configured Claude profile has the usage integration installed, the
/// installed provider CLIs are at least an unverified match for a version Relay has validated,
/// automatic handoff is actually enabled in preferences, and (informational only) git and Herdr.
pub fn assess(
    service: &ProfileService,
    registered: &[Profile],
    preferences: &Preferences,
    executables: &providers::ExecutableOverrides,
) -> Readiness {
    let mut readiness = Readiness::default();
    let profiles = configured_profiles(registered, preferences);

    if profiles.is_empty() {
        readiness.push(
            "Relay is configured",
            Level::Blocking,
            Some("no primary profile is configured yet".to_owned()),
            Some("relay setup".to_owned()),
        );
        return readiness;
    }

    for profile in &profiles {
        let label = role_label(profile);
        auth_check(&mut readiness, service, profile, &label, executables);
        if profile.provider == ProviderKind::Claude {
            integration_check(&mut readiness, profile, &label);
        }
    }

    version_checks(&mut readiness, &profiles, executables);

    if preferences.usage_integration_enabled == Some(true) {
        readiness.push("Automatic handoff enabled", Level::Ok, None, None);
    } else {
        readiness.push(
            "Automatic handoff enabled",
            Level::Blocking,
            Some("the usage integration is not enabled in preferences".to_owned()),
            Some("relay setup".to_owned()),
        );
    }

    git_check(&mut readiness);
    herdr_check(&mut readiness);

    readiness
}

fn auth_check(
    readiness: &mut Readiness,
    service: &ProfileService,
    profile: &Profile,
    label: &str,
    executables: &providers::ExecutableOverrides,
) {
    let Ok(backend) = providers::provider_backend(profile.provider, executables) else {
        readiness.push(
            format!("{label} authenticated"),
            Level::Blocking,
            Some("the provider CLI could not be discovered".to_owned()),
            None,
        );
        return;
    };
    match service.status(&profile.name, backend.as_ref()) {
        Ok(status) if status.authentication == AuthenticationState::Authenticated => {
            readiness.push(format!("{label} authenticated"), Level::Ok, None, None);
        }
        Ok(_) => readiness.push(
            format!("{label} authenticated"),
            Level::Blocking,
            Some(format!("{} is logged out", profile.name)),
            Some(format!("relay login {}", profile.name)),
        ),
        Err(error) => readiness.push(
            format!("{label} authenticated"),
            Level::Blocking,
            Some(error.to_string()),
            Some(format!("relay login {}", profile.name)),
        ),
    }
}

fn integration_check(readiness: &mut Readiness, profile: &Profile, label: &str) {
    let installed = integration_status(&profile.config_dir)
        .map(|status| status.installed)
        .unwrap_or(false);
    if installed {
        readiness.push(
            format!("{label} usage integration installed"),
            Level::Ok,
            None,
            None,
        );
    } else {
        readiness.push(
            format!("{label} usage integration installed"),
            Level::Blocking,
            Some(format!("usage integration missing for {}", profile.name)),
            Some(format!(
                "relay integration claude install --profile {}",
                profile.name
            )),
        );
    }
}

fn version_checks(
    readiness: &mut Readiness,
    profiles: &[&Profile],
    executables: &providers::ExecutableOverrides,
) {
    if let Some(profile) = profiles
        .iter()
        .find(|profile| profile.provider == ProviderKind::Claude)
    {
        claude_version_check(readiness, profile, executables.claude.as_deref());
    }
    if let Some(profile) = profiles
        .iter()
        .find(|profile| profile.provider == ProviderKind::Codex)
    {
        codex_version_check(readiness, profile, executables.codex.as_deref());
    }
}

fn claude_version_check(
    readiness: &mut Readiness,
    profile: &Profile,
    claude_executable: Option<&Path>,
) {
    match assess_installed(
        claude_executable,
        &profile.config_dir,
        profile.effective_claude_config_mode(),
    ) {
        Ok(report) => {
            let required = [
                relay_provider_claude::Capability::AgentsJsonShape,
                relay_provider_claude::Capability::TranscriptLayout,
                relay_provider_claude::Capability::AuthStatusSchema,
            ];
            let worst = required
                .iter()
                .map(|capability| report.status_of(*capability))
                .max_by_key(|status| match status {
                    CapabilityStatus::Verified => 0,
                    CapabilityStatus::Unverified => 1,
                    CapabilityStatus::Unsupported => 2,
                });
            match worst {
                Some(CapabilityStatus::Unsupported) | None => readiness.push(
                    format!("Claude Code {} supported", report.version),
                    Level::Blocking,
                    Some("this Claude Code version is unsupported".to_owned()),
                    None,
                ),
                Some(CapabilityStatus::Unverified) => readiness.push(
                    format!("Claude Code {} supported", report.version),
                    Level::Warning,
                    Some(
                        "newer than Relay has live-validated; automatic handoff remains \
                         fail-closed on ambiguous signals"
                            .to_owned(),
                    ),
                    None,
                ),
                Some(CapabilityStatus::Verified) => readiness.push(
                    format!("Claude Code {} supported", report.version),
                    Level::Ok,
                    None,
                    None,
                ),
            }
        }
        Err(error) => readiness.push(
            "Claude Code version supported",
            Level::Blocking,
            Some(error.to_string()),
            None,
        ),
    }
}

fn codex_version_check(
    readiness: &mut Readiness,
    profile: &Profile,
    codex_executable: Option<&Path>,
) {
    let _ = &profile.config_dir; // Codex's version check is executable-scoped, not profile-scoped.
    match CodexInspector::discover(codex_executable)
        .and_then(|inspector| inspector.inspect_version())
    {
        Ok(version) => match assess_version(&version) {
            VersionStatus::Verified => {
                readiness.push(
                    format!("Codex CLI {version} supported"),
                    Level::Ok,
                    None,
                    None,
                );
            }
            VersionStatus::Unverified => readiness.push(
                format!("Codex CLI {version} supported"),
                Level::Warning,
                Some(
                    "newer than Relay has live-validated; automatic handoff remains fail-closed \
                     on ambiguous signals"
                        .to_owned(),
                ),
                None,
            ),
        },
        Err(error) => readiness.push(
            "Codex CLI version supported",
            Level::Blocking,
            Some(error.to_string()),
            None,
        ),
    }
}

fn git_check(readiness: &mut Readiness) {
    let available = std::process::Command::new("git")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if available {
        readiness.push("Git available", Level::Ok, None, None);
    } else {
        readiness.push(
            "Git available",
            Level::Warning,
            Some("state-continuation handoffs capture less context without git".to_owned()),
            None,
        );
    }
}

/// Only shown when actually running inside a Herdr pane — Herdr is optional, so its absence is
/// never reported as a problem at all (matches the same `HERDR_ENV` signal `relay status` uses).
fn herdr_check(readiness: &mut Readiness) {
    let in_herdr = std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
    if in_herdr {
        readiness.push("Herdr connected", Level::Ok, None, None);
    }
}
