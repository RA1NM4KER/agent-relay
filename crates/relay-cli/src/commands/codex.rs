//! `relay codex`: start a NEW Relay-managed Codex conversation, symmetric with `relay claude`,
//! including pre-launch rerouting away from an already-exhausted profile.

use std::path::{Path, PathBuf};

use relay_core::{
    Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
    automation::{AutomationDecision, AutomationPolicy, LedgerStore, ProfileCandidate, decide},
    handoff::{ExecutionIntent, ProjectId},
    usage::UsageState,
};
use serde_json::{Value, json};

use crate::{
    auth::{apply_provider_exhaustion, doctor_is_healthy, friendly_auth_state, select_profile},
    auto_handoff,
    cli::{ClaudeArgs, CodexArgs},
    launch::record_writer_process,
    output::{CommandOutput, success},
    preferences, progress, provider_args, providers, sessions,
    terminal_session::{ContinuationContext, plan_codex_resume, run_managed_terminal},
    util::{current_unix_ms, project_display_name},
};

use super::claude::run as run_claude;

/// Starting `codex app-server` takes a moment: shown while it is checked, never in `--json` mode.
/// The returned reading's `max_used_percent` is GitHub #13's adaptive-polling seed: whichever
/// call site attaches the resulting interactive terminal (`relay codex`, `relay resume`, `relay
/// switch`) reuses it via [`crate::terminal_session::ContinuationContext::seed_codex_poll_schedule`]
/// instead of spending a second structured read purely to initialize the scheduler.
pub(crate) fn codex_preflight(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
    json_mode: bool,
) -> relay_provider_codex::CodexUsageReading {
    // Starting `codex app-server` takes a moment: show an interactive human that Relay is working
    // (nothing at all in --json mode or when output is not a terminal).
    let progress = progress::Progress::start("Checking Codex availability…", json_mode);
    let reading = codex_usage_now(profile, executables, project_dir);
    progress.finish();
    reading
}

pub(crate) fn codex_usage_now(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
) -> relay_provider_codex::CodexUsageReading {
    let _ = project_dir; // the structured rate-limit read is project-independent.
    providers::codex_usage_reading(executables, &profile.config_dir)
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
        claude_config_mode: Some(exhausted.effective_claude_config_mode()),
    };
    progress.say(&format!("Codex profile '{}' is exhausted.", exhausted.name));
    progress.set_label("Finding the next eligible profile…");
    let mut candidates = Vec::new();
    for name in auto_handoff::hierarchy_without(preferences, &exhausted.name, |_| true) {
        let Some(candidate) = registered.iter().find(|profile| &profile.name == name) else {
            continue;
        };
        let candidate_usage = apply_provider_exhaustion(
            paths,
            candidate,
            executables,
            providers::usage_signal_for(
                candidate.provider,
                executables,
                false,
                None,
                candidate.effective_claude_config_mode(),
            )
            .detect(&candidate.config_dir, canonical_project, "")?,
            current_unix_ms(),
        )?;
        candidates.push(ProfileCandidate {
            name: candidate.name.clone(),
            provider: candidate.provider,
            config_dir: candidate.config_dir.clone(),
            identity_stable_id: Some(candidate.expected_identity.stable_id.clone()),
            enabled: candidate.enabled,
            healthy: doctor_is_healthy(service, candidate, executables)?,
            usage: candidate_usage,
            claude_config_mode: Some(candidate.effective_claude_config_mode()),
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
                autonomous: args.autonomous,
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

/// The Codex counterpart of [`perform_launch`]: creates a fresh Codex thread (Relay's own
/// arguments only) and a new Relay session with its lease for it.
fn perform_codex_launch(
    service: &ProfileService,
    paths: &RelayPaths,
    profile: &Profile,
    canonical_project: &Path,
    executables: &providers::ExecutableOverrides,
    execution_intent: ExecutionIntent,
) -> Result<(sessions::SessionCtx, relay_core::handoff::WriterLease), Error> {
    let _ = service;
    let launched = relay_provider_codex::launch_new_thread(
        &profile.config_dir,
        canonical_project,
        executables.codex.as_deref(),
    )?;
    sessions::create_session(
        paths,
        canonical_project,
        profile,
        &launched.thread_id,
        launched
            .process
            .unwrap_or(relay_core::handoff::ProcessIdentity {
                pid: 0,
                start_time_fingerprint: None,
            }),
        None,
        false,
        current_unix_ms(),
        execution_intent,
    )
}

/// `relay codex`: start a NEW Relay-managed Codex conversation, symmetric with `relay claude`.
/// Profile choice is deterministic (see [`select_profile`]); the single-writer rules, `--new`
/// semantics and supervised terminal are the same; everything after `--` is forwarded verbatim to
/// the interactive `codex` session that continues the new thread.
pub(crate) fn run(
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
    if args.new && allow_reroute && !json_mode {
        eprintln!(
            "Note: `--new` is no longer needed — `relay codex` always starts a new Relay session and never stops another one."
        );
    }
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
    // From here on every step can be slow (starting `codex app-server`, scanning fallbacks,
    // creating the thread). ONE indicator lives across all of it — label changes, result lines
    // print above it — so there is never an unexplained gap. Other Relay sessions of this project
    // (any provider, any profile) are no obstacle: every `relay codex` starts its own session.
    let progress = progress::Progress::start("Checking Codex availability…", json_mode);

    // Immediate structured preflight — before anything is created or spent. Its authenticated
    // rate-limit read also proves the profile is logged in, so the slow `codex doctor` inspection
    // is only run afterwards, to explain a failure.
    progress.set_label("Checking Codex availability…");
    let preflight_reading = codex_usage_now(profile, &executables, &canonical_project);
    // GitHub #13's adaptive-polling seed: this profile's own trustworthy usedPercent from the
    // read this preflight already had to make, so a session that starts near its limit begins
    // supervision on the fast cadence immediately rather than waiting for a first 120s-away poll.
    let codex_poll_seed = preflight_reading.max_used_percent;
    let usage = apply_provider_exhaustion(
        paths,
        profile,
        &executables,
        preflight_reading.observation,
        current_unix_ms(),
    )?;
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

    // Deliberately one-shot, unlike Claude's live `[Relay · <profile>]` badge (`crate::badge`):
    // Codex's TUI status line only accepts a fixed set of built-in items (no custom-command item
    // the way Claude's `statusLine` hook works), and its hook system fires on lifecycle events
    // (SessionStart, PreToolUse, ...), never on a render tick — there is nowhere to plug in a live
    // indicator that stays honest across a handoff. See `docs/codex-status-line.md` for the full,
    // live-verified investigation before attempting this again.
    progress.say(&format!(
        "Agent Relay\nProject: {}\nProfile: {}\nStarting new managed Codex session...",
        project_display_name(&canonical_project),
        profile.name
    ));
    progress.set_label("Creating the Codex session…");
    let (session, lease) = perform_codex_launch(
        service,
        paths,
        profile,
        &canonical_project,
        &executables,
        if args.autonomous {
            ExecutionIntent::Autonomous
        } else {
            ExecutionIntent::Interactive
        },
    )?;
    progress.finish();
    // A brand-new managed conversation: only Codex's arguments carry over to it.
    provider_args::ProviderArgs::fresh_for(ProviderKind::Codex, args.provider_args.clone())
        .save(&session.dir)?;

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
                "relay_session_id": session.id.as_str(),
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
    let lease_store = session.lease_store();
    let lock = session.lock();
    let native = lease.session_id.clone();
    let record_process = |pid: u32| record_writer_process(&lease_store, &lock, &native, pid);
    let context = ContinuationContext::new(
        service,
        paths,
        &canonical_project,
        &session,
        args.claude_executable.clone(),
        args.codex_executable.clone(),
        json_mode,
    )?;
    context.seed_codex_poll_schedule(codex_poll_seed);
    let result = run_managed_terminal(
        &context,
        command,
        profile.name.clone(),
        Some(&record_process),
    );
    if result.is_err() {
        sessions::release_after_exit(paths, &canonical_project, &session.id, &registered);
    }
    result
}
