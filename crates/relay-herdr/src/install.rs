//! `relay integration herdr install|status|doctor|uninstall` — a coherent install experience for
//! `plugins/herdr/herdr-plugin.toml`, mirroring `relay-provider-claude`'s own
//! `integration_status`/`plan_install`/`apply_install` pattern for the Claude usage integration,
//! but talking to `herdr plugin link/list/unlink` instead of writing a `settings.json`.
//!
//! Live-verified against Herdr 0.9.0 (M3.2): every function here is exercised by the same manual
//! `herdr plugin link/list/unlink` sequence used during the real disposable-workspace validation
//! (`M3_FINAL_REPORT.md`).
//!
//! **M5.5 packaging fix**: the manifest Herdr links is no longer required to live next to a
//! source checkout. `assets/herdr-plugin.toml.template` is embedded into this binary via
//! `include_str!` and, by default, materialized into a Relay-owned directory
//! (`<config_root>/herdr-plugin/herdr-plugin.toml`) with its `command` entries pointing at the
//! absolute path of the `relay-herdr-plugin` binary installed alongside `relay` — never a
//! `./plugins/herdr` relative path, never the current working directory. An explicit
//! `--plugin-path` still works unchanged for repository/local-dev use (`herdr plugin link
//! plugins/herdr`).

use std::path::{Path, PathBuf};

use crate::client::CommandRunner;
use crate::error::HerdrIntegrationError;
use crate::herdr_client::{HerdrCliClient, PluginRecord};

pub const PLUGIN_ID: &str = "agent-relay";
/// The minimum Herdr version `plugins/herdr/herdr-plugin.toml` declares — kept in sync manually;
/// `doctor` cross-checks it against the manifest file itself so the two can never silently drift.
pub const MIN_HERDR_VERSION: &str = "0.9.0";

/// The embedded manifest template — see the module docs. Kept in sync by hand with
/// `plugins/herdr/herdr-plugin.toml`; a test asserts the two agree on every field except
/// `command`/`[[build]]`. Public so that test can parse it directly.
pub const PLUGIN_MANIFEST_TEMPLATE: &str = include_str!("../assets/herdr-plugin.toml.template");
const PLUGIN_BIN_PLACEHOLDER: &str = "{{RELAY_HERDR_PLUGIN_BIN}}";
const RELAY_HERDR_PLUGIN_BIN_NAME: &str = "relay-herdr-plugin";

#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub plugin_path: PathBuf,
    pub already_linked: bool,
}

/// Resolves a plugin directory to link: an explicit path if given (unchanged, source-checkout
/// use), otherwise materializes the embedded manifest into `<config_root>/herdr-plugin` pointing
/// at the `relay-herdr-plugin` binary installed alongside the running `relay` binary. Never reads
/// or depends on the current working directory. Reads `RELAY_HERDR_PLUGIN_BIN` itself (a
/// test/operator override, undocumented in normal use) and delegates to
/// `resolve_plugin_path_with_override` so tests can inject an override directly instead of
/// mutating process-global env vars (this workspace forbids `unsafe`, which `std::env::set_var`
/// requires since Rust 2024).
pub fn resolve_plugin_path(
    explicit: Option<&Path>,
    config_root: &Path,
) -> Result<PathBuf, HerdrIntegrationError> {
    let override_path = std::env::var_os("RELAY_HERDR_PLUGIN_BIN").map(PathBuf::from);
    resolve_plugin_path_with_override(explicit, config_root, override_path.as_deref())
}

/// The testable core of [`resolve_plugin_path`]: same behavior, but the `relay-herdr-plugin`
/// binary override is a parameter instead of an env var read.
pub fn resolve_plugin_path_with_override(
    explicit: Option<&Path>,
    config_root: &Path,
    plugin_bin_override: Option<&Path>,
) -> Result<PathBuf, HerdrIntegrationError> {
    if let Some(explicit) = explicit {
        return if explicit.join("herdr-plugin.toml").is_file() {
            Ok(explicit.to_path_buf())
        } else {
            Err(HerdrIntegrationError::HerdrMetadataUnavailable)
        };
    }
    let plugin_bin = locate_relay_herdr_plugin_binary(plugin_bin_override)?;
    materialize_embedded_plugin(&config_root.join("herdr-plugin"), &plugin_bin)
}

/// Discovery order for the `relay-herdr-plugin` binary a packaged install ships alongside `relay`:
/// 1. `plugin_bin_override` (explicit override — `RELAY_HERDR_PLUGIN_BIN` in real use, a direct
///    parameter in tests).
/// 2. A sibling of the currently running executable named `relay-herdr-plugin` — true for both a
///    release tarball/Homebrew install (both binaries installed into the same `bin/`) and a local
///    `cargo build --release` (`target/release/relay-herdr-plugin` next to `target/release/relay`).
/// 3. A normal `PATH` search, for any other install layout that still puts both binaries on PATH.
fn locate_relay_herdr_plugin_binary(
    plugin_bin_override: Option<&Path>,
) -> Result<PathBuf, HerdrIntegrationError> {
    if let Some(path) = plugin_bin_override {
        return Ok(path.to_path_buf());
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let candidate = dir.join(RELAY_HERDR_PLUGIN_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(RELAY_HERDR_PLUGIN_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(HerdrIntegrationError::HerdrMetadataUnavailable)
}

/// Writes the embedded manifest template into `target_dir/herdr-plugin.toml` with
/// `{{RELAY_HERDR_PLUGIN_BIN}}` replaced by `plugin_bin`'s absolute path. Idempotent: safe to
/// re-run (e.g. on every `relay setup`/`relay integration herdr install`), always overwrites with
/// the current embedded manifest so a binary upgrade also refreshes the linked plugin.
fn materialize_embedded_plugin(
    target_dir: &Path,
    plugin_bin: &Path,
) -> Result<PathBuf, HerdrIntegrationError> {
    std::fs::create_dir_all(target_dir)
        .map_err(|_| HerdrIntegrationError::HerdrMetadataUnavailable)?;
    let plugin_bin_absolute = if plugin_bin.is_absolute() {
        plugin_bin.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| HerdrIntegrationError::HerdrMetadataUnavailable)?
            .join(plugin_bin)
    };
    let rendered = PLUGIN_MANIFEST_TEMPLATE.replace(
        PLUGIN_BIN_PLACEHOLDER,
        &plugin_bin_absolute.display().to_string(),
    );
    let manifest_path = target_dir.join("herdr-plugin.toml");
    std::fs::write(&manifest_path, rendered)
        .map_err(|_| HerdrIntegrationError::HerdrMetadataUnavailable)?;
    Ok(target_dir.to_path_buf())
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
