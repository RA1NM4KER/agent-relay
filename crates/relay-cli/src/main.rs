use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use relay_core::{
    AddProfileRequest, Error, IdentityMetadata, Profile, ProfileName, ProfileService,
    ProfileSetupMode, Provider, ProviderKind, RelayPaths,
    handoff::{
        HandoffCoordinator, HandoffRequest, JournalStore, LeaseStore, OrchestrationLock, ProjectId,
        SourceLiveness as _,
    },
};
use relay_provider_claude::{
    ClaudeAdoptionProvider, ClaudeIdentityPin, ClaudeInspectionReport, ClaudeInspector,
    ClaudeSessionStager, ClaudeSessionStopper, ClaudeSourceLiveness, ClaudeTargetLauncher,
    EnvironmentOverrideStatus, SystemProcessLister, inspect_environment, stage_transfer,
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
    /// M2A: minimal, explicit, manual cross-profile Claude session staging.
    Session(SessionArgs),
    /// M2B: project-level writer lease and orchestration lock inspection.
    Lock(LockArgs),
    /// M2B: crash-safe transactional handoff between two registered profiles.
    Handoff(HandoffArgs),
    /// M2B: decide the safe next action for an interrupted handoff transaction.
    Recover {
        transaction_id: String,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
    },
    /// M2B.5: launch Claude as a Relay-managed writer, recording a durable writer lease tied to
    /// its real pid and start-time fingerprint (not a `ps` text scan). Refuses if another
    /// verified-live writer already holds this project.
    Launch {
        #[arg(long)]
        profile: ProfileName,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        prompt: String,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct LockArgs {
    #[command(subcommand)]
    command: LockCommand,
}

#[derive(Debug, Subcommand)]
enum LockCommand {
    /// Report whether a project's orchestration lock is currently held and who its writer
    /// lease says owns it. Advisory only: there is an inherent check-then-report race.
    Status {
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
struct HandoffArgs {
    #[command(subcommand)]
    command: HandoffCommand,
}

#[derive(Debug, Subcommand)]
enum HandoffCommand {
    /// Run one complete transactional handoff: verifies the source is stopped, stages the
    /// session (M2A guarantees apply), launches and verifies the target, then moves the
    /// writer lease. Fails closed at every step; a `relay handoff status` and durable journal
    /// remain even when this command exits non-zero.
    Run {
        #[arg(long = "from")]
        source_profile: ProfileName,
        #[arg(long = "to")]
        target_profile: ProfileName,
        #[arg(long = "project", value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long = "session")]
        session_id: String,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Show a transaction's durable journal.
    Status {
        transaction_id: String,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Stage a stopped Claude session's transcript from one registered profile to another so
    /// the target profile can `claude --resume <session-id>` it. Never touches credentials;
    /// refuses if the source profile still has a live Claude process, if no matching session
    /// exists, or if the target already holds a divergent artifact.
    StageTransfer {
        #[arg(long)]
        source_profile: ProfileName,
        #[arg(long)]
        target_profile: ProfileName,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long)]
        session_id: String,
    },
    /// M2B.5: classify and safely resolve a target profile's session-transcript conflict.
    Conflict(ConflictArgs),
}

#[derive(Debug, Args)]
struct ConflictArgs {
    #[command(subcommand)]
    command: ConflictCommand,
}

#[derive(Debug, Subcommand)]
enum ConflictCommand {
    /// Read-only: classify the target's transcript relative to the source (missing, identical,
    /// a known-stale ancestor, divergent/contains unique turns, or currently active).
    Inspect {
        #[arg(long)]
        source_profile: ProfileName,
        #[arg(long)]
        target_profile: ProfileName,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long)]
        session_id: String,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Resolve the conflict. Without `--yes`, always previews (equivalent to `--dry-run`) and
    /// writes nothing. A stale-ancestor target requires `--yes`; a genuinely divergent target
    /// additionally requires `--force-discard-divergent`. Every actual replacement backs up the
    /// displaced file first and journals the resolution.
    Resolve {
        #[arg(long)]
        source_profile: ProfileName,
        #[arg(long)]
        target_profile: ProfileName,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long)]
        session_id: String,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        force_discard_divergent: bool,
    },
    /// Restore the most recent backup for a session back onto the target profile.
    Rollback {
        #[arg(long)]
        target_profile: ProfileName,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long)]
        session_id: String,
    },
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
    Status {
        name: ProfileName,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Unregister a profile while retaining its provider-owned directory.
    Remove { name: ProfileName },
    /// Run directory, authentication, and identity safety checks.
    Doctor {
        name: ProfileName,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
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
    /// Adopt an existing Claude profile by reference, or preview the adoption.
    Adopt {
        name: ProfileName,
        #[arg(long, value_enum)]
        provider: ExistingProvider,
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
        /// Preview only: report what adoption would do without changing Relay's registry.
        /// Without this flag, adoption is performed and the registry is written.
        #[arg(long)]
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
            ProfileCommand::Status {
                name,
                claude_executable,
            } => {
                let provider = provider_for_profile(&service, name, claude_executable.as_deref())?;
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
            } => {
                let provider = provider_for_profile(&service, name, claude_executable.as_deref())?;
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
            } if !dry_run => {
                let report = inspect_existing_claude(
                    &paths,
                    config_dir,
                    *allow_external,
                    claude_executable.as_deref(),
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
                let claude_provider =
                    ClaudeAdoptionProvider::discover(claude_executable.as_deref())?;
                let profile = service.add(
                    AddProfileRequest {
                        name: name.clone(),
                        provider: ProviderKind::Claude,
                        config_dir: Some(report.config_dir.clone()),
                        mode: ProfileSetupMode::AdoptExisting,
                        expected_identity: Some(expected_identity),
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
            } => {
                let report = inspect_existing_claude(
                    &paths,
                    config_dir,
                    *allow_external,
                    claude_executable.as_deref(),
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
        },
        Command::Session(session) => match &session.command {
            SessionCommand::StageTransfer {
                source_profile,
                target_profile,
                project_dir,
                session_id,
            } => {
                let registered = service.list()?;
                let source = registered
                    .iter()
                    .find(|profile| &profile.name == source_profile)
                    .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
                let target = registered
                    .iter()
                    .find(|profile| &profile.name == target_profile)
                    .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                if source.name == target.name {
                    return Err(Error::ProviderMismatch {
                        expected: "distinct source and target profiles".to_owned(),
                        observed: source.name.to_string(),
                    });
                }
                let report = stage_transfer(
                    &SystemProcessLister,
                    &source.config_dir,
                    &target.config_dir,
                    project_dir,
                    session_id,
                )?;
                let human = format!(
                    "Staged session {} from '{}' to '{}'\nProject key: {}\nArtifacts: {}",
                    report.session_id,
                    source_profile,
                    target_profile,
                    report.project_key,
                    report
                        .artifacts
                        .iter()
                        .map(|artifact| format!(
                            "{} (sha256={}, {} bytes{})",
                            artifact.relative_path,
                            artifact.sha256,
                            artifact.size_bytes,
                            if artifact.already_present_and_identical {
                                ", already staged"
                            } else {
                                ""
                            }
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                success("session.stage_transfer", human, report)
            }
            SessionCommand::Conflict(conflict) => match &conflict.command {
                ConflictCommand::Inspect {
                    source_profile,
                    target_profile,
                    project_dir,
                    session_id,
                    claude_executable,
                } => {
                    let registered = service.list()?;
                    let source = registered
                        .iter()
                        .find(|profile| &profile.name == source_profile)
                        .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
                    let target = registered
                        .iter()
                        .find(|profile| &profile.name == target_profile)
                        .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                    let target_active = target_is_active(
                        &paths,
                        target,
                        project_dir,
                        session_id,
                        claude_executable.as_deref(),
                    )?;
                    let report = relay_provider_claude::inspect_conflict(
                        &source.config_dir,
                        &target.config_dir,
                        project_dir,
                        session_id,
                        target_active,
                    )?;
                    let human = format!(
                        "Session {session_id}: target ({target_profile}) is {:?}",
                        report.classification
                    );
                    success("session.conflict.inspect", human, report)
                }
                ConflictCommand::Resolve {
                    source_profile,
                    target_profile,
                    project_dir,
                    session_id,
                    claude_executable,
                    dry_run,
                    yes,
                    force_discard_divergent,
                } => {
                    let registered = service.list()?;
                    let source = registered
                        .iter()
                        .find(|profile| &profile.name == source_profile)
                        .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
                    let target = registered
                        .iter()
                        .find(|profile| &profile.name == target_profile)
                        .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                    let target_active = target_is_active(
                        &paths,
                        target,
                        project_dir,
                        session_id,
                        claude_executable.as_deref(),
                    )?;
                    let decision = if *dry_run {
                        relay_provider_claude::ResolveDecision::Preview
                    } else if *force_discard_divergent {
                        relay_provider_claude::ResolveDecision::ForceDiscardDivergent
                    } else if *yes {
                        relay_provider_claude::ResolveDecision::Confirm
                    } else {
                        relay_provider_claude::ResolveDecision::Preview
                    };
                    let resolution = relay_provider_claude::resolve_conflict(
                        &source.config_dir,
                        &target.config_dir,
                        project_dir,
                        session_id,
                        target_active,
                        decision,
                    )?;
                    let human = format!(
                        "Session {session_id}: {} (dry_run={}){}",
                        resolution.action,
                        resolution.dry_run,
                        resolution
                            .backup_path
                            .as_ref()
                            .map(|path| format!("\nBackup: {}", path.display()))
                            .unwrap_or_default()
                    );
                    success("session.conflict.resolve", human, resolution)
                }
                ConflictCommand::Rollback {
                    target_profile,
                    project_dir,
                    session_id,
                } => {
                    let registered = service.list()?;
                    let target = registered
                        .iter()
                        .find(|profile| &profile.name == target_profile)
                        .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                    let restored_path = relay_provider_claude::rollback_conflict(
                        &target.config_dir,
                        project_dir,
                        session_id,
                    )?;
                    success(
                        "session.conflict.rollback",
                        format!("Restored backup to {}", restored_path.display()),
                        json!({ "restored_path": restored_path }),
                    )
                }
            },
        },
        Command::Lock(lock) => match &lock.command {
            LockCommand::Status { project_dir } => {
                let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                    path: project_dir.clone(),
                    source,
                })?;
                let project_id = ProjectId::for_canonical_path(&canonical)?;
                let project_state_dir = paths.project_state_dir(&project_id);
                let held = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"))
                    .is_currently_held();
                let lease = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?;
                let current_transaction =
                    std::fs::read_to_string(project_state_dir.join("current_transaction.json"))
                        .ok();
                let human = format!(
                    "Project: {}\nLock held: {}\nCurrent owner: {}\nMost recent transaction: {}",
                    canonical.display(),
                    held,
                    lease
                        .as_ref()
                        .map(|lease| lease.owner_profile.to_string())
                        .unwrap_or_else(|| "none yet".to_owned()),
                    current_transaction.as_deref().unwrap_or("none")
                );
                success(
                    "lock.status",
                    human,
                    json!({
                        "project_id": project_id.as_str(),
                        "locked": held,
                        "lease": lease,
                        "current_transaction": current_transaction,
                    }),
                )
            }
        },
        Command::Handoff(handoff) => match &handoff.command {
            HandoffCommand::Run {
                source_profile,
                target_profile,
                project_dir,
                session_id,
                claude_executable,
            } => {
                let registered = service.list()?;
                let source = registered
                    .iter()
                    .find(|profile| &profile.name == source_profile)
                    .ok_or_else(|| Error::ProfileNotFound(source_profile.to_string()))?;
                let target = registered
                    .iter()
                    .find(|profile| &profile.name == target_profile)
                    .ok_or_else(|| Error::ProfileNotFound(target_profile.to_string()))?;
                let liveness = ClaudeSourceLiveness::new(claude_executable.clone());
                let stopper = ClaudeSessionStopper::new(claude_executable.clone());
                let stager = ClaudeSessionStager;
                let launcher = ClaudeTargetLauncher::new(claude_executable.clone());
                let coordinator = HandoffCoordinator {
                    paths: &paths,
                    liveness: &liveness,
                    stopper: &stopper,
                    stager: &stager,
                    launcher: &launcher,
                };
                let journal = coordinator.run(HandoffRequest {
                    project_dir: project_dir.clone(),
                    source_profile: source.name.clone(),
                    source_config_dir: source.config_dir.clone(),
                    target_profile: target.name.clone(),
                    target_config_dir: target.config_dir.clone(),
                    session_id: session_id.clone(),
                })?;
                let human = format!(
                    "Handoff {} ({} -> {}): {:?}\nSession: {}\nTransaction: {}",
                    journal.transaction_id,
                    source_profile,
                    target_profile,
                    journal.state,
                    journal.session_id,
                    journal.transaction_id
                );
                success("handoff.run", human, journal)
            }
            HandoffCommand::Status {
                transaction_id,
                project_dir,
            } => {
                let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                    path: project_dir.clone(),
                    source,
                })?;
                let project_id = ProjectId::for_canonical_path(&canonical)?;
                let project_state_dir = paths.project_state_dir(&project_id);
                let parsed = relay_core::handoff::TransactionId::parse(transaction_id)?;
                let journal_store = JournalStore::at_path(
                    project_state_dir
                        .join("handoffs")
                        .join(format!("{parsed}.json")),
                );
                let journal = journal_store.load()?;
                let human = format!(
                    "Transaction {}: {:?}\nRevision: {}",
                    journal.transaction_id, journal.state, journal.revision
                );
                success("handoff.status", human, journal)
            }
        },
        Command::Recover {
            transaction_id,
            project_dir,
        } => {
            let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                path: project_dir.clone(),
                source,
            })?;
            let project_id = ProjectId::for_canonical_path(&canonical)?;
            let project_state_dir = paths.project_state_dir(&project_id);
            let parsed = relay_core::handoff::TransactionId::parse(transaction_id)?;
            let liveness = ClaudeSourceLiveness::new(None);
            let stopper = ClaudeSessionStopper::new(None);
            let stager = ClaudeSessionStager;
            let launcher = ClaudeTargetLauncher::new(None);
            let coordinator = HandoffCoordinator {
                paths: &paths,
                liveness: &liveness,
                stopper: &stopper,
                stager: &stager,
                launcher: &launcher,
            };
            let journal = coordinator.recover(&project_state_dir, &parsed)?;
            let human = format!(
                "Recovery decision for {}: {:?}",
                journal.transaction_id, journal.state
            );
            success("recover", human, journal)
        }
        Command::Launch {
            profile,
            project_dir,
            prompt,
            claude_executable,
        } => {
            let registered = service.list()?;
            let target = registered
                .iter()
                .find(|candidate| &candidate.name == profile)
                .ok_or_else(|| Error::ProfileNotFound(profile.to_string()))?;
            let canonical_project =
                std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                    path: project_dir.clone(),
                    source,
                })?;
            let project_id = ProjectId::for_canonical_path(&canonical_project)?;
            let project_state_dir = paths.project_state_dir(&project_id);
            std::fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
                path: project_state_dir.clone(),
                source,
            })?;
            let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
            let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));

            let lease = lock.try_with(|| -> Result<relay_core::handoff::WriterLease, Error> {
                if let Some(existing) = lease_store.load()? {
                    let owner_config_dir = registered
                        .iter()
                        .find(|candidate| candidate.name == existing.owner_profile)
                        .map(|candidate| candidate.config_dir.clone());
                    let still_active = match &owner_config_dir {
                        Some(config_dir) => {
                            let liveness = ClaudeSourceLiveness::new(claude_executable.clone());
                            liveness
                                .check(
                                    config_dir,
                                    &canonical_project,
                                    &existing.session_id,
                                    Some(&existing.owner_process),
                                )?
                                .active
                        }
                        // Owning profile is no longer registered at all: cannot verify safely.
                        None => true,
                    };
                    if still_active {
                        return Err(Error::WriterAlreadyActive(
                            existing.owner_profile.to_string(),
                        ));
                    }
                }

                let launched = relay_provider_claude::launch_background(
                    &target.config_dir,
                    &canonical_project,
                    prompt,
                    claude_executable.as_deref(),
                )?;
                let owner_process = launched
                    .pid
                    .map(relay_core::handoff::ProcessIdentity::query)
                    .unwrap_or(relay_core::handoff::ProcessIdentity {
                        pid: 0,
                        start_time_fingerprint: None,
                    });
                let lease = relay_core::handoff::WriterLease::new(
                    project_id.clone(),
                    target.name.clone(),
                    owner_process,
                    launched.session_id.clone(),
                    relay_core::handoff::TransactionId::generate(),
                    current_unix_ms(),
                )
                .with_provider_handle(Some(launched.provider_handle.clone()));
                lease_store.save(&lease)?;
                Ok(lease)
            })?;

            let human = format!(
                "Launched '{}' as writer for {}\nSession: {}\nPid: {}\nBackground job: {}",
                profile,
                canonical_project.display(),
                lease.session_id,
                lease.owner_process.pid,
                lease.provider_handle.clone().unwrap_or_default()
            );
            success("launch", human, lease)
        }
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Checks whether a session is currently active for `target` using the same M2B.5 liveness
/// mechanism the handoff coordinator uses (pid + fingerprint, corroborated by `claude agents
/// --json`), so `session conflict` commands never touch a genuinely in-use target.
fn target_is_active(
    paths: &RelayPaths,
    target: &Profile,
    project_dir: &Path,
    session_id: &str,
    claude_executable: Option<&Path>,
) -> Result<bool, Error> {
    let canonical_project = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.to_path_buf(),
        source,
    })?;
    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let lease =
        LeaseStore::at_path(paths.project_state_dir(&project_id).join("lease.json")).load()?;
    let recorded_owner = lease
        .filter(|lease| lease.owner_profile == target.name)
        .map(|lease| lease.owner_process);
    let liveness = ClaudeSourceLiveness::new(claude_executable.map(Path::to_path_buf));
    let verdict = liveness.check(
        &target.config_dir,
        &canonical_project,
        session_id,
        recorded_owner.as_ref(),
    )?;
    Ok(verdict.active)
}

/// `status`/`doctor` must inspect through the profile's own provider, not always the fake one:
/// a real Claude profile that has been adopted needs a real Claude inspection, not a fake marker.
fn provider_for_profile(
    service: &ProfileService,
    name: &ProfileName,
    claude_executable: Option<&std::path::Path>,
) -> Result<Box<dyn Provider>, Error> {
    let kind = service
        .list()?
        .into_iter()
        .find(|profile| &profile.name == name)
        .map(|profile| profile.provider);
    Ok(match kind {
        Some(ProviderKind::Claude) => {
            Box::new(ClaudeAdoptionProvider::discover(claude_executable)?)
        }
        _ => Box::new(FakeProvider::default()),
    })
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
