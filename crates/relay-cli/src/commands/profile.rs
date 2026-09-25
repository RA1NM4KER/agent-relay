//! `relay profile <subcommand>`: create/list/status/remove/doctor a registered profile, and
//! inspect/adopt an existing Claude directory by reference (`inspect-existing`/`adopt`,
//! including `--native-default` for Claude's own default account).

use std::path::{Path, PathBuf};

use relay_core::{
    AddProfileRequest, AtomicWrite, AuthenticationState, Error, FsAtomicWriter, IdentityMetadata,
    ProfileName, ProfileService, ProfileSetupMode, Provider, ProviderKind, RelayPaths,
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
    preferences, progress, providers,
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
    json_mode: bool,
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
            let progress = progress::Progress::start(&format!("Checking {name}…"), json_mode);
            let provider = provider_for_profile(
                service,
                name,
                &providers::ExecutableOverrides {
                    claude: claude_executable.clone(),
                    codex: codex_executable.clone(),
                },
            )?;
            let status = service.status(name, provider.as_ref())?;
            progress.finish();
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
        ProfileCommand::Rename { old_name, new_name } => {
            rename_profile(service, paths, old_name, new_name)
        }
        ProfileCommand::Doctor {
            name,
            claude_executable,
            codex_executable,
        } => {
            let progress = progress::Progress::start(&format!("Checking {name}…"), json_mode);
            let provider = provider_for_profile(
                service,
                name,
                &providers::ExecutableOverrides {
                    claude: claude_executable.clone(),
                    codex: codex_executable.clone(),
                },
            )?;
            let report = service.doctor(name, provider.as_ref())?;
            progress.finish();
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

/// Keys that hold a profile name in Relay's own JSON state — never a generic `name` or path-like
/// key, so this only ever touches what it is meant to. A generic key-targeted walk was chosen
/// over one typed struct per file because the real on-disk layout under `state_root/projects/`
/// mixes a legacy (pre-multi-session) project-level shape with the current per-session shape —
/// this covers both without having to enumerate every historical shape by hand.
const PROFILE_NAME_JSON_KEYS: &[&str] = &[
    "owner_profile",
    "last_profile",
    "source_profile",
    "target_profile",
    "profile",
    "source",
    "target",
];

/// Renames a profile's registered label everywhere Relay keeps a durable reference to it —
/// registry, preferences, and every session/ledger/handoff-journal file under the state root —
/// without touching authentication, identity pins, or the provider's own config directory.
///
/// Refuses outright if the profile currently owns any lease anywhere, live or not yet reconciled:
/// a running `relay claude`/`relay codex` terminal re-reads its own lease on every tick and would
/// treat the label changing underneath it as an external handoff, which is exactly the kind of
/// surprise this must never cause. Once nothing references the profile as a current lease owner,
/// every other file is a plain historical record (`last_profile`, journal entries, the automation
/// ledger) and is safe to relabel in place.
fn rename_profile(
    service: &ProfileService,
    paths: &RelayPaths,
    old: &ProfileName,
    new: &ProfileName,
) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    if !registered.iter().any(|profile| &profile.name == old) {
        return Err(Error::ProfileNotFound(old.to_string()));
    }
    if let Some(active) = find_lease_owner(paths, old) {
        return Err(Error::ProfileHasActiveSession(format!(
            "{old} (see {})",
            active.display()
        )));
    }

    let renamed = service.rename(old, new)?;

    let mut preferences_updated = false;
    if let Some(mut prefs) = preferences::Preferences::load(paths.config_root())? {
        let mut changed = false;
        if prefs.primary_profile.as_ref() == Some(old) {
            prefs.primary_profile = Some(new.clone());
            changed = true;
        }
        for fallback in &mut prefs.fallback_profiles {
            if fallback == old {
                *fallback = new.clone();
                changed = true;
            }
        }
        if changed {
            prefs.save(paths.config_root())?;
            preferences_updated = true;
        }
    }

    let files_updated = rename_in_state_files(paths, old, new)?;

    success(
        "profile.rename",
        format!(
            "Renamed profile '{old}' to '{new}'. Preferences updated: {preferences_updated}. \
             {files_updated} historical state file(s) updated. Authentication, identity pin and \
             provider config directory ({}) were not touched.",
            renamed.config_dir.display()
        ),
        json!({
            "profile": renamed,
            "preferences_updated": preferences_updated,
            "state_files_updated": files_updated,
        }),
    )
}

/// The first `lease.json` anywhere under the state root whose `owner_profile` is `name`, if any —
/// regardless of whether that lease's own process is still actually alive: a stale, not-yet-
/// reconciled lease is exactly as unsafe to relabel underneath as a genuinely live one, since
/// reconciliation itself still trusts the label until it runs.
///
/// A lease whose recorded process is *confirmed dead* (`is_still_the_same_process() ==
/// Some(false)`) does not block: it is stale, not live, and the profile label it names is a
/// historical fact at that point, exactly like `last_profile` elsewhere. Anything else — genuinely
/// alive, or ambiguous (`None`, the same "cannot prove either way" case every other liveness check
/// in this codebase fails closed on) — blocks.
fn find_lease_owner(paths: &RelayPaths, name: &ProfileName) -> Option<PathBuf> {
    for path in walk_json_files(&paths.projects_state_root()) {
        if path.file_name().and_then(|n| n.to_str()) != Some("lease.json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(lease) = serde_json::from_slice::<relay_core::handoff::WriterLease>(&bytes)
            && &lease.owner_profile == name
            && lease.owner_process.is_still_the_same_process() != Some(false)
        {
            return Some(path);
        }
    }
    None
}

/// Renames `old` to `new` in every JSON file's profile-name-bearing fields (see
/// [`PROFILE_NAME_JSON_KEYS`]) under the state root, except `supervisor.json` (a live process's
/// own heartbeat file, rewritten by it on every tick, never Relay's durable history). By the time
/// this runs, [`find_lease_owner`] has already refused the whole rename if any lease confirmed-live
/// or ambiguous still names `old` as owner — so a `lease.json` reached here can only be a
/// confirmed-dead, stale one, and is re-checked (defense in depth, not trust in the earlier
/// refusal alone) before being relabeled like any other historical record; still-live is skipped
/// rather than touched. Returns how many files were actually changed.
fn rename_in_state_files(
    paths: &RelayPaths,
    old: &ProfileName,
    new: &ProfileName,
) -> Result<usize, Error> {
    let mut updated = 0usize;
    for path in walk_json_files(&paths.projects_state_root()) {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if file_name == "supervisor.json" {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if file_name == "lease.json"
            && let Ok(lease) = serde_json::from_slice::<relay_core::handoff::WriterLease>(&bytes)
            && lease.owner_process.is_still_the_same_process() != Some(false)
        {
            continue;
        }
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if !rename_profile_fields(&mut value, old.as_str(), new.as_str()) {
            continue;
        }
        let text = serde_json::to_string_pretty(&value).map_err(|_| Error::SerializationFailed)?;
        FsAtomicWriter.write_atomic(&path, text.as_bytes())?;
        updated += 1;
    }
    Ok(updated)
}

fn rename_profile_fields(value: &mut serde_json::Value, old: &str, new: &str) -> bool {
    let mut changed = false;
    match value {
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if PROFILE_NAME_JSON_KEYS.contains(&key.as_str())
                    && let serde_json::Value::String(text) = entry
                    && text == old
                {
                    *text = new.to_owned();
                    changed = true;
                    continue;
                }
                changed |= rename_profile_fields(entry, old, new);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                changed |= rename_profile_fields(item, old, new);
            }
        }
        _ => {}
    }
    changed
}

/// Every `.json` file anywhere under `root`, depth-first — the on-disk layout has both a legacy
/// (pre-multi-session) project-level shape and the current per-session shape side by side on a
/// real dogfood machine, so this walks structurally rather than assuming one fixed depth.
fn walk_json_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files
}
