//! `relay resume [profile]`: continue a dormant Relay Session on the profile that owns it.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use clap::Parser as _;
use relay_core::{
    Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths, usage::UsageState,
};

use crate::{
    auto_handoff,
    cli::{Cli, ResumeArgs},
    commands::codex::codex_preflight,
    launch::record_writer_process,
    output::CommandOutput,
    preferences, progress, provider_args, providers, sessions,
    terminal_session::{ContinuationContext, plan_terminal_for_lease, run_managed_terminal},
    util::{current_unix_ms, project_display_name},
};

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
pub(crate) fn run(
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
    let progress = progress::Progress::start("Finding the conversation to resume…", json_mode);
    let registered = service.list()?;
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };

    // Which Relay session: the only resumable one directly; several ask (terminal) or must be
    // named with `--session`. `relay resume <profile>` is a filter on the session's owner/last
    // owner, never a silent pick. Sessions that are active in another terminal are shown but never
    // started a second time.
    let store = sessions::open_store(paths, &canonical_project)?;
    progress.set_label("Checking session liveness…");
    let views = sessions::reconcile(paths, &canonical_project, &registered, &executables)?;
    // Finished before `choose_session`, which can prompt interactively in a real terminal when
    // several sessions are resumable — the spinner must never still be animating underneath that.
    progress.finish();
    let chosen = sessions::choose_session(
        &store,
        &views,
        &registered,
        sessions::Want::Resumable,
        args.session.as_deref(),
        args.profile.as_ref(),
        json_mode,
    )?;
    let id = chosen.record.relay_session_id.clone();

    // A dormant session gets a fresh active lease on the profile it last ran on (history, not an
    // owner) before anything is launched; an active background job is attached to as it is.
    let (session, mut lease) = match chosen.lease.clone() {
        Some(lease) => (sessions::SessionCtx::of(&store, &id), lease),
        None => {
            let last = registered
                .iter()
                .find(|profile| profile.name == chosen.record.last_profile)
                .ok_or_else(|| Error::ProfileNotFound(chosen.record.last_profile.to_string()))?;
            let native = chosen
                .record
                .native_session_id
                .clone()
                .ok_or(Error::CorruptedState)?;
            sessions::activate_session(
                paths,
                &canonical_project,
                &id,
                last,
                &native,
                relay_core::handoff::ProcessIdentity {
                    pid: 0,
                    start_time_fingerprint: None,
                },
                current_unix_ms(),
            )?
        }
    };
    let acquired_here = chosen.lease.is_none();
    let outcome = resume_session(
        service,
        paths,
        args,
        json_mode,
        &canonical_project,
        &registered,
        &session,
        &mut lease,
    );
    if outcome.is_err() && acquired_here {
        // Nothing was launched: the session goes back to being dormant.
        sessions::release_after_exit(paths, &canonical_project, &id, &registered);
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn resume_session(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &ResumeArgs,
    json_mode: bool,
    canonical_project: &Path,
    registered: &[Profile],
    session: &sessions::SessionCtx,
    lease: &mut relay_core::handoff::WriterLease,
) -> Result<CommandOutput, Error> {
    let project_state_dir = session.dir.clone();
    let canonical_project = canonical_project.to_path_buf();
    let mut resolved_profile = lease.owner_profile.clone();
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
                // No process was started for this session, so its source is trivially quiescent:
                // record that proof (a process that has provably ended) for the transaction.
                let gone = sessions::gone_process_identity();
                let store = session.lease_store();
                session.lock().try_with(|| -> Result<(), Error> {
                    if let Some(mut current) = store.load()? {
                        current.owner_process = gone;
                        store.save(&current)?;
                    }
                    Ok(())
                })?;
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
                if let Some(reloaded) = session
                    .lease_store()
                    .load()?
                    .filter(|reloaded| reloaded.owner_profile != profile.name)
                {
                    *lease = reloaded;
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

    // Explicit arguments replace the owner provider's stored ones for this Relay session (the
    // other provider's are untouched); otherwise the stored ones are reused.
    let mut stored_args = provider_args::ProviderArgs::load(&project_state_dir)?;
    if !args.provider_args.is_empty() {
        provider_args::validate(profile.provider, &args.provider_args)?;
        stored_args.set(profile.provider, args.provider_args.clone());
    }

    // Resolved before printing anything: on `AmbiguousSessionLiveness` this must fail closed
    // without ever claiming to be "resuming" a session it then can't safely continue.
    let command = plan_terminal_for_lease(
        profile,
        lease,
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
            "Agent Relay\nProject: {}\nProfile: {}\nResuming {provider_label} session {}...",
            project_display_name(&canonical_project),
            resolved_profile,
            session.id.short()
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }

    let lease_store = session.lease_store();
    let lock = session.lock();
    let native = lease.session_id.clone();
    let record_process = |pid: u32| record_writer_process(&lease_store, &lock, &native, pid);
    run_managed_terminal(
        &ContinuationContext::new(
            service,
            paths,
            &canonical_project,
            session,
            args.claude_executable.clone(),
            args.codex_executable.clone(),
            json_mode,
        )?,
        command,
        resolved_profile,
        Some(&record_process),
    )
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
    crate::commands::dispatch(&inner)
}
