use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
use relay_core::{
    AddProfileRequest, Error, ProfileName, ProfileService, ProfileSetupMode, ProviderKind,
    RelayPaths,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliProvider {
    Fake,
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
    let service = ProfileService::new(paths);
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
        },
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
