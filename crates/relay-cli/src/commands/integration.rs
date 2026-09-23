//! `relay integration <subcommand>`: Claude usage-hook install/status/uninstall and the
//! optional Herdr plugin link.

use std::path::PathBuf;

use relay_core::{ClaudeConfigMode, Error, ProfileService, ProviderKind, RelayPaths};
use relay_herdr::herdr_client::HerdrCliClient;
use relay_herdr::install as herdr_install;
use relay_provider_claude::{
    apply_install, apply_uninstall, assess_installed, integration_status, plan_install,
    plan_uninstall,
};
use serde_json::{Value, json};

use crate::{
    cli::{
        ClaudeIntegrationCommand, HerdrIntegrationArgs, HerdrIntegrationCommand, IntegrationArgs,
        IntegrationCommand, IntegrationTarget,
    },
    output::{CommandOutput, success},
    progress,
    util::current_unix_ms,
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    integration: &IntegrationArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    match &integration.command {
        IntegrationCommand::Codex(codex) => {
            use crate::cli::CodexIntegrationCommand;
            let (CodexIntegrationCommand::Install { profile: name }
            | CodexIntegrationCommand::Status { profile: name }
            | CodexIntegrationCommand::Uninstall { profile: name }) = &codex.command;
            let profile = service
                .list()?
                .into_iter()
                .find(|profile| &profile.name == name)
                .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
            if profile.provider != ProviderKind::Codex {
                return Err(Error::ProviderMismatch {
                    expected: "codex".to_owned(),
                    observed: profile.provider.to_string(),
                });
            }
            let action = match &codex.command {
                CodexIntegrationCommand::Install { .. } => {
                    crate::codex_integration::install(&profile.config_dir)?;
                    "integration.codex.install"
                }
                CodexIntegrationCommand::Uninstall { .. } => {
                    crate::codex_integration::uninstall(&profile.config_dir)?;
                    "integration.codex.uninstall"
                }
                CodexIntegrationCommand::Status { .. } => "integration.codex.status",
            };
            let installed = crate::codex_integration::installed(&profile.config_dir);
            success(
                action,
                format!(
                    "Codex Relay skill for {name}: {}.{}",
                    if installed {
                        "installed"
                    } else {
                        "not installed"
                    },
                    if installed {
                        " In a fresh Relay-managed Codex session, use $relay doctor (not /relay)."
                    } else {
                        ""
                    }
                ),
                json!({ "profile": name, "installed": installed, "skill_path": crate::codex_integration::skill_path(&profile.config_dir), "invocation": "$relay" }),
            )
        }
        IntegrationCommand::Herdr(herdr) => run_herdr_integration(herdr, paths, json_mode),
        IntegrationCommand::Claude(claude) => {
            let resolve =
                |target: &IntegrationTarget| -> Result<(PathBuf, ClaudeConfigMode), Error> {
                    if target.native_default {
                        let dir = relay_provider_claude::native_default_dir()
                            .ok_or(Error::MissingEnvironment("HOME"))?;
                        return Ok((dir, ClaudeConfigMode::NativeDefault));
                    }
                    match (&target.profile, &target.config_dir) {
                        (Some(name), None) => {
                            let profile = service
                                .list()?
                                .into_iter()
                                .find(|candidate| &candidate.name == name)
                                .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
                            if profile.provider != ProviderKind::Claude {
                                return Err(Error::ProviderMismatch {
                                    expected: "claude".to_owned(),
                                    observed: format!("{:?}", profile.provider),
                                });
                            }
                            let mode = profile.effective_claude_config_mode();
                            Ok((profile.config_dir, mode))
                        }
                        (None, Some(path)) => Ok((path.clone(), ClaudeConfigMode::Explicit)),
                        _ => Err(Error::ProviderUnsupported),
                    }
                };
            match &claude.command {
                ClaudeIntegrationCommand::Install {
                    target,
                    dry_run,
                    allow_unverified_version,
                    claude_executable,
                } => {
                    let (config_dir, mode) = resolve(target)?;
                    let install_progress =
                        progress::Progress::start("Checking Claude Code version…", json_mode);
                    let capabilities =
                        assess_installed(claude_executable.as_deref(), &config_dir, mode)?;
                    install_progress.finish();
                    capabilities
                        .usage_integration_ready(*allow_unverified_version)
                        .map_err(Error::IntegrationRefused)?;
                    let relay_executable = std::env::current_exe().map_err(|source| Error::Io {
                        path: PathBuf::from("relay"),
                        source,
                    })?;
                    let plan = plan_install(&config_dir, &relay_executable)?;
                    if !dry_run {
                        apply_install(&plan, current_unix_ms())?;
                    }
                    let human = format!(
                        "{} for {}:\n{}{}",
                        if *dry_run {
                            "Dry run (nothing written): would install the Relay usage integration"
                        } else if plan.already_installed {
                            "Relay usage integration was already installed"
                        } else {
                            "Installed the Relay usage integration"
                        },
                        config_dir.display(),
                        plan.changes
                            .iter()
                            .map(|change| format!("  - {change}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        if *dry_run || plan.already_installed {
                            String::new()
                        } else {
                            "\nThe original settings were backed up under relay-integration/. \
                             Undo with `relay integration claude uninstall`."
                                .to_owned()
                        }
                    );
                    success(
                        "integration.install",
                        human,
                        json!({
                            "config_dir": config_dir,
                            "dry_run": dry_run,
                            "already_installed": plan.already_installed,
                            "changes": plan.changes,
                            "claude_version": capabilities.version,
                        }),
                    )
                }
                ClaudeIntegrationCommand::Status {
                    target,
                    claude_executable,
                } => {
                    let (config_dir, mode) = resolve(target)?;
                    let status_progress =
                        progress::Progress::start("Checking integration status…", json_mode);
                    let status = integration_status(&config_dir)?;
                    let capabilities =
                        assess_installed(claude_executable.as_deref(), &config_dir, mode).ok();
                    status_progress.finish();
                    let human = format!(
                        "Config dir: {}\nInstalled: {}\nStopFailure hook: {}\nStatusLine: {}\n\
                         Settings changed since install: {}\nHooks disabled: {}\n\
                         Recorded: statusline snapshot={}, StopFailure events={}, rate_limit events={}\n\
                         Claude Code: {}",
                        config_dir.display(),
                        status.installed,
                        status.stop_failure_hook,
                        status.statusline,
                        status.settings_drifted_since_install,
                        status.hooks_disabled,
                        status.statusline_snapshot_present,
                        status.recorded_stop_failures,
                        status.recorded_rate_limit_events,
                        capabilities.as_ref().map_or_else(
                            || "could not be assessed".to_owned(),
                            |report| format!(
                                "{} ({})",
                                report.version,
                                if report.usage_integration_ready(false).is_ok() {
                                    "verified"
                                } else {
                                    "NOT fully verified"
                                }
                            )
                        )
                    );
                    success(
                        "integration.status",
                        human,
                        json!({ "config_dir": config_dir, "status": status, "capabilities": capabilities }),
                    )
                }
                ClaudeIntegrationCommand::Uninstall { target, dry_run } => {
                    let (config_dir, _mode) = resolve(target)?;
                    let plan = plan_uninstall(&config_dir)?;
                    if !dry_run {
                        apply_uninstall(&plan)?;
                    }
                    let human = format!(
                        "{} for {}:\n{}",
                        if *dry_run {
                            "Dry run (nothing written): would uninstall"
                        } else {
                            "Uninstalled the Relay usage integration"
                        },
                        config_dir.display(),
                        plan.changes
                            .iter()
                            .map(|change| format!("  - {change}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    success(
                        "integration.uninstall",
                        human,
                        json!({ "config_dir": config_dir, "dry_run": dry_run, "installed": plan.installed, "changes": plan.changes }),
                    )
                }
            }
        }
    }
}

fn run_herdr_integration(
    herdr: &HerdrIntegrationArgs,
    paths: &RelayPaths,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let refused =
        |error: relay_herdr::HerdrIntegrationError| Error::IntegrationRefused(error.to_string());
    match &herdr.command {
        HerdrIntegrationCommand::Install {
            plugin_path,
            dry_run,
            herdr_executable,
        } => {
            let install_progress = progress::Progress::start("Contacting Herdr…", json_mode);
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let resolved_path =
                herdr_install::resolve_plugin_path(plugin_path.as_deref(), paths.config_root())
                    .map_err(refused)?;
            let plan = herdr_install::plan_install(&client, &resolved_path).map_err(refused)?;
            install_progress.finish();
            if *dry_run {
                let human = format!(
                    "Dry run (nothing linked): would link {} (already_linked={})",
                    resolved_path.display(),
                    plan.already_linked
                );
                return success(
                    "integration.herdr.install",
                    human,
                    json!({ "plugin_path": resolved_path, "dry_run": true, "already_linked": plan.already_linked }),
                );
            }
            let record = herdr_install::apply_install(&client, &resolved_path).map_err(refused)?;
            let human = format!(
                "Linked '{}' v{} (min_herdr_version {}) from {}",
                record.plugin_id,
                record.version,
                record.min_herdr_version,
                resolved_path.display()
            );
            success(
                "integration.herdr.install",
                human,
                json!({ "plugin_path": resolved_path, "dry_run": false, "plugin": record.plugin_id, "version": record.version }),
            )
        }
        HerdrIntegrationCommand::Status { herdr_executable } => {
            let status_progress = progress::Progress::start("Contacting Herdr…", json_mode);
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let report = herdr_install::status(&client).map_err(refused)?;
            status_progress.finish();
            let human = format!(
                "Herdr: client {}, server running={} version={} compatible={}\nPlugin: {}",
                report.herdr_client_version,
                report.herdr_server_running,
                report.herdr_server_version,
                report.herdr_compatible,
                report.plugin.as_ref().map_or_else(
                    || "not registered".to_owned(),
                    |p| format!("{} v{} (enabled={})", p.plugin_id, p.version, p.enabled)
                )
            );
            success(
                "integration.herdr.status",
                human,
                json!({
                    "herdr_client_version": report.herdr_client_version,
                    "herdr_server_running": report.herdr_server_running,
                    "herdr_server_version": report.herdr_server_version,
                    "herdr_compatible": report.herdr_compatible,
                    "plugin_registered": report.plugin.is_some(),
                }),
            )
        }
        HerdrIntegrationCommand::Doctor { herdr_executable } => {
            let doctor_progress =
                progress::Progress::start("Running Herdr diagnostics…", json_mode);
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let report = herdr_install::doctor(&client).map_err(refused)?;
            doctor_progress.finish();
            let mut lines = vec![format!(
                "Herdr integration is {}",
                if report.healthy {
                    "healthy"
                } else {
                    "unhealthy"
                }
            )];
            lines.extend(report.checks.iter().map(|check| {
                format!(
                    "[{}] {}: {}",
                    if check.passed { "ok" } else { "failed" },
                    check.name,
                    check.message
                )
            }));
            let checks_json: Vec<Value> = report
                .checks
                .iter()
                .map(|check| json!({ "name": check.name, "passed": check.passed, "message": check.message }))
                .collect();
            success(
                "integration.herdr.doctor",
                lines.join("\n"),
                json!({ "healthy": report.healthy, "checks": checks_json }),
            )
        }
        HerdrIntegrationCommand::Uninstall { herdr_executable } => {
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let removed = herdr_install::apply_uninstall(&client).map_err(refused)?;
            let human = if removed {
                "Unlinked the Agent Relay Herdr plugin".to_owned()
            } else {
                "Agent Relay Herdr plugin was not registered; nothing to do".to_owned()
            };
            success(
                "integration.herdr.uninstall",
                human,
                json!({ "removed": removed }),
            )
        }
    }
}
