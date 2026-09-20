mod preferences;
mod providers;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use relay_core::{
    AddProfileRequest, AuthenticationState, Error, IdentityMetadata, Profile, ProfileDirectory,
    ProfileName, ProfileService, ProfileSetupMode, Provider, ProviderKind, RelayPaths,
    automation::{
        AutomationPolicy, LedgerStore, ProfileCandidate, WatchCoordinator, WatchOutcome,
        WatchRequest,
    },
    handoff::{
        HandoffCoordinator, HandoffRequest, JournalStore, LeaseStore, OrchestrationLock, ProjectId,
        SourceLiveness as _,
    },
    usage::{UsageSignal, UsageState},
};
use relay_herdr::herdr_client::HerdrCliClient;
use relay_herdr::install as herdr_install;
use relay_provider_claude::{
    AUTHENTICATION_OVERRIDE_VARIABLES, CapabilityStatus, ClaudeAdoptionProvider, ClaudeIdentityPin,
    ClaudeInspectionReport, ClaudeInspector, ClaudeSessionStager, ClaudeSessionStopper,
    ClaudeSourceLiveness, ClaudeTargetLauncher, EnvironmentOverrideStatus, SimulatedUsageSignal,
    SystemProcessLister, apply_install, apply_uninstall, assess_installed, handle_statusline,
    handle_stop_failure, inspect_environment, integration_status, plan_install, plan_uninstall,
    query_active_sessions, read_stdin_bounded, stage_transfer,
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
        /// Instead of deciding a safe next action, record that you have confirmed no target
        /// process is still running for a `RECOVERY_REQUIRED` transaction, moving it to `FAILED`
        /// so the project is no longer blocked from starting a new handoff.
        #[arg(long)]
        acknowledge: bool,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
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
    /// M2C: explicit, opt-in, usage-triggered automatic handoff. Nothing here runs unless this
    /// command is invoked; there is no background monitoring.
    Watch(WatchArgs),
    /// M2C.1: opt-in Claude usage integration (StopFailure hook + statusline snapshot) for one
    /// isolated profile.
    Integration(IntegrationArgs),
    /// Internal: commands Claude Code runs on behalf of an installed integration. Never fails the
    /// calling Claude session.
    #[command(hide = true)]
    Hook(HookArgs),
    /// M4: interactive first-run wizard — authenticate/adopt Claude profiles, choose a primary
    /// and fallback order, and optionally enable the usage and Herdr integrations. Safe to
    /// re-run any time; detects and reuses what already exists rather than starting over.
    Setup(SetupArgs),
    /// M4: the normal daily entry point. `cd` into a project and run `relay claude` — no
    /// `--profile`/`--project-dir`/`--session` required once `relay setup` has run once.
    Claude(ClaudeArgs),
    /// M4: a short, human-readable summary of the current project's Relay/Claude/Herdr state.
    Status {
        #[arg(long = "project", value_name = "PATH")]
        project_dir: Option<PathBuf>,
    },
    /// M4: a friendly list of registered profiles with their primary/fallback role and
    /// authentication state (unlike `relay profile list`, which is provider-neutral and does not
    /// show M4 preferences).
    Profiles,
    /// M4/M6: friendly wrapper around the official `claude auth login`/`codex login` flow for one
    /// isolated profile's config directory — the same flow `relay setup` uses for a new profile.
    /// Dispatches by the profile's already-registered provider; `--provider` picks the provider
    /// for a brand-new profile name (defaults to `claude`, preserving pre-M6 behavior).
    Login {
        name: ProfileName,
        #[arg(long, value_enum, default_value_t = ProviderArg::Claude)]
        provider: ProviderArg,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        codex_executable: Option<PathBuf>,
    },
    /// M4/M6: friendly wrapper around the official `claude auth logout`/`codex logout` flow for
    /// one isolated profile's config directory. Never touches credential files directly.
    Logout {
        name: ProfileName,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        codex_executable: Option<PathBuf>,
    },
    /// M6: explicit manual handoff to a different registered profile, same provider or not.
    /// Uses SESSION_CONTINUATION when the current writer and the target are both Claude (the
    /// only pairing with proven cross-profile session transfer), STATE_CONTINUATION otherwise.
    Switch(SwitchArgs),
    /// The normal way to continue an already-active Relay-managed session: reattaches
    /// interactively to the session/thread the current writer lease already records, under the
    /// *lease owner's* own isolated config — Codex's NATIVE_RESUME, or (for a Claude profile) an
    /// interactive `claude --resume`. `relay resume` (no profile) resolves the owner
    /// automatically; an explicit `relay resume <profile>` still works but refuses if that
    /// profile does not already own this project's writer lease (use `relay switch` to move
    /// ownership first).
    Resume(ResumeArgs),
}

#[derive(Debug, Args)]
struct SwitchArgs {
    target: ProfileName,
    #[arg(long = "project-dir", value_name = "PATH")]
    project_dir: Option<PathBuf>,
    /// Stop after the transaction completes; print a status summary instead of exec'ing an
    /// interactive continuation.
    #[arg(long)]
    no_attach: bool,
    #[arg(long, value_name = "PATH")]
    claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    codex_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ResumeArgs {
    /// Advanced form: resume only if this exact profile already owns the project's writer lease.
    /// Normally omitted — the owning profile is resolved automatically from the lease.
    profile: Option<ProfileName>,
    #[arg(long = "project-dir", value_name = "PATH")]
    project_dir: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    codex_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SetupArgs {
    /// Skip all prompts; requires the flags below instead. Fails closed (a clear error) rather
    /// than guessing if a value this needs is missing.
    #[arg(long)]
    non_interactive: bool,
    /// Show the same technical detail `relay doctor`-style output would (config dirs, identity
    /// pins' stable ids, capability status) instead of the plain-language summary.
    #[arg(long)]
    verbose: bool,
    /// Non-interactive only: the primary profile name (must already be registered, or created
    /// via a separate `relay login`/adoption step first).
    #[arg(long)]
    primary: Option<ProfileName>,
    /// Non-interactive only: fallback profiles in priority order.
    #[arg(long)]
    fallback: Vec<ProfileName>,
    /// Non-interactive only: enable/disable the Claude usage integration for the selected
    /// profiles without prompting.
    #[arg(long)]
    usage_integration: Option<bool>,
    /// Non-interactive only: enable/disable the Herdr integration without prompting.
    #[arg(long)]
    herdr: Option<bool>,
    #[arg(long, value_name = "PATH")]
    claude_executable: Option<PathBuf>,
    /// Explicit Codex executable, primarily for controlled validation.
    #[arg(long, value_name = "PATH")]
    codex_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ClaudeArgs {
    /// The first message for the new session. `relay claude` always starts a fresh Relay-managed
    /// conversation — see `relay resume` to continue an existing one instead.
    message: Vec<String>,
    /// Override the configured primary profile for this run only.
    #[arg(long)]
    profile: Option<ProfileName>,
    /// Override the configured fallback order for this run only.
    #[arg(long)]
    fallback: Vec<ProfileName>,
    #[arg(long = "project-dir", value_name = "PATH")]
    project_dir: Option<PathBuf>,
    /// Stop after creating/confirming the writer lease and (if applicable) Herdr metadata;
    /// print a status summary instead of attaching interactively. Used by scripts/tests and by
    /// environments with no real TTY to attach to.
    #[arg(long)]
    no_attach: bool,
    /// Explicitly replace an already-active Relay-managed session for this project: safely stop
    /// it (the same authoritative stop-and-verify machinery `relay switch`/recovery use), confirm
    /// it is gone, then start a fresh managed conversation. Without this flag, `relay claude`
    /// never silently replaces or reattaches to an active session — it fails closed instead (use
    /// `relay resume` to continue it). Has no effect if there is no active session; behaves like
    /// a plain `relay claude` in that case.
    #[arg(long)]
    new: bool,
    #[arg(long, value_name = "PATH")]
    claude_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct IntegrationArgs {
    #[command(subcommand)]
    command: IntegrationCommand,
}

#[derive(Debug, Subcommand)]
enum IntegrationCommand {
    /// Claude Code usage integration.
    Claude(ClaudeIntegrationArgs),
    /// Herdr plugin integration (M3).
    Herdr(HerdrIntegrationArgs),
}

#[derive(Debug, Args)]
struct HerdrIntegrationArgs {
    #[command(subcommand)]
    command: HerdrIntegrationCommand,
}

#[derive(Debug, Subcommand)]
enum HerdrIntegrationCommand {
    /// Link `plugins/herdr` into the local Herdr server (`herdr plugin link`). Run from the
    /// `agent-relay` repository root, or pass `--plugin-path`.
    Install {
        #[arg(long, value_name = "PATH")]
        plugin_path: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_name = "PATH")]
        herdr_executable: Option<PathBuf>,
    },
    /// Show whether the plugin is registered, enabled, and Herdr's own compatibility.
    Status {
        #[arg(long, value_name = "PATH")]
        herdr_executable: Option<PathBuf>,
    },
    /// Validate the whole Herdr -> Relay chain.
    Doctor {
        #[arg(long, value_name = "PATH")]
        herdr_executable: Option<PathBuf>,
    },
    /// Unlink the plugin (`herdr plugin unlink agent-relay`). Idempotent; never touches any other
    /// plugin or Herdr configuration.
    Uninstall {
        #[arg(long, value_name = "PATH")]
        herdr_executable: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct ClaudeIntegrationArgs {
    #[command(subcommand)]
    command: ClaudeIntegrationCommand,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct IntegrationTarget {
    /// A registered Relay profile whose isolated Claude config directory is modified.
    #[arg(long)]
    profile: Option<ProfileName>,
    /// An explicit Claude config directory (for example `~/.claude`, only when you mean it).
    #[arg(long, value_name = "PATH")]
    config_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum ClaudeIntegrationCommand {
    /// Add a StopFailure(rate_limit) hook and a rate_limits-recording statusLine to one profile's
    /// settings.json. Existing hooks are kept and an existing statusLine is chained, never
    /// replaced. The original settings.json is backed up. Use `--dry-run` to preview.
    Install {
        #[command(flatten)]
        target: IntegrationTarget,
        #[arg(long)]
        dry_run: bool,
        /// Accept a Claude Code version newer than any Relay has validated.
        #[arg(long)]
        allow_unverified_version: bool,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Show whether the integration is installed and which signals it has recorded.
    Status {
        #[command(flatten)]
        target: IntegrationTarget,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Remove the integration and restore the profile's settings (byte for byte when unchanged
    /// since install). The settings backup is kept.
    Uninstall {
        #[command(flatten)]
        target: IntegrationTarget,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
struct HookArgs {
    #[command(subcommand)]
    command: HookCommand,
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    Claude(ClaudeHookArgs),
}

#[derive(Debug, Args)]
struct ClaudeHookArgs {
    #[command(subcommand)]
    command: ClaudeHookCommand,
}

#[derive(Debug, Subcommand)]
enum ClaudeHookCommand {
    StopFailure {
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
    },
    Statusline {
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
        #[arg(long)]
        chain: Option<String>,
    },
}

#[derive(Debug, Args)]
struct WatchArgs {
    #[command(subcommand)]
    command: WatchCommand,
}

#[derive(Debug, Subcommand)]
enum WatchCommand {
    /// Evaluate the current writer's usage state once and, only if it is EXHAUSTED, hand off to
    /// the first healthy, non-exhausted, distinct-identity profile in `--fallback` order. Reuses
    /// the same M2B transactional `relay handoff run` machinery unchanged. Safe to invoke
    /// repeatedly on a timer (cron, a shell loop) — it is not itself a background daemon.
    Run {
        /// The profile currently expected to hold the writer lease.
        #[arg(long)]
        profile: ProfileName,
        /// Fallback profiles in priority order; the first eligible one is selected.
        #[arg(long, required = true)]
        fallback: Vec<ProfileName>,
        #[arg(long = "project", value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long = "session")]
        session_id: String,
        /// Report what would happen without performing the handoff or persisting ledger state.
        #[arg(long)]
        dry_run: bool,
        /// Explicit diagnostic fallback: allow a real `claude -p` request (SPENDS REAL API USAGE)
        /// when no free structured signal (StopFailure/statusline/rate_limit_event) is
        /// conclusive. Never needed for normal automatic handoff and never run against a profile
        /// already recorded exhausted.
        #[arg(long)]
        probe: bool,
        /// The model the watched workload runs (e.g. `opus`, `claude-sonnet-5`). A model-scoped
        /// limit (Opus/Sonnet/Fable) only counts as exhaustion when it matches this; without it,
        /// such limits never trigger a handoff.
        #[arg(long)]
        workload_model: Option<String>,
        /// Fault injection: force the primary profile's usage state instead of detecting it, so
        /// the real handoff machinery can be validated without waiting for or burning real quota.
        #[arg(long, value_enum)]
        simulate_usage: Option<SimulateUsageArg>,
        /// Paired with `--simulate-usage`: an optional simulated reset time.
        #[arg(long)]
        simulate_reset_unix_ms: Option<u64>,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
    },
    /// Read-only: the project's automation ledger (recent automatic handoffs, profiles ever
    /// observed exhausted) plus its current writer lease.
    Status {
        #[arg(long = "project", value_name = "PATH")]
        project_dir: PathBuf,
    },
    /// Explicitly clear a project's automation ledger (cooldown, handoff counters, and the
    /// known-exhausted list). Automatic fail-back never happens on its own; this is the operator
    /// action that un-blocks a profile the ledger has marked exhausted.
    Clear {
        #[arg(long = "project", value_name = "PATH")]
        project_dir: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SimulateUsageArg {
    Available,
    NearLimit,
    Exhausted,
    ResetPending,
    Unknown,
}

impl From<SimulateUsageArg> for UsageState {
    fn from(value: SimulateUsageArg) -> Self {
        match value {
            SimulateUsageArg::Available => Self::Available,
            SimulateUsageArg::NearLimit => Self::NearLimit,
            SimulateUsageArg::Exhausted => Self::Exhausted,
            SimulateUsageArg::ResetPending => Self::ResetPending,
            SimulateUsageArg::Unknown => Self::Unknown,
        }
    }
}

/// M6: which provider to create a brand-new profile as. Defaults to `claude` everywhere it
/// appears, so a pre-M6 invocation with no `--provider` flag behaves exactly as before.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ProviderArg {
    Claude,
    Codex,
}

impl From<ProviderArg> for ProviderKind {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Claude => Self::Claude,
            ProviderArg::Codex => Self::Codex,
        }
    }
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
    /// Restore the most recent backup for a session back onto the target profile. Refuses,
    /// exactly like `resolve`, if the target session is currently active.
    Rollback {
        #[arg(long)]
        target_profile: ProfileName,
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long)]
        session_id: String,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
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
        /// Explicit Codex executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        codex_executable: Option<PathBuf>,
    },
    /// Unregister a profile while retaining its provider-owned directory.
    Remove { name: ProfileName },
    /// Run directory, authentication, and identity safety checks.
    Doctor {
        name: ProfileName,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        /// Explicit Codex executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        codex_executable: Option<PathBuf>,
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
    if let Command::Hook(hook) = &cli.command {
        return run_hook(hook);
    }
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

/// Runs inside a live Claude Code session: never prints errors, never fails the session.
fn run_hook(hook: &HookArgs) -> ExitCode {
    let HookCommand::Claude(claude) = &hook.command;
    let stdin = read_stdin_bounded(std::io::stdin());
    let now = current_unix_ms();
    match &claude.command {
        ClaudeHookCommand::StopFailure { config_dir } => {
            handle_stop_failure(config_dir, &stdin, now);
            ExitCode::SUCCESS
        }
        ClaudeHookCommand::Statusline { config_dir, chain } => {
            let code = handle_statusline(config_dir, &stdin, now, chain.as_deref());
            ExitCode::from(u8::try_from(code).unwrap_or(0))
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
                codex_executable,
            } => {
                let provider = provider_for_profile(
                    &service,
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
                    &service,
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
                    claude_executable,
                } => {
                    let registered = service.list()?;
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
                    let restored_path = relay_provider_claude::rollback_conflict(
                        &target.config_dir,
                        project_dir,
                        session_id,
                        target_active,
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
                    source_stopper: &stopper,
                    target_stopper: &stopper,
                    stager: Some(&stager),
                    context_capturer: None,
                    launcher: &launcher,
                };
                // `relay handoff run` is the M2B low-level debugging entry point and predates
                // multi-provider profiles; it stays Claude-only (SESSION_CONTINUATION), exactly
                // as before M6. `relay switch` is the provider-aware M6 entry point.
                let journal = coordinator.run(HandoffRequest {
                    project_dir: project_dir.clone(),
                    source_profile: source.name.clone(),
                    source_provider: relay_core::ProviderKind::Claude,
                    source_config_dir: source.config_dir.clone(),
                    target_profile: target.name.clone(),
                    target_provider: relay_core::ProviderKind::Claude,
                    target_config_dir: target.config_dir.clone(),
                    session_id: session_id.clone(),
                    continuity_type: relay_core::handoff::ContinuityType::SessionContinuation,
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
            acknowledge,
            claude_executable,
        } => {
            let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                path: project_dir.clone(),
                source,
            })?;
            let project_id = ProjectId::for_canonical_path(&canonical)?;
            let project_state_dir = paths.project_state_dir(&project_id);
            let parsed = relay_core::handoff::TransactionId::parse(transaction_id)?;
            let liveness = ClaudeSourceLiveness::new(claude_executable.clone());
            let stopper = ClaudeSessionStopper::new(claude_executable.clone());
            let stager = ClaudeSessionStager;
            let launcher = ClaudeTargetLauncher::new(claude_executable.clone());
            let coordinator = HandoffCoordinator {
                paths: &paths,
                liveness: &liveness,
                source_stopper: &stopper,
                target_stopper: &stopper,
                stager: Some(&stager),
                context_capturer: None,
                launcher: &launcher,
            };
            let journal = if *acknowledge {
                coordinator.acknowledge_recovery(&project_state_dir, &parsed)?
            } else {
                coordinator.recover(&project_state_dir, &parsed)?
            };
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
            let lease = perform_launch(
                &service,
                &paths,
                profile,
                project_dir,
                prompt,
                claude_executable.as_deref(),
            )?;
            let human = format!(
                "Launched '{}' as writer for {}\nSession: {}\nPid: {}\nBackground job: {}",
                profile,
                project_dir.display(),
                lease.session_id,
                lease.owner_process.pid,
                lease.provider_handle.clone().unwrap_or_default()
            );
            success("launch", human, lease)
        }
        Command::Hook(_) => Err(Error::ProviderUnsupported),
        Command::Integration(integration) => match &integration.command {
            IntegrationCommand::Herdr(herdr) => run_herdr_integration(herdr, &paths),
            IntegrationCommand::Claude(claude) => {
                let resolve = |target: &IntegrationTarget| -> Result<PathBuf, Error> {
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
                            Ok(profile.config_dir)
                        }
                        (None, Some(path)) => Ok(path.clone()),
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
                        let config_dir = resolve(target)?;
                        let capabilities =
                            assess_installed(claude_executable.as_deref(), &config_dir)?;
                        capabilities
                            .usage_integration_ready(*allow_unverified_version)
                            .map_err(Error::IntegrationRefused)?;
                        let relay_executable =
                            std::env::current_exe().map_err(|source| Error::Io {
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
                        let config_dir = resolve(target)?;
                        let status = integration_status(&config_dir)?;
                        let capabilities =
                            assess_installed(claude_executable.as_deref(), &config_dir).ok();
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
                        let config_dir = resolve(target)?;
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
        },
        Command::Watch(watch) => match &watch.command {
            WatchCommand::Run {
                profile,
                fallback,
                project_dir,
                session_id,
                dry_run,
                probe,
                workload_model,
                simulate_usage,
                simulate_reset_unix_ms,
                claude_executable,
            } => {
                let registered = service.list()?;
                let source = registered
                    .iter()
                    .find(|candidate| &candidate.name == profile)
                    .ok_or_else(|| Error::ProfileNotFound(profile.to_string()))?;
                let fallback_profiles: Vec<&Profile> = fallback
                    .iter()
                    .map(|name| {
                        registered
                            .iter()
                            .find(|candidate| &candidate.name == name)
                            .ok_or_else(|| Error::ProfileNotFound(name.to_string()))
                    })
                    .collect::<Result<_, Error>>()?;

                let executables = providers::ExecutableOverrides {
                    claude: claude_executable.clone(),
                    codex: None,
                };
                // M6: one coordinator per (source_profile, target_profile) pair this round could
                // possibly select, each built from its own two profiles' registered providers.
                // `decide()` itself stays fully provider-neutral (see relay_core::automation);
                // this is the only place that picks a concrete mix of adapters. Every
                // `ProviderPorts` used by a coordinator must outlive `watch.evaluate(..)` below,
                // so they are all collected up front rather than built lazily per decision.
                let mut all_ports: std::collections::BTreeMap<
                    ProfileName,
                    providers::ProviderPorts,
                > = std::collections::BTreeMap::new();
                for profile in std::iter::once(source).chain(fallback_profiles.iter().copied()) {
                    all_ports
                        .entry(profile.name.clone())
                        .or_insert_with(|| providers::ports_for(profile.provider, &executables));
                }

                let coordinators: std::collections::BTreeMap<
                    (ProfileName, ProfileName),
                    HandoffCoordinator<'_>,
                > = {
                    let mut map = std::collections::BTreeMap::new();
                    for target_profile in
                        std::iter::once(source).chain(fallback_profiles.iter().copied())
                    {
                        if target_profile.name == source.name {
                            continue;
                        }
                        let continuity_type = relay_core::handoff::ContinuityType::for_transition(
                            source.provider,
                            target_profile.provider,
                        );
                        let source_ports = all_ports.get(&source.name).expect("inserted above");
                        let target_ports =
                            all_ports.get(&target_profile.name).expect("inserted above");
                        map.insert(
                            (source.name.clone(), target_profile.name.clone()),
                            HandoffCoordinator {
                                paths: &paths,
                                liveness: source_ports.liveness.as_ref(),
                                source_stopper: source_ports.stopper.as_ref(),
                                target_stopper: target_ports.stopper.as_ref(),
                                stager: match continuity_type {
                                    relay_core::handoff::ContinuityType::SessionContinuation => {
                                        source_ports.stager.as_deref()
                                    }
                                    _ => None,
                                },
                                context_capturer: match continuity_type {
                                    relay_core::handoff::ContinuityType::StateContinuation => {
                                        Some(source_ports.context_capturer.as_ref())
                                    }
                                    _ => None,
                                },
                                launcher: target_ports.launcher.as_ref(),
                            },
                        );
                    }
                    map
                };
                let resolve_coordinator = |from: &ProfileName, to: &ProfileName| {
                    coordinators
                        .get(&(from.clone(), to.clone()))
                        .expect("decide() only selects a name present in fallbacks")
                };
                let watch = WatchCoordinator {
                    paths: &paths,
                    handoff_for: &resolve_coordinator,
                    policy: AutomationPolicy::default(),
                };

                let canonical_project =
                    std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                        path: project_dir.clone(),
                        source,
                    })?;
                let project_id = ProjectId::for_canonical_path(&canonical_project)?;
                let project_state_dir = paths.project_state_dir(&project_id);

                // Startup recovery comes first: nothing below (usage detection, and above all a
                // possible probe request) runs while an earlier transaction is unresolved.
                if let Some(outcome) = watch.recover_pending(&project_state_dir, *dry_run)? {
                    return watch_run_output(outcome);
                }

                // Version/capability gate: fail closed when a capability the handoff machinery
                // depends on cannot be verified; merely-unverified newer versions only warn. Only
                // meaningful for a Claude source (this is Claude's own capability-probing
                // machinery); Codex's separate version gate lives in
                // relay_provider_codex::assess_version and is checked inside the adapter itself.
                if source.provider == ProviderKind::Claude {
                    let capabilities =
                        assess_installed(claude_executable.as_deref(), &source.config_dir)?;
                    for required in [
                        relay_provider_claude::Capability::AgentsJsonShape,
                        relay_provider_claude::Capability::TranscriptLayout,
                        relay_provider_claude::Capability::AuthStatusSchema,
                    ] {
                        if capabilities.status_of(required) == CapabilityStatus::Unsupported {
                            return Err(Error::UnsupportedProviderVersion);
                        }
                    }
                    if !cli.json
                        && capabilities
                            .entries
                            .iter()
                            .any(|entry| entry.status == CapabilityStatus::Unverified)
                    {
                        // Live-found (M4.12): this must never print in --json mode. `--json`'s
                        // contract is that stderr on a failed invocation is exactly the stable
                        // error envelope and nothing else; an extra human-readable line ahead of
                        // it breaks every machine consumer that parses stderr as JSON on failure
                        // (relay-herdr's own client included — this is exactly what caught it).
                        eprintln!(
                            "warning: Claude Code {} has not been validated by Relay; usage \
                             detection stays fail-closed, but re-validate before trusting handoffs",
                            capabilities.version
                        );
                    }
                }

                let ledger =
                    LedgerStore::at_path(project_state_dir.join("automation_state.json")).load()?;
                let now = current_unix_ms();
                // The probe spends real API usage: never against a profile already known
                // exhausted, and only when the operator explicitly opted in. Only meaningful for
                // Claude candidates; Codex's usage signal ignores the flag (see
                // relay_provider_codex::usage).
                let signal_for = |profile: &Profile| {
                    providers::usage_signal_for(
                        profile.provider,
                        &executables,
                        *probe && !ledger.is_known_exhausted(&profile.name, now),
                        workload_model.clone(),
                    )
                };

                // `--simulate-usage` fault-injects the SOURCE's own usage state only; fallback
                // candidates are always checked for real.
                let source_usage_signal: Box<dyn UsageSignal> = match simulate_usage {
                    Some(state) => Box::new(SimulatedUsageSignal {
                        state: UsageState::from(*state),
                        reset_unix_ms: *simulate_reset_unix_ms,
                    }),
                    None => signal_for(source),
                };

                let source_usage =
                    source_usage_signal.detect(&source.config_dir, project_dir, session_id)?;
                let fallback_candidates = fallback_profiles
                    .iter()
                    .map(|candidate| -> Result<ProfileCandidate, Error> {
                        let usage = signal_for(candidate).detect(
                            &candidate.config_dir,
                            project_dir,
                            session_id,
                        )?;
                        let healthy = doctor_is_healthy(&service, candidate, &executables)?;
                        Ok(ProfileCandidate {
                            name: candidate.name.clone(),
                            provider: candidate.provider,
                            config_dir: candidate.config_dir.clone(),
                            identity_stable_id: Some(candidate.expected_identity.stable_id.clone()),
                            enabled: candidate.enabled,
                            healthy,
                            usage,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;

                let outcome = watch.evaluate(
                    WatchRequest {
                        project_dir: project_dir.clone(),
                        source_profile: source.name.clone(),
                        source_provider: source.provider,
                        source_config_dir: source.config_dir.clone(),
                        source_identity_stable_id: Some(source.expected_identity.stable_id.clone()),
                        session_id: session_id.clone(),
                        source_usage,
                        fallbacks: fallback_candidates,
                        dry_run: *dry_run,
                    },
                    current_unix_ms(),
                )?;

                watch_run_output(outcome)
            }
            WatchCommand::Status { project_dir } => {
                let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                    path: project_dir.clone(),
                    source,
                })?;
                let project_id = ProjectId::for_canonical_path(&canonical)?;
                let project_state_dir = paths.project_state_dir(&project_id);
                let ledger =
                    LedgerStore::at_path(project_state_dir.join("automation_state.json")).load()?;
                let lease = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?;
                let human = format!(
                    "Project: {}\nCurrent owner: {}\nKnown-exhausted profiles: {}\nRecent automatic handoffs: {}",
                    canonical.display(),
                    lease
                        .as_ref()
                        .map(|lease| lease.owner_profile.to_string())
                        .unwrap_or_else(|| "none yet".to_owned()),
                    if ledger.known_exhausted.is_empty() {
                        "none".to_owned()
                    } else {
                        ledger
                            .known_exhausted
                            .iter()
                            .map(|record| record.profile.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    ledger.recent_handoffs.len()
                );
                success(
                    "watch.status",
                    human,
                    json!({ "lease": lease, "ledger": ledger }),
                )
            }
            WatchCommand::Clear { project_dir } => {
                let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                    path: project_dir.clone(),
                    source,
                })?;
                let project_id = ProjectId::for_canonical_path(&canonical)?;
                let project_state_dir = paths.project_state_dir(&project_id);
                LedgerStore::at_path(project_state_dir.join("automation_state.json")).clear()?;
                success(
                    "watch.clear",
                    format!("Cleared automation ledger for {}", canonical.display()),
                    json!({ "project_id": project_id.as_str() }),
                )
            }
        },
        Command::Setup(args) => run_setup(&service, &paths, args),
        Command::Claude(args) => run_claude(&service, &paths, args, cli.json),
        Command::Status { project_dir } => run_status(&service, &paths, project_dir.as_deref()),
        Command::Profiles => run_profiles(&service, &paths),
        Command::Login {
            name,
            provider,
            claude_executable,
            codex_executable,
        } => run_login(
            &service,
            &paths,
            name,
            (*provider).into(),
            cli.json,
            &providers::ExecutableOverrides {
                claude: claude_executable.clone(),
                codex: codex_executable.clone(),
            },
        ),
        Command::Logout {
            name,
            claude_executable,
            codex_executable,
        } => run_logout(
            &service,
            name,
            &providers::ExecutableOverrides {
                claude: claude_executable.clone(),
                codex: codex_executable.clone(),
            },
        ),
        Command::Switch(args) => run_switch(&service, &paths, args, cli.json),
        Command::Resume(args) => run_resume(&service, &paths, args, cli.json),
    }
}

fn doctor_is_healthy(
    service: &ProfileService,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> Result<bool, Error> {
    let provider = provider_for_profile(service, &profile.name, executables)?;
    Ok(service.doctor(&profile.name, provider.as_ref())?.healthy)
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum WatchRunOutput {
    NoActionNeeded {
        source_usage: String,
    },
    WaitingForCapacity {
        reason: String,
    },
    CooldownActive {
        retry_after_unix_ms: u64,
    },
    LoopPrevented {
        reason: String,
    },
    DryRunWouldHandoff {
        target: ProfileName,
    },
    Handoff {
        target: ProfileName,
        journal: Box<relay_core::handoff::HandoffJournal>,
    },
    Recovered {
        transactions: Vec<String>,
    },
    TransactionInFlight {
        transaction_id: String,
    },
}

fn run_herdr_integration(
    herdr: &HerdrIntegrationArgs,
    paths: &RelayPaths,
) -> Result<CommandOutput, Error> {
    let refused =
        |error: relay_herdr::HerdrIntegrationError| Error::IntegrationRefused(error.to_string());
    match &herdr.command {
        HerdrIntegrationCommand::Install {
            plugin_path,
            dry_run,
            herdr_executable,
        } => {
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let resolved_path =
                herdr_install::resolve_plugin_path(plugin_path.as_deref(), paths.config_root())
                    .map_err(refused)?;
            let plan = herdr_install::plan_install(&client, &resolved_path).map_err(refused)?;
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
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let report = herdr_install::status(&client).map_err(refused)?;
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
            let client = HerdrCliClient::discover(herdr_executable.as_deref()).map_err(refused)?;
            let report = herdr_install::doctor(&client).map_err(refused)?;
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

fn watch_run_output(outcome: WatchOutcome) -> Result<CommandOutput, Error> {
    let (human, data) = match outcome {
        WatchOutcome::NoActionNeeded { source_usage } => (
            format!("No action needed: source usage is {source_usage:?}"),
            WatchRunOutput::NoActionNeeded {
                source_usage: format!("{source_usage:?}"),
            },
        ),
        WatchOutcome::WaitingForCapacity { reason } => (
            format!("Waiting for capacity: {reason}"),
            WatchRunOutput::WaitingForCapacity { reason },
        ),
        WatchOutcome::CooldownActive {
            retry_after_unix_ms,
        } => (
            format!("Cooldown active; retry after unix_ms={retry_after_unix_ms}"),
            WatchRunOutput::CooldownActive {
                retry_after_unix_ms,
            },
        ),
        WatchOutcome::LoopPrevented { reason } => (
            format!("Loop prevented: {reason}"),
            WatchRunOutput::LoopPrevented { reason },
        ),
        WatchOutcome::DryRunWouldHandoff { target } => (
            format!("Dry run: would hand off to '{target}' (no mutation performed)"),
            WatchRunOutput::DryRunWouldHandoff { target },
        ),
        WatchOutcome::Recovered { transactions } => (
            format!(
                "Recovered {} incomplete transaction(s) from an earlier run; no new work was \
                 started this round. Run again to re-evaluate.\n{}",
                transactions.len(),
                transactions
                    .iter()
                    .map(|item| format!("  {} -> {}", item.transaction_id, item.final_state))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            WatchRunOutput::Recovered {
                transactions: transactions
                    .iter()
                    .map(|item| format!("{} -> {}", item.transaction_id, item.final_state))
                    .collect(),
            },
        ),
        WatchOutcome::RecoveryRequired {
            transaction_id,
            reason,
        } => {
            // A non-zero exit so a cron/shell loop notices; nothing new was started.
            return Err(Error::RecoveryRequired(format!(
                "{transaction_id}: {reason}. Run `relay recover {transaction_id} --project-dir <dir>`"
            )));
        }
        WatchOutcome::TransactionInFlight { transaction_id } => (
            format!(
                "Transaction {transaction_id} is in progress in another process; not starting new work"
            ),
            WatchRunOutput::TransactionInFlight { transaction_id },
        ),
        WatchOutcome::Handoff { journal, target } => (
            format!(
                "Automatic handoff to '{target}': {:?}\nTransaction: {}",
                journal.state, journal.transaction_id
            ),
            WatchRunOutput::Handoff { target, journal },
        ),
    };
    success("watch.run", human, data)
}

/// A short, human-friendly project name for the terminal banner (`relay claude`/`relay resume`) —
/// never the full path, and never a native session UUID (per the M6 UX contract: normal output
/// shows people and projects, not internal identifiers).
fn project_display_name(canonical_project: &Path) -> String {
    canonical_project
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| canonical_project.to_string_lossy().into_owned())
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

const LAUNCH_LIVENESS_CONFIRM_ATTEMPTS: u32 = 3;
const LAUNCH_LIVENESS_POLL_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// A single liveness check has a real transient gap: live testing during M2B.75 showed that
/// checking immediately after a competing writer's pid died (but before Claude's own background
/// daemon had reassigned a replacement) can read as "not active" for one instant even though the
/// session is about to come back. Requires `LAUNCH_LIVENESS_CONFIRM_ATTEMPTS` *consecutive*
/// not-active readings before concluding it is genuinely safe to launch a new writer; a single
/// active reading is trusted immediately (no race in that direction — evidence of activity is
/// evidence of activity).
fn confirm_not_active(
    config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    recorded_owner: &relay_core::handoff::ProcessIdentity,
    claude_executable: Option<PathBuf>,
) -> Result<bool, Error> {
    let liveness = ClaudeSourceLiveness::new(claude_executable);
    for attempt in 0..LAUNCH_LIVENESS_CONFIRM_ATTEMPTS {
        let verdict = liveness.check(config_dir, project_dir, session_id, Some(recorded_owner))?;
        if verdict.active {
            return Ok(false);
        }
        if attempt + 1 < LAUNCH_LIVENESS_CONFIRM_ATTEMPTS {
            std::thread::sleep(LAUNCH_LIVENESS_POLL_DELAY);
        }
    }
    Ok(true)
}

/// The exact `relay launch` logic (M2B.5), factored out so `relay claude` (M4) can reuse it
/// unchanged rather than re-implementing writer creation: refuses a still-active existing writer,
/// otherwise spawns `claude --bg` and records a fresh `WriterLease`. Both callers get the same
/// safety guarantees; `relay claude` just chooses the profile/prompt/session for the caller.
fn perform_launch(
    service: &ProfileService,
    paths: &RelayPaths,
    profile: &ProfileName,
    project_dir: &Path,
    prompt: &str,
    claude_executable: Option<&Path>,
) -> Result<relay_core::handoff::WriterLease, Error> {
    let registered = service.list()?;
    let target = registered
        .iter()
        .find(|candidate| &candidate.name == profile)
        .ok_or_else(|| Error::ProfileNotFound(profile.to_string()))?;
    let canonical_project = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.to_path_buf(),
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

    lock.try_with(|| -> Result<relay_core::handoff::WriterLease, Error> {
        if let Some(existing) = lease_store.load()? {
            let owner_config_dir = registered
                .iter()
                .find(|candidate| candidate.name == existing.owner_profile)
                .map(|candidate| candidate.config_dir.clone());
            let still_active = match &owner_config_dir {
                Some(config_dir) => confirm_not_active(
                    config_dir,
                    &canonical_project,
                    &existing.session_id,
                    &existing.owner_process,
                    claude_executable.map(Path::to_path_buf),
                )
                .map(|confirmed_inactive| !confirmed_inactive)?,
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
            claude_executable,
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
    })
}

// =================================================================================================
// M4: friendly authentication, setup wizard, daily entrypoint, and status commands.
//
// Security rule (M4.17), enforced structurally: every Claude authentication interaction below is
// exactly `claude auth login`/`claude auth logout`/`claude auth status`, run as a foreground child
// process with this terminal's own stdio inherited. Relay never reads the child's output for
// anything except the already-existing, already-tested `claude auth status --json` parser
// (`ClaudeInspector`/`parse_auth_status`) that only ever extracts a non-secret identity pin — it
// never touches credential files, cookies, or Keychain, and never implements its own OAuth client.
// =================================================================================================

/// Runs `claude auth login` or `claude auth logout` for one profile's isolated `CLAUDE_CONFIG_DIR`,
/// with this terminal's stdin/stdout/stderr inherited so the user sees and drives Claude's own
/// real login UI (browser open, device code, etc.) directly. Relay only waits for it to exit;
/// nothing about the child's output is read.
fn run_claude_auth_subcommand(
    claude_executable: Option<&Path>,
    config_dir: &Path,
    subcommand: &str,
) -> Result<(), Error> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let mut command = std::process::Command::new(inspector.executable());
    command
        .arg("auth")
        .arg(subcommand)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let status = command.status().map_err(|_| Error::ProviderCommandFailed)?;
    if !status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    Ok(())
}

/// After a successful `claude auth login`, runs the exact same strict inspection
/// (`ClaudeInspector::inspect`) `relay profile adopt`/`inspect-existing` already use, and confirms
/// the three things M4.1 Step 2 requires: authenticated, identity available, and (implicitly, since
/// `config_dir` is the exact directory just logged into) the config directory matches.
fn verify_authenticated(
    config_dir: &Path,
    claude_executable: Option<&Path>,
) -> Result<ClaudeInspectionReport, Error> {
    let environment = inspect_environment(config_dir);
    let inspector = ClaudeInspector::discover(claude_executable)?;
    inspector.inspect(config_dir, environment)
}

/// Registers a freshly authenticated (or re-authenticated) directory as a Relay profile through
/// the unchanged, already-tested adoption path (`ProfileSetupMode::AdoptExisting`) — the same code
/// `relay profile adopt` uses. `ClaudeAdoptionProvider` only ever inspects; it never creates a
/// Claude profile itself (`setup_profile` returns `ProviderUnsupported` for anything but
/// `AdoptExisting`), which is exactly why the directory must already be authenticated before this
/// is called.
fn adopt_authenticated_profile(
    service: &ProfileService,
    name: &ProfileName,
    config_dir: &Path,
    report: &ClaudeInspectionReport,
    claude_executable: Option<&Path>,
) -> Result<Profile, Error> {
    let pin = report
        .identity_pin
        .clone()
        .ok_or(Error::IdentityUnavailable)?;
    let expected_identity = IdentityMetadata {
        stable_id: pin.stable_id(),
        display_label: pin.email.clone().or_else(|| pin.account_id.clone()),
    };
    let claude_provider = ClaudeAdoptionProvider::discover(claude_executable)?;
    service.add(
        AddProfileRequest {
            name: name.clone(),
            provider: ProviderKind::Claude,
            config_dir: Some(config_dir.to_path_buf()),
            mode: ProfileSetupMode::AdoptExisting,
            expected_identity: Some(expected_identity),
        },
        &claude_provider,
    )
}

/// M4.1 Step 2A: a brand-new isolated profile. Relay creates the private (mode 0700) directory
/// itself (`ProfileDirectory::create_managed`, the same safety-checked call `relay profile add`
/// uses), launches Claude's own official login flow there, verifies the result with the existing
/// strict inspector, and adopts it — all through machinery that already existed before M4.
fn create_and_authenticate_profile(
    service: &ProfileService,
    paths: &RelayPaths,
    name: &ProfileName,
    claude_executable: Option<&Path>,
) -> Result<Profile, Error> {
    let config_dir = paths.default_profile_dir(name, ProviderKind::Claude);
    ProfileDirectory::new(paths.profiles_root())?.create_managed(&config_dir)?;
    run_claude_auth_subcommand(claude_executable, &config_dir, "login")?;
    let report = verify_authenticated(&config_dir, claude_executable)?;
    if !report.authenticated || report.identity_pin.is_none() {
        return Err(Error::AuthenticationRequired);
    }
    adopt_authenticated_profile(service, name, &config_dir, &report, claude_executable)
}

/// M6: mirrors `create_and_authenticate_profile` for Codex. `CodexBackend::setup_profile`
/// only ever inspects (never creates credentials itself — see its doc comment), so exactly like
/// the Claude path, Relay creates the private directory itself, runs the official `codex login`
/// there (inherited stdio: if browser/device interaction is required, it happens in this exact
/// process, which is the M6 spec's designated stop point for human authorization), then adopts
/// the now-authenticated directory.
fn create_and_authenticate_codex_profile(
    service: &ProfileService,
    paths: &RelayPaths,
    name: &ProfileName,
    codex_executable: Option<&Path>,
) -> Result<Profile, Error> {
    let config_dir = paths.default_profile_dir(name, ProviderKind::Codex);
    ProfileDirectory::new(paths.profiles_root())?.create_managed(&config_dir)?;
    run_codex_auth_subcommand(codex_executable, &config_dir, "login")?;
    let backend = relay_provider_codex::CodexBackend::discover(codex_executable)?;
    let observation = backend.inspect_profile(&config_dir)?;
    if observation.authentication != AuthenticationState::Authenticated {
        return Err(Error::AuthenticationRequired);
    }
    let expected_identity = observation
        .identity
        .clone()
        .ok_or(Error::IdentityUnavailable)?;
    service.add(
        AddProfileRequest {
            name: name.clone(),
            provider: ProviderKind::Codex,
            config_dir: Some(config_dir),
            mode: ProfileSetupMode::AdoptExisting,
            expected_identity: Some(expected_identity),
        },
        &backend,
    )
}

fn run_codex_auth_subcommand(
    codex_executable: Option<&Path>,
    config_dir: &Path,
    subcommand: &str,
) -> Result<(), Error> {
    let inspector = relay_provider_codex::CodexInspector::discover(codex_executable)?;
    let mut command = std::process::Command::new(inspector.executable());
    command
        .arg(subcommand)
        .env("CODEX_HOME", config_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    for variable in relay_provider_codex::AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let status = command.status().map_err(|_| Error::ProviderCommandFailed)?;
    if !status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    Ok(())
}

fn run_login(
    service: &ProfileService,
    paths: &RelayPaths,
    name: &ProfileName,
    provider: ProviderKind,
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
                run_claude_auth_subcommand(
                    executables.claude.as_deref(),
                    &existing.config_dir,
                    "login",
                )?;
                let report =
                    verify_authenticated(&existing.config_dir, executables.claude.as_deref())?;
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
                let observation = backend.inspect_profile(&existing.config_dir)?;
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

fn run_logout(
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

/// M6: `relay switch <target>` — the manual cross-profile (same provider or not) handoff entry
/// point. Reads the project's current writer lease to find the source (never takes it as an
/// argument, so it can never be spoofed to claim ownership of a profile that isn't the real
/// current writer), verifies the target is authenticated, chooses SESSION_CONTINUATION or
/// STATE_CONTINUATION from the two profiles' providers, and runs one
/// [`HandoffCoordinator::run`] transaction — the exact same safety machinery `relay watch run`'s
/// automatic path uses.
fn run_switch(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &SwitchArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let project_dir = match &args.project_dir {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let canonical_project = std::fs::canonicalize(&project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;
    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    let lease = LeaseStore::at_path(project_state_dir.join("lease.json"))
        .load()?
        .ok_or(Error::NoActiveWriterForProject)?;

    let registered = service.list()?;
    let source = registered
        .iter()
        .find(|profile| profile.name == lease.owner_profile)
        .ok_or_else(|| Error::ProfileNotFound(lease.owner_profile.to_string()))?;
    let target = registered
        .iter()
        .find(|profile| profile.name == args.target)
        .ok_or_else(|| Error::ProfileNotFound(args.target.to_string()))?;
    if target.name == source.name {
        return Err(Error::AlreadyCurrentWriter(target.name.to_string()));
    }

    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };

    let target_backend = providers::provider_backend(target.provider, &executables)?;
    let target_status = service.status(&target.name, target_backend.as_ref())?;
    if target_status.authentication != AuthenticationState::Authenticated {
        return Err(Error::AuthenticationRequired);
    }

    let continuity_type =
        relay_core::handoff::ContinuityType::for_transition(source.provider, target.provider);
    let source_ports = providers::ports_for(source.provider, &executables);
    let target_ports = providers::ports_for(target.provider, &executables);
    let coordinator = HandoffCoordinator {
        paths,
        liveness: source_ports.liveness.as_ref(),
        source_stopper: source_ports.stopper.as_ref(),
        target_stopper: target_ports.stopper.as_ref(),
        stager: match continuity_type {
            relay_core::handoff::ContinuityType::SessionContinuation => {
                source_ports.stager.as_deref()
            }
            _ => None,
        },
        context_capturer: match continuity_type {
            relay_core::handoff::ContinuityType::StateContinuation => {
                Some(source_ports.context_capturer.as_ref())
            }
            _ => None,
        },
        launcher: target_ports.launcher.as_ref(),
    };

    if !json_mode {
        println!(
            "Switching '{}' -> '{}' ({:?})...",
            source.name, target.name, continuity_type
        );
    }
    let journal = coordinator.run(HandoffRequest {
        project_dir: canonical_project.clone(),
        source_profile: source.name.clone(),
        source_provider: source.provider,
        source_config_dir: source.config_dir.clone(),
        target_profile: target.name.clone(),
        target_provider: target.provider,
        target_config_dir: target.config_dir.clone(),
        session_id: lease.session_id.clone(),
        continuity_type,
    })?;

    let new_session_id = journal
        .verification
        .as_ref()
        .map(|verification| verification.target_session_id.clone())
        .unwrap_or_default();
    let human = format!(
        "Switch {} ('{}' -> '{}'): {:?}\nContinuity: {:?}\nNew session: {}",
        journal.transaction_id,
        source.name,
        target.name,
        journal.state,
        continuity_type,
        new_session_id
    );
    if args.no_attach || journal.state != relay_core::handoff::HandoffState::Complete {
        return success("switch", human, journal);
    }

    match target.provider {
        ProviderKind::Codex => exec_codex_resume(
            providers::executable_override(ProviderKind::Codex, &executables),
            &target.config_dir,
            &canonical_project,
            &new_session_id,
        ),
        ProviderKind::Claude | ProviderKind::Fake => success(
            "switch",
            format!("{human}\n\nRun `relay claude` to attach."),
            journal,
        ),
    }
}

/// The normal way to continue an existing Relay-managed session: `relay resume` (no profile)
/// resolves the project's writer lease and reattaches under the *actual lease owner* — never
/// assumed to be the configured primary, since a completed handoff can leave a fallback profile
/// as the owner (the same invariant the M6 dogfood fix in `run_claude`/`exec_claude_attach`
/// established; see commit `142668f`). `NATIVE_RESUME` for Codex (`codex resume <thread-id>`) —
/// Codex has no background-job/attach concept at all, so this is unconditional. For Claude, see
/// [`resolve_claude_resume_action`]: dogfood-found (M6, second finding) that `claude --resume
/// <id>` unconditionally fails when the recorded background job is still live — Claude's own
/// `attach` is required in that case instead.
///
/// The advanced explicit form (`relay resume <profile>`) is preserved unchanged: it refuses
/// unless `profile` is already the project's current writer (`relay switch` moves ownership;
/// this command only ever reattaches to it).
fn run_resume(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &ResumeArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let project_dir = match &args.project_dir {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let canonical_project = std::fs::canonicalize(&project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;
    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    let lease = LeaseStore::at_path(project_state_dir.join("lease.json"))
        .load()?
        .ok_or(Error::NoActiveWriterForProject)?;
    if let Some(requested) = &args.profile {
        if &lease.owner_profile != requested {
            return Err(Error::WriterLeaseOwnedByAnotherProfile(
                lease.owner_profile.to_string(),
            ));
        }
    }
    // Bare `relay resume`: the resolved profile is whoever the lease says owns it right now,
    // never the configured primary.
    let resolved_profile = lease.owner_profile.clone();
    let registered = service.list()?;
    let profile = registered
        .iter()
        .find(|profile| profile.name == resolved_profile)
        .ok_or_else(|| Error::ProfileNotFound(resolved_profile.to_string()))?;

    // Resolved before printing anything: on `AmbiguousSessionLiveness` this must fail closed
    // without ever claiming to be "resuming" a session it then can't safely continue.
    let claude_action = match profile.provider {
        ProviderKind::Codex => None,
        ProviderKind::Claude | ProviderKind::Fake => Some(resolve_claude_resume_action(
            &profile.config_dir,
            args.claude_executable.as_deref(),
            &lease,
        )?),
    };

    if !json_mode {
        let provider_label = match profile.provider {
            ProviderKind::Codex => "Codex",
            ProviderKind::Claude | ProviderKind::Fake => "Claude",
        };
        println!(
            "Agent Relay\nProject: {}\nProfile: {}\nResuming managed {provider_label} session...",
            project_display_name(&canonical_project),
            resolved_profile
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }

    match profile.provider {
        ProviderKind::Codex => exec_codex_resume(
            args.codex_executable.as_deref(),
            &profile.config_dir,
            &canonical_project,
            &lease.session_id,
        ),
        ProviderKind::Claude | ProviderKind::Fake => {
            match claude_action.expect("computed above for this provider arm") {
                ClaudeResumeAction::Attach(short_id) => {
                    let inspector = ClaudeInspector::discover(args.claude_executable.as_deref())?;
                    exec_claude_attach(inspector.executable(), &profile.config_dir, &short_id)
                }
                ClaudeResumeAction::NativeResume => exec_claude_resume(
                    args.claude_executable.as_deref(),
                    &profile.config_dir,
                    &canonical_project,
                    &lease.session_id,
                ),
            }
        }
    }
}

/// Which real Claude command safely continues this lease. Dogfood-found (M6): `relay resume`
/// used to always run `claude --resume <session_id>` — but a session with a still-live
/// `claude --bg` background job rejects `--resume` outright ("running as a background session
/// ... run `claude attach <id>`"); only Claude's own `attach` works for a live job. Conversely,
/// once the background job is confirmed gone, `attach` would find nothing — `--resume` (native
/// session/thread resumption) is what's actually safe there.
///
/// - The lease's `provider_handle` (present for a direct launch, absent for a lease a cross-
///   profile handoff produced — see `WriterLease::provider_handle`'s own doc comment) is checked
///   against `claude agents --json`'s live listing: found → definitely live → `Attach`.
/// - Not found, but present: fall back to the recorded owner process's own pid+start-time
///   fingerprint (the same authoritative signal `ClaudeSourceLiveness`/`SessionStopper` already
///   use elsewhere). Confirmed gone (`Some(false)`) → `NativeResume`. Anything else — genuinely
///   indeterminate (`None`), *or* the recorded process improbably still matches yet Claude's own
///   listing disagrees with it (`Some(true)`) — is an inconsistent state this must never guess
///   through, so it fails closed with `AmbiguousSessionLiveness` rather than risking either a
///   failed attach or, worse, racing a background job that is in fact still there.
/// - No `provider_handle` at all: no background job was ever recorded for this lease (a
///   handoff's target launch is a foreground verification turn that has already exited by the
///   time anyone resumes later, never a persistent `--bg` job) — nothing to check liveness
///   against, so this is unconditionally `NativeResume`, not ambiguous.
fn resolve_claude_resume_action(
    config_dir: &Path,
    claude_executable: Option<&Path>,
    lease: &relay_core::handoff::WriterLease,
) -> Result<ClaudeResumeAction, Error> {
    let Some(handle) = &lease.provider_handle else {
        return Ok(ClaudeResumeAction::NativeResume);
    };
    let sessions = query_active_sessions(config_dir, claude_executable)?;
    let listed = sessions
        .iter()
        .any(|record| record.id.as_deref() == Some(handle.as_str()));
    if listed {
        return Ok(ClaudeResumeAction::Attach(handle.clone()));
    }
    match lease.owner_process.is_still_the_same_process() {
        Some(false) => Ok(ClaudeResumeAction::NativeResume),
        Some(true) | None => Err(Error::AmbiguousSessionLiveness(
            lease.owner_profile.to_string(),
        )),
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ClaudeResumeAction {
    /// `claude attach <id>` — the recorded background job is confirmed live.
    Attach(String),
    /// `claude --resume <session_id>` — no live background job; the native session itself is
    /// what gets resumed.
    NativeResume,
}

#[cfg(unix)]
fn exec_codex_resume(
    codex_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
) -> Result<CommandOutput, Error> {
    use std::os::unix::process::CommandExt as _;
    let inspector = relay_provider_codex::CodexInspector::discover(codex_executable)?;
    let error = std::process::Command::new(inspector.executable())
        .current_dir(project_dir)
        .arg("resume")
        .arg(thread_id)
        .env("CODEX_HOME", config_dir)
        .exec();
    Err(Error::Io {
        path: inspector.executable().to_path_buf(),
        source: error,
    })
}

#[cfg(not(unix))]
fn exec_codex_resume(
    codex_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
) -> Result<CommandOutput, Error> {
    let inspector = relay_provider_codex::CodexInspector::discover(codex_executable)?;
    let status = std::process::Command::new(inspector.executable())
        .current_dir(project_dir)
        .arg("resume")
        .arg(thread_id)
        .env("CODEX_HOME", config_dir)
        .status()
        .map_err(|_| Error::ProviderCommandFailed)?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(unix)]
fn exec_claude_resume(
    claude_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
) -> Result<CommandOutput, Error> {
    use std::os::unix::process::CommandExt as _;
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let error = std::process::Command::new(inspector.executable())
        .current_dir(project_dir)
        .arg("--resume")
        .arg(session_id)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .exec();
    Err(Error::Io {
        path: inspector.executable().to_path_buf(),
        source: error,
    })
}

#[cfg(not(unix))]
fn exec_claude_resume(
    claude_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
) -> Result<CommandOutput, Error> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let status = std::process::Command::new(inspector.executable())
        .current_dir(project_dir)
        .arg("--resume")
        .arg(session_id)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .status()
        .map_err(|_| Error::ProviderCommandFailed)?;
    std::process::exit(status.code().unwrap_or(1));
}

// -------------------------------------------------------------------------------------------
// Prompt helpers: minimal, readline-based, work over any stdin (a real TTY or piped input for
// scripting/tests). Never used for anything credential-related — only friendly names and
// yes/no/choice prompts.
// -------------------------------------------------------------------------------------------

fn prompt_line(question: &str, default: Option<&str>) -> Result<String, Error> {
    use std::io::Write as _;
    match default {
        Some(default) => print!("{question} [{default}]: "),
        None => print!("{question}: "),
    }
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let bytes_read = std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| Error::MissingEnvironment("stdin"))?;
    if bytes_read == 0 {
        // True EOF (closed/exhausted stdin), not just an empty line: never loop forever waiting
        // for input that will never come.
        return Err(Error::MissingEnvironment("stdin"));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        if let Some(default) = default {
            return Ok(default.to_owned());
        }
    }
    Ok(trimmed.to_owned())
}

fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool, Error> {
    use std::io::Write as _;
    let hint = if default_yes { "Y/n" } else { "y/N" };
    print!("{question} [{hint}]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let bytes_read = std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| Error::MissingEnvironment("stdin"))?;
    if bytes_read == 0 {
        return Err(Error::MissingEnvironment("stdin"));
    }
    Ok(match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    })
}

/// Friendly per-profile authentication summary for `relay profiles`/`relay status`. Never fails
/// the whole listing on one profile's inspection error — reports it as "unreachable" instead, so
/// one broken profile does not hide every other one.
fn friendly_auth_state(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> (&'static str, Option<String>) {
    if profile.provider == ProviderKind::Claude {
        // Kept on the strict Claude-specific inspector (identity pin required, not just
        // "authenticated") rather than the generic Provider::inspect_profile dispatch: this is
        // the pre-M6 behavior and changing it is out of M6's scope.
        let Ok(inspector) = ClaudeInspector::discover(executables.claude.as_deref()) else {
            return ("unreachable", None);
        };
        let environment = inspect_environment(&profile.config_dir);
        return match inspector.inspect(&profile.config_dir, environment) {
            Ok(report) if report.authenticated && report.identity_pin.is_some() => {
                ("authenticated", None)
            }
            Ok(_) => ("needs login", None),
            Err(error) => ("unreachable", Some(error.to_string())),
        };
    }
    let Ok(provider) = providers::provider_backend(profile.provider, executables) else {
        return ("unreachable", None);
    };
    match provider.inspect_profile(&profile.config_dir) {
        Ok(observation) if observation.authentication == AuthenticationState::Authenticated => {
            ("authenticated", None)
        }
        Ok(_) => ("needs login", None),
        Err(error) => ("unreachable", Some(error.to_string())),
    }
}

fn run_profiles(service: &ProfileService, paths: &RelayPaths) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for profile in &registered {
        let role = if preferences.primary_profile.as_ref() == Some(&profile.name) {
            "primary"
        } else if preferences.fallback_profiles.contains(&profile.name) {
            "fallback"
        } else {
            "unassigned"
        };
        let (auth_state, _detail) =
            friendly_auth_state(profile, &providers::ExecutableOverrides::default());
        lines.push(format!(
            "{:<12} {:<8} {:<10} {}",
            profile.name, profile.provider, role, auth_state
        ));
        rows.push(json!({
            "name": profile.name.as_str(),
            "provider": profile.provider.to_string(),
            "role": role,
            "authentication": auth_state,
        }));
    }
    if lines.is_empty() {
        lines.push("No profiles registered yet. Run `relay setup` to get started.".to_owned());
    } else {
        lines.insert(
            0,
            format!(
                "{:<12} {:<8} {:<10} {}",
                "PROFILE", "PROVIDER", "ROLE", "AUTH"
            ),
        );
    }
    success("profiles", lines.join("\n"), json!({ "profiles": rows }))
}

fn run_status(
    service: &ProfileService,
    paths: &RelayPaths,
    project_dir: Option<&Path>,
) -> Result<CommandOutput, Error> {
    let cwd = match project_dir {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let canonical =
        std::fs::canonicalize(&cwd).map_err(|source| Error::Io { path: cwd, source })?;

    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let Some(primary) = preferences.primary_profile.clone() else {
        return success(
            "status",
            "Agent Relay is not set up yet.\n\nRun:\n    relay setup".to_owned(),
            json!({ "configured": false }),
        );
    };

    let registered = service.list()?;
    let primary_profile = registered.iter().find(|profile| profile.name == primary);
    let (primary_auth, _) = primary_profile.map_or(("not registered", None), |profile| {
        friendly_auth_state(profile, &providers::ExecutableOverrides::default())
    });

    let project_id = ProjectId::for_canonical_path(&canonical)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    let lease = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?;
    let locked = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"))
        .is_currently_held();
    let current_transaction =
        std::fs::read_to_string(project_state_dir.join("current_transaction.json")).ok();

    // A lease *record* existing does not mean the process behind it is still running; confirm
    // with the same liveness check `relay launch`/`watch run` use before calling it "active"
    // rather than naively trusting the file.
    let session_state = if locked {
        "handoff in progress"
    } else if let Some(lease) = &lease {
        let owner_config_dir = registered
            .iter()
            .find(|profile| profile.name == lease.owner_profile)
            .map(|profile| profile.config_dir.clone());
        let live = owner_config_dir.is_some_and(|config_dir| {
            ClaudeSourceLiveness::new(None)
                .check(
                    &config_dir,
                    &canonical,
                    &lease.session_id,
                    Some(&lease.owner_process),
                )
                .map(|verdict| verdict.active)
                .unwrap_or(false)
        });
        if live {
            "active"
        } else {
            "idle (last session ended)"
        }
    } else {
        "not started"
    };

    let herdr_connected =
        std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));

    let human = format!(
        "Project: {}\nClaude session: {}\nCurrent profile: {}\nFallback: {}\nPrimary profile auth: {}\nAutomatic handoff: {}\nHerdr: {}",
        canonical.display(),
        session_state,
        lease.as_ref().map_or_else(
            || primary.to_string(),
            |lease| lease.owner_profile.to_string()
        ),
        preferences
            .fallback_profiles
            .iter()
            .map(ProfileName::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        primary_auth,
        if preferences.usage_integration_enabled == Some(true) {
            "enabled"
        } else {
            "not enabled"
        },
        if herdr_connected {
            "connected"
        } else {
            "not connected"
        },
    );
    success(
        "status",
        human,
        json!({
            "configured": true,
            "project": canonical,
            "session_state": session_state,
            "primary_profile": primary.as_str(),
            "primary_authenticated": primary_auth,
            "fallback_profiles": preferences.fallback_profiles.iter().map(ProfileName::to_string).collect::<Vec<_>>(),
            "lease_owner": lease.as_ref().map(|lease| lease.owner_profile.to_string()),
            "current_transaction": current_transaction,
            "usage_integration_enabled": preferences.usage_integration_enabled.unwrap_or(false),
            "herdr_connected": herdr_connected,
        }),
    )
}

fn resolve_initial_message(args_message: &[String]) -> Result<String, Error> {
    if !args_message.is_empty() {
        return Ok(args_message.join(" "));
    }
    loop {
        let line = prompt_line("What would you like Claude to help with?", None)?;
        if !line.trim().is_empty() {
            return Ok(line);
        }
        println!("(please enter a message)");
    }
}

/// M4.4: the officially supported way to give the user a real, live, interactive terminal on a
/// session Relay itself launched with `claude --bg` — `claude attach <short-id>` ("Open the
/// background session in this terminal ... The session keeps running either way", per `claude
/// attach --help`, live-checked against Claude Code 2.1.278). This is not a Relay-invented
/// workaround: it is Claude's own documented attach mechanism for exactly this session kind, so
/// it preserves the process Relay's `WriterLease`/liveness checks already track — no new pid is
/// spawned independently of the one Relay recorded.
///
/// Replaces this process's image entirely (`exec`, POSIX `execve`) so the user's terminal ends up
/// running the real `claude` binary with full TTY control, identical to running `claude attach
/// <id>` themselves. Only returns at all if `exec` itself failed to start (e.g. permissions) —
/// on success there is no "after" to return to.
///
/// `config_dir` must be the *lease owner's* registered `CLAUDE_CONFIG_DIR` (looked up by
/// `lease.owner_profile`, never assumed to be the configured primary — a handoff can leave a
/// fallback profile holding the lease). Without it, `claude attach` falls back to whatever
/// `CLAUDE_CONFIG_DIR` this process inherited from its parent shell — typically the default
/// account, not the isolated profile that actually owns the background job registry entry — so
/// attach fails with "No job matching '<id>'" even though the session is live under the correct
/// profile (dogfood-found: M6). Set only on the child's environment (`Command::env`), never on
/// this process's own, so credential isolation between profiles is preserved and no global state
/// is mutated.
#[cfg(unix)]
fn exec_claude_attach(
    executable: &Path,
    config_dir: &Path,
    short_id: &str,
) -> Result<CommandOutput, Error> {
    use std::os::unix::process::CommandExt as _;
    let error = std::process::Command::new(executable)
        .arg("attach")
        .arg(short_id)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .exec();
    Err(Error::Io {
        path: executable.to_path_buf(),
        source: error,
    })
}

#[cfg(not(unix))]
fn exec_claude_attach(
    executable: &Path,
    config_dir: &Path,
    short_id: &str,
) -> Result<CommandOutput, Error> {
    let status = std::process::Command::new(executable)
        .arg("attach")
        .arg(short_id)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .status()
        .map_err(|_| Error::ProviderCommandFailed)?;
    std::process::exit(status.code().unwrap_or(1));
}

/// M4.2/M4.3/M4.4/M4.5, revised post-M6: `relay claude` — the normal daily entry point for
/// *starting* a new Relay-managed conversation. `relay claude = claude + Relay supervision`: it
/// always launches a fresh session under the configured primary profile via `perform_launch`
/// (M2B.5), auto-writes Herdr pane/workspace metadata when running inside a Herdr pane, then hands
/// the user a live interactive terminal via `claude attach` (M4.4) — never printing a session UUID
/// for the user to copy anywhere.
///
/// It never silently reattaches to an already-active session for this project — that changed the
/// product contract (M4 originally chose silent reattach so a daily `cd && relay claude` worked
/// regardless of state; the UX cost was that "start fresh" and "resume" were indistinguishable to
/// the user). A live existing session now fails closed with `Error::ManagedSessionAlreadyActive`,
/// pointing at `relay resume` (continue it) or `relay claude --new` (the explicit escape hatch:
/// safely stop it via the same stop-and-verify machinery `relay switch`/recovery use, confirm it
/// is gone, then start fresh — see the `--new` handling below). A *stale* lease (owner process
/// confirmed dead) never blocks anything; `perform_launch` already recovers that case on its own.
fn run_claude(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &ClaudeArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let project_dir = match &args.project_dir {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let canonical_project = std::fs::canonicalize(&project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;

    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let primary = args
        .profile
        .clone()
        .or_else(|| preferences.primary_profile.clone())
        .ok_or(Error::AdoptionIdentityRequired)?;
    let fallback: Vec<ProfileName> = if args.fallback.is_empty() {
        preferences.fallback_profiles.clone()
    } else {
        args.fallback.clone()
    };

    let registered = service.list()?;
    let primary_profile = registered
        .iter()
        .find(|profile| profile.name == primary)
        .ok_or_else(|| Error::ProfileNotFound(primary.to_string()))?;

    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: None,
    };
    // M4.7: safe reauthentication for the primary; fallback unauthenticated is a warning only.
    let (primary_auth, _) = friendly_auth_state(primary_profile, &executables);
    if primary_auth != "authenticated" {
        if !json_mode {
            println!(
                "Profile \"{primary}\" needs Claude authentication.\n\nOpening Claude login..."
            );
        }
        run_claude_auth_subcommand(
            args.claude_executable.as_deref(),
            &primary_profile.config_dir,
            "login",
        )?;
        let report = verify_authenticated(
            &primary_profile.config_dir,
            args.claude_executable.as_deref(),
        )?;
        if !report.authenticated {
            return Err(Error::AuthenticationRequired);
        }
    }
    for fallback_name in &fallback {
        if let Some(fallback_profile) = registered
            .iter()
            .find(|profile| &profile.name == fallback_name)
        {
            let (fallback_auth, _) = friendly_auth_state(fallback_profile, &executables);
            if fallback_auth != "authenticated" && !json_mode {
                eprintln!(
                    "Warning: fallback profile '{fallback_name}' is not authenticated ({fallback_auth}); primary work may still proceed."
                );
            }
        }
    }

    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    let existing_lease = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?;

    let still_active_existing = match &existing_lease {
        Some(existing) => {
            let owner_config_dir = registered
                .iter()
                .find(|profile| profile.name == existing.owner_profile)
                .map(|profile| profile.config_dir.clone());
            match owner_config_dir {
                Some(config_dir) => confirm_not_active(
                    &config_dir,
                    &canonical_project,
                    &existing.session_id,
                    &existing.owner_process,
                    args.claude_executable.clone(),
                )
                .map(|confirmed_inactive| !confirmed_inactive)?,
                None => false,
            }
        }
        None => false,
    };

    // Product contract (post-M4): `relay claude` ALWAYS starts a new Relay-managed conversation —
    // it never silently reattaches to a live one (that's `relay resume`'s job now). A genuinely
    // live existing session blocks a plain `relay claude` outright; `--new` is the explicit,
    // opt-in escape hatch that safely stops it first. A *stale* lease (owner process confirmed
    // dead) never blocks anything, with or without `--new` — `perform_launch` below already
    // handles that case by overwriting it once its own liveness recheck agrees.
    if still_active_existing {
        let existing = existing_lease
            .as_ref()
            .expect("still_active_existing implies Some");
        if args.new {
            // The explicit escape hatch: authoritatively stop the *current owner's* writer (which
            // may not be `primary` — a prior handoff can leave a fallback profile holding it) via
            // the same stop-and-verify machinery `relay switch`/recovery already use, and never
            // return `Ok` until quiescence is confirmed. A failure here propagates and stops
            // right here — no launch is attempted, so a failed stop can never leave two writers.
            let owner_profile = registered
                .iter()
                .find(|profile| profile.name == existing.owner_profile)
                .ok_or_else(|| Error::ProfileNotFound(existing.owner_profile.to_string()))?;
            let owner_ports = providers::ports_for(owner_profile.provider, &executables);
            owner_ports.stopper.stop_and_verify(
                &owner_profile.config_dir,
                &canonical_project,
                &existing.session_id,
                Some(&existing.owner_process),
            )?;
        } else {
            return Err(Error::ManagedSessionAlreadyActive(
                existing.owner_profile.to_string(),
            ));
        }
    }

    if !json_mode {
        println!(
            "Agent Relay\nProject: {}\nProfile: {}\nStarting new managed Claude session...",
            project_display_name(&canonical_project),
            primary
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }

    // By this point either there was never a live existing writer, or `--new` just safely
    // stopped it — `perform_launch`'s own liveness recheck (inside its orchestration lock) is the
    // final authority and fails closed if anything raced in the meantime, so this can never
    // create a second writer.
    let message = resolve_initial_message(&args.message)?;
    let lease = perform_launch(
        service,
        paths,
        &primary,
        &canonical_project,
        &message,
        args.claude_executable.as_deref(),
    )?;

    // M4.3/M4.5: automatic Herdr metadata, only when actually running inside a Herdr pane.
    let herdr_env = std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
    let mut herdr_bound = false;
    if herdr_env {
        if let Ok(pane_id) = std::env::var("HERDR_PANE_ID") {
            let herdr_bin = std::env::var_os("HERDR_BIN_PATH").map(PathBuf::from);
            if let Ok(herdr_client) = HerdrCliClient::discover(herdr_bin.as_deref()) {
                let fallback_value = fallback
                    .iter()
                    .map(ProfileName::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let mut pane_tokens: Vec<(&str, &str)> = vec![("relay_profile", primary.as_str())];
                if !fallback_value.is_empty() {
                    pane_tokens.push(("relay_profile_fallback", fallback_value.as_str()));
                }
                pane_tokens.push(("relay_session_id", lease.session_id.as_str()));
                if herdr_client
                    .set_pane_tokens(&pane_id, "agent-relay", &pane_tokens)
                    .is_ok()
                {
                    herdr_bound = true;
                }
            }
        }
    }

    if args.no_attach {
        let human = format!(
            "Profile: {}\nNew session started.\nHerdr metadata: {}\n\nAttach with:\n    relay resume",
            primary,
            if herdr_bound {
                "written"
            } else if herdr_env {
                "not written (see relay doctor)"
            } else {
                "not connected"
            },
        );
        return success(
            "claude",
            human,
            json!({
                "profile": primary.as_str(),
                "fallback": fallback.iter().map(ProfileName::to_string).collect::<Vec<_>>(),
                "session_id": lease.session_id,
                "background_job": lease.provider_handle,
                "herdr_bound": herdr_bound,
                // Always true: `relay claude` never silently reattaches to an existing session
                // any more (see `Error::ManagedSessionAlreadyActive`/`--new`) — kept as a stable
                // field for existing JSON consumers rather than removed.
                "new_session": true,
            }),
        );
    }

    let short_id = lease
        .provider_handle
        .clone()
        .ok_or(Error::MalformedProviderOutput)?;
    // Bug found dogfooding M6: attach must use the *lease owner's* config_dir, not `primary`'s —
    // after a handoff the owner may be a fallback profile.
    let owner_config_dir = registered
        .iter()
        .find(|profile| profile.name == lease.owner_profile)
        .map(|profile| profile.config_dir.clone())
        .ok_or_else(|| Error::ProfileNotFound(lease.owner_profile.to_string()))?;
    let inspector = ClaudeInspector::discover(args.claude_executable.as_deref())?;
    exec_claude_attach(inspector.executable(), &owner_config_dir, &short_id)
}

/// M4.1: the interactive first-run wizard. Every step reuses existing, already-tested machinery
/// (`ClaudeInspector`, `service.add`/`AdoptExisting`, `plan_install`/`apply_install`,
/// `herdr_install::{plan,apply}_install`) — this function only sequences prompts around them and
/// saves the result to `preferences.toml`. Safe to re-run: it detects and offers to reuse existing
/// profiles/integrations (M4.9) rather than starting over.
fn run_setup(
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
    match &claude_version {
        Ok(version) => println!("  Claude Code        \u{2713} ({version})"),
        Err(_) => println!("  Claude Code        \u{2717} not found"),
    }
    // M6: detected only — never required. A user with no Codex CLI installed sees exactly the
    // pre-M6 wizard; the Codex-profile prompt below only appears when this succeeds.
    let codex_executable = args.codex_executable.as_deref();
    let codex_version = relay_provider_codex::CodexInspector::discover(codex_executable)
        .and_then(|inspector| inspector.inspect_version());
    match &codex_version {
        Ok(version) => println!("  Codex CLI          \u{2713} ({version})"),
        Err(_) => println!("  Codex CLI          (not found; optional)"),
    }
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
    if claude_version.is_err() {
        println!(
            "\nClaude Code was not found. Install it first: https://docs.claude.com/en/docs/claude-code"
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
                    codex: None,
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
        let use_codex = codex_available
            && prompt_yes_no(
                &format!("Is '{name}' a Codex profile? (no = Claude)"),
                false,
            )?;
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
            let report = match inspect_existing_claude(paths, &config_dir, true, claude_executable)
            {
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
    let enable_usage = prompt_yes_no("\nEnable automatic quota detection?", true)?;
    if enable_usage {
        for name in std::iter::once(&primary).chain(fallback.iter()) {
            let Some(profile) = registered.iter().find(|profile| &profile.name == name) else {
                continue;
            };
            install_usage_integration_interactive(profile, claude_executable)?;
        }
    }
    preferences.usage_integration_enabled = Some(enable_usage);

    // --- Step 5: Herdr ---
    let enable_herdr = if herdr_probe.is_some() {
        prompt_yes_no("\nEnable Herdr integration?", true)?
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
    let human = format!(
        "Agent Relay is ready.\n\nPrimary:  {}\nFallback: {}\n\nAutomatic usage detection: {}\nHerdr integration: {}\n\nStart working with:\n\n    relay claude",
        primary,
        if fallback.is_empty() {
            "(none)".to_owned()
        } else {
            fallback
                .iter()
                .map(ProfileName::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        },
        if enable_usage { "enabled" } else { "disabled" },
        if enable_herdr { "enabled" } else { "disabled" },
    );
    success(
        "setup",
        human,
        json!({
            "primary": primary.as_str(),
            "fallback": fallback.iter().map(ProfileName::to_string).collect::<Vec<_>>(),
            "usage_integration_enabled": enable_usage,
            "herdr_enabled": enable_herdr,
        }),
    )
}

/// Shared by the interactive and non-interactive setup paths: installs the usage integration for
/// one profile via the unchanged `plan_install`/`apply_install`, explaining (never silently
/// bypassing) an unverified Claude Code version per M4.1 Step 4.
fn install_usage_integration_interactive(
    profile: &Profile,
    claude_executable: Option<&Path>,
) -> Result<(), Error> {
    let capabilities = match assess_installed(claude_executable, &profile.config_dir) {
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
                let capabilities =
                    assess_installed(args.claude_executable.as_deref(), &profile.config_dir)?;
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
    success(
        "setup",
        format!("Configured. Primary: {primary}"),
        json!({
            "primary": primary.as_str(),
            "fallback": args.fallback.iter().map(ProfileName::to_string).collect::<Vec<_>>(),
            "usage_integration_enabled": preferences.usage_integration_enabled,
            "herdr_enabled": preferences.herdr_enabled,
        }),
    )
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
    executables: &providers::ExecutableOverrides,
) -> Result<Box<dyn Provider>, Error> {
    let kind = service
        .list()?
        .into_iter()
        .find(|profile| &profile.name == name)
        .map(|profile| profile.provider);
    Ok(match kind {
        Some(ProviderKind::Claude) => Box::new(ClaudeAdoptionProvider::discover(
            executables.claude.as_deref(),
        )?),
        Some(ProviderKind::Codex) => providers::provider_backend(ProviderKind::Codex, executables)?,
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
