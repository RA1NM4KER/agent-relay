use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
use relay_core::{
    AddProfileRequest, Error, ProfileName, ProfileService, ProfileSetupMode, ProviderKind,
    RelayPaths,
};
use relay_provider_claude::{
    ClaudeIdentityPin, ClaudeInspectionReport, ClaudeInspector, EnvironmentOverrideStatus,
    inspect_environment,
};
use relay_testkit::FakeProvider;
use serde::Serialize;
use serde_json::{Value, json};

const OUTPUT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Parser)]
#[command(
    name = "relay",
    version,
    about = "Explicit coding-agent profile handoff orchestration"
)]
struct Cli {
    /// Emit a stable machine-readable JSON envelope.
    #[arg(long, global = true)]
    json: bool,

    /// Override Agent Relay's configuration root.
    #[arg(long, global = true, value_name = "PATH")]
    config_root: Option<PathBuf>,

    /// Override Agent Relay's state root.
    #[arg(long, global = true, value_name = "PATH")]
    state_root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage explicit provider profiles.
    Profile(ProfileArgs),
}

#[derive(Debug, Args)]
struct ProfileArgs {
    #[command(subcommand)]
    command: ProfileCommand,
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Create and register a profile.
    Add {
        name: ProfileName,
        /// M1 intentionally executes only the fake provider.
        #[arg(long, value_enum, default_value_t = CliProvider::Fake)]
        provider: CliProvider,
        /// Optional managed directory; it must remain below the Relay profiles root.
        #[arg(long, value_name = "PATH")]
        config_dir: Option<PathBuf>,
    },
    /// List registered profiles without inspecting provider authentication.
    List,
    /// Inspect current provider status and identity match.
    Status { name: ProfileName },
    /// Unregister a profile while retaining its provider-owned directory.
    Remove { name: ProfileName },
    /// Run directory, authentication, and identity safety checks.
    Doctor { name: ProfileName },
    /// Inspect an existing Claude profile without changing it.
    InspectExisting {
        #[arg(long, value_enum)]
        provider: ExistingProvider,
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
        /// Permit a private directory outside Relay's managed profiles root.
        #[arg(long)]
        allow_external: bool,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Preview reference-only adoption of an existing Claude profile.
    Adopt {
        name: ProfileName,
        #[arg(long, value_enum)]
        provider: ExistingProvider,
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
        /// M1.5 supports dry-run only; no profile registry is changed.
        #[arg(long, required = true)]
        dry_run: bool,
        /// Permit a private directory outside Relay's managed profiles root.
        #[arg(long)]
        allow_external: bool,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliProvider {
    Fake,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ExistingProvider {
    Claude,
}

impl From<CliProvider> for ProviderKind {
    fn from(value: CliProvider) -> Self {
        match value {
            CliProvider::Fake => Self::Fake,
        }
    }
}

#[derive(Serialize)]
struct SuccessEnvelope<T> {
    schema_version: u32,
    ok: bool,
    command: &'static str,
    data: T,
}

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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(output) => {
            if cli.json {
                let serialized = serde_json::to_string_pretty(&output.json)
                    .expect("serializing known Relay output must succeed");
                println!("{serialized}");
            } else {
                println!("{}", output.human);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if cli.json {
                let output = json!({
                    "schema_version": OUTPUT_SCHEMA_VERSION,
                    "ok": false,
                    "error": {
                        "code": error.code(),
                        "message": error.to_string(),
                    }
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&output)
                        .expect("serializing known Relay error must succeed")
                );
            } else {
                eprintln!("error [{}]: {error}", error.code());
            }
            ExitCode::from(1)
        }
    }
}

struct CommandOutput {
    human: String,
    json: Value,
}

fn run(cli: &Cli) -> Result<CommandOutput, Error> {
    let discovered = RelayPaths::discover()?;
    let config_root = cli
        .config_root
        .clone()
        .unwrap_or_else(|| discovered.config_root().to_path_buf());
    let state_root = cli
        .state_root
        .clone()
        .unwrap_or_else(|| discovered.state_root().to_path_buf());
    let paths = RelayPaths::new(config_root, state_root)?;
    let service = ProfileService::new(paths.clone());
    let provider = FakeProvider::default();

    match &cli.command {
        Command::Profile(profile) => match &profile.command {
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
                    },
                    &provider,
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
            ProfileCommand::Status { name } => {
                let status = service.status(name, &provider)?;
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
            ProfileCommand::Doctor { name } => {
                let report = service.doctor(name, &provider)?;
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
            } => {
                let report = inspect_existing_claude(
                    &paths,
                    config_dir,
                    *allow_external,
                    claude_executable.as_deref(),
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
            } => {
                if !dry_run {
                    return Err(Error::ProviderUnsupported);
                }
                let report = inspect_existing_claude(
                    &paths,
                    config_dir,
                    *allow_external,
                    claude_executable.as_deref(),
                )?;
                let mut reasons = report.reasons.clone();
                let duplicate = service.list()?.iter().any(|profile| profile.name == *name);
                if duplicate {
                    reasons.push(format!("profile name '{name}' is already registered"));
                }
                let would_succeed = report.safe_to_adopt && !duplicate;
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
        },
    }
}

fn inspect_existing_claude(
    paths: &RelayPaths,
    requested_config_dir: &std::path::Path,
    allow_external: bool,
    requested_executable: Option<&std::path::Path>,
) -> Result<ClaudeInspectionReport, Error> {
    let config_dir = paths.validate_adoption_path(requested_config_dir, allow_external)?;
    let environment = inspect_environment(&config_dir);
    let inspector = ClaudeInspector::discover(requested_executable)?;
    inspector.inspect(&config_dir, environment)
}

fn inspection_human(report: &ClaudeInspectionReport) -> String {
    let identity = report
        .identity_pin
        .as_ref()
        .map(identity_summary)
        .unwrap_or_else(|| "unavailable".to_owned());
    let conflicts = report
        .environment_override_status
        .conflicting_names()
        .join(", ");
    format!(
        "Claude profile: {}\nVersion: {}\nAuthenticated: {}\nIdentity: {}\nEnvironment overrides: {}\nSafe to adopt: {}",
        report.config_dir.display(),
        report.claude_version,
        if report.authenticated { "yes" } else { "no" },
        identity,
        if conflicts.is_empty() {
            "none".to_owned()
        } else {
            format!("conflict ({conflicts})")
        },
        if report.safe_to_adopt { "yes" } else { "no" }
    )
}

fn identity_summary(identity: &ClaudeIdentityPin) -> String {
    if let Some(account_id) = &identity.account_id {
        format!("account_id={account_id}")
    } else if let Some(email) = &identity.email {
        match &identity.organization_id {
            Some(organization) => format!("email={email}, organization_id={organization}"),
            None => format!("email={email}"),
        }
    } else {
        "unavailable".to_owned()
    }
}

fn success<T: Serialize>(
    command: &'static str,
    human: String,
    data: T,
) -> Result<CommandOutput, Error> {
    let envelope = SuccessEnvelope {
        schema_version: OUTPUT_SCHEMA_VERSION,
        ok: true,
        command,
        data,
    };
    let json = serde_json::to_value(envelope).map_err(|_| Error::SerializationFailed)?;
    Ok(CommandOutput { human, json })
}
