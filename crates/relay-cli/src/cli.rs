//! Every `relay` CLI argument type: the top-level [`Cli`]/[`Command`], and each subcommand's own
//! `*Args`/`*Command` structs and enums. Purely declarative (clap derives only) — command
//! *behavior* lives in [`crate::commands`], one module per command family.

use std::path::PathBuf;

use crate::auto_handoff;
use clap::{Args, Parser, Subcommand, ValueEnum};
use relay_core::{ProfileName, ProviderKind, usage::UsageState};

#[derive(Debug, Parser)]
#[command(
    name = "relay",
    version = env!("RELAY_VERSION"),
    about = "Supervise coding-agent sessions across isolated Claude and Codex profiles, and move work to the next eligible profile when one is exhausted"
)]
pub(crate) struct Cli {
    /// Emit a stable machine-readable JSON envelope.
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// Override Agent Relay's configuration root.
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) config_root: Option<PathBuf>,

    /// Override Agent Relay's state root.
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) state_root: Option<PathBuf>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
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
    /// Claude usage hooks, the Codex Relay skill, and the optional Herdr plugin.
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
    /// Explicitly move a Relay session (conversation) to another profile, of the same provider or
    /// not. With no profile you choose from a list; with several active sessions you choose which
    /// one (or pass `--session`). Claude → Claude continues the same session; anything involving
    /// Codex continues from a Relay state bundle in a new session.
    Switch(SwitchArgs),
    /// Internal: the `$relay switch` Codex skill's own side-channel to the Relay process already
    /// supervising this session (see `crate::control`) — never invoked directly by a user, and
    /// never itself performs the switch. Reports whether a verified supervisor exists and, if so,
    /// its answer; the skill falls back to the manual `relay switch --no-attach` instructions only
    /// when it does not.
    #[command(hide = true)]
    SwitchRequest(SwitchRequestArgs),
    /// Continue a closed (dormant) Relay session of this project on the profile it last ran on
    /// (native resume of the same Claude session / Codex thread). With several you choose, or pass
    /// `--session`; an active session is never started twice.
    Resume(ResumeArgs),
    /// Bring an ALREADY-RUNNING Claude conversation under Relay from outside it — no in-session
    /// `/relay adopt` required. Exists because a Claude process started before Relay's `/relay`
    /// command was installed never sees it (Claude only loads custom commands at session start),
    /// so the in-session path is unreachable for a conversation that predates the install. Every
    /// fact comes from Claude's own structural session registry and transcript layout, exactly as
    /// the in-session adoption hook requires — never a guess from a profile name. Refuses (leaves
    /// everything untouched) on any ambiguity; never restarts or stops the Claude process.
    Adopt(AdoptArgs),
    /// Is Agent Relay actually ready to save you when your current account runs out? Checks every
    /// configured profile's authentication and project trust, the Claude usage integration, provider CLI version
    /// support, and the automatic-handoff preference — the same facts `relay setup`'s completion
    /// screen and `relay status`'s "Automatic handoff" line report, kept in one place so they can
    /// never disagree.
    Doctor(DoctorArgs),
    /// Explain, in plain language, what Relay's automatic-handoff decision currently is and why —
    /// from the same durable state (usage, ledger, target eligibility) automatic handoff itself
    /// reads. Never guesses beyond what that state actually shows.
    Why(WhyArgs),
    /// A readable timeline of recent Relay activity for this project (session starts, exhaustion,
    /// handoffs, recoveries) — derived entirely from existing durable state, never a new log.
    History(HistoryArgs),
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Project whose unattended handoff readiness to check (defaults to the current directory).
    #[arg(long = "project", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct WhyArgs {
    #[arg(long = "project", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    /// Which Relay session to explain (an id or unambiguous prefix from `relay status`). Needed
    /// only when the project has several active sessions and there is no terminal to ask in.
    #[arg(long, value_name = "ID")]
    pub(crate) session: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryArgs {
    #[arg(long = "project", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    /// Which Relay session's history to show. Needed only when the project has several sessions
    /// and there is no terminal to ask in.
    #[arg(long, value_name = "ID")]
    pub(crate) session: Option<String>,
    /// How many recent events to show.
    #[arg(long, default_value_t = 10)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub(crate) struct AdoptArgs {
    /// The exact native Claude session id to adopt (see the Claude pane's own status, or
    /// `claude agents --json` under the owning profile).
    #[arg(long)]
    pub(crate) session: String,
    /// Pin the search to this registered profile instead of searching every registered Claude
    /// profile (NativeDefault included) for the one that structurally owns this live session.
    #[arg(long)]
    pub(crate) profile: Option<ProfileName>,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct SwitchArgs {
    /// The profile to move to. Omit it, in a terminal, to choose from a list (current, exhausted
    /// and unavailable profiles are shown but cannot be selected).
    pub(crate) target: Option<ProfileName>,
    /// Which Relay session to move (an id or unambiguous prefix from `relay status`). Needed
    /// only when the project has several active sessions and there is no terminal to ask in.
    #[arg(long, value_name = "ID")]
    pub(crate) session: Option<String>,
    #[arg(long = "project-dir", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    /// Stop after the transaction completes; print a status summary instead of exec'ing an
    /// interactive continuation.
    #[arg(long)]
    pub(crate) no_attach: bool,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_executable: Option<PathBuf>,
    /// Provider CLI arguments for the *target* profile's provider, after `--`, forwarded verbatim
    /// to its interactive session (never translated between providers).
    #[arg(last = true, allow_hyphen_values = true, value_name = "PROVIDER_ARGS")]
    pub(crate) provider_args: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SwitchRequestArgs {
    /// The profile to request a switch to. Unlike `relay switch`, this always names a profile —
    /// the interactive picker has no meaning over this non-interactive, machine-answered channel.
    pub(crate) target: ProfileName,
    #[arg(long = "project", value_name = "PATH")]
    pub(crate) project_dir: PathBuf,
    /// The exact Relay session this request is on behalf of (from `RELAY_SESSION_ID`) — cross-
    /// checked against the live supervisor's own record, never merely trusted.
    #[arg(long)]
    pub(crate) session: String,
}

#[derive(Debug, Args)]
pub(crate) struct ResumeArgs {
    /// Optional filter: only sessions whose current or last profile is this one. Normally omitted.
    /// With one resumable session it is resumed directly; with several, you choose (or pass
    /// `--session`).
    pub(crate) profile: Option<ProfileName>,
    /// Resume exactly this Relay session (an id or unambiguous prefix from `relay status`). With
    /// several resumable sessions and no terminal to ask in, this is required.
    #[arg(long, value_name = "ID")]
    pub(crate) session: Option<String>,
    #[arg(long = "project-dir", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_executable: Option<PathBuf>,
    /// Provider CLI arguments after `--`, for the provider that currently owns the session;
    /// replaces that provider's stored arguments for this project. Omit to reuse the stored ones.
    #[arg(last = true, allow_hyphen_values = true, value_name = "PROVIDER_ARGS")]
    pub(crate) provider_args: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
    /// Skip all prompts; requires the flags below instead. Fails closed (a clear error) rather
    /// than guessing if a value this needs is missing.
    #[arg(long)]
    pub(crate) non_interactive: bool,
    /// Show the same technical detail `relay doctor`-style output would (config dirs, identity
    /// pins' stable ids, capability status) instead of the plain-language summary.
    #[arg(long)]
    pub(crate) verbose: bool,
    /// Non-interactive only: the primary profile name (must already be registered, or created
    /// via a separate `relay login`/adoption step first).
    #[arg(long)]
    pub(crate) primary: Option<ProfileName>,
    /// Non-interactive only: fallback profiles in priority order.
    #[arg(long)]
    pub(crate) fallback: Vec<ProfileName>,
    /// Non-interactive only: enable/disable the Claude usage integration for the selected
    /// profiles without prompting.
    #[arg(long)]
    pub(crate) usage_integration: Option<bool>,
    /// Non-interactive only: enable/disable the Herdr integration without prompting.
    #[arg(long)]
    pub(crate) herdr: Option<bool>,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    /// Explicit Codex executable, primarily for controlled validation.
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_executable: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ClaudeArgs {
    /// The first message for the new session. `relay claude` always starts a fresh Relay-managed
    /// conversation — see `relay resume` to continue an existing one instead.
    pub(crate) message: Vec<String>,
    /// Override the configured primary profile for this run only.
    #[arg(long)]
    pub(crate) profile: Option<ProfileName>,
    /// Override the configured fallback order for this run only.
    #[arg(long)]
    pub(crate) fallback: Vec<ProfileName>,
    #[arg(long = "project-dir", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    /// Stop after creating/confirming the writer lease and (if applicable) Herdr metadata;
    /// print a status summary instead of attaching interactively. Used by scripts/tests and by
    /// environments with no real TTY to attach to.
    #[arg(long)]
    pub(crate) no_attach: bool,
    /// Deprecated and unnecessary: every `relay claude` already starts a NEW Relay session and
    /// never stops or replaces any other one. Accepted (and ignored) for old scripts.
    #[arg(long, hide = true)]
    pub(crate) new: bool,
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
    pub(crate) resume: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    /// Everything after `--` is forwarded verbatim to `claude`
    /// (`relay claude --profile work -- --model opus --dangerously-skip-permissions`).
    #[arg(last = true, allow_hyphen_values = true, value_name = "CLAUDE_ARGS")]
    pub(crate) provider_args: Vec<String>,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct CodexArgs {
    /// An optional first message, typed into the interactive Codex session once it opens.
    pub(crate) message: Vec<String>,
    /// Use this Codex profile instead of the highest-priority configured one.
    #[arg(long)]
    pub(crate) profile: Option<ProfileName>,
    #[arg(long = "project-dir", value_name = "PATH")]
    pub(crate) project_dir: Option<PathBuf>,
    /// Create the writer lease and the Codex thread, then print a summary instead of opening the
    /// interactive session (`relay resume` opens it later).
    #[arg(long)]
    pub(crate) no_attach: bool,
    /// Deprecated and unnecessary: every `relay codex` already starts a NEW Relay session and
    /// never stops or replaces any other one. Accepted (and ignored) for old scripts.
    #[arg(long, hide = true)]
    pub(crate) new: bool,
    #[arg(long, value_name = "PATH")]
    pub(crate) claude_executable: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_executable: Option<PathBuf>,
    /// Everything after `--` is forwarded verbatim to `codex`
    /// (`relay codex --profile codex-main -- --sandbox workspace-write`).
    #[arg(last = true, allow_hyphen_values = true, value_name = "CODEX_ARGS")]
    pub(crate) provider_args: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct IntegrationArgs {
    #[command(subcommand)]
    pub(crate) command: IntegrationCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IntegrationCommand {
    /// Claude Code usage integration.
    Claude(ClaudeIntegrationArgs),
    /// Codex `$relay` skill (usage polling works independently).
    Codex(CodexIntegrationArgs),
    /// Herdr plugin integration.
    Herdr(HerdrIntegrationArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CodexIntegrationArgs {
    #[command(subcommand)]
    pub(crate) command: CodexIntegrationCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CodexIntegrationCommand {
    /// Install the `$relay` skill in this profile; restart Codex to discover it.
    Install {
        #[arg(long)]
        profile: ProfileName,
    },
    /// Show whether this profile has the Relay skill.
    Status {
        #[arg(long)]
        profile: ProfileName,
    },
    /// Remove the unmodified Relay skill; preserve user edits.
    Uninstall {
        #[arg(long)]
        profile: ProfileName,
    },
}

#[derive(Debug, Args)]
pub(crate) struct HerdrIntegrationArgs {
    #[command(subcommand)]
    pub(crate) command: HerdrIntegrationCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HerdrIntegrationCommand {
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
pub(crate) struct ClaudeIntegrationArgs {
    #[command(subcommand)]
    pub(crate) command: ClaudeIntegrationCommand,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(crate) struct IntegrationTarget {
    /// A registered Relay profile whose isolated Claude config directory is modified.
    #[arg(long, conflicts_with = "native_default")]
    pub(crate) profile: Option<ProfileName>,
    /// An explicit, isolated Claude config directory (never `~/.claude` — use
    /// `--native-default` for that, since Claude's own auth lookup differs by mode even for the
    /// identical path).
    #[arg(long, value_name = "PATH", conflicts_with = "native_default")]
    pub(crate) config_dir: Option<PathBuf>,
    /// Target Claude's own native-default account (`~/.claude`, launched with
    /// `CLAUDE_CONFIG_DIR` left unset) instead of a profile or an explicit directory.
    #[arg(long)]
    pub(crate) native_default: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ClaudeIntegrationCommand {
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
pub(crate) struct HookArgs {
    #[command(subcommand)]
    pub(crate) command: HookCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HookCommand {
    Claude(ClaudeHookArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ClaudeHookArgs {
    #[command(subcommand)]
    pub(crate) command: ClaudeHookCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ClaudeHookCommand {
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
pub(crate) struct WatchArgs {
    #[command(subcommand)]
    pub(crate) command: WatchCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WatchCommand {
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
    /// Explicitly clear a project's session automation ledgers (cooldown, handoff counters, and
    /// known-exhausted lists). This does not clear durable provider-account exhaustion unless
    /// `--provider-account` names the exact currently verified profile identity.
    Clear {
        #[arg(long = "project", value_name = "PATH")]
        project_dir: PathBuf,
        /// Also clear the durable exhaustion record for this profile's currently verified
        /// provider identity. Without this flag, only per-session project automation state is
        /// cleared.
        #[arg(long = "provider-account", value_name = "PROFILE")]
        provider_account: Option<ProfileName>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum SimulateUsageArg {
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
pub(crate) enum ProviderArg {
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
pub(crate) struct LockArgs {
    #[command(subcommand)]
    pub(crate) command: LockCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LockCommand {
    /// Report whether a project's orchestration lock is currently held and who its writer
    /// lease says owns it. Advisory only: there is an inherent check-then-report race.
    Status {
        #[arg(long, value_name = "PATH")]
        project_dir: PathBuf,
        /// Report the top-level lock/lease of the Relay session holding this provider-native
        /// conversation (used by the Herdr plugin, which knows its pane's conversation).
        #[arg(long, value_name = "ID")]
        native_session: Option<String>,
    },
}

#[derive(Debug, Args)]
pub(crate) struct HandoffArgs {
    #[command(subcommand)]
    pub(crate) command: HandoffCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HandoffCommand {
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
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    pub(crate) command: SessionCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SessionCommand {
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
pub(crate) struct ConflictArgs {
    #[command(subcommand)]
    pub(crate) command: ConflictCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConflictCommand {
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
pub(crate) struct ProfileArgs {
    #[command(subcommand)]
    pub(crate) command: ProfileCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProfileCommand {
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
    /// Rename a profile's registered label everywhere Relay references it (registry,
    /// preferences, session/ledger/handoff history) — never its authentication, identity pin, or
    /// provider config directory. Refuses if the profile currently has a live active session.
    Rename {
        old_name: ProfileName,
        new_name: ProfileName,
    },
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
        /// Required unless `--native-default` is given.
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "native_default",
            required_unless_present = "native_default"
        )]
        config_dir: Option<PathBuf>,
        /// Permit a private directory outside Relay's managed profiles root. Implied by
        /// `--native-default`, since `~/.claude` is never inside it.
        #[arg(long)]
        allow_external: bool,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        /// Inspect Claude's own native-default account (`~/.claude`, launched with
        /// `CLAUDE_CONFIG_DIR` left unset) instead of an explicit isolated profile directory.
        /// These are not interchangeable: Claude's own auth lookup differs by mode even for the
        /// identical path.
        #[arg(long, conflicts_with = "config_dir")]
        native_default: bool,
    },
    /// Adopt an existing Claude profile by reference, or preview the adoption.
    Adopt {
        name: ProfileName,
        #[arg(long, value_enum)]
        provider: ExistingProvider,
        /// Required unless `--native-default` is given.
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "native_default",
            required_unless_present = "native_default"
        )]
        config_dir: Option<PathBuf>,
        /// Preview only: report what adoption would do without changing Relay's registry.
        /// Without this flag, adoption is performed and the registry is written.
        #[arg(long)]
        dry_run: bool,
        /// Permit a private directory outside Relay's managed profiles root. Implied by
        /// `--native-default`, since `~/.claude` is never inside it.
        #[arg(long)]
        allow_external: bool,
        /// Explicit Claude executable, primarily for controlled validation.
        #[arg(long, value_name = "PATH")]
        claude_executable: Option<PathBuf>,
        /// Adopt Claude's own native-default account (`~/.claude`, launched with
        /// `CLAUDE_CONFIG_DIR` left unset) instead of an explicit isolated profile directory.
        #[arg(long, conflicts_with = "config_dir")]
        native_default: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliProvider {
    Fake,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ExistingProvider {
    Claude,
}

impl From<CliProvider> for ProviderKind {
    fn from(value: CliProvider) -> Self {
        match value {
            CliProvider::Fake => Self::Fake,
        }
    }
}
