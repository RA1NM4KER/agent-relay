//! `relay profile <subcommand>`: create/list/status/remove/doctor a registered profile, and
//! inspect/adopt an existing Claude directory by reference (`inspect-existing`/`adopt`,
//! including `--native-default` for Claude's own default account).

use std::path::PathBuf;

use relay_core::{
    AddProfileRequest, AuthenticationState, Error, IdentityMetadata, ProfileName, ProfileService,
    ProfileSetupMode, Provider, ProviderKind, RelayPaths,
};
use relay_provider_claude::{ClaudeAdoptionProvider, ClaudeIdentityPin, EnvironmentOverrideStatus};
use relay_testkit::FakeProvider;
use serde::Serialize;
use serde_json::json;

use crate::{
    auth::{
        create_and_authenticate_codex_profile, create_and_authenticate_profile, identity_summary,
        inspect_existing_claude, inspection_human, provider_for_profile,
        resolve_claude_adoption_target, resolve_new_profile_provider, run_claude_auth_subcommand,
        run_codex_auth_subcommand, verify_authenticated,
    },
    cli::{ExistingProvider, ProfileArgs, ProfileCommand},
    output::{CommandOutput, success},
    providers,
};

#[derive(Serialize)]
struct AdoptionDryRun {
    profile_name: ProfileName,
    provider: &'static str,
    config_dir: PathBuf,
    claude_version: String,
    authenticated: bool,
    detected_identity: Option<ClaudeIdentityPin>,
    identity_pin_to_store: Option<ClaudeIdentityPin>,
    environment_override_status: EnvironmentOverrideStatus,
    relay_owned_writes: Vec<PlannedWrite>,
    claude_profile_changes: Vec<String>,
    warnings: Vec<String>,
    reasons: Vec<String>,
    would_succeed: bool,
}

#[derive(Serialize)]
struct PlannedWrite {
    path: String,
    purpose: &'static str,
    contains_secrets: bool,
}

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    provider: &FakeProvider,
    profile: &ProfileArgs,
) -> Result<CommandOutput, Error> {
    match &profile.command {
        ProfileCommand::Add {
            name,
            provider: selected_provider,
            config_dir,
        } => {
            let provider_kind = ProviderKind::from(*selected_provider);
            let profile = service.add(
                AddProfileRequest {
                    name: name.clone(),
                    provider: provider_kind,
                    config_dir: config_dir.clone(),
                    mode: ProfileSetupMode::Create,
                    expected_identity: None,
                    claude_config_mode: None,
                },
                provider,
            )?;
            success(
                "profile.add",
                format!(
                    "Added fake profile '{}' at {}",
                    profile.name,
                    profile.config_dir.display()
                ),
                profile,
            )
        }
        ProfileCommand::List => {
            let profiles = service.list()?;
            let human = if profiles.is_empty() {
                "No profiles registered.".to_owned()
            } else {
                profiles
                    .iter()
                    .map(|profile| {
                        format!(
                            "{}\t{}\t{}",
                            profile.name,
                            profile.provider,
                            profile.config_dir.display()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            success("profile.list", human, profiles)
        }
        ProfileCommand::Status {
            name,
            claude_executable,
            codex_executable,
        } => {
            let provider = provider_for_profile(
                service,
                name,
                &providers::ExecutableOverrides {
                    claude: claude_executable.clone(),
                    codex: codex_executable.clone(),
                },
            )?;
            let status = service.status(name, provider.as_ref())?;
            let human = format!(
                "Profile: {}\nProvider: {}\nAuthentication: {:?}\nAvailability: {:?}\nIdentity matches: {}",
                status.profile.name,
                status.profile.provider,
                status.authentication,
                status.availability.state,
                status.identity_matches
            );
            success("profile.status", human, status)
        }
        ProfileCommand::Remove { name } => {
            let profile = service.remove(name)?;
            let human = format!(
                "Removed profile '{}'. Provider directory retained at {}",
                profile.name,
                profile.config_dir.display()
            );
            success(
                "profile.remove",
                human,
                json!({ "profile": profile, "directory_retained": true }),
            )
        }
        ProfileCommand::Doctor {
            name,
            claude_executable,
            codex_executable,
        } => {
            let provider = provider_for_profile(
                service,
                name,
                &providers::ExecutableOverrides {
                    claude: claude_executable.clone(),
                    codex: codex_executable.clone(),
                },
            )?;
            let report = service.doctor(name, provider.as_ref())?;
            let mut lines = vec![format!(
                "Profile '{}' is {}",
                report.profile,
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
            success("profile.doctor", lines.join("\n"), report)
        }
        ProfileCommand::InspectExisting {
            provider: ExistingProvider::Claude,
            config_dir,
            allow_external,
            claude_executable,
            native_default,
        } => {
            let (config_dir, allow_external, mode) = resolve_claude_adoption_target(
                config_dir.as_deref(),
                *native_default,
                *allow_external,
            )?;
            let report = inspect_existing_claude(
                paths,
                &config_dir,
                allow_external,
                claude_executable.as_deref(),
                mode,
            )?;
            let human = inspection_human(&report);
            success("profile.inspect_existing", human, report)
        }
        ProfileCommand::Adopt {
            name,
            provider: ExistingProvider::Claude,
            config_dir,
            dry_run,
            allow_external,
            claude_executable,
            native_default,
        } if !dry_run => {
            let (config_dir, allow_external, mode) = resolve_claude_adoption_target(
                config_dir.as_deref(),
                *native_default,
                *allow_external,
            )?;
            let report = inspect_existing_claude(
                paths,
                &config_dir,
                allow_external,
                claude_executable.as_deref(),
                mode,
            )?;
            if !report.safe_to_adopt {
                return Err(if report.authenticated {
                    Error::IdentityUnavailable
                } else {
                    Error::AuthenticationRequired
                });
            }
            let pin = report
                .identity_pin
                .clone()
                .ok_or(Error::IdentityUnavailable)?;
            let expected_identity = IdentityMetadata {
                stable_id: pin.stable_id(),
                display_label: pin.email.clone().or_else(|| pin.account_id.clone()),
            };
            let claude_provider = ClaudeAdoptionProvider::discover(claude_executable.as_deref())?;
            let profile = service.add(
                AddProfileRequest {
                    name: name.clone(),
                    provider: ProviderKind::Claude,
                    config_dir: Some(report.config_dir.clone()),
                    mode: ProfileSetupMode::AdoptExisting,
                    expected_identity: Some(expected_identity),
                    claude_config_mode: Some(mode),
                },
                &claude_provider,
            )?;
            let human = format!(
                "Adopted Claude profile '{}'\nDirectory: {}\nIdentity: {}\nWarnings: {}",
                profile.name,
                profile.config_dir.display(),
                identity_summary(&pin),
                if report.warnings.is_empty() {
                    "none".to_owned()
                } else {
                    report.warnings.join("; ")
                }
            );
            success("profile.adopt", human, profile)
        }
        ProfileCommand::Adopt {
            name,
            provider: ExistingProvider::Claude,
            config_dir,
            dry_run: _,
            allow_external,
            claude_executable,
            native_default,
        } => {
            let (config_dir, allow_external, mode) = resolve_claude_adoption_target(
                config_dir.as_deref(),
                *native_default,
                *allow_external,
            )?;
            let report = inspect_existing_claude(
                paths,
                &config_dir,
                allow_external,
                claude_executable.as_deref(),
                mode,
            )?;
            let mut reasons = report.reasons.clone();
            let registered_profiles = service.list()?;
            let duplicate_name = registered_profiles
                .iter()
                .any(|profile| profile.name == *name);
            if duplicate_name {
                reasons.push(format!("profile name '{name}' is already registered"));
            }
            let duplicate_identity = report.identity_pin.as_ref().is_some_and(|pin| {
                let stable_id = pin.stable_id();
                registered_profiles.iter().any(|profile| {
                    profile.provider == ProviderKind::Claude
                        && profile.expected_identity.stable_id == stable_id
                })
            });
            if duplicate_identity {
                reasons.push(
                        "provider identity is already registered to another profile; aliases are not allowed"
                            .to_owned(),
                    );
            }
            let would_succeed = report.safe_to_adopt && !duplicate_name && !duplicate_identity;
            let registry_path = paths.profile_state_file();
            let registry_parent = registry_path.parent().ok_or(Error::AtomicWriteFailed)?;
            let dry_run = AdoptionDryRun {
                profile_name: name.clone(),
                provider: "claude",
                config_dir: report.config_dir,
                claude_version: report.claude_version,
                authenticated: report.authenticated,
                detected_identity: report.identity_pin.clone(),
                identity_pin_to_store: report.identity_pin,
                environment_override_status: report.environment_override_status,
                relay_owned_writes: vec![
                    PlannedWrite {
                        path: registry_path.display().to_string(),
                        purpose: "permanent atomic update of Relay's profile registry",
                        contains_secrets: false,
                    },
                    PlannedWrite {
                        path: format!(
                            "{}/.profiles.toml.tmp.<pid>.<timestamp>.<sequence>",
                            registry_parent.display()
                        ),
                        purpose: "transient same-directory file used for atomic replacement",
                        contains_secrets: false,
                    },
                ],
                claude_profile_changes: Vec::new(),
                warnings: report.warnings,
                reasons,
                would_succeed,
            };
            let identity = dry_run
                .identity_pin_to_store
                .as_ref()
                .map(identity_summary)
                .unwrap_or_else(|| "unavailable".to_owned());
            let human = format!(
                "Adoption dry-run for '{}'\nProvider: Claude\nDirectory: {}\nVersion: {}\nIdentity pin: {}\nWould write: {}\nClaude profile changes: none\nWould succeed: {}{}",
                dry_run.profile_name,
                dry_run.config_dir.display(),
                dry_run.claude_version,
                identity,
                registry_path.display(),
                if dry_run.would_succeed { "yes" } else { "no" },
                if dry_run.reasons.is_empty() {
                    String::new()
                } else {
                    format!("\nReasons: {}", dry_run.reasons.join("; "))
                }
            );
            success("profile.adopt.dry_run", human, dry_run)
        }
    }
}

pub(crate) fn run_login(
    service: &ProfileService,
    paths: &RelayPaths,
    name: &ProfileName,
    provider: Option<ProviderKind>,
    json_mode: bool,
    executables: &providers::ExecutableOverrides,
) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    if let Some(existing) = registered.iter().find(|profile| &profile.name == name) {
        match existing.provider {
            ProviderKind::Claude => {
                if !json_mode {
                    println!("Opening Claude login for '{name}'...");
                }
                let mode = existing.effective_claude_config_mode();
                run_claude_auth_subcommand(
                    executables.claude.as_deref(),
                    &existing.config_dir,
                    mode,
                    "login",
                )?;
                let report = verify_authenticated(
                    &existing.config_dir,
                    mode,
                    executables.claude.as_deref(),
                )?;
                if !report.authenticated {
                    return Err(Error::AuthenticationRequired);
                }
            }
            ProviderKind::Codex => {
                if !json_mode {
                    println!("Opening Codex login for '{name}'...");
                }
                run_codex_auth_subcommand(
                    executables.codex.as_deref(),
                    &existing.config_dir,
                    "login",
                )?;
                let backend =
                    relay_provider_codex::CodexBackend::discover(executables.codex.as_deref())?;
                let observation = backend.inspect_profile(&existing.config_dir, None)?;
                if observation.authentication != AuthenticationState::Authenticated {
                    return Err(Error::AuthenticationRequired);
                }
            }
            ProviderKind::Fake => return Err(Error::ProviderUnsupported),
        }
        return success(
            "login",
            format!("\u{2713} {name} authenticated"),
            json!({ "profile": name.as_str(), "authenticated": true }),
        );
    }
    let provider = match provider {
        Some(provider) => provider,
        None => resolve_new_profile_provider(executables, json_mode)?,
    };
    if !json_mode {
        println!("'{name}' is not a registered profile yet; creating it as {provider}.");
    }
    let profile = match provider {
        ProviderKind::Codex => create_and_authenticate_codex_profile(
            service,
            paths,
            name,
            executables.codex.as_deref(),
        )?,
        ProviderKind::Claude | ProviderKind::Fake => {
            create_and_authenticate_profile(service, paths, name, executables.claude.as_deref())?
        }
    };
    success(
        "login",
        format!("\u{2713} {name} authenticated"),
        json!({
            "profile": profile.name.as_str(),
            "provider": profile.provider.to_string(),
            "authenticated": true,
            "created": true,
        }),
    )
}

pub(crate) fn run_logout(
    service: &ProfileService,
    name: &ProfileName,
    executables: &providers::ExecutableOverrides,
) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    let profile = registered
        .iter()
        .find(|profile| &profile.name == name)
        .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
    match profile.provider {
        ProviderKind::Codex => {
            run_codex_auth_subcommand(executables.codex.as_deref(), &profile.config_dir, "logout")?;
        }
        ProviderKind::Claude | ProviderKind::Fake => {
            run_claude_auth_subcommand(
                executables.claude.as_deref(),
                &profile.config_dir,
                profile.effective_claude_config_mode(),
                "logout",
            )?;
        }
    }
    success(
        "logout",
        format!(
            "Logged out '{name}' (registration kept; run `relay login {name}` to sign back in)"
        ),
        json!({ "profile": name.as_str() }),
    )
}
