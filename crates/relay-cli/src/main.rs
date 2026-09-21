mod agent_cmd;
mod auto_handoff;
mod badge;
mod control;
mod live;
mod preferences;
mod progress;
mod provider_args;
mod providers;
mod target;
mod terminal;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use relay_core::{
    AddProfileRequest, AuthenticationState, Error, IdentityMetadata, Profile, ProfileDirectory,
    ProfileName, ProfileService, ProfileSetupMode, Provider, ProviderKind, RelayPaths,
    automation::{
        AutomationDecision, AutomationPolicy, LedgerStore, ProfileCandidate, WatchCoordinator,
        WatchOutcome, WatchRequest, decide,
    },
    handoff::{
        HandoffCoordinator, HandoffRequest, JournalStore, LeaseStore, OrchestrationLock, ProjectId,
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
    version = env!("RELAY_VERSION"),
    about = "Supervise coding-agent sessions across isolated Claude and Codex profiles, and move work to the next eligible profile when one is exhausted"
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
    /// Advanced: stage a Claude session transcript from one profile into another by hand.
    Session(SessionArgs),
    /// Advanced: inspect a project's writer lease and orchestration lock.
    Lock(LockArgs),
    /// Advanced: run or inspect a crash-safe handoff between two Claude profiles by hand.
    Handoff(HandoffArgs),
    /// Advanced: decide the safe next action for an interrupted handoff.
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
    /// Advanced: start a background Claude session as this project's Relay-managed writer
    /// (scripting form of `relay claude`). Refuses if another live writer holds the project.
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
    /// Advanced: run one automatic-handoff evaluation by hand (Relay never runs a background
    /// monitor).
    Watch(WatchArgs),
    /// Claude usage integration (StopFailure hook + status line) and the optional Herdr plugin.
    /// Codex needs no installed integration.
    Integration(IntegrationArgs),
    /// Internal: commands Claude Code runs on behalf of an installed integration. Never fails the
    /// calling Claude session.
    #[command(hide = true)]
    Hook(HookArgs),
    /// First-run wizard: set up Claude and/or Codex profiles (whichever CLIs you have installed),
    /// choose one priority order, and optionally enable integrations. Safe to re-run any time;
    /// it reuses what already exists.
    Setup(SetupArgs),
    /// Start a new Relay-managed Claude conversation in this project and open Claude directly
    /// (type your first message inside Claude). Options after `--` go straight to `claude`.
    Claude(ClaudeArgs),
    /// Start a new Relay-managed Codex conversation in this project and open Codex directly.
    /// Options after `--` go straight to `codex`.
    Codex(CodexArgs),
    /// Show this project's current writer, fallback order and integrations at a glance.
    Status {
        #[arg(long = "project", value_name = "PATH")]
        project_dir: Option<PathBuf>,
    },
    /// List registered profiles with their provider, priority role and login state.
    Profiles,
    /// Log a profile in through its provider's own official login (Claude or Codex), in an
    /// isolated config home. An existing profile uses its registered provider; `--provider`
    /// chooses one for a brand-new profile name.
    Login {
        name: ProfileName,
        /// The provider for a brand-new profile. Omit it when only one of Claude Code / Codex is
        /// installed (that one is used); with both installed you are asked, or must pass this
        /// when there is no terminal to ask in.
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        codex_executable: Option<PathBuf>,
    },
    /// Log a profile out through its provider's own official logout. Relay never touches
    /// credential files itself.
    Logout {
        name: ProfileName,
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        codex_executable: Option<PathBuf>,
    },
    /// Explicitly move the current conversation to another profile, of the same provider or not.
    /// Claude → Claude continues the same session; anything involving Codex continues from a
    /// Relay state bundle in a new session.
    Switch(SwitchArgs),
    /// Continue this project's current Relay-managed conversation, on whichever profile and
    /// provider owns it now (native resume of the same Claude session / Codex thread).
    Resume(ResumeArgs),
}

#[derive(Debug, Args)]
struct SwitchArgs {
    /// The profile to move to. Omit it, in a terminal, to choose from a list (current, exhausted
    /// and unavailable profiles are shown but cannot be selected).
    target: Option<ProfileName>,
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
    /// Provider CLI arguments for the *target* profile's provider, after `--`, forwarded verbatim
    /// to its interactive session (never translated between providers).
    #[arg(last = true, allow_hyphen_values = true, value_name = "PROVIDER_ARGS")]
    provider_args: Vec<String>,
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
    /// Provider CLI arguments after `--`, for the provider that currently owns the session;
    /// replaces that provider's stored arguments for this project. Omit to reuse the stored ones.
    #[arg(last = true, allow_hyphen_values = true, value_name = "PROVIDER_ARGS")]
    provider_args: Vec<String>,
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
    /// Adopt an EXISTING Claude conversation instead of starting a new one: opens Claude's own
    /// resume picker (or resumes `SESSION_ID` directly), then brings exactly the conversation you
    /// pick under Relay — same session, no fork, no second conversation. To continue a
    /// conversation Relay already manages, use `relay resume`.
    #[arg(
        long,
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["message", "new", "no_attach"]
    )]
    resume: Option<String>,
    #[arg(long, value_name = "PATH")]
    claude_executable: Option<PathBuf>,
    /// Everything after `--` is forwarded verbatim to `claude`
    /// (`relay claude --profile work -- --model opus --dangerously-skip-permissions`).
    #[arg(last = true, allow_hyphen_values = true, value_name = "CLAUDE_ARGS")]
    provider_args: Vec<String>,
}

#[derive(Clone, Debug, Args)]
struct CodexArgs {
    /// An optional first message, typed into the interactive Codex session once it opens.
    message: Vec<String>,
    /// Use this Codex profile instead of the highest-priority configured one.
    #[arg(long)]
    profile: Option<ProfileName>,
    #[arg(long = "project-dir", value_name = "PATH")]
    project_dir: Option<PathBuf>,
    /// Create the writer lease and the Codex thread, then print a summary instead of opening the
    /// interactive session (`relay resume` opens it later).
    #[arg(long)]
    no_attach: bool,
    /// Explicitly replace an already-active Relay-managed session for this project (safely
    /// stopped and verified first), like `relay claude --new`.
    #[arg(long)]
    new: bool,
    #[arg(long, value_name = "PATH")]
    claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    codex_executable: Option<PathBuf>,
    /// Everything after `--` is forwarded verbatim to `codex`
    /// (`relay codex --profile codex-main -- --sandbox workspace-write`).
    #[arg(last = true, allow_hyphen_values = true, value_name = "CODEX_ARGS")]
    provider_args: Vec<String>,
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
    /// Herdr plugin integration.
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
    /// Internal: the `UserPromptSubmit` hook that answers `/relay status|switch|adopt` inside a
    /// Claude session without a model turn.
    Prompt {
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
    },
    /// Internal: the `SessionStart` hook `relay claude --resume` injects to adopt the exact
    /// conversation Claude just resumed.
    SessionStart {
        #[arg(long, value_name = "PATH")]
        config_dir: PathBuf,
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
    /// the same transactional `relay handoff run` machinery unchanged. Safe to invoke
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
    /// Internal: what the Claude `StopFailure` hook starts when a rate-limit failure hits the
    /// session Relay manages. Runs `watch run`'s exact evaluation, retrying (bounded) only while
    /// the answer is "no action needed" — the statusline snapshot that corroborates a limit can
    /// land just after the failure itself. Not a daemon: it exits on any decisive outcome or when
    /// the attempts are used up.
    #[command(hide = true)]
    Auto {
        #[arg(long)]
        profile: ProfileName,
        #[arg(long, required = true)]
        fallback: Vec<ProfileName>,
        #[arg(long = "project", value_name = "PATH")]
        project_dir: PathBuf,
        #[arg(long = "session")]
        session_id: String,
        #[arg(long, default_value_t = auto_handoff::DEFAULT_ATTEMPTS)]
        attempts: u32,
        #[arg(long, default_value_t = auto_handoff::DEFAULT_INTERVAL_MS)]
        interval_ms: u64,
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
    /// session (its transfer guarantees apply), launches and verifies the target, then moves the
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
    /// Inspect and resolve a session-transcript conflict on a target profile.
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
        /// Provider for the new profile.
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
        return run_hook(hook, &cli);
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
fn run_hook(hook: &HookArgs, cli: &Cli) -> ExitCode {
    let HookCommand::Claude(claude) = &hook.command;
    let stdin = read_stdin_bounded(std::io::stdin());
    let now = current_unix_ms();
    match &claude.command {
        ClaudeHookCommand::StopFailure { config_dir } => {
            handle_stop_failure(config_dir, &stdin, now);
            // The evidence is recorded first so the evaluation this may start can see it.
            trigger_automatic_handoff(cli, config_dir, &stdin);
            ExitCode::SUCCESS
        }
        ClaudeHookCommand::Statusline { config_dir, chain } => {
            // The badge is best-effort and additive: any doubt about the session means no badge.
            let badge = hook_paths(cli)
                .and_then(|paths| badge::badge_for(&paths, &stdin))
                .map(|plain| badge::styled(&plain));
            let code =
                handle_statusline(config_dir, &stdin, now, chain.as_deref(), badge.as_deref());
            ExitCode::from(u8::try_from(code).unwrap_or(0))
        }
        ClaudeHookCommand::Prompt { config_dir } => {
            // Any prompt that is not `/relay …` passes through untouched, silently.
            if let Some(paths) = hook_paths(cli)
                && let Some(output) = agent_cmd::answer(&paths, config_dir, &stdin)
            {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        ClaudeHookCommand::SessionStart { config_dir } => {
            if let Some(paths) = hook_paths(cli)
                && let Some(output) = resume_adoption_hook(&paths, config_dir, &stdin)
            {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// Environment the supervising `relay claude --resume` sets for the Claude it launches, so that
/// the `SessionStart` hook acts only inside *that* launch and never in an unrelated session.
const ADOPT_PROFILE_ENV: &str = "RELAY_ADOPT_PROFILE";
const ADOPT_RESULT_ENV: &str = "RELAY_ADOPT_RESULT";
const ADOPT_SESSION_ENV: &str = "RELAY_ADOPT_SESSION";

/// The `SessionStart` half of `relay claude --resume`: Claude reports which conversation it just
/// resumed (picker or explicit id); Relay proves it structurally and adopts exactly that one.
/// Records the outcome for the waiting supervisor and tells the user in Claude's own UI.
fn resume_adoption_hook(paths: &RelayPaths, config_dir: &Path, stdin: &[u8]) -> Option<String> {
    let expected = ProfileName::new(std::env::var(ADOPT_PROFILE_ENV).ok()?).ok()?;
    let result_path = PathBuf::from(std::env::var_os(ADOPT_RESULT_ENV)?);
    let input = live::HookInput::parse(stdin)?;
    // Only the resume itself: `/clear`, compaction and fresh starts are not what was requested.
    if input.source.as_deref() != Some("resume") || result_path.exists() {
        return None;
    }
    let outcome = (|| -> Result<live::AdoptionOutcome, Error> {
        if let Ok(wanted) = std::env::var(ADOPT_SESSION_ENV)
            && input.session_id.as_deref() != Some(wanted.as_str())
        {
            return Err(Error::AdoptionRefused(
                "Claude resumed a different conversation than the one requested".to_owned(),
            ));
        }
        // Claude registers the running session a moment after `SessionStart`; wait briefly.
        let mut last = None;
        for _ in 0..30 {
            match live::identify(&input, config_dir, &live::HookEnv::from_process()) {
                Ok(session) => {
                    let service = ProfileService::new(paths.clone());
                    return live::adopt_claude(
                        &service,
                        paths,
                        &session,
                        Some(&expected),
                        &providers::ExecutableOverrides::default(),
                    );
                }
                Err(error) => last = Some(error),
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(last.unwrap_or(Error::ProviderUnsupported))
    })();
    let (ok, message) = match &outcome {
        Ok(live::AdoptionOutcome::Adopted { profile, .. })
        | Ok(live::AdoptionOutcome::AlreadyManaged { profile }) => (
            true,
            format!("Agent Relay: this conversation is now managed (profile {profile})."),
        ),
        Err(error) => (
            false,
            format!("Agent Relay did not adopt this conversation: {error}"),
        ),
    };
    let _ignored = std::fs::write(
        &result_path,
        json!({"ok": ok, "message": message}).to_string(),
    );
    Some(json!({"systemMessage": message}).to_string())
}

/// Starts a one-shot, detached automatic-handoff evaluation when a rate-limit `StopFailure` hook
/// fires for the session Relay manages (see [`auto_handoff`] for why this is the trigger). Best
/// effort and silent: a hook must never fail, print into, or block the Claude session it runs in.
/// The Relay roots a hook process should use: the global overrides when given, else the defaults.
fn hook_paths(cli: &Cli) -> Option<RelayPaths> {
    let discovered = RelayPaths::discover().ok()?;
    let config_root = cli
        .config_root
        .clone()
        .unwrap_or_else(|| discovered.config_root().to_path_buf());
    let state_root = cli
        .state_root
        .clone()
        .unwrap_or_else(|| discovered.state_root().to_path_buf());
    RelayPaths::new(config_root, state_root).ok()
}

fn trigger_automatic_handoff(cli: &Cli, config_dir: &Path, stdin: &[u8]) {
    let Some(paths) = hook_paths(cli) else {
        return;
    };
    let service = ProfileService::new(paths.clone());
    let Ok(Some(preferences)) = preferences::Preferences::load(paths.config_root()) else {
        return;
    };
    if let Some(plan) = auto_handoff::plan(&paths, &service, &preferences, config_dir, stdin) {
        auto_handoff::spawn_detached(&plan);
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
                &[],
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
            WatchCommand::Auto {
                profile,
                fallback,
                project_dir,
                session_id,
                attempts,
                interval_ms,
            } => run_watch_auto(
                cli,
                profile,
                fallback,
                project_dir,
                session_id,
                *attempts,
                *interval_ms,
            ),
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
        Command::Codex(args) => run_codex(&service, &paths, args, cli.json),
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
            provider.map(Into::into),
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

/// `relay watch auto`: `watch run`'s evaluation, retried a bounded number of times while it keeps
/// answering "no action needed". Every attempt is a full, ordinary `watch run` (so recovery,
/// cooldown, the per-window cap, the known-exhausted ledger and the orchestration lock all apply
/// exactly as they do for a manual run); this only decides whether to ask again.
fn run_watch_auto(
    cli: &Cli,
    profile: &ProfileName,
    fallback: &[ProfileName],
    project_dir: &Path,
    session_id: &str,
    attempts: u32,
    interval_ms: u64,
) -> Result<CommandOutput, Error> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut argv: Vec<OsString> = vec!["relay".into(), "--json".into()];
        if let Some(root) = &cli.config_root {
            argv.extend(["--config-root".into(), root.clone().into_os_string()]);
        }
        if let Some(root) = &cli.state_root {
            argv.extend(["--state-root".into(), root.clone().into_os_string()]);
        }
        argv.extend([
            "watch".into(),
            "run".into(),
            "--profile".into(),
            profile.as_str().into(),
        ]);
        for name in fallback {
            argv.extend(["--fallback".into(), name.as_str().into()]);
        }
        argv.extend([
            "--project".into(),
            project_dir.as_os_str().to_owned(),
            "--session".into(),
            session_id.into(),
        ]);
        let inner = Cli::try_parse_from(argv).map_err(|_| Error::ProviderUnsupported)?;
        let output = run(&inner)?;
        // Each attempt is logged as it happens (stderr is the triggered run's log file), so
        // "did it fire and what did it decide" is answerable while the retries are still going.
        eprintln!("[attempt {attempt}/{attempts}] {}", output.human);
        let undecided = output.json["data"]["outcome"] == "no_action_needed";
        if !undecided || attempt >= attempts {
            return Ok(output);
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
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
    owner: &Profile,
    project_dir: &Path,
    session_id: &str,
    recorded_owner: &relay_core::handoff::ProcessIdentity,
    executables: &providers::ExecutableOverrides,
) -> Result<bool, Error> {
    // The OWNER's provider decides what "still running" means: a Codex-owned lease must never be
    // judged by Claude's session registry (or the reverse).
    let ports = providers::ports_for(owner.provider, executables);
    for attempt in 0..LAUNCH_LIVENESS_CONFIRM_ATTEMPTS {
        let verdict = ports.liveness.check(
            &owner.config_dir,
            project_dir,
            session_id,
            Some(recorded_owner),
        )?;
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
    extra_args: &[String],
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
            let owner = registered
                .iter()
                .find(|candidate| candidate.name == existing.owner_profile);
            let still_active = match owner {
                Some(owner) => confirm_not_active(
                    owner,
                    &canonical_project,
                    &existing.session_id,
                    &existing.owner_process,
                    &providers::ExecutableOverrides {
                        claude: claude_executable.map(Path::to_path_buf),
                        codex: None,
                    },
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
            extra_args,
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

/// Which provider a brand-new `relay login <name>` profile is for when `--provider` was not
/// given: never silently Claude. Exactly one installed CLI decides it; with both, an interactive
/// terminal is asked and anything else fails clearly.
fn resolve_new_profile_provider(
    executables: &providers::ExecutableOverrides,
    json_mode: bool,
) -> Result<ProviderKind, Error> {
    use std::io::IsTerminal as _;
    let claude = ClaudeInspector::discover(executables.claude.as_deref()).is_ok();
    let codex =
        relay_provider_codex::CodexInspector::discover(executables.codex.as_deref()).is_ok();
    let interactive =
        !json_mode && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    choose_provider(claude, codex, interactive, || {
        prompt_line("Provider for the new profile (claude/codex)", None)
    })
}

fn choose_provider(
    claude_installed: bool,
    codex_installed: bool,
    interactive: bool,
    mut ask: impl FnMut() -> Result<String, Error>,
) -> Result<ProviderKind, Error> {
    match (claude_installed, codex_installed) {
        (true, false) => Ok(ProviderKind::Claude),
        (false, true) => Ok(ProviderKind::Codex),
        (false, false) => Err(Error::ProviderExecutableMissing),
        (true, true) if !interactive => Err(Error::ProviderChoiceRequired),
        (true, true) => loop {
            match ask()?.trim().to_ascii_lowercase().as_str() {
                "claude" => return Ok(ProviderKind::Claude),
                "codex" => return Ok(ProviderKind::Codex),
                _ => println!("Please answer 'claude' or 'codex'."),
            }
        },
    }
}

fn run_login(
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
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };
    let target_name = match &args.target {
        Some(name) => name.clone(),
        None => choose_switch_target(
            paths,
            &registered,
            source,
            &executables,
            &canonical_project,
            json_mode,
        )?,
    };
    let target = registered
        .iter()
        .find(|profile| profile.name == target_name)
        .ok_or_else(|| Error::ProfileNotFound(target_name.to_string()))?;
    if target.name == source.name {
        return Err(Error::AlreadyCurrentWriter(target.name.to_string()));
    }
    provider_args::validate(target.provider, &args.provider_args)?;

    let target_backend = providers::provider_backend(target.provider, &executables)?;
    let target_status = service.status(&target.name, target_backend.as_ref())?;
    if target_status.authentication != AuthenticationState::Authenticated {
        return Err(Error::AuthenticationRequired);
    }
    // Known before anything is stopped: the account behind the target must still be the one
    // registered for it.
    if !target_status.identity_matches {
        return Err(Error::IdentityMismatch);
    }
    // An explicit switch to Codex is preflighted before anything is committed: an exhausted (or
    // unverifiable) target is refused outright — manual intent is never silently rerouted.
    if target.provider == ProviderKind::Codex {
        let usage = codex_preflight(target, &executables, &canonical_project, json_mode);
        if usage.state.is_blocking() {
            return Err(Error::TargetProfileExhausted(target.name.to_string()));
        }
        if usage.state == UsageState::Unknown {
            return Err(Error::CodexUsageUnverified(target.name.to_string()));
        }
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

    // Explicit arguments are for the TARGET's provider only and replace that provider's stored
    // ones; nothing is ever translated from the source provider's arguments.
    let mut stored_args = provider_args::ProviderArgs::load(&project_state_dir)?;
    if !args.provider_args.is_empty() {
        stored_args.set(target.provider, args.provider_args.clone());
        stored_args.save(&project_state_dir)?;
    }
    match target.provider {
        ProviderKind::Codex => {
            let command = plan_codex_resume(
                providers::executable_override(ProviderKind::Codex, &executables),
                &target.config_dir,
                &canonical_project,
                &new_session_id,
                stored_args.for_provider(ProviderKind::Codex),
                None,
            )?;
            run_managed_terminal(
                &ContinuationContext::new(
                    service,
                    paths,
                    &canonical_project,
                    args.claude_executable.clone(),
                    args.codex_executable.clone(),
                    json_mode,
                )?,
                command,
                target.name.clone(),
                None,
            )
        }
        ProviderKind::Claude | ProviderKind::Fake => success(
            "switch",
            format!("{human}\n\nContinue it with:\n    relay resume"),
            journal,
        ),
    }
}

/// Bare `relay switch`: choose the target in a terminal-native list. The list is the shared
/// target model (global priority order; the current writer is marked and not selectable;
/// exhausted, disabled or unverifiable profiles are shown as unavailable). Cancelling changes
/// nothing, and the chosen name then takes the exact same path as `relay switch <profile>`.
fn choose_switch_target(
    paths: &RelayPaths,
    registered: &[Profile],
    source: &Profile,
    executables: &providers::ExecutableOverrides,
    project: &Path,
    json_mode: bool,
) -> Result<ProfileName, Error> {
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    if json_mode || !target::interactive() {
        let shallow = target::build_targets(
            registered,
            &preferences,
            &source.name,
            executables,
            project,
            false,
        );
        return Err(Error::SwitchPickerNeedsTerminal(target::describe_rows(
            &shallow,
        )));
    }
    let progress = progress::Progress::start("Checking profiles…", json_mode);
    let targets = target::build_targets(
        registered,
        &preferences,
        &source.name,
        executables,
        project,
        true,
    );
    progress.finish();
    if !targets.iter().any(target::SwitchTarget::selectable) {
        return Err(Error::NoEligibleProfile(source.name.to_string()));
    }
    match target::pick(&targets, &source.name) {
        Ok(Some(index)) => Ok(targets[index].name.clone()),
        Ok(None) => Err(Error::SwitchCancelled),
        Err(_) => Err(Error::SwitchPickerNeedsTerminal(target::describe_rows(
            &targets,
        ))),
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
    let mut lease = LeaseStore::at_path(project_state_dir.join("lease.json"))
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
    let mut resolved_profile = lease.owner_profile.clone();
    let registered = service.list()?;
    let mut profile = registered
        .iter()
        .find(|profile| profile.name == resolved_profile)
        .ok_or_else(|| Error::ProfileNotFound(resolved_profile.to_string()))?;

    // Immediate structured Codex preflight: an already-exhausted Codex thread is handed off NOW
    // (the same evaluation, hierarchy, ledger and transaction the periodic check would start)
    // instead of launching Codex into a quota failure and waiting for the next poll. `Unknown`
    // never triggers a handoff.
    if profile.provider == ProviderKind::Codex {
        let executables = providers::ExecutableOverrides {
            claude: args.claude_executable.clone(),
            codex: args.codex_executable.clone(),
        };
        let usage = codex_preflight(profile, &executables, &canonical_project, json_mode);
        if usage.state.is_blocking() {
            let preferences =
                preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
            let fallback = auto_handoff::hierarchy_without(&preferences, &profile.name, |name| {
                registered.iter().any(|candidate| &candidate.name == name)
            });
            if fallback.is_empty() {
                if !json_mode {
                    eprintln!(
                        "Agent Relay: Codex profile '{}' is exhausted and no fallback profile is configured.",
                        profile.name
                    );
                }
            } else {
                let outcome = evaluate_handoff_now(
                    paths,
                    &profile.name,
                    &fallback,
                    &canonical_project,
                    &lease.session_id,
                    args.claude_executable.as_deref(),
                )?;
                if !json_mode {
                    println!("{}\n", outcome.human);
                }
                if let Some(reloaded) = LeaseStore::at_path(project_state_dir.join("lease.json"))
                    .load()?
                    .filter(|reloaded| reloaded.owner_profile != profile.name)
                {
                    lease = reloaded;
                    resolved_profile = lease.owner_profile.clone();
                    profile = registered
                        .iter()
                        .find(|candidate| candidate.name == resolved_profile)
                        .ok_or_else(|| Error::ProfileNotFound(resolved_profile.to_string()))?;
                }
            }
        } else if usage.state == UsageState::Unknown && !json_mode {
            eprintln!(
                "Agent Relay: could not verify Codex usage for '{}'; resuming without an automatic handoff decision.",
                profile.name
            );
        }
    }

    // Explicit arguments replace the owner provider's stored ones for this project (the other
    // provider's are untouched); otherwise the stored ones are reused.
    let mut stored_args = provider_args::ProviderArgs::load(&project_state_dir)?;
    if !args.provider_args.is_empty() {
        provider_args::validate(profile.provider, &args.provider_args)?;
        stored_args.set(profile.provider, args.provider_args.clone());
    }

    // Resolved before printing anything: on `AmbiguousSessionLiveness` this must fail closed
    // without ever claiming to be "resuming" a session it then can't safely continue.
    let command = plan_terminal_for_lease(
        profile,
        &lease,
        &canonical_project,
        args.claude_executable.as_deref(),
        args.codex_executable.as_deref(),
        &stored_args,
    )?;
    if !args.provider_args.is_empty() {
        stored_args.save(&project_state_dir)?;
    }

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

    run_managed_terminal(
        &ContinuationContext::new(
            service,
            paths,
            &canonical_project,
            args.claude_executable.clone(),
            args.codex_executable.clone(),
            json_mode,
        )?,
        command,
        resolved_profile,
        None,
    )
}

/// The interactive command that safely continues `lease` under its *owner's* provider and
/// isolated config directory: Codex is unconditionally `NATIVE_RESUME`; Claude is decided by
/// [`resolve_claude_resume_action`] (attach to a live background job, native resume otherwise, or
/// fail closed on ambiguous liveness). Shared by `relay resume` and by the automatic continuation
/// after a handoff, so both always pick the identical command for the identical lease.
fn plan_terminal_for_lease(
    profile: &Profile,
    lease: &relay_core::handoff::WriterLease,
    canonical_project: &Path,
    claude_executable: Option<&Path>,
    codex_executable: Option<&Path>,
    stored_args: &provider_args::ProviderArgs,
) -> Result<terminal::TerminalCommand, Error> {
    // Only the OWNER's provider's arguments are ever used here; the other provider's stay unread.
    let provider_args = stored_args.for_provider(profile.provider);
    match profile.provider {
        ProviderKind::Codex => plan_codex_resume(
            codex_executable,
            &profile.config_dir,
            canonical_project,
            &lease.session_id,
            provider_args,
            None,
        ),
        ProviderKind::Claude | ProviderKind::Fake => {
            match resolve_claude_resume_action(&profile.config_dir, claude_executable, lease)? {
                ClaudeResumeAction::Attach(short_id) => {
                    let inspector = ClaudeInspector::discover(claude_executable)?;
                    Ok(plan_claude_attach(
                        inspector.executable(),
                        &profile.config_dir,
                        &short_id,
                    ))
                }
                ClaudeResumeAction::NativeResume => plan_claude_resume(
                    claude_executable,
                    &profile.config_dir,
                    canonical_project,
                    &lease.session_id,
                    provider_args,
                ),
            }
        }
    }
}

/// Everything [`run_managed_terminal`] needs to continue a conversation on whichever profile
/// owns the project's lease *now*, without re-deriving anything from the configured primary.
struct ContinuationContext<'a> {
    service: &'a ProfileService,
    project_state_dir: PathBuf,
    canonical_project: PathBuf,
    claude_executable: Option<PathBuf>,
    codex_executable: Option<PathBuf>,
    json_mode: bool,
    paths: RelayPaths,
    preferences: preferences::Preferences,
    /// `relay claude --resume`: where the `SessionStart` hook records whether the resumed
    /// conversation was adopted, so a failure can be explained when the session ends.
    adopt_result: Option<PathBuf>,
}

/// How often a supervised *Codex* session asks Codex's structured usage interface whether it is
/// exhausted (Codex has no limit event to hook). Seconds; `RELAY_CODEX_POLL_SECS=0` disables.
const CODEX_POLL_DEFAULT_SECS: u64 = 120;
const CODEX_POLL_ENV: &str = "RELAY_CODEX_POLL_SECS";

impl<'a> ContinuationContext<'a> {
    fn new(
        service: &'a ProfileService,
        paths: &RelayPaths,
        canonical_project: &Path,
        claude_executable: Option<PathBuf>,
        codex_executable: Option<PathBuf>,
        json_mode: bool,
    ) -> Result<Self, Error> {
        let project_id = ProjectId::for_canonical_path(canonical_project)?;
        Ok(Self {
            service,
            project_state_dir: paths.project_state_dir(&project_id),
            canonical_project: canonical_project.to_path_buf(),
            claude_executable,
            codex_executable,
            json_mode,
            paths: paths.clone(),
            preferences: preferences::Preferences::load(paths.config_root())?.unwrap_or_default(),
            adopt_result: None,
        })
    }
}

/// How many times one terminal invocation will follow the conversation across handoffs before it
/// stops and leaves the rest to an explicit `relay resume` (main -> fallback1 -> fallback2 ->
/// fallback3 is already more than a realistic priority list).
const MAX_CONTINUATIONS: usize = 4;

/// Runs an interactive provider session in the user's terminal as a child of Relay and, if the
/// project's writer lease moves to a different profile while (or right after) it runs — i.e. an
/// automatic or manual handoff completed — continues the *same conversation* on the new owner
/// with no command for the user to discover.
///
/// This is deliberately not a daemon and not a usage poller: it lives only as long as the user's
/// own interactive session, only re-reads its own project's lease, never starts or stops any
/// writer itself (every ownership change still goes through the `HandoffCoordinator` under the
/// orchestration lock), and treats anything unexpected — a missing/unreadable lease, a handoff
/// that never settles, an ambiguous-liveness lease — as a reason to stop and hand control back,
/// never to guess.
fn run_managed_terminal(
    context: &ContinuationContext<'_>,
    first: terminal::TerminalCommand,
    first_owner: ProfileName,
    on_first_spawn: Option<&dyn Fn(u32)>,
) -> Result<CommandOutput, Error> {
    let control = control::ControlDir::for_project(&context.project_state_dir);
    let code = run_managed_terminal_inner(context, first, first_owner, on_first_spawn, &control);
    control.clear_supervisor();
    // An in-agent switch that failed after the session was stopped: say why, right here.
    if !context.json_mode
        && let Some(last) = control.last_result()
        && !last.ok
        && current_unix_ms().saturating_sub(last.unix_ms) < 120_000
    {
        eprintln!(
            "\nAgent Relay: {} — run `relay resume` to continue.",
            last.message
        );
    }
    if let Some(result) = &context.adopt_result {
        report_adoption_outcome(context, result);
    }
    std::process::exit(code?)
}

/// Keeps `supervisor.json` describing the conversation this terminal currently supervises (it
/// only appears once a lease for the launched profile exists, e.g. after an adoption).
fn publish_supervisor_record(
    control: &control::ControlDir,
    lease_store: &LeaseStore,
    owner: &ProfileName,
) {
    if let Ok(Some(lease)) = lease_store.load()
        && &lease.owner_profile == owner
    {
        let current = control.live_supervisor();
        if current
            .as_ref()
            .is_none_or(|record| record.session_id != lease.session_id)
        {
            control.publish_supervisor(owner.as_str(), &lease.session_id);
        }
    }
}

/// Answers one in-agent `/relay switch` request. The request must name the current lease's
/// session and owner and come from the very process this terminal runs; the target is vetted
/// (enabled, usage, login, identity) *before* anything is promised. The switch itself then runs as
/// the ordinary `relay switch <target> --no-attach` on a helper thread (the transaction stops the
/// running session, which this supervisor must keep reaping), and this terminal follows the new
/// owner exactly as it follows an automatic handoff.
fn serve_control_request(
    context: &ContinuationContext<'_>,
    control: &control::ControlDir,
    lease_store: &LeaseStore,
    child_pid: u32,
) -> Option<std::thread::JoinHandle<()>> {
    let request = control.take_request()?;
    let refuse = |message: &str| {
        control.respond(&control::Response {
            id: request.id.clone(),
            ok: false,
            message: format!("Agent Relay refused: {message}"),
        });
        None
    };
    let control::RequestKind::Switch { target } = &request.request;
    let Ok(Some(lease)) = lease_store.load() else {
        return refuse("there is no active Relay conversation for this project");
    };
    if lease.session_id != request.session_id
        || lease.owner_profile.as_str() != request.owner_profile
    {
        return refuse("that request is stale — the conversation has moved on");
    }
    if child_pid == 0 || request.caller_pid != child_pid {
        return refuse("the request did not come from the session this terminal is running");
    }
    let Ok(registered) = context.service.list() else {
        return refuse("Relay could not read its profiles");
    };
    let Some(profile) = registered
        .iter()
        .find(|profile| profile.name.as_str() == target)
    else {
        return refuse(&format!("'{target}' is not a registered profile"));
    };
    let executables = providers::ExecutableOverrides {
        claude: context.claude_executable.clone(),
        codex: context.codex_executable.clone(),
    };
    let row = target::row_for(
        profile,
        &lease.owner_profile,
        0,
        &executables,
        &context.canonical_project,
        true,
    );
    if row.current {
        return refuse(&format!("'{target}' already holds this conversation"));
    }
    if let Some(reason) = row
        .unavailable
        .or_else(|| target::verification_reason(context.service, profile, &executables))
    {
        return refuse(&format!("'{target}' is unavailable ({reason})"));
    }
    let source = registered
        .iter()
        .find(|candidate| candidate.name == lease.owner_profile);
    let source_provider = source.map_or(ProviderKind::Claude, |candidate| candidate.provider);
    // The same preflight the transaction runs before it stops anything: a foreseeable refusal
    // (another writer in this project, a missing session) is answered now, with the session
    // untouched, instead of after the agent has been closed.
    if let Some(source) = source
        && source_provider == ProviderKind::Claude
        && profile.provider == ProviderKind::Claude
        && let Some(stager) = providers::ports_for(ProviderKind::Claude, &executables).stager
        && let Err(error) = stager.preflight(
            &source.config_dir,
            &context.canonical_project,
            &lease.session_id,
            Some(&lease.owner_process),
        )
    {
        return refuse(&format!(
            "the switch cannot start safely ({error}); the session was left running"
        ));
    }
    control.respond(&control::Response {
        id: request.id.clone(),
        ok: true,
        message: format!(
            "Agent Relay: switching to '{target}' ({}). This terminal reopens the conversation there in a moment.",
            if source_provider == ProviderKind::Claude && profile.provider == ProviderKind::Claude {
                "the same Claude conversation continues"
            } else {
                "continues from Relay's state bundle in a new session"
            }
        ),
    });
    // Let the acknowledgement render in the agent before its session is stopped.
    std::thread::sleep(std::time::Duration::from_millis(400));

    let program = std::env::current_exe().ok()?;
    let mut command = std::process::Command::new(program);
    command
        .arg("--json")
        .arg("--config-root")
        .arg(context.paths.config_root())
        .arg("--state-root")
        .arg(context.paths.state_root())
        .args(["switch", target.as_str(), "--no-attach", "--project-dir"])
        .arg(&context.canonical_project)
        .stdin(std::process::Stdio::null());
    if let Some(claude) = &context.claude_executable {
        command.arg("--claude-executable").arg(claude);
    }
    if let Some(codex) = &context.codex_executable {
        command.arg("--codex-executable").arg(codex);
    }
    let control_dir = control::ControlDir::for_project(&context.project_state_dir);
    let target = target.clone();
    let lease_path = context.project_state_dir.join("lease.json");
    let preferences = context.preferences.clone();
    let profile_name = profile.name.clone();
    Some(std::thread::spawn(move || {
        let output = command.output();
        let succeeded = output.as_ref().is_ok_and(|output| output.status.success());
        let message = if succeeded {
            format!("switched to '{target}'")
        } else {
            let detail = output
                .as_ref()
                .ok()
                .and_then(|output| serde_json::from_slice::<Value>(&output.stderr).ok())
                .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "the switch did not complete".to_owned());
            format!("the switch to '{target}' failed: {detail}")
        };
        control_dir.record_last(succeeded, &message);
        if succeeded && let Ok(Some(lease)) = LeaseStore::at_path(lease_path).load() {
            let fallback: Vec<ProfileName> =
                auto_handoff::hierarchy_without(&preferences, &profile_name, |_| true)
                    .into_iter()
                    .cloned()
                    .collect();
            bind_herdr_pane(&profile_name, &fallback, &lease.session_id);
        }
    }))
}

/// `relay claude --resume` ended: say why the conversation was not adopted, if it was not.
fn report_adoption_outcome(context: &ContinuationContext<'_>, result: &Path) {
    let recorded: Option<Value> = std::fs::read(result)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let _ignored = std::fs::remove_file(result);
    if context.json_mode {
        return;
    }
    match recorded {
        Some(value) if value["ok"] == true => {}
        Some(value) => eprintln!(
            "\n{}",
            value["message"]
                .as_str()
                .unwrap_or("Agent Relay did not adopt this conversation.")
        ),
        None => eprintln!(
            "\nAgent Relay did not adopt a conversation (none was resumed), so nothing is managed."
        ),
    }
}

fn run_managed_terminal_inner(
    context: &ContinuationContext<'_>,
    first: terminal::TerminalCommand,
    first_owner: ProfileName,
    on_first_spawn: Option<&dyn Fn(u32)>,
    control: &control::ControlDir,
) -> Result<i32, Error> {
    use std::io::Write as _;
    let lease_store = LeaseStore::at_path(context.project_state_dir.join("lease.json"));
    let lock = OrchestrationLock::at_path(context.project_state_dir.join("orchestration.lock"));
    let timing = terminal::Timing::default();
    let mut command = first;
    let mut owner = terminal::LeaseOwner(first_owner);
    // Failed in-agent switches whose source session has already been reopened (each once).
    let mut restored_after: Vec<u64> = Vec::new();

    for continuation in 0..=MAX_CONTINUATIONS {
        let _ = std::io::stdout().flush();
        // A Codex session has no "limit reached" event to hang a trigger on, so while the user's
        // own terminal session is running, periodically start the same one-shot evaluation the
        // Claude hook starts. It only *evaluates*: any handoff goes through the unchanged
        // coordinator, lock, cooldown and ledger.
        let poll_secs = std::env::var(CODEX_POLL_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(CODEX_POLL_DEFAULT_SECS);
        let poll_action = || {
            let Ok(Some(lease)) = lease_store.load() else {
                return;
            };
            let Ok(registered) = context.service.list() else {
                return;
            };
            let Some(profile) = registered
                .iter()
                .find(|candidate| candidate.name == lease.owner_profile)
            else {
                return;
            };
            if let Some(plan) = auto_handoff::plan_poll(
                &context.paths,
                &context.preferences,
                &registered,
                profile,
                &lease,
                &context.canonical_project,
            ) {
                auto_handoff::spawn_detached(&plan);
            }
        };
        let owner_is_codex = context.service.list().is_ok_and(|registered| {
            registered
                .iter()
                .any(|profile| profile.name == owner.0 && profile.provider == ProviderKind::Codex)
        });
        // One 300ms tick serves both duties, each on its own cadence: the in-agent control
        // channel (a request is answered within a fraction of a second) and, for Codex only, the
        // periodic usage evaluation.
        let child_pid = std::cell::Cell::new(0_u32);
        let mut last_codex_poll = std::time::Instant::now();
        let mut pending_switch: Option<std::thread::JoinHandle<()>> = None;
        let mut tick_action = || {
            publish_supervisor_record(control, &lease_store, &owner.0);
            if pending_switch
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            {
                pending_switch = None;
            }
            if pending_switch.is_none() {
                pending_switch =
                    serve_control_request(context, control, &lease_store, child_pid.get());
            }
            if owner_is_codex
                && poll_secs > 0
                && last_codex_poll.elapsed() >= std::time::Duration::from_secs(poll_secs)
            {
                last_codex_poll = std::time::Instant::now();
                poll_action();
            }
        };
        let tick = Some(terminal::Tick {
            every: std::time::Duration::from_millis(300),
            action: &mut tick_action,
        });
        // The interactive process of a *continuation* is recorded in the lease too, so the next
        // stop-and-verify (and in-agent switch) can identify it exactly.
        let continuation_session = lease_store
            .load()
            .ok()
            .flatten()
            .map(|lease| lease.session_id);
        let record_spawn = |pid: u32| {
            child_pid.set(pid);
            match (continuation, on_first_spawn, &continuation_session) {
                (0, Some(first), _) => first(pid),
                (1.., _, Some(session)) => record_writer_process(&lease_store, &lock, session, pid),
                _ => {}
            }
        };
        let end = terminal::run_watching_lease(
            &command,
            &lease_store,
            &owner,
            &timing,
            tick,
            Some(&record_spawn),
        )
        .map_err(|source| Error::Io {
            path: command.program.clone(),
            source,
        })?;
        // The switch helper (if any) finishes recording its outcome before the decision below.
        if let Some(helper) = pending_switch.take() {
            let _ignored = helper.join();
        }
        let code = match end {
            terminal::TerminalEnd::Exited(code) => code,
            terminal::TerminalEnd::OwnerMoved => 0,
        };

        // A handoff stops the source session itself, so the session can end *before* the lease
        // has moved: wait for any in-flight transaction to settle before deciding.
        if !terminal::wait_until_settled(
            &lock,
            timing.settle_timeout,
            std::time::Duration::from_millis(500),
        ) {
            if !context.json_mode {
                eprintln!(
                    "\nAgent Relay: a handoff is still in progress; run `relay resume` once it completes."
                );
            }
            return Ok(code);
        }
        let Ok(Some(lease)) = lease_store.load() else {
            return Ok(code);
        };
        // A switch that failed AFTER the session was deliberately stopped, but before ownership
        // moved, leaves the lease exactly as it was: the same owner, the same session, no process.
        // Reopening that very session natively is safe and deterministic (one writer: nobody else
        // holds the project), so the terminal does that instead of leaving the user stranded.
        if terminal::LeaseOwner::of(&lease) == owner
            && continuation < MAX_CONTINUATIONS
            && control.last_result().is_some_and(|last| {
                !last.ok
                    && current_unix_ms().saturating_sub(last.unix_ms) < 120_000
                    && !restored_after.contains(&last.unix_ms)
            })
            && let Some(last) = control.last_result()
        {
            restored_after.push(last.unix_ms);
            let registered = context.service.list()?;
            if let Some(profile) = registered
                .iter()
                .find(|candidate| candidate.name == lease.owner_profile)
                && let Ok(reopened) = provider_args::ProviderArgs::load(&context.project_state_dir)
                    .and_then(|stored| {
                        plan_terminal_for_lease(
                            profile,
                            &lease,
                            &context.canonical_project,
                            context.claude_executable.as_deref(),
                            context.codex_executable.as_deref(),
                            &stored,
                        )
                    })
            {
                if !context.json_mode {
                    println!(
                        "\nAgent Relay: {} — reopening the same conversation on '{}' (nothing moved).",
                        last.message, lease.owner_profile
                    );
                }
                command = reopened;
                continue;
            }
        }
        if terminal::LeaseOwner::of(&lease) == owner || continuation == MAX_CONTINUATIONS {
            if continuation == MAX_CONTINUATIONS
                && terminal::LeaseOwner::of(&lease) != owner
                && !context.json_mode
            {
                eprintln!(
                    "\nAgent Relay: the conversation moved to '{}'; run `relay resume` to continue it.",
                    lease.owner_profile
                );
            }
            return Ok(code);
        }

        // Continue the same conversation on whoever owns it now — resolved from the lease, never
        // from the configured primary.
        let registered = context.service.list()?;
        let Some(profile) = registered
            .iter()
            .find(|candidate| candidate.name == lease.owner_profile)
        else {
            return Ok(code);
        };
        let next = match provider_args::ProviderArgs::load(&context.project_state_dir).and_then(
            |stored| {
                plan_terminal_for_lease(
                    profile,
                    &lease,
                    &context.canonical_project,
                    context.claude_executable.as_deref(),
                    context.codex_executable.as_deref(),
                    &stored,
                )
            },
        ) {
            Ok(next) => next,
            Err(error) => {
                if !context.json_mode {
                    eprintln!(
                        "\nAgent Relay: the conversation moved to '{}' but could not be continued automatically ({error}); run `relay resume`.",
                        lease.owner_profile
                    );
                }
                return Ok(code);
            }
        };
        if !context.json_mode {
            println!(
                "\nAgent Relay: the conversation moved from '{}' to '{}' — continuing on '{}'...",
                owner.0, lease.owner_profile, lease.owner_profile
            );
        }
        owner = terminal::LeaseOwner::of(&lease);
        command = next;
    }
    unreachable!("the loop above always returns")
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

/// Builds (does not run) `codex resume <thread-id>` under the profile's own `CODEX_HOME`.
/// Codex has no background-job/attach concept, so this is unconditional (`NATIVE_RESUME`).
fn verify_codex_thread(
    codex_executable: &Path,
    config_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
) -> Result<(), Error> {
    let not_verified = || Error::CodexThreadNotVerified(thread_id.to_owned());
    let identity =
        relay_provider_codex::app_server::read_thread(codex_executable, config_dir, thread_id)
            .map_err(|_| not_verified())?;
    if identity.id != thread_id {
        return Err(not_verified());
    }
    if let Some(cwd) = identity.cwd {
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if canonical(&cwd) != canonical(project_dir) {
            return Err(not_verified());
        }
    }
    Ok(())
}

fn plan_codex_resume(
    codex_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    extra_args: &[String],
    initial_message: Option<&str>,
) -> Result<terminal::TerminalCommand, Error> {
    let inspector = relay_provider_codex::CodexInspector::discover(codex_executable)?;
    // NATIVE_RESUME is only claimed for a thread Codex itself confirms: the id must exist in this
    // profile's own CODEX_HOME and belong to this project. (An interactive `codex resume` with a
    // missing or stale id can otherwise start a different thread without any clear failure.)
    verify_codex_thread(inspector.executable(), config_dir, project_dir, thread_id)?;
    let mut args: Vec<OsString> = vec!["resume".into(), thread_id.into()];
    args.extend(extra_args.iter().map(OsString::from));
    if let Some(message) = initial_message {
        // After `--` so a variadic option (`-i FILE…`) or a leading `-` can never swallow it.
        args.push("--".into());
        args.push(message.into());
    }
    Ok(terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args,
        envs: vec![("CODEX_HOME".into(), config_dir.into())],
        current_dir: Some(project_dir.to_path_buf()),
    })
}

/// Builds (does not run) an interactive `claude --resume <session-id>` under the given profile's
/// own `CLAUDE_CONFIG_DIR`.
fn plan_claude_resume(
    claude_executable: Option<&Path>,
    config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
    extra_args: &[String],
) -> Result<terminal::TerminalCommand, Error> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let mut args: Vec<OsString> = vec!["--resume".into(), session_id.into()];
    // The user's own arguments follow Relay's `--resume <id>` verbatim; the session is always the
    // one Relay chose (a conflicting flag was rejected up front).
    args.extend(extra_args.iter().map(OsString::from));
    Ok(terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args,
        envs: vec![("CLAUDE_CONFIG_DIR".into(), config_dir.into())],
        current_dir: Some(project_dir.to_path_buf()),
    })
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
    let owner_profile = lease.as_ref().and_then(|lease| {
        registered
            .iter()
            .find(|profile| profile.name == lease.owner_profile)
    });
    let session_state = if locked {
        "handoff in progress"
    } else if let Some(lease) = &lease {
        match owner_profile {
            // The lease owner's OWN provider decides liveness — never inferred from the
            // session/thread shape, never assumed to be Claude.
            Some(owner) => match owner_is_live(
                owner,
                &canonical,
                &lease.session_id,
                Some(&lease.owner_process),
                &providers::ExecutableOverrides::default(),
            ) {
                Ok(true) => "active",
                Ok(false) => "idle (last session ended)",
                Err(_) => "unknown (could not verify the session)",
            },
            None => "unknown (the owning profile is not registered)",
        }
    } else {
        "not started"
    };
    let session_label = match owner_profile.map(|profile| profile.provider) {
        Some(ProviderKind::Codex) => "Codex session",
        Some(ProviderKind::Claude | ProviderKind::Fake) => "Claude session",
        None => "Session",
    };

    let herdr_connected =
        std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));

    // The one global priority order, minus whoever is writing right now: the current writer is
    // never shown as its own fallback, and the primary is a fallback candidate once it is not the
    // writer (routing considers it first again after its window resets).
    let current_owner = lease
        .as_ref()
        .map_or_else(|| primary.clone(), |lease| lease.owner_profile.clone());
    let fallback_order: Vec<String> =
        auto_handoff::hierarchy_without(&preferences, &current_owner, |_| true)
            .into_iter()
            .map(ToString::to_string)
            .collect();
    let fallback_display = if fallback_order.is_empty() {
        "none".to_owned()
    } else {
        fallback_order.join(", ")
    };

    let human = format!(
        "Project: {}\n{}: {}\nCurrent profile: {}\nFallback: {}\nPrimary profile auth: {}\nAutomatic handoff: {}\nHerdr: {}",
        canonical.display(),
        session_label,
        session_state,
        current_owner,
        fallback_display,
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
            "session_provider": owner_profile.map(|profile| profile.provider.to_string()),
            "primary_profile": primary.as_str(),
            "primary_authenticated": primary_auth,
            "fallback_profiles": fallback_order,
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
/// Builds (does not run) that command. It is run through [`run_managed_terminal`] as a child in
/// the user's own terminal (full TTY, identical to typing `claude attach <id>` yourself) rather
/// than `exec`'d, so Relay can continue the conversation on a fallback profile if a handoff
/// happens while the user is attached.
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
fn plan_claude_attach(
    executable: &Path,
    config_dir: &Path,
    short_id: &str,
) -> terminal::TerminalCommand {
    terminal::TerminalCommand {
        program: executable.to_path_buf(),
        args: vec!["attach".into(), short_id.into()],
        envs: vec![("CLAUDE_CONFIG_DIR".into(), config_dir.into())],
        current_dir: None,
    }
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
/// The profile a provider-specific entrypoint starts: `--profile` when given (it must belong to
/// that provider), otherwise the highest-priority configured profile *of that provider* in the one
/// global order (`primary`, then the fallbacks). Deterministic, never prompts.
fn select_profile<'a>(
    registered: &'a [Profile],
    preferences: &preferences::Preferences,
    explicit: Option<&ProfileName>,
    expected: ProviderKind,
) -> Result<&'a Profile, Error> {
    let matches_provider = |provider: ProviderKind| match expected {
        ProviderKind::Codex => provider == ProviderKind::Codex,
        ProviderKind::Claude | ProviderKind::Fake => {
            matches!(provider, ProviderKind::Claude | ProviderKind::Fake)
        }
    };
    let label = match expected {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude | ProviderKind::Fake => "Claude",
    };
    if let Some(name) = explicit {
        let profile = registered
            .iter()
            .find(|candidate| &candidate.name == name)
            .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
        if !matches_provider(profile.provider) {
            return Err(Error::ProfileProviderMismatch {
                profile: name.to_string(),
                expected: label,
                actual: profile.provider.to_string(),
            });
        }
        return Ok(profile);
    }
    if preferences.primary_profile.is_none() && preferences.fallback_profiles.is_empty() {
        // Not set up at all: keep the long-standing "run relay setup" failure.
        return Err(Error::AdoptionIdentityRequired);
    }
    preferences
        .primary_profile
        .iter()
        .chain(preferences.fallback_profiles.iter())
        .find_map(|name| {
            registered
                .iter()
                .find(|candidate| &candidate.name == name && matches_provider(candidate.provider))
        })
        .ok_or(Error::NoProfileForProvider(label))
}

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

    provider_args::validate(ProviderKind::Claude, &args.provider_args)?;
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let registered = service.list()?;
    if let Some(session) = &args.resume {
        return run_claude_resume(
            service,
            paths,
            args,
            session,
            &canonical_project,
            &registered,
            &preferences,
            json_mode,
        );
    }
    let primary_profile = select_profile(
        &registered,
        &preferences,
        args.profile.as_ref(),
        ProviderKind::Claude,
    )?;
    let primary = primary_profile.name.clone();
    let fallback: Vec<ProfileName> = if args.fallback.is_empty() {
        auto_handoff::hierarchy_without(&preferences, &primary, |_| true)
            .into_iter()
            .cloned()
            .collect()
    } else {
        args.fallback.clone()
    };

    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: None,
    };
    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    let existing_lease = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?;

    // Everything from here until the provider takes over the terminal can be slow (session
    // liveness confirmation, `claude auth status`, stopping a previous session). One indicator
    // covers all of it, so the terminal never looks frozen.
    let mut progress = Some(progress::Progress::start(
        "Preparing Claude profile…",
        json_mode,
    ));

    let still_active_existing = match &existing_lease {
        Some(existing) => {
            let owner = registered
                .iter()
                .find(|profile| profile.name == existing.owner_profile);
            match owner {
                Some(owner) => confirm_not_active(
                    owner,
                    &canonical_project,
                    &existing.session_id,
                    &existing.owner_process,
                    &executables,
                )
                .map(|confirmed_inactive| !confirmed_inactive)?,
                None => false,
            }
        }
        None => false,
    };

    // Cheap, authoritative fast-fail: a live managed writer makes a plain `relay claude` impossible,
    // so say so before any slow provider work (auth inspection, login prompts). The lock-guarded
    // recheck inside the launch stays the final authority for races.
    if still_active_existing && !args.new {
        let existing = existing_lease
            .as_ref()
            .expect("still_active_existing implies Some");
        return Err(Error::ManagedSessionAlreadyActive {
            owner: existing.owner_profile.to_string(),
            entrypoint: "claude",
        });
    }

    // M4.7: safe reauthentication for the primary; fallback unauthenticated is a warning only.
    let (primary_auth, _) = friendly_auth_state(primary_profile, &executables);
    if primary_auth != "authenticated" {
        // The login flow is interactive: the indicator must not be drawing over it.
        drop(progress.take());
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
        progress = Some(progress::Progress::start(
            "Preparing Claude profile…",
            json_mode,
        ));
    }
    for fallback_name in &fallback {
        // Only Claude fallbacks are checked here: a Codex profile's login inspection
        // (`codex doctor`) takes ~13s, which would sit in front of the terminal handover for a
        // mere warning. `relay profiles` reports every profile's login state on demand.
        if let Some(fallback_profile) = registered
            .iter()
            .find(|profile| &profile.name == fallback_name)
            .filter(|profile| profile.provider != ProviderKind::Codex)
        {
            let (fallback_auth, _) = friendly_auth_state(fallback_profile, &executables);
            if fallback_auth != "authenticated" && !json_mode {
                let warning = format!(
                    "Warning: fallback profile '{fallback_name}' is not authenticated ({fallback_auth}); primary work may still proceed."
                );
                match &progress {
                    Some(progress) => progress.say(&warning),
                    None => eprintln!("{warning}"),
                }
            }
        }
    }

    // Product contract (post-M4): `relay claude` ALWAYS starts a new Relay-managed conversation —
    // it never silently reattaches to a live one (that's `relay resume`'s job now). A genuinely
    // live existing session blocks a plain `relay claude` outright; `--new` is the explicit,
    // opt-in escape hatch that safely stops it first. A *stale* lease (owner process confirmed
    // dead) never blocks anything, with or without `--new`.
    if still_active_existing {
        let existing = existing_lease
            .as_ref()
            .expect("still_active_existing implies Some");
        if args.new {
            if let Some(progress) = &progress {
                progress.set_label("Stopping the previous session…");
            }
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
            return Err(Error::ManagedSessionAlreadyActive {
                owner: existing.owner_profile.to_string(),
                entrypoint: "claude",
            });
        }
    }

    let header = format!(
        "Agent Relay\nProject: {}\nProfile: {}\nStarting new managed Claude session...",
        project_display_name(&canonical_project),
        primary
    );

    // `--no-attach` is the scripting form: it creates a background session, which needs its first
    // message up front. The interactive form below never asks Relay-side for one.
    if args.no_attach {
        let message = if args.message.is_empty() {
            // resolving may prompt: the indicator must not be drawing over it
            drop(progress.take());
            resolve_initial_message(&args.message)?
        } else {
            args.message.join(" ")
        };
        match &progress {
            Some(progress) => {
                progress.say(&header);
                progress.set_label("Starting Claude session…");
            }
            None if !json_mode => println!("{header}"),
            None => {}
        }
        let lease = perform_launch(
            service,
            paths,
            &primary,
            &canonical_project,
            &message,
            args.claude_executable.as_deref(),
            &args.provider_args,
        )?;
        drop(progress.take());
        provider_args::ProviderArgs::fresh_for(ProviderKind::Claude, args.provider_args.clone())
            .save(&project_state_dir)?;
        let herdr_bound = bind_herdr_pane(&primary, &fallback, &lease.session_id);
        let herdr_env = std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
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

    // Interactive fresh launch: Relay is a router/supervisor, so it gets the user INTO Claude and
    // gets out of the way — the first message is typed inside Claude. Relay assigns the session id
    // itself (`claude --session-id`, a supported flag) so it knows the native session before Claude
    // starts, records the writer lease under the orchestration lock first (a placeholder process
    // that liveness treats as "unverifiable = active", so no second writer can slip in), and fills
    // in the real process id the moment the child exists.
    match &progress {
        Some(progress) => progress.say(&header),
        None if !json_mode => println!("{header}"),
        None => {}
    }
    let session_id = new_session_uuid()?;
    let lease = begin_interactive_claude_lease(
        paths,
        &registered,
        primary_profile,
        &canonical_project,
        &session_id,
        &executables,
    )?;
    provider_args::ProviderArgs::fresh_for(ProviderKind::Claude, args.provider_args.clone())
        .save(&project_state_dir)?;
    bind_herdr_pane(&primary, &fallback, &lease.session_id);

    let inspector = ClaudeInspector::discover(args.claude_executable.as_deref())?;
    let message = args.message.join(" ");
    let mut command_args: Vec<OsString> = Vec::new();
    if !message.trim().is_empty() {
        // The optional first message stays *before* the user's own arguments so a variadic
        // option can never swallow it.
        command_args.push(message.into());
    }
    command_args.extend(["--session-id".into(), session_id.clone().into()]);
    command_args.extend(args.provider_args.iter().map(OsString::from));
    let command = terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args: command_args,
        envs: vec![(
            "CLAUDE_CONFIG_DIR".into(),
            primary_profile.config_dir.clone().into(),
        )],
        current_dir: Some(canonical_project.clone()),
    };

    let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));
    let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
    let record_process = |pid: u32| record_writer_process(&lease_store, &lock, &session_id, pid);
    drop(progress.take());
    let result = run_managed_terminal(
        &ContinuationContext::new(
            service,
            paths,
            &canonical_project,
            args.claude_executable.clone(),
            None,
            json_mode,
        )?,
        command,
        primary.clone(),
        Some(&record_process),
    );
    if result.is_err() {
        discard_pending_lease(&lease_store, &session_id);
    }
    result
}

/// `relay claude --resume [SESSION_ID]`: adopt an existing Claude conversation.
///
/// Claude's own resume flow does the choosing (its picker, or the explicit id). Relay learns which
/// conversation was actually chosen from Claude itself — the `SessionStart` hook reports the
/// resumed session — proves it structurally (see [`live`]) and only then creates the lease, in
/// place: the same session id, no fork, no copy. Everything before that is read-only checks.
#[allow(clippy::too_many_arguments)]
fn run_claude_resume(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &ClaudeArgs,
    session: &str,
    canonical_project: &Path,
    registered: &[Profile],
    preferences: &preferences::Preferences,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    if !session.is_empty() && !live::is_session_uuid(session) {
        return Err(Error::AdoptionRefused(format!(
            "'{session}' is not a Claude session id (run `relay claude --resume` with no id to \
             pick one)"
        )));
    }
    let profile = select_profile(
        registered,
        preferences,
        args.profile.as_ref(),
        ProviderKind::Claude,
    )?;
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: None,
    };
    let project_id = ProjectId::for_canonical_path(canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);

    let progress = progress::Progress::start("Checking the project's Relay state…", json_mode);
    // A live Relay writer means there is already a managed conversation: refuse rather than
    // ever creating a second writer or silently replacing it.
    if let Some(existing) = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?
        && let Some(owner) = registered
            .iter()
            .find(|candidate| candidate.name == existing.owner_profile)
        && !confirm_not_active(
            owner,
            canonical_project,
            &existing.session_id,
            &existing.owner_process,
            &executables,
        )?
    {
        return Err(Error::ManagedSessionAlreadyActive {
            owner: existing.owner_profile.to_string(),
            entrypoint: "claude",
        });
    }
    if !session.is_empty() && !claude_transcript_exists(&profile.config_dir, session) {
        return Err(Error::AdoptionRefused(format!(
            "profile '{}' has no saved conversation with that id",
            profile.name
        )));
    }
    let (auth, _) = friendly_auth_state(profile, &executables);
    if auth != "authenticated" {
        return Err(Error::AuthenticationRequired);
    }
    progress.finish();

    let inspector = ClaudeInspector::discover(args.claude_executable.as_deref())?;
    let relay = std::env::current_exe().map_err(|source| Error::Io {
        path: PathBuf::from("relay"),
        source,
    })?;
    let hook_command = format!(
        "{} --config-root {} --state-root {} hook claude session-start --config-dir {}",
        shell_quote(&relay.to_string_lossy()),
        shell_quote(&paths.config_root().to_string_lossy()),
        shell_quote(&paths.state_root().to_string_lossy()),
        shell_quote(&profile.config_dir.to_string_lossy()),
    );
    let settings = json!({"hooks": {"SessionStart": [{"hooks": [
        {"type": "command", "command": hook_command, "timeout": 30}
    ]}]}})
    .to_string();
    std::fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
        path: project_state_dir.clone(),
        source,
    })?;
    let result_path = project_state_dir.join(format!("adopt-{}.json", new_session_uuid()?));

    let mut command_args: Vec<OsString> = vec!["--resume".into()];
    if !session.is_empty() {
        command_args.push(session.into());
    }
    command_args.extend(["--settings".into(), settings.into()]);
    command_args.extend(args.provider_args.iter().map(OsString::from));
    let mut envs: Vec<(OsString, OsString)> = vec![
        (
            "CLAUDE_CONFIG_DIR".into(),
            profile.config_dir.clone().into(),
        ),
        (ADOPT_PROFILE_ENV.into(), profile.name.as_str().into()),
        (ADOPT_RESULT_ENV.into(), result_path.clone().into()),
    ];
    if !session.is_empty() {
        envs.push((ADOPT_SESSION_ENV.into(), session.into()));
    }
    let command = terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args: command_args,
        envs,
        current_dir: Some(canonical_project.to_path_buf()),
    };
    if !json_mode {
        println!(
            "Agent Relay\nProject: {}\nProfile: {}\nOpening Claude's resume flow — the conversation you pick will be adopted...",
            project_display_name(canonical_project),
            profile.name
        );
    }
    let mut context = ContinuationContext::new(
        service,
        paths,
        canonical_project,
        args.claude_executable.clone(),
        None,
        json_mode,
    )?;
    context.adopt_result = Some(result_path);
    provider_args::ProviderArgs::fresh_for(ProviderKind::Claude, args.provider_args.clone())
        .save(&project_state_dir)?;
    run_managed_terminal(&context, command, profile.name.clone(), None)
}

/// Whether profile `config_dir` holds a saved transcript for `session_id` in any project.
fn claude_transcript_exists(config_dir: &Path, session_id: &str) -> bool {
    std::fs::read_dir(config_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|project| project.path().join(format!("{session_id}.jsonl")).is_file())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Automatic Herdr metadata, only when actually running inside a Herdr pane. Returns whether the
/// pane tokens were written.
fn bind_herdr_pane(primary: &ProfileName, fallback: &[ProfileName], session_id: &str) -> bool {
    let herdr_env = std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
    if !herdr_env {
        return false;
    }
    let Ok(pane_id) = std::env::var("HERDR_PANE_ID") else {
        return false;
    };
    let herdr_bin = std::env::var_os("HERDR_BIN_PATH").map(PathBuf::from);
    let Ok(herdr_client) = HerdrCliClient::discover(herdr_bin.as_deref()) else {
        return false;
    };
    let fallback_value = fallback
        .iter()
        .map(ProfileName::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut pane_tokens: Vec<(&str, &str)> = vec![("relay_profile", primary.as_str())];
    if !fallback_value.is_empty() {
        pane_tokens.push(("relay_profile_fallback", fallback_value.as_str()));
    }
    pane_tokens.push(("relay_session_id", session_id));
    herdr_client
        .set_pane_tokens(&pane_id, "agent-relay", &pane_tokens)
        .is_ok()
}

/// A random RFC 4122 version-4 UUID, as `claude --session-id` requires.
fn new_session_uuid() -> Result<String, Error> {
    use std::io::Read as _;
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|source| Error::Io {
            path: PathBuf::from("/dev/urandom"),
            source,
        })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// Under the orchestration lock: refuse a still-live writer (the owner provider's own liveness),
/// then record the new writer lease for a session Relay is about to start interactively. The
/// process is a placeholder (pid 0, which every liveness check treats as unverifiable, i.e.
/// active) until [`record_writer_process`] fills in the real one.
fn begin_interactive_claude_lease(
    paths: &RelayPaths,
    registered: &[Profile],
    profile: &Profile,
    canonical_project: &Path,
    session_id: &str,
    executables: &providers::ExecutableOverrides,
) -> Result<relay_core::handoff::WriterLease, Error> {
    let project_id = ProjectId::for_canonical_path(canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    std::fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
        path: project_state_dir.clone(),
        source,
    })?;
    let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
    let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));
    lock.try_with(|| -> Result<relay_core::handoff::WriterLease, Error> {
        if let Some(existing) = lease_store.load()? {
            let still_active = match registered
                .iter()
                .find(|candidate| candidate.name == existing.owner_profile)
            {
                Some(owner) => confirm_not_active(
                    owner,
                    canonical_project,
                    &existing.session_id,
                    &existing.owner_process,
                    executables,
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
        let lease = relay_core::handoff::WriterLease::new(
            project_id.clone(),
            profile.name.clone(),
            relay_core::handoff::ProcessIdentity {
                pid: 0,
                start_time_fingerprint: None,
            },
            session_id.to_owned(),
            relay_core::handoff::TransactionId::generate(),
            current_unix_ms(),
        );
        lease_store.save(&lease)?;
        Ok(lease)
    })
}

/// Records the interactive provider's real process in the lease as soon as it exists (identity =
/// pid + start time, exactly what liveness checks and the verified stop use). Best effort with a
/// short retry: the lock may briefly be held by an evaluation.
fn record_writer_process(
    lease_store: &LeaseStore,
    lock: &OrchestrationLock,
    session_id: &str,
    pid: u32,
) {
    for _ in 0..40 {
        let attempt = lock.try_with(|| -> Result<(), Error> {
            if let Some(mut lease) = lease_store.load()?
                && lease.session_id == session_id
            {
                lease.owner_process = relay_core::handoff::ProcessIdentity::query(pid);
                lease_store.save(&lease)?;
            }
            Ok(())
        });
        if attempt.is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The interactive provider never started: drop the placeholder lease (only if it is still the
/// untouched placeholder for this session) so it cannot block the project.
fn discard_pending_lease(lease_store: &LeaseStore, session_id: &str) {
    if let Ok(Some(lease)) = lease_store.load()
        && lease.session_id == session_id
        && lease.owner_process.pid == 0
    {
        let _ignored = lease_store.clear();
    }
}

fn codex_preflight(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
    json_mode: bool,
) -> relay_core::usage::UsageObservation {
    // Starting `codex app-server` takes a moment: show an interactive human that Relay is working
    // (nothing at all in --json mode or when output is not a terminal).
    let progress = progress::Progress::start("Checking Codex availability…", json_mode);
    let observation = codex_usage_now(profile, executables, project_dir);
    progress.finish();
    observation
}

fn codex_usage_now(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
) -> relay_core::usage::UsageObservation {
    providers::usage_signal_for(ProviderKind::Codex, executables, false, None)
        .detect(&profile.config_dir, project_dir, "")
        .unwrap_or_else(|_| relay_core::usage::UsageObservation {
            state: UsageState::Unknown,
            evidence: relay_core::usage::UsageEvidence::ProviderRateLimitApi,
            detected_via: "codex usage check failed".to_owned(),
            observed_unix_ms: current_unix_ms(),
            reset_unix_ms: None,
        })
}

/// Runs the same one-shot automatic-handoff evaluation `relay watch run` performs, in-process, for
/// the given owner/session (so `relay resume` does not have to wait for the periodic check).
fn evaluate_handoff_now(
    paths: &RelayPaths,
    profile: &ProfileName,
    fallback: &[&ProfileName],
    project_dir: &Path,
    session_id: &str,
    claude_executable: Option<&Path>,
) -> Result<CommandOutput, Error> {
    let mut argv: Vec<OsString> = vec![
        "relay".into(),
        "--json".into(),
        "--config-root".into(),
        paths.config_root().into(),
        "--state-root".into(),
        paths.state_root().into(),
        "watch".into(),
        "run".into(),
        "--profile".into(),
        profile.as_str().into(),
    ];
    for name in fallback {
        argv.extend(["--fallback".into(), name.as_str().into()]);
    }
    argv.extend([
        "--project".into(),
        project_dir.as_os_str().to_owned(),
        "--session".into(),
        session_id.into(),
    ]);
    if let Some(claude) = claude_executable {
        argv.extend(["--claude-executable".into(), claude.as_os_str().to_owned()]);
    }
    let inner = Cli::try_parse_from(argv).map_err(|_| Error::ProviderUnsupported)?;
    run(&inner)
}

/// Pre-launch routing for a fresh `relay codex` whose chosen profile is already exhausted. There
/// is no Codex conversation yet, so this is NOT a handoff transaction: it is the ordinary global
/// hierarchy (the same `decide` the automatic path uses, minus the cooldown/loop guard that only
/// concern real handoffs) picking the first eligible other profile to start the new conversation on.
#[allow(clippy::too_many_arguments)]
fn route_exhausted_codex_start(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &CodexArgs,
    exhausted: &Profile,
    usage: relay_core::usage::UsageObservation,
    registered: &[Profile],
    preferences: &preferences::Preferences,
    executables: &providers::ExecutableOverrides,
    canonical_project: &Path,
    json_mode: bool,
    allow_reroute: bool,
    progress: progress::Progress,
) -> Result<CommandOutput, Error> {
    let no_eligible = || Error::NoEligibleProfile(exhausted.name.to_string());
    if !allow_reroute {
        return Err(no_eligible());
    }
    let source = ProfileCandidate {
        name: exhausted.name.clone(),
        provider: exhausted.provider,
        config_dir: exhausted.config_dir.clone(),
        identity_stable_id: Some(exhausted.expected_identity.stable_id.clone()),
        enabled: exhausted.enabled,
        healthy: true,
        usage,
    };
    progress.say(&format!("Codex profile '{}' is exhausted.", exhausted.name));
    progress.set_label("Finding the next eligible profile…");
    let mut candidates = Vec::new();
    for name in auto_handoff::hierarchy_without(preferences, &exhausted.name, |_| true) {
        let Some(candidate) = registered.iter().find(|profile| &profile.name == name) else {
            continue;
        };
        let candidate_usage =
            providers::usage_signal_for(candidate.provider, executables, false, None).detect(
                &candidate.config_dir,
                canonical_project,
                "",
            )?;
        candidates.push(ProfileCandidate {
            name: candidate.name.clone(),
            provider: candidate.provider,
            config_dir: candidate.config_dir.clone(),
            identity_stable_id: Some(candidate.expected_identity.stable_id.clone()),
            enabled: candidate.enabled,
            healthy: doctor_is_healthy(service, candidate, executables)?,
            usage: candidate_usage,
        });
    }
    let project_id = ProjectId::for_canonical_path(canonical_project)?;
    let mut ledger = LedgerStore::at_path(
        paths
            .project_state_dir(&project_id)
            .join("automation_state.json"),
    )
    .load()?;
    // Reset-pending / known-exhausted profiles stay skipped; past handoffs (cooldown, loop guard)
    // are about real transactions and do not apply to starting a fresh conversation.
    ledger.recent_handoffs.clear();
    let AutomationDecision::Handoff { target } = decide(
        current_unix_ms(),
        &source,
        &candidates,
        &ledger,
        &AutomationPolicy::default(),
    ) else {
        return Err(no_eligible());
    };
    let target_profile = registered
        .iter()
        .find(|profile| profile.name == target)
        .ok_or_else(|| Error::ProfileNotFound(target.to_string()))?;

    // Announced as a *decision* only: the launch itself prints its own "Starting…" line once it
    // has passed every check that could still stop it.
    progress.say(&format!(
        "Using next eligible profile: '{}'.\n",
        target_profile.name
    ));
    // The chosen entrypoint starts its own indicator immediately, so nothing is ever blank.
    progress.finish();
    let mut output = match target_profile.provider {
        ProviderKind::Codex => run_codex_inner(
            service,
            paths,
            &CodexArgs {
                profile: Some(target_profile.name.clone()),
                ..args.clone()
            },
            json_mode,
            false,
        )?,
        ProviderKind::Claude | ProviderKind::Fake => run_claude(
            service,
            paths,
            &ClaudeArgs {
                message: args.message.clone(),
                profile: Some(target_profile.name.clone()),
                fallback: Vec::new(),
                project_dir: args.project_dir.clone(),
                no_attach: args.no_attach,
                new: args.new,
                resume: None,
                claude_executable: args.claude_executable.clone(),
                // Codex's arguments are never translated to Claude.
                provider_args: Vec::new(),
            },
            json_mode,
        )?,
    };
    if let Some(data) = output.json.get_mut("data").and_then(Value::as_object_mut) {
        data.insert(
            "prelaunch_fallback".to_owned(),
            json!({
                "requested_profile": exhausted.name.as_str(),
                "reason": "exhausted",
                "routed_to": target_profile.name.as_str(),
                "handoff": false,
            }),
        );
    }
    // (In text mode the notice was already printed above, before the launch output.)
    Ok(output)
}

/// The Codex counterpart of [`perform_launch`]: under the project's orchestration lock, refuses a
/// still-active existing writer (judged by the *owner's* provider), creates a fresh Codex thread
/// (Relay's own arguments only) and records a new writer lease for it.
fn perform_codex_launch(
    service: &ProfileService,
    paths: &RelayPaths,
    profile: &Profile,
    canonical_project: &Path,
    executables: &providers::ExecutableOverrides,
) -> Result<relay_core::handoff::WriterLease, Error> {
    let registered = service.list()?;
    let project_id = ProjectId::for_canonical_path(canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    std::fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
        path: project_state_dir.clone(),
        source,
    })?;
    let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
    let lease_store = LeaseStore::at_path(project_state_dir.join("lease.json"));

    lock.try_with(|| -> Result<relay_core::handoff::WriterLease, Error> {
        if let Some(existing) = lease_store.load()? {
            let still_active = match registered
                .iter()
                .find(|candidate| candidate.name == existing.owner_profile)
            {
                Some(owner) => confirm_not_active(
                    owner,
                    canonical_project,
                    &existing.session_id,
                    &existing.owner_process,
                    executables,
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
        let launched = relay_provider_codex::launch_new_thread(
            &profile.config_dir,
            canonical_project,
            executables.codex.as_deref(),
        )?;
        let lease = relay_core::handoff::WriterLease::new(
            project_id.clone(),
            profile.name.clone(),
            launched
                .process
                .unwrap_or(relay_core::handoff::ProcessIdentity {
                    pid: 0,
                    start_time_fingerprint: None,
                }),
            launched.thread_id,
            relay_core::handoff::TransactionId::generate(),
            current_unix_ms(),
        );
        lease_store.save(&lease)?;
        Ok(lease)
    })
}

/// `relay codex`: start a NEW Relay-managed Codex conversation, symmetric with `relay claude`.
/// Profile choice is deterministic (see [`select_profile`]); the single-writer rules, `--new`
/// semantics and supervised terminal are the same; everything after `--` is forwarded verbatim to
/// the interactive `codex` session that continues the new thread.
fn run_codex(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &CodexArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    run_codex_inner(service, paths, args, json_mode, true)
}

fn run_codex_inner(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &CodexArgs,
    json_mode: bool,
    allow_reroute: bool,
) -> Result<CommandOutput, Error> {
    provider_args::validate(ProviderKind::Codex, &args.provider_args)?;
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
    let registered = service.list()?;
    let profile = select_profile(
        &registered,
        &preferences,
        args.profile.as_ref(),
        ProviderKind::Codex,
    )?;
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };
    let project_id = ProjectId::for_canonical_path(&canonical_project)?;
    let project_state_dir = paths.project_state_dir(&project_id);
    let existing_lease = LeaseStore::at_path(project_state_dir.join("lease.json")).load()?;

    // From here on every step can be slow (session-liveness confirmation, starting
    // `codex app-server`, scanning fallbacks, creating the thread). ONE indicator lives across all
    // of it — label changes, result lines print above it — so there is never an unexplained gap.
    let progress = progress::Progress::start(
        if existing_lease.is_some() {
            "Checking for an active session…"
        } else {
            "Checking Codex availability…"
        },
        json_mode,
    );

    // 1. Cheap, authoritative local invariant first: is a managed writer live for this project?
    //    (Read from Relay's lease and judged by the OWNER's own provider.) A live writer makes a
    //    plain `relay codex` impossible, so fail at once — before any Codex app-server round trip,
    //    before any routing message. The lock-guarded recheck inside `perform_codex_launch` stays
    //    the final authority for races.
    let still_active_existing = match &existing_lease {
        Some(existing) => match registered
            .iter()
            .find(|candidate| candidate.name == existing.owner_profile)
        {
            Some(owner) => confirm_not_active(
                owner,
                &canonical_project,
                &existing.session_id,
                &existing.owner_process,
                &executables,
            )
            .map(|confirmed_inactive| !confirmed_inactive)?,
            None => false,
        },
        None => false,
    };
    if still_active_existing && !args.new {
        let existing = existing_lease
            .as_ref()
            .expect("still_active_existing implies Some");
        return Err(Error::ManagedSessionAlreadyActive {
            owner: existing.owner_profile.to_string(),
            entrypoint: "codex",
        });
    }

    // 2. Immediate structured preflight — before anything is stopped, created or spent. Its
    //    authenticated rate-limit read also proves the profile is logged in, so the slow
    //    `codex doctor` inspection is only run afterwards, to explain a failure. With `--new` the
    //    existing writer is still untouched here: if the route turns out not to be viable it stays
    //    exactly as it was.
    progress.set_label("Checking Codex availability…");
    let usage = codex_usage_now(profile, &executables, &canonical_project);
    if usage.state.is_blocking() {
        return route_exhausted_codex_start(
            service,
            paths,
            args,
            profile,
            usage,
            &registered,
            &preferences,
            &executables,
            &canonical_project,
            json_mode,
            allow_reroute,
            progress,
        );
    }
    if usage.state == UsageState::Unknown {
        progress.set_label("Checking Codex login…");
        let (auth, _) = friendly_auth_state(profile, &executables);
        return Err(if auth == "authenticated" {
            Error::CodexUsageUnverified(profile.name.to_string())
        } else {
            Error::AuthenticationRequired
        });
    }

    // 3. Only now that a new conversation can actually start is the old writer (if `--new`)
    //    stopped, authoritatively and verified, before the replacement is created.
    if still_active_existing {
        progress.set_label("Stopping the previous session…");
        let existing = existing_lease
            .as_ref()
            .expect("still_active_existing implies Some");
        let owner_profile = registered
            .iter()
            .find(|candidate| candidate.name == existing.owner_profile)
            .ok_or_else(|| Error::ProfileNotFound(existing.owner_profile.to_string()))?;
        providers::ports_for(owner_profile.provider, &executables)
            .stopper
            .stop_and_verify(
                &owner_profile.config_dir,
                &canonical_project,
                &existing.session_id,
                Some(&existing.owner_process),
            )?;
    }

    progress.say(&format!(
        "Agent Relay\nProject: {}\nProfile: {}\nStarting new managed Codex session...",
        project_display_name(&canonical_project),
        profile.name
    ));
    progress.set_label("Creating the Codex session…");
    let lease = perform_codex_launch(service, paths, profile, &canonical_project, &executables)?;
    progress.finish();
    // A brand-new managed conversation: only Codex's arguments carry over to it.
    provider_args::ProviderArgs::fresh_for(ProviderKind::Codex, args.provider_args.clone())
        .save(&project_state_dir)?;

    let fallback: Vec<String> =
        auto_handoff::hierarchy_without(&preferences, &profile.name, |_| true)
            .into_iter()
            .map(ProfileName::to_string)
            .collect();
    if args.no_attach {
        let human = format!(
            "Profile: {}\nNew Codex session started (thread {}).\n\nOpen it with:\n    relay resume",
            profile.name, lease.session_id
        );
        return success(
            "codex",
            human,
            json!({
                "profile": profile.name.as_str(),
                "fallback": fallback,
                "session_id": lease.session_id,
                "new_session": true,
            }),
        );
    }

    let message = args.message.join(" ");
    let command = plan_codex_resume(
        providers::executable_override(ProviderKind::Codex, &executables),
        &profile.config_dir,
        &canonical_project,
        &lease.session_id,
        &args.provider_args,
        (!message.trim().is_empty()).then_some(message.as_str()),
    )?;
    run_managed_terminal(
        &ContinuationContext::new(
            service,
            paths,
            &canonical_project,
            args.claude_executable.clone(),
            args.codex_executable.clone(),
            json_mode,
        )?,
        command,
        profile.name.clone(),
        None,
    )
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
    let human = format!(
        "Agent Relay is ready.\n\nPrimary:  {}\nFallback: {}\n\nAutomatic usage detection: {}\nHerdr integration: {}\n\nStart a new managed conversation with:\n\n{}\n\nContinue the current conversation with:\n\n    relay resume",
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
        start_commands
            .iter()
            .map(|command| format!("    {command}"))
            .collect::<Vec<_>>()
            .join("\n"),
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
                // The usage integration is a Claude hook + status line; Codex needs none.
                if profile.provider == ProviderKind::Codex {
                    continue;
                }
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
            "start_commands": start_commands_for(
                std::iter::once(&primary)
                    .chain(args.fallback.iter())
                    .filter_map(|name| registered.iter().find(|profile| &profile.name == name))
                    .map(|profile| profile.provider),
            ),
        }),
    )
}

/// Whether `owner`'s session is live, judged by **that profile's own provider** (Claude: session
/// registry + pid fingerprint; Codex: recorded pid + processes under its `CODEX_HOME`). The one
/// place status/session-conflict paths dispatch on provider, so none of them can quietly ask
/// Claude about a Codex lease (or the reverse).
fn owner_is_live(
    owner: &Profile,
    project_dir: &Path,
    session_id: &str,
    recorded_owner: Option<&relay_core::handoff::ProcessIdentity>,
    executables: &providers::ExecutableOverrides,
) -> Result<bool, Error> {
    let ports = providers::ports_for(owner.provider, executables);
    Ok(ports
        .liveness
        .check(&owner.config_dir, project_dir, session_id, recorded_owner)?
        .active)
}

/// Checks whether a session is currently active for `target` using the same liveness mechanism the
/// handoff coordinator uses for that profile's provider, so `session conflict` commands never touch
/// a genuinely in-use target.
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
    owner_is_live(
        target,
        &canonical_project,
        session_id,
        recorded_owner.as_ref(),
        &providers::ExecutableOverrides {
            claude: claude_executable.map(Path::to_path_buf),
            codex: None,
        },
    )
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

#[cfg(test)]
mod tests {
    use super::{Error, ProviderKind, choose_provider};

    #[test]
    fn a_new_profile_provider_is_implied_by_the_only_installed_cli() {
        let never = || -> Result<String, Error> { panic!("must not ask") };
        assert_eq!(
            choose_provider(true, false, true, never).unwrap(),
            ProviderKind::Claude
        );
        assert_eq!(
            choose_provider(false, true, false, never).unwrap(),
            ProviderKind::Codex
        );
        assert!(matches!(
            choose_provider(false, false, true, never),
            Err(Error::ProviderExecutableMissing)
        ));
    }

    #[test]
    fn with_both_installed_it_asks_in_a_terminal_and_fails_clearly_otherwise() {
        assert!(matches!(
            choose_provider(true, true, false, || Ok("claude".to_owned())),
            Err(Error::ProviderChoiceRequired)
        ));
        let mut answers = ["maybe", "Codex"].into_iter();
        let chosen = choose_provider(true, true, true, || Ok(answers.next().unwrap().to_owned()));
        assert_eq!(chosen.unwrap(), ProviderKind::Codex);
    }
}
