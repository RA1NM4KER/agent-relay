//! `relay integration herdr install|status|doctor|uninstall` — a coherent install experience for
//! `plugins/herdr/herdr-plugin.toml`, mirroring `relay-provider-claude`'s own
//! `integration_status`/`plan_install`/`apply_install` pattern for the Claude usage integration,
//! but talking to `herdr plugin link/list/unlink` instead of writing a `settings.json`.
//!
//! Live-verified against Herdr 0.9.0 (M3.2): every function here is exercised by the same manual
//! `herdr plugin link/list/unlink` sequence used during the real disposable-workspace validation
//! (`M3_FINAL_REPORT.md`).
//!
//! **Known packaging gap, not solved here**: `install` requires a local path to
//! `plugins/herdr` (defaulting to `./plugins/herdr` relative to the current working directory,
//! i.e. running from the `agent-relay` repo root) rather than locating it automatically from an
//! arbitrary installed `relay` binary location. A real system-wide install
//! (`cargo install --path crates/relay-cli`) loses the repository layout, so there is currently no
//! way to find the manifest without either running from the repo checkout or passing
//! `--plugin-path` explicitly. Solving this is the same deferred "marketplace/`plugin install`
//! packaging" question `docs/herdr-integration.md` already flags for `relay-herdr-plugin` itself.

use std::path::{Path, PathBuf};

use crate::client::CommandRunner;
use crate::error::HerdrIntegrationError;
use crate::herdr_client::{HerdrCliClient, PluginRecord};

pub const PLUGIN_ID: &str = "agent-relay";
/// The minimum Herdr version `plugins/herdr/herdr-plugin.toml` declares — kept in sync manually;
/// `doctor` cross-checks it against the manifest file itself so the two can never silently drift.
pub const MIN_HERDR_VERSION: &str = "0.9.0";

#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub plugin_path: PathBuf,
    pub already_linked: bool,
}

/// Resolves a plugin directory to link: an explicit path if given, otherwise `./plugins/herdr`
/// relative to the current working directory (the documented "run from the repo root" case). Only
/// checks the manifest file exists; never inspects its contents (Herdr itself validates the
/// schema on link).
pub fn resolve_plugin_path(explicit: Option<&Path>) -> Result<PathBuf, HerdrIntegrationError> {
    let candidate = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("plugins/herdr"));
    if candidate.join("herdr-plugin.toml").is_file() {
        Ok(candidate)
    } else {
        Err(HerdrIntegrationError::HerdrMetadataUnavailable)
    }
}

pub fn plan_install<R: CommandRunner>(
    herdr: &HerdrCliClient<R>,
    plugin_path: &Path,
) -> Result<InstallPlan, HerdrIntegrationError> {
    let already_linked = herdr
        .plugin_list()?
        .iter()
        .any(|plugin| plugin.plugin_id == PLUGIN_ID);
    Ok(InstallPlan {
        plugin_path: plugin_path.to_path_buf(),
        already_linked,
    })
}

/// `herdr plugin link` is itself idempotent/safe to re-run (it re-reads the manifest and updates
/// the existing registration), so `apply_install` never needs a separate "already linked" branch
/// beyond what `plan_install` reports for display.
pub fn apply_install<R: CommandRunner>(
    herdr: &HerdrCliClient<R>,
    plugin_path: &Path,
) -> Result<PluginRecord, HerdrIntegrationError> {
    herdr.plugin_link(plugin_path)
}

#[derive(Debug)]
pub struct StatusReport {
    pub herdr_client_version: String,
    pub herdr_server_running: bool,
    pub herdr_server_version: String,
    pub herdr_compatible: bool,
    pub plugin: Option<PluginRecord>,
}

pub fn status<R: CommandRunner>(
    herdr: &HerdrCliClient<R>,
) -> Result<StatusReport, HerdrIntegrationError> {
    let herdr_status = herdr.status()?;
    let plugin = herdr
        .plugin_list()?
        .into_iter()
        .find(|plugin| plugin.plugin_id == PLUGIN_ID);
    Ok(StatusReport {
        herdr_client_version: herdr_status.client.version,
        herdr_server_running: herdr_status.server.running,
        herdr_server_version: herdr_status.server.version,
        herdr_compatible: herdr_status.server.compatible,
        plugin,
    })
}

#[derive(Debug)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug)]
pub struct DoctorReport {
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

/// Validates the whole Herdr → Relay chain an operator would otherwise have to check by hand:
/// Herdr itself reachable and version-compatible, the plugin actually registered and enabled, its
/// manifest's `min_herdr_version` not silently drifted from what this binary expects, and the
/// `relay` executable this plugin would fall back to actually present.
pub fn doctor<R: CommandRunner>(
    herdr: &HerdrCliClient<R>,
) -> Result<DoctorReport, HerdrIntegrationError> {
    let mut checks = Vec::new();

    let herdr_status = herdr.status();
    checks.push(match &herdr_status {
        Ok(status) if status.server.running && status.server.compatible => DoctorCheck {
            name: "herdr_server",
            passed: true,
            message: format!(
                "Herdr server running, version {} (compatible)",
                status.server.version
            ),
        },
        Ok(status) => DoctorCheck {
            name: "herdr_server",
            passed: false,
            message: format!(
                "Herdr server running={}, compatible={}",
                status.server.running, status.server.compatible
            ),
        },
        Err(error) => DoctorCheck {
            name: "herdr_server",
            passed: false,
            message: format!("could not reach Herdr: {error}"),
        },
    });

    let plugin = herdr.plugin_list().ok().and_then(|plugins| {
        plugins
            .into_iter()
            .find(|plugin| plugin.plugin_id == PLUGIN_ID)
    });
    checks.push(match &plugin {
        Some(plugin) if plugin.enabled => DoctorCheck {
            name: "plugin_registered",
            passed: true,
            message: format!(
                "registered and enabled (manifest {})",
                plugin.manifest_path.display()
            ),
        },
        Some(_) => DoctorCheck {
            name: "plugin_registered",
            passed: false,
            message: "registered but disabled (`herdr plugin enable agent-relay`)".to_owned(),
        },
        None => DoctorCheck {
            name: "plugin_registered",
            passed: false,
            message: "not registered (`relay integration herdr install`)".to_owned(),
        },
    });

    checks.push(match &plugin {
        Some(plugin) if plugin.min_herdr_version == MIN_HERDR_VERSION => DoctorCheck {
            name: "manifest_version_pin",
            passed: true,
            message: format!("manifest min_herdr_version matches ({MIN_HERDR_VERSION})"),
        },
        Some(plugin) => DoctorCheck {
            name: "manifest_version_pin",
            passed: false,
            message: format!(
                "linked manifest declares min_herdr_version={}, this binary expects {MIN_HERDR_VERSION} \
                 (relink after upgrading)",
                plugin.min_herdr_version
            ),
        },
        None => DoctorCheck {
            name: "manifest_version_pin",
            passed: false,
            message: "no linked plugin to check".to_owned(),
        },
    });

    let healthy = checks.iter().all(|check| check.passed);
    Ok(DoctorReport { healthy, checks })
}

/// Removes only Relay's own plugin registration (`herdr plugin unlink`); never touches any other
/// plugin, Herdr's config, or any Herdr-managed workspace/pane state. Idempotent: unlinking an
/// already-absent plugin id is treated as success (nothing to do), matching
/// `relay integration claude uninstall`'s own "restores byte for byte or is a no-op" idempotence.
pub fn apply_uninstall<R: CommandRunner>(
    herdr: &HerdrCliClient<R>,
) -> Result<bool, HerdrIntegrationError> {
    let was_registered = herdr
        .plugin_list()?
        .iter()
        .any(|plugin| plugin.plugin_id == PLUGIN_ID);
    if !was_registered {
        return Ok(false);
    }
    herdr.plugin_unlink(PLUGIN_ID)?;
    Ok(true)
}
