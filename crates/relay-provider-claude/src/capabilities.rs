//! M2C.1: Claude Code version/capability gating. Relay does not hardcode "the" supported version;
//! it holds a small table of what each feature needs and reports, per capability, whether the
//! installed Claude Code is verified, unverified (newer than anything validated), or unsupported.
//! A required capability that cannot be verified fails closed.

use std::{
    io::Read,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;

use relay_core::{Error, Result};

use crate::{AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeInspector, session_registry};

/// Claude Code versions on which every capability below was exercised (live or against real
/// output). Extend this after validating a new release; never assume a release is on it.
pub const VERIFIED_VERSIONS: &[&str] = &["2.1.276", "2.1.277"];
/// The supported release line. A different major/minor is `Unsupported`.
const SUPPORTED_MAJOR: u32 = 2;
const SUPPORTED_MINOR: u32 = 1;
/// Lowest patch any capability is claimed for (the first version Relay validated).
const MIN_PATCH: u32 = 276;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    AuthStatusSchema,
    AgentsJsonShape,
    StopFailureHook,
    StatuslineRateLimits,
    StreamJsonRateLimitEvent,
    TranscriptLayout,
}

impl Capability {
    pub const ALL: [Self; 6] = [
        Self::AuthStatusSchema,
        Self::AgentsJsonShape,
        Self::StopFailureHook,
        Self::StatuslineRateLimits,
        Self::StreamJsonRateLimitEvent,
        Self::TranscriptLayout,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// Validated on this exact version.
    Verified,
    /// Same release line and not older than the first validated patch, but this exact version has
    /// not been validated. Allowed only where a wrong assumption fails closed on its own.
    Unverified,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityEntry {
    pub capability: Capability,
    pub status: CapabilityStatus,
    pub basis: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityReport {
    pub version: String,
    pub entries: Vec<CapabilityEntry>,
}

impl CapabilityReport {
    #[must_use]
    pub fn status_of(&self, capability: Capability) -> CapabilityStatus {
        self.entries
            .iter()
            .find(|entry| entry.capability == capability)
            .map_or(CapabilityStatus::Unsupported, |entry| entry.status)
    }

    /// The capabilities that must be at least `Unverified` for Relay's usage integration, and
    /// `Verified` when `allow_unverified` is false.
    pub fn usage_integration_ready(
        &self,
        allow_unverified: bool,
    ) -> std::result::Result<(), String> {
        for capability in [
            Capability::StopFailureHook,
            Capability::StatuslineRateLimits,
            Capability::StreamJsonRateLimitEvent,
        ] {
            match self.status_of(capability) {
                CapabilityStatus::Verified => {}
                CapabilityStatus::Unverified if allow_unverified => {}
                CapabilityStatus::Unverified => {
                    return Err(format!(
                        "{capability:?} is not verified on Claude Code {}; re-run with \
                         --allow-unverified-version to accept that risk",
                        self.version
                    ));
                }
                CapabilityStatus::Unsupported => {
                    return Err(format!(
                        "{capability:?} is unsupported on Claude Code {}",
                        self.version
                    ));
                }
            }
        }
        Ok(())
    }
}

fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Runtime observations that can confirm a capability without spending API usage.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeChecks {
    /// `claude --help` advertises `--output-format` with `stream-json` (and `--verbose`).
    pub help_advertises_stream_json: Option<bool>,
    /// `claude agents --json` parsed as the known record shape.
    pub agents_json_parses: Option<bool>,
}

#[must_use]
pub fn assess(version: &str, runtime: &RuntimeChecks) -> CapabilityReport {
    let base = match parse_version(version) {
        Some((major, minor, patch))
            if major == SUPPORTED_MAJOR && minor == SUPPORTED_MINOR && patch >= MIN_PATCH =>
        {
            if VERIFIED_VERSIONS.contains(&version.trim()) {
                (
                    CapabilityStatus::Verified,
                    "validated on this exact version",
                )
            } else {
                (
                    CapabilityStatus::Unverified,
                    "same release line as validated versions, but this version was not validated",
                )
            }
        }
        Some(_) => (
            CapabilityStatus::Unsupported,
            "outside the validated 2.1.x release line or older than the first validated patch",
        ),
        None => (CapabilityStatus::Unsupported, "version could not be parsed"),
    };

    let entries = Capability::ALL
        .into_iter()
        .map(|capability| {
            let (mut status, mut basis) = (base.0, base.1.to_owned());
            // Where the capability can be observed at runtime, that observation is authoritative:
            // a failed check downgrades to Unsupported; a passed check upgrades an unvalidated
            // version to Verified for that capability only.
            let observed = match capability {
                Capability::StreamJsonRateLimitEvent => runtime.help_advertises_stream_json,
                Capability::AgentsJsonShape => runtime.agents_json_parses,
                _ => None,
            };
            match observed {
                Some(false) => {
                    status = CapabilityStatus::Unsupported;
                    basis = "runtime check failed".to_owned();
                }
                Some(true) if status == CapabilityStatus::Unverified => {
                    status = CapabilityStatus::Verified;
                    basis = "confirmed by a runtime check".to_owned();
                }
                _ => {}
            }
            CapabilityEntry {
                capability,
                status,
                basis,
            }
        })
        .collect();
    CapabilityReport {
        version: version.trim().to_owned(),
        entries,
    }
}

/// Assesses the installed Claude Code: version plus the runtime checks that need no API request
/// (`--version`, `--help`, `agents --json`).
pub fn assess_installed(
    claude_executable: Option<&Path>,
    config_dir: &Path,
) -> Result<CapabilityReport> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let version = inspector.inspect_version()?;
    let agents_json_parses =
        match session_registry::query_active_sessions(config_dir, claude_executable) {
            Ok(_) => Some(true),
            Err(Error::MalformedProviderOutput) => Some(false),
            Err(_) => None,
        };
    Ok(assess(
        &version,
        &RuntimeChecks {
            help_advertises_stream_json: probe_help_for_stream_json(inspector.executable()),
            agents_json_parses,
        },
    ))
}

/// Runs `claude --help` (no API call) and reports whether stream-json output is advertised.
#[must_use]
pub fn probe_help_for_stream_json(executable: &Path) -> Option<bool> {
    let mut command = std::process::Command::new(executable);
    command
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ignored = stdout.by_ref().take(512 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if started.elapsed() > Duration::from_secs(15) {
            let _ignored = child.kill();
            let _ignored = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let help = String::from_utf8_lossy(&reader.join().ok()?).into_owned();
    Some(help.contains("stream-json") && help.contains("--verbose"))
}

#[cfg(test)]
mod tests {
    use super::{Capability, CapabilityStatus, RuntimeChecks, assess};

    #[test]
    fn validated_versions_are_verified_including_2_1_277() {
        for version in ["2.1.276", "2.1.277"] {
            let report = assess(version, &RuntimeChecks::default());
            assert!(
                report
                    .entries
                    .iter()
                    .all(|e| e.status == CapabilityStatus::Verified)
            );
            assert!(report.usage_integration_ready(false).is_ok());
        }
    }

    #[test]
    fn a_newer_patch_is_unverified_and_fails_closed_without_an_explicit_override() {
        let report = assess("2.1.290", &RuntimeChecks::default());
        assert_eq!(
            report.status_of(Capability::StopFailureHook),
            CapabilityStatus::Unverified
        );
        assert!(report.usage_integration_ready(false).is_err());
        assert!(report.usage_integration_ready(true).is_ok());
    }

    #[test]
    fn other_release_lines_and_old_patches_are_unsupported() {
        for version in ["2.2.0", "3.0.1", "2.1.100", "1.0.0", "garbage", "2.1.277.1"] {
            let report = assess(version, &RuntimeChecks::default());
            assert!(report.usage_integration_ready(true).is_err(), "{version}");
        }
    }

    #[test]
    fn a_failed_runtime_check_downgrades_and_a_passed_one_upgrades() {
        let bad = assess(
            "2.1.277",
            &RuntimeChecks {
                help_advertises_stream_json: Some(false),
                agents_json_parses: Some(false),
            },
        );
        assert_eq!(
            bad.status_of(Capability::StreamJsonRateLimitEvent),
            CapabilityStatus::Unsupported
        );
        assert!(bad.usage_integration_ready(true).is_err());
        let good = assess(
            "2.1.290",
            &RuntimeChecks {
                help_advertises_stream_json: Some(true),
                agents_json_parses: Some(true),
            },
        );
        assert_eq!(
            good.status_of(Capability::StreamJsonRateLimitEvent),
            CapabilityStatus::Verified
        );
        assert_eq!(
            good.status_of(Capability::StopFailureHook),
            CapabilityStatus::Unverified
        );
    }
}
