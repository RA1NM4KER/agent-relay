//! Creating and recording a Relay-managed writer process: the shared logic behind `relay
//! launch`, `relay claude`'s background (`--no-attach`) path, and the pid Relay records into a
//! lease the moment a supervised interactive process actually exists.

use std::path::Path;

use relay_core::{
    Error, Profile, ProfileName, ProfileService, RelayPaths,
    handoff::{LeaseStore, OrchestrationLock},
};

use crate::{providers, sessions, util::current_unix_ms};

const LAUNCH_LIVENESS_CONFIRM_ATTEMPTS: u32 = 3;
const LAUNCH_LIVENESS_POLL_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// A single liveness check has a real transient gap: live testing during M2B.75 showed that
/// checking immediately after a competing writer's pid died (but before Claude's own background
/// daemon had reassigned a replacement) can read as "not active" for one instant even though the
/// session is about to come back. Requires `LAUNCH_LIVENESS_CONFIRM_ATTEMPTS` *consecutive*
/// not-active readings before concluding it is genuinely safe to launch a new writer; a single
/// active reading is trusted immediately (no race in that direction — evidence of activity is
/// evidence of activity).
pub(crate) fn confirm_not_active(
    owner: &Profile,
    project_dir: &Path,
    session_id: &str,
    recorded_owner: &relay_core::handoff::ProcessIdentity,
    executables: &providers::ExecutableOverrides,
) -> Result<bool, Error> {
    // The OWNER's provider decides what "still running" means: a Codex-owned lease must never be
    // judged by Claude's session registry (or the reverse).
    let ports = providers::ports_for(
        owner.provider,
        executables,
        owner.effective_claude_config_mode(),
    );
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
pub(crate) fn perform_launch(
    service: &ProfileService,
    paths: &RelayPaths,
    profile: &ProfileName,
    project_dir: &Path,
    prompt: &str,
    claude_executable: Option<&Path>,
    extra_args: &[String],
) -> Result<(sessions::SessionCtx, relay_core::handoff::WriterLease), Error> {
    let registered = service.list()?;
    let target = registered
        .iter()
        .find(|candidate| &candidate.name == profile)
        .ok_or_else(|| Error::ProfileNotFound(profile.to_string()))?;
    let canonical_project = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.to_path_buf(),
        source,
    })?;
    // A new background conversation is a new Relay session; other sessions of the project are
    // none of its business.
    let launched = relay_provider_claude::launch_background(
        &target.config_dir,
        target.effective_claude_config_mode(),
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
    sessions::create_session(
        paths,
        &canonical_project,
        target,
        &launched.session_id,
        owner_process,
        Some(launched.provider_handle.clone()),
        false,
        current_unix_ms(),
    )
}

/// Records the interactive provider's real process in the lease as soon as it exists (identity =
/// pid + start time, exactly what liveness checks and the verified stop use). Best effort with a
/// short retry: the lock may briefly be held by an evaluation.
pub(crate) fn record_writer_process(
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
