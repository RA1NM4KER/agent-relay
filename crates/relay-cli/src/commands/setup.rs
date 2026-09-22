//! `relay setup`: the interactive first-run wizard (and its `--non-interactive` scripting form).

use std::path::{Path, PathBuf};

use relay_core::{
    ClaudeConfigMode, Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
};
use relay_herdr::herdr_client::HerdrCliClient;
use relay_herdr::install as herdr_install;
use relay_provider_claude::{ClaudeInspector, apply_install, assess_installed, plan_install};
use serde_json::json;

use crate::{
    auth::{
        adopt_authenticated_profile, create_and_authenticate_codex_profile,
        create_and_authenticate_profile, friendly_auth_state, inspect_existing_claude,
    },
    cli::SetupArgs,
    output::{CommandOutput, success},
    preferences, providers, readiness,
    util::{current_unix_ms, prompt_line, prompt_yes_no},
};

/// M4.1: the interactive first-run wizard. Every step reuses existing, already-tested machinery
/// (`ClaudeInspector`, `service.add`/`AdoptExisting`, `plan_install`/`apply_install`,
/// `herdr_install::{plan,apply}_install`) — this function only sequences prompts around them and
/// saves the result to `preferences.toml`. Safe to re-run: it detects and offers to reuse existing
/// profiles/integrations (M4.9) rather than starting over.
pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &SetupArgs,
) -> Result<CommandOutput, Error> {
    if args.non_interactive {
        return run_setup_non_interactive(service, paths, args);
    }

    println!("Agent Relay setup\n");

    // --- Step 1: environment ---
    println!("Checking your environment...");
    let claude_executable = args.claude_executable.as_deref();
    let claude_version = ClaudeInspector::discover(claude_executable)
        .and_then(|inspector| inspector.inspect_version());
    let codex_executable = args.codex_executable.as_deref();
    let codex_version = relay_provider_codex::CodexInspector::discover(codex_executable)
        .and_then(|inspector| inspector.inspect_version());
    // Either provider is enough on its own; neither is privileged.
    match &claude_version {
        Ok(version) => println!("  Claude Code        \u{2713} ({version})"),
        Err(_) => println!("  Claude Code        (not installed)"),
    }
    match &codex_version {
        Ok(version) => println!("  Codex CLI          \u{2713} ({version})"),
        Err(_) => println!("  Codex CLI          (not installed)"),
    }
    let claude_available = claude_version.is_ok();
    let codex_available = codex_version.is_ok();
    let herdr_client = HerdrCliClient::discover(None);
    let herdr_probe = herdr_client
        .as_ref()
        .ok()
        .and_then(|client| client.status().ok());
    match &herdr_probe {
        Some(status) => println!("  Herdr              \u{2713} ({})", status.server.version),
        None => println!("  Herdr              (not found; optional)"),
    }
    println!(
        "  Agent Relay config \u{2713} ({})",
        paths.config_root().display()
    );
    if args.verbose {
        println!(
            "  (verbose) config_root={}, state_root={}",
            paths.config_root().display(),
            paths.state_root().display()
        );
    }
    if !claude_available && !codex_available {
        println!(
            "\nAgent Relay needs at least one supported coding-agent CLI installed:\n  \u{2022} Claude Code  https://docs.claude.com/en/docs/claude-code\n  \u{2022} Codex CLI    https://developers.openai.com/codex\nInstall one (or both), then run `relay setup` again."
        );
        return Err(Error::ProviderExecutableMissing);
    }

    // --- Step 2: profiles ---
    let mut registered = service.list()?;
    if !registered.is_empty() {
        println!("\nExisting profiles found:");
        for profile in &registered {
            let (auth, _) = friendly_auth_state(
                profile,
                &providers::ExecutableOverrides {
                    claude: claude_executable.map(Path::to_path_buf),
                    codex: codex_executable.map(Path::to_path_buf),
                },
            );
            println!("  \u{2713} {} ({auth})", profile.name);
        }
        prompt_yes_no("\nUse these?", true)?;
    }
    loop {
        let must_add_one = registered.is_empty();
        if !must_add_one && !prompt_yes_no("\nAdd another profile?", false)? {
            break;
        }
        let name_text = prompt_line("Profile name", None)?;
        let name = ProfileName::new(&name_text)
            .map_err(|_| Error::InvalidProfileName(name_text.clone()))?;
        if registered.iter().any(|profile| profile.name == name) {
            println!("'{name}' is already registered.");
            continue;
        }
        // Which provider is this profile for? Implied when only one CLI is installed; asked (with
        // no preferred answer) when both are.
        let use_codex = match (claude_available, codex_available) {
            (true, true) => loop {
                let answer = prompt_line(&format!("Provider for '{name}' (claude/codex)"), None)?;
                match answer.trim().to_ascii_lowercase().as_str() {
                    "claude" => break false,
                    "codex" => break true,
                    _ => println!("Please answer 'claude' or 'codex'."),
                }
            },
            (false, true) => true,
            _ => false,
        };
        if use_codex {
            println!("\nOpening Codex login for '{name}'...");
            match create_and_authenticate_codex_profile(service, paths, &name, codex_executable) {
                Ok(profile) => {
                    println!("\u{2713} {} authenticated", profile.name);
                    registered.push(profile);
                }
                Err(error) => println!("Could not authenticate '{name}': {error}"),
            }
            continue;
        }
        let create_new = prompt_yes_no(
            &format!(
                "Authenticate a NEW Claude account for '{name}'? (no = adopt an already-authenticated isolated profile)"
            ),
            true,
        )?;
        let profile = if create_new {
            println!("\nOpening Claude login for '{name}'...");
            match create_and_authenticate_profile(service, paths, &name, claude_executable) {
                Ok(profile) => profile,
                Err(error) => {
                    println!("Could not authenticate '{name}': {error}");
                    continue;
                }
            }
        } else {
            let config_dir_text = prompt_line("Existing isolated Claude config directory", None)?;
            let config_dir = PathBuf::from(config_dir_text);
            let report = match inspect_existing_claude(
                paths,
                &config_dir,
                true,
                claude_executable,
                ClaudeConfigMode::Explicit,
            ) {
                Ok(report) => report,
                Err(error) => {
                    println!("Could not inspect that directory: {error}");
                    continue;
                }
            };
            if !report.safe_to_adopt {
                println!(
                    "That directory is not safe to adopt: {}",
                    report.reasons.join(", ")
                );
                continue;
            }
            match adopt_authenticated_profile(
                service,
                &name,
                &report.config_dir,
                &report,
                claude_executable,
            ) {
                Ok(profile) => profile,
                Err(error) => {
                    println!("Could not adopt '{name}': {error}");
                    continue;
                }
            }
        };
        println!("\u{2713} {} authenticated", profile.name);
        registered.push(profile);
    }
    if registered.is_empty() {
        return Err(Error::AdoptionIdentityRequired);
    }

    // --- Step 3: primary/fallback ---
    let mut preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let primary = if registered.len() == 1 {
        registered[0].name.clone()
    } else {
        println!("\nPrimary profile:");
        for profile in &registered {
            println!("  {}", profile.name);
        }
        let default = preferences
            .primary_profile
            .clone()
            .filter(|name| registered.iter().any(|profile| &profile.name == name))
            .unwrap_or_else(|| registered[0].name.clone());
        loop {
            let chosen = prompt_line("Primary profile", Some(default.as_str()))?;
            if let Some(profile) = registered
                .iter()
                .find(|profile| profile.name.as_str() == chosen)
            {
                break profile.name.clone();
            }
            println!("Not one of the registered profiles above.");
        }
    };
    let fallback_candidates: Vec<ProfileName> = registered
        .iter()
        .filter(|profile| profile.name != primary)
        .map(|profile| profile.name.clone())
        .collect();
    let fallback = if fallback_candidates.is_empty() {
        Vec::new()
    } else {
        println!("\nFallback order (comma-separated, in priority order):");
        for candidate in &fallback_candidates {
            println!("  {candidate}");
        }
        let default = fallback_candidates
            .iter()
            .map(ProfileName::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let chosen = prompt_line("Fallback order", Some(&default))?;
        chosen
            .split(',')
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .filter_map(|text| {
                fallback_candidates
                    .iter()
                    .find(|c| c.as_str() == text)
                    .cloned()
            })
            .collect::<Vec<_>>()
    };
    preferences.primary_profile = Some(primary.clone());
    preferences.fallback_profiles = fallback.clone();

    // --- Step 4: usage integration ---
    // Claude reports a rate limit through an installed hook + status line; Codex's quota is read
    // from Codex's own structured interface and needs nothing installed. So only Claude profiles
    // are offered the integration.
    let claude_profiles: Vec<&Profile> = std::iter::once(&primary)
        .chain(fallback.iter())
        .filter_map(|name| registered.iter().find(|profile| &profile.name == name))
        .filter(|profile| profile.provider != ProviderKind::Codex)
        .collect();
    let enable_usage = if claude_profiles.is_empty() {
        println!(
            "\nCodex quota is checked automatically; there is no usage integration to install."
        );
        true
    } else {
        let enable = prompt_yes_no(
            if registered
                .iter()
                .any(|profile| profile.provider == ProviderKind::Codex)
            {
                "\nEnable automatic quota detection? (installs a hook for Claude profiles; Codex needs nothing)"
            } else {
                "\nEnable automatic quota detection?"
            },
            true,
        )?;
        if enable {
            for profile in &claude_profiles {
                install_usage_integration_interactive(profile, claude_executable)?;
            }
        }
        enable
    };
    preferences.usage_integration_enabled = Some(enable_usage);

    // --- Step 5: Herdr ---
    let enable_herdr = if herdr_probe.is_some() {
        prompt_yes_no(
            "\nEnable Herdr integration? (works with Claude panes today)",
            true,
        )?
    } else {
        println!("\nHerdr not found.\nAgent Relay will work without it.\nYou can add Herdr later.");
        false
    };
    if enable_herdr {
        match install_herdr_integration(paths) {
            Ok(healthy) => println!(
                "  \u{2713} Herdr integration installed ({})",
                if healthy {
                    "healthy"
                } else {
                    "needs attention; run `relay integration herdr doctor`"
                }
            ),
            Err(error) => println!("  Could not install the Herdr integration: {error}"),
        }
    }
    preferences.herdr_enabled = Some(enable_herdr);

    preferences.save(paths.config_root())?;

    // --- Step 6: finish ---
    let start_commands = start_commands_for(
        std::iter::once(&primary)
            .chain(fallback.iter())
            .filter_map(|name| registered.iter().find(|profile| &profile.name == name))
            .map(|profile| profile.provider),
    );
    // Reuses `relay doctor`'s exact readiness model so setup's completion screen and `relay
    // doctor` never give two different answers to "is automatic handoff actually ready".
    let refreshed = service.list()?;
    let readiness = readiness::assess(
        service,
        &refreshed,
        &preferences,
        &providers::ExecutableOverrides {
            claude: claude_executable.map(std::path::Path::to_path_buf),
            codex: codex_executable.map(std::path::Path::to_path_buf),
        },
    );
    let human = render_completion(
        &primary,
        &fallback,
        &readiness,
        enable_herdr,
        &start_commands,
    );
    success(
        "setup",
        human,
        json!({
            "primary": primary.as_str(),
            "fallback": fallback.iter().map(ProfileName::to_string).collect::<Vec<_>>(),
            "usage_integration_enabled": enable_usage,
            "herdr_enabled": enable_herdr,
            "start_commands": start_commands,
            "ready": readiness.ready(),
            "readiness_checks": readiness.checks,
        }),
    )
}

fn render_completion(
    primary: &ProfileName,
    fallback: &[ProfileName],
    readiness: &readiness::Readiness,
    herdr_enabled: bool,
    start_commands: &[&str],
) -> String {
    let fallback_lines = if fallback.is_empty() {
        "  (none)".to_owned()
    } else {
        fallback
            .iter()
            .enumerate()
            .map(|(index, name)| format!("  {}. {name}", index + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if readiness.ready() {
        format!(
            "Agent Relay is ready.\n\nPrimary\n  {primary}\n\nFallbacks\n{fallback_lines}\n\n\
             Automatic handoff\n  Ready\n\nHerdr\n  {}\n\nStart with:\n{}\n\nContinue the current \
             conversation with:\n  relay resume",
            if herdr_enabled {
                "Connected"
            } else {
                "Not enabled"
            },
            start_commands
                .iter()
                .map(|command| format!("  {command}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        let problems = readiness
            .checks
            .iter()
            .filter(|check| check.level == readiness::Level::Blocking)
            .map(|check| {
                let detail = check
                    .detail
                    .as_deref()
                    .unwrap_or(&check.label)
                    .trim_end_matches('.');
                check.remedy.as_ref().map_or_else(
                    || format!("{detail}."),
                    |remedy| format!("{detail}.\nRun:\n  {remedy}"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "Setup finished, but automatic handoff is not ready yet.\n\n{problems}\n\n\
             Run `relay doctor` any time to re-check."
        )
    }
}

fn run_setup_non_interactive(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &SetupArgs,
) -> Result<CommandOutput, Error> {
    let primary = args
        .primary
        .clone()
        .ok_or(Error::AdoptionIdentityRequired)?;
    let registered = service.list()?;
    let primary_profile = registered
        .iter()
        .find(|profile| profile.name == primary)
        .ok_or_else(|| Error::ProfileNotFound(primary.to_string()))?;
    for fallback_name in &args.fallback {
        if !registered
            .iter()
            .any(|profile| &profile.name == fallback_name)
        {
            return Err(Error::ProfileNotFound(fallback_name.to_string()));
        }
    }

    let mut preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    preferences.primary_profile = Some(primary.clone());
    preferences.fallback_profiles = args.fallback.clone();

    if let Some(enable_usage) = args.usage_integration {
        if enable_usage {
            for name in std::iter::once(&primary).chain(args.fallback.iter()) {
                let profile = registered
                    .iter()
                    .find(|profile| &profile.name == name)
                    .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
                // The usage integration is a Claude hook + status line; Codex needs none.
                if profile.provider == ProviderKind::Codex {
                    continue;
                }
                let capabilities = assess_installed(
                    args.claude_executable.as_deref(),
                    &profile.config_dir,
                    profile.effective_claude_config_mode(),
                )?;
                capabilities
                    .usage_integration_ready(false)
                    .map_err(Error::IntegrationRefused)?;
                let relay_executable = std::env::current_exe().map_err(|source| Error::Io {
                    path: PathBuf::from("relay"),
                    source,
                })?;
                let plan = plan_install(&profile.config_dir, &relay_executable)?;
                apply_install(&plan, current_unix_ms())?;
            }
        }
        preferences.usage_integration_enabled = Some(enable_usage);
    }

    if let Some(enable_herdr) = args.herdr {
        if enable_herdr {
            install_herdr_integration(paths)?;
        }
        preferences.herdr_enabled = Some(enable_herdr);
    }

    preferences.save(paths.config_root())?;
    let _ = primary_profile;
    let readiness = readiness::assess(
        service,
        &registered,
        &preferences,
        &providers::ExecutableOverrides {
            claude: args.claude_executable.clone(),
            codex: args.codex_executable.clone(),
        },
    );
    success(
        "setup",
        format!("Configured. Primary: {primary}"),
        json!({
            "primary": primary.as_str(),
            "fallback": args.fallback.iter().map(ProfileName::to_string).collect::<Vec<_>>(),
            "usage_integration_enabled": preferences.usage_integration_enabled,
            "herdr_enabled": preferences.herdr_enabled,
            "start_commands": start_commands_for(
                std::iter::once(&primary)
                    .chain(args.fallback.iter())
                    .filter_map(|name| registered.iter().find(|profile| &profile.name == name))
                    .map(|profile| profile.provider),
            ),
            // Additive (M-UX): same shared readiness model `relay doctor` uses.
            "ready": readiness.ready(),
            "readiness_checks": readiness.checks,
        }),
    )
}

/// The "start a new conversation" commands worth showing for the providers actually configured:
/// only commands the user can really use, each provider a peer.
fn start_commands_for(providers: impl Iterator<Item = ProviderKind>) -> Vec<&'static str> {
    let mut claude = false;
    let mut codex = false;
    for provider in providers {
        match provider {
            ProviderKind::Codex => codex = true,
            ProviderKind::Claude | ProviderKind::Fake => claude = true,
        }
    }
    let mut commands = Vec::new();
    if claude {
        commands.push("relay claude");
    }
    if codex {
        commands.push("relay codex");
    }
    commands
}

/// Shared by the interactive and non-interactive setup paths: installs the usage integration for
/// one profile via the unchanged `plan_install`/`apply_install`, explaining (never silently
/// bypassing) an unverified Claude Code version per M4.1 Step 4.
fn install_usage_integration_interactive(
    profile: &Profile,
    claude_executable: Option<&Path>,
) -> Result<(), Error> {
    let capabilities = match assess_installed(
        claude_executable,
        &profile.config_dir,
        profile.effective_claude_config_mode(),
    ) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            println!("  Could not assess '{}': {error}", profile.name);
            return Ok(());
        }
    };
    let mut allow_unverified = false;
    if let Err(error) = capabilities.usage_integration_ready(false) {
        println!(
            "  '{}' is running a Claude Code version Relay has not verified for the usage integration ({error}).",
            profile.name
        );
        if !prompt_yes_no("  Install anyway (unverified)?", false)? {
            println!("  Skipped usage detection for '{}'.", profile.name);
            return Ok(());
        }
        allow_unverified = true;
        if let Err(error) = capabilities.usage_integration_ready(true) {
            println!("  Still not installable for '{}': {error}", profile.name);
            return Ok(());
        }
    }
    let _ = allow_unverified;
    let relay_executable = std::env::current_exe().map_err(|source| Error::Io {
        path: PathBuf::from("relay"),
        source,
    })?;
    let plan = plan_install(&profile.config_dir, &relay_executable)?;
    apply_install(&plan, current_unix_ms())?;
    println!("  \u{2713} usage detection enabled for {}", profile.name);
    Ok(())
}

/// Shared by the interactive and non-interactive setup paths: links `plugins/herdr` and confirms
/// it with the same `doctor` check `relay integration herdr doctor` exposes.
fn install_herdr_integration(paths: &RelayPaths) -> Result<bool, Error> {
    let client = HerdrCliClient::discover(None)
        .map_err(|error| Error::IntegrationRefused(error.to_string()))?;
    let plugin_path = herdr_install::resolve_plugin_path(None, paths.config_root())
        .map_err(|error| Error::IntegrationRefused(error.to_string()))?;
    herdr_install::apply_install(&client, &plugin_path)
        .map_err(|error| Error::IntegrationRefused(error.to_string()))?;
    let report = herdr_install::doctor(&client)
        .map_err(|error| Error::IntegrationRefused(error.to_string()))?;
    Ok(report.healthy)
}
