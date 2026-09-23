//! `relay switch-request <target>`: the `$relay switch` Codex skill's own external side-channel
//! to the Relay process already supervising this session — reuses the exact control channel
//! Claude's in-agent `/relay:switch` already uses (see [`crate::control`]), rather than inventing
//! a second orchestration path. This process never performs the switch itself; it only asks the
//! supervisor to, and reports what happened so the skill can decide what to tell the user.
//!
//! The requester can never be the same pid as the process it speaks for (a shell the skill's
//! tool-use spawned is not the Codex TUI process), so the supervisor verifies it by ancestry
//! instead of exact match — see [`control::caller_is_verified`]. This command's own job is
//! narrower: find the right session, confirm a live supervisor is actually there before bothering
//! to ask, and hand over a truthful process identity for the supervisor to check.

use std::time::Duration;

use relay_core::{Error, RelayPaths, handoff::ProcessIdentity};

use crate::{
    cli::SwitchRequestArgs,
    control::{self, RequestKind},
    output::{CommandOutput, success},
    sessions,
    util::new_session_uuid,
};

/// The supervisor's own tick answers a pending request within a fraction of a second; this is
/// generous headroom for a loaded machine, not an expected wait.
const AWAIT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn run(paths: &RelayPaths, args: &SwitchRequestArgs) -> Result<CommandOutput, Error> {
    let canonical_project =
        std::fs::canonicalize(&args.project_dir).map_err(|source| Error::Io {
            path: args.project_dir.clone(),
            source,
        })?;
    let session_id = relay_core::handoff::RelaySessionId::parse(&args.session)?;
    let store = sessions::open_store(paths, &canonical_project)?;
    let session = sessions::SessionCtx::of(&store, &session_id);
    let control = session.control();

    let Some(supervisor) = control.live_supervisor() else {
        return success(
            "switch-request",
            "No verified Relay supervisor for this session.".to_owned(),
            serde_json::json!({ "outcome": "no_supervisor" }),
        );
    };
    // The lease — never the caller's own env vars, which could be stale if the conversation
    // moved on since this Codex session started — is the authority on the *native* session id
    // and owner profile this control channel actually speaks in terms of (`SupervisorRecord`
    // and `control::Request` both carry the provider-native session id, not this Relay session's
    // own uuid). A live supervisor whose own published record disagrees with the lease it should
    // be following right now is itself a staleness signal, not something to paper over.
    let Ok(Some(lease)) = session.lease_store().load() else {
        return success(
            "switch-request",
            "This Relay session has no active writer lease.".to_owned(),
            serde_json::json!({ "outcome": "stale_session" }),
        );
    };
    if supervisor.session_id != lease.session_id
        || supervisor.owner_profile != lease.owner_profile.as_str()
    {
        return success(
            "switch-request",
            "The supervising process is following a different session now.".to_owned(),
            serde_json::json!({ "outcome": "stale_session" }),
        );
    }

    let id = new_session_uuid().unwrap_or_else(|_| "req".to_owned());
    let request = control::request(
        id.clone(),
        RequestKind::Switch {
            target: args.target.to_string(),
        },
        &lease.session_id,
        lease.owner_profile.as_str(),
        ProcessIdentity::current(),
    );
    if control.submit(&request).is_err() {
        return success(
            "switch-request",
            "Could not reach the supervising process.".to_owned(),
            serde_json::json!({ "outcome": "unreachable" }),
        );
    }
    match control.await_response(&id, AWAIT_TIMEOUT) {
        Some(response) => success(
            "switch-request",
            response.message.clone(),
            serde_json::json!({
                "outcome": "answered",
                "ok": response.ok,
                "message": response.message,
            }),
        ),
        None => success(
            "switch-request",
            "The supervising process did not answer in time.".to_owned(),
            serde_json::json!({ "outcome": "timeout" }),
        ),
    }
}
