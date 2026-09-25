//! `relay claude`: start a new Relay-managed Claude conversation in this project and open
//! Claude directly; `relay claude --resume [SESSION_ID]` adopts an existing one instead.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use relay_core::{Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths};
use relay_provider_claude::ClaudeInspector;
use serde_json::json;

use crate::{
    auth::{
        friendly_auth_state, inherited_provider_exhaustion, run_claude_auth_subcommand,
        select_profile, verify_authenticated,
    },
    auto_handoff,
    cli::{ClaudeArgs, CodexArgs},
    hook::{
        ADOPT_ARGS_ENV, ADOPT_PROFILE_ENV, ADOPT_RELAY_SESSION_ENV, ADOPT_RESULT_ENV,
        ADOPT_SESSION_ENV,
    },
    launch::{perform_launch, record_writer_process},
    live,
    output::{CommandOutput, success},
    preferences, progress, provider_args, providers, sessions, terminal,
    terminal_session::{ContinuationContext, claude_terminal_env, run_managed_terminal},
    util::{
        bind_herdr_pane, current_unix_ms, new_session_uuid, project_display_name,
        resolve_initial_message, shell_quote,
    },
};

pub(crate) fn run(
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
    if args.new && !json_mode {
        eprintln!(
            "Note: `--new` is no longer needed — `relay claude` always starts a new Relay session and never stops another one."
        );
    }
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
    if inherited_provider_exhaustion(paths, primary_profile, &executables, current_unix_ms())?
        .is_some()
    {
        return route_known_exhausted_claude_start(
            service,
            paths,
            args,
            primary_profile,
            &registered,
            &preferences,
            json_mode,
        );
    }
    // Everything from here until the provider takes over the terminal can be slow (`claude auth
    // status`, login prompts). One indicator covers all of it, so the terminal never looks frozen.
    // (Another active Relay session in this project — under any profile, this one included — is
    // no obstacle: every `relay claude` starts its own Relay session.)
    let mut progress = Some(progress::Progress::start(
        "Preparing Claude profile…",
        json_mode,
    ));

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
            primary_profile.effective_claude_config_mode(),
            "login",
        )?;
        let report = verify_authenticated(
            &primary_profile.config_dir,
            primary_profile.effective_claude_config_mode(),
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
        let (session, lease) = perform_launch(
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
            .save(&session.dir)?;
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
                "relay_session_id": session.id.as_str(),
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
    // A new Relay session with its first lease (a placeholder process until Claude is spawned).
    // Provisional: if Claude ends before a conversation was ever persisted, the session is
    // removed instead of remembered.
    let (session, lease) = sessions::create_session(
        paths,
        &canonical_project,
        primary_profile,
        &session_id,
        relay_core::handoff::ProcessIdentity {
            pid: 0,
            start_time_fingerprint: None,
        },
        None,
        true,
        current_unix_ms(),
    )?;
    provider_args::ProviderArgs::fresh_for(ProviderKind::Claude, args.provider_args.clone())
        .save(&session.dir)?;
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
    let (envs, env_removals) = claude_terminal_env(
        &primary_profile.config_dir,
        primary_profile.effective_claude_config_mode(),
    );
    let command = terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args: command_args,
        envs,
        env_removals,
        current_dir: Some(canonical_project.clone()),
    };

    let lease_store = session.lease_store();
    let lock = session.lock();
    let record_process = |pid: u32| record_writer_process(&lease_store, &lock, &session_id, pid);
    drop(progress.take());
    let result = run_managed_terminal(
        &ContinuationContext::new(
            service,
            paths,
            &canonical_project,
            &session,
            args.claude_executable.clone(),
            None,
            json_mode,
        )?,
        command,
        primary.clone(),
        Some(&record_process),
    );
    if result.is_err() {
        // The provider never ran: no ghost session, no lease.
        sessions::release_after_exit(paths, &canonical_project, &session.id, &registered);
    }
    result
}

/// A fresh conversation has no lease to hand off, so an account-reset-window reroute is ordinary
/// startup selection. It deliberately re-enters the target provider's normal launch path, which
/// still performs its own authentication, trust, usage, and process checks.
#[allow(clippy::too_many_arguments)]
fn route_known_exhausted_claude_start(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &ClaudeArgs,
    exhausted: &Profile,
    registered: &[Profile],
    preferences: &preferences::Preferences,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: None,
    };
    for name in auto_handoff::hierarchy_without(preferences, &exhausted.name, |_| true) {
        let Some(candidate) = registered
            .iter()
            .find(|profile| &profile.name == name && profile.enabled)
        else {
            continue;
        };
        if inherited_provider_exhaustion(paths, candidate, &executables, current_unix_ms())?
            .is_some()
        {
            continue;
        }
        return match candidate.provider {
            ProviderKind::Claude | ProviderKind::Fake => run(
                service,
                paths,
                &ClaudeArgs {
                    message: args.message.clone(),
                    profile: Some(candidate.name.clone()),
                    fallback: args.fallback.clone(),
                    project_dir: args.project_dir.clone(),
                    no_attach: args.no_attach,
                    new: args.new,
                    resume: None,
                    claude_executable: args.claude_executable.clone(),
                    provider_args: args.provider_args.clone(),
                },
                json_mode,
            ),
            ProviderKind::Codex => super::codex::run(
                service,
                paths,
                &CodexArgs {
                    message: args.message.clone(),
                    profile: Some(candidate.name.clone()),
                    project_dir: args.project_dir.clone(),
                    no_attach: args.no_attach,
                    new: args.new,
                    claude_executable: args.claude_executable.clone(),
                    codex_executable: None,
                    provider_args: Vec::new(),
                },
                json_mode,
            ),
        };
    }
    Err(Error::NoEligibleProfile(exhausted.name.to_string()))
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
    // Exact-UUID owner discovery (M4.3): a concrete session id names exactly one native
    // conversation, which lives under exactly one profile's transcripts. Rather than defaulting
    // to the primary/configured profile and failing there, every registered Claude profile
    // (NativeDefault included) is checked structurally; `--profile` still pins the search, but
    // its error names the real owner when one is provable instead of a bare "not found".
    let profile = if session.is_empty() {
        select_profile(
            registered,
            preferences,
            args.profile.as_ref(),
            ProviderKind::Claude,
        )?
    } else if let Some(explicit) = &args.profile {
        let profile = registered
            .iter()
            .find(|candidate| &candidate.name == explicit)
            .ok_or_else(|| Error::ProfileNotFound(explicit.to_string()))?;
        if profile.provider != ProviderKind::Claude {
            return Err(Error::ProfileProviderMismatch {
                profile: explicit.to_string(),
                expected: "Claude",
                actual: profile.provider.to_string(),
            });
        }
        if !claude_transcript_exists(&profile.config_dir, session) {
            let owners = claude_transcript_owners(registered, session);
            return Err(Error::AdoptionRefused(match owners.as_slice() {
                [owner] => format!(
                    "profile '{explicit}' has no saved conversation with id '{session}'; it \
                     belongs to profile '{}' instead — run `relay claude --resume {session} \
                     --profile {}`",
                    owner.name, owner.name
                ),
                [] => format!(
                    "profile '{explicit}' has no saved conversation with id '{session}', and no \
                     other registered Claude profile has it either"
                ),
                _ => format!(
                    "profile '{explicit}' has no saved conversation with id '{session}'; it was \
                     found under more than one other registered profile ({}) — this should never \
                     happen for one native session id",
                    owners
                        .iter()
                        .map(|owner| owner.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }));
        }
        profile
    } else {
        let owners = claude_transcript_owners(registered, session);
        match owners.as_slice() {
            [owner] => {
                if !json_mode {
                    println!("Found session {session} under profile '{}'.", owner.name);
                }
                *owner
            }
            [] => {
                return Err(Error::AdoptionRefused(format!(
                    "no registered Claude profile has a saved conversation with id '{session}'; \
                     register the profile that owns it first (`relay profile adopt --provider \
                     claude --native-default` for Claude's own default account, or `relay \
                     profile adopt --provider claude --config-dir <dir>` for an isolated one)"
                )));
            }
            _ => {
                return Err(Error::AdoptionRefused(format!(
                    "conversation '{session}' is saved under more than one registered profile \
                     ({}); pass `--profile <name>` to choose",
                    owners
                        .iter()
                        .map(|owner| owner.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    };
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: None,
    };
    let store = sessions::open_store(paths, canonical_project)?;
    let progress = progress::Progress::start("Checking the project's Relay state…", json_mode);
    // Other Relay sessions of this project — active or not, on any profile — are irrelevant: this
    // conversation joins or rejoins its own Relay session. Only an EXACT conversation that is
    // already active under Relay is refused (one native conversation, one owner).
    if !session.is_empty() {
        let views = sessions::reconcile(paths, canonical_project, registered, &executables)?;
        if let Some(active) = views.iter().find(|view| {
            view.state() == relay_core::handoff::SessionState::Active
                && view.native_session_id() == Some(session)
        }) {
            return Err(Error::NativeSessionAlreadyActive(
                active.record.relay_session_id.short().to_owned(),
            ));
        }
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
    // The Relay session id this launch will use if the conversation turns out to be new to Relay
    // (a known dormant one is reactivated under its own id, and this terminal follows it).
    let preassigned = relay_core::handoff::RelaySessionId::generate()?;
    let placeholder = sessions::SessionCtx::of(&store, &preassigned);
    let control_root = paths.project_state_dir(&placeholder.project_id);
    std::fs::create_dir_all(&control_root).map_err(|source| Error::Io {
        path: control_root.clone(),
        source,
    })?;
    let result_path = control_root.join(format!("adopt-{}.json", new_session_uuid()?));

    let mut command_args: Vec<OsString> = vec!["--resume".into()];
    if !session.is_empty() {
        command_args.push(session.into());
    }
    command_args.extend(["--settings".into(), settings.into()]);
    command_args.extend(args.provider_args.iter().map(OsString::from));
    let (mut envs, env_removals) =
        claude_terminal_env(&profile.config_dir, profile.effective_claude_config_mode());
    envs.extend([
        (ADOPT_PROFILE_ENV.into(), profile.name.as_str().into()),
        (ADOPT_RESULT_ENV.into(), result_path.clone().into()),
        (ADOPT_RELAY_SESSION_ENV.into(), preassigned.as_str().into()),
        (
            ADOPT_ARGS_ENV.into(),
            serde_json::to_string(&args.provider_args)
                .unwrap_or_default()
                .into(),
        ),
    ]);
    if !session.is_empty() {
        envs.push((ADOPT_SESSION_ENV.into(), session.into()));
    }
    let command = terminal::TerminalCommand {
        program: inspector.executable().to_path_buf(),
        args: command_args,
        envs,
        env_removals,
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
        &placeholder,
        args.claude_executable.clone(),
        None,
        json_mode,
    )?;
    context.adopt_result = Some(result_path);
    run_managed_terminal(&context, command, profile.name.clone(), None)
}

/// Whether profile `config_dir` holds a saved transcript for `session_id` in any project.
pub(crate) fn claude_transcript_exists(config_dir: &Path, session_id: &str) -> bool {
    std::fs::read_dir(config_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|project| project.path().join(format!("{session_id}.jsonl")).is_file())
}

/// Every registered Claude profile (NativeDefault included — it is a registered profile like any
/// other) that structurally owns a saved transcript for `session_id`, straight from Claude's own
/// on-disk transcript layout — never a guess from a profile name, email, or model output. Used so
/// `relay claude --resume <exact-uuid>` finds the real owner instead of defaulting to the
/// configured primary profile and failing there.
fn claude_transcript_owners<'a>(registered: &'a [Profile], session_id: &str) -> Vec<&'a Profile> {
    registered
        .iter()
        .filter(|profile| {
            profile.provider == ProviderKind::Claude
                && claude_transcript_exists(&profile.config_dir, session_id)
        })
        .collect()
}
