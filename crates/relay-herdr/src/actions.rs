//! The first production-quality Herdr integration slice (M3.1): one function per action a Herdr
//! plugin action/event handler invokes. Every function here does exactly two things — resolve
//! *which* Relay profile/project a pane means (never guessing, see [`crate::mapping`]), then
//! shell out to the corresponding `relay ... --json` subcommand and return a typed view of its
//! output. None of them hold a lock, a lease, or any other piece of Relay's safety state; Relay's
//! own CLI is the only thing that ever touches those.

use serde::{Deserialize, Serialize};

use crate::HerdrPaneContext;
use crate::client::{CommandRunner, RelayClient};
use crate::error::HerdrIntegrationError;
use crate::mapping::{self, ResolvedProfile};

fn project_dir(pane: &HerdrPaneContext) -> String {
    pane.working_directory.to_string_lossy().into_owned()
}

fn require_session_id(pane: &HerdrPaneContext) -> Result<&str, HerdrIntegrationError> {
    pane.agent_session_id
        .as_deref()
        .ok_or(HerdrIntegrationError::MissingSessionIdentity)
}

// ---------------------------------------------------------------------------------------------
// `relay status` for the focused Claude agent: profile health + project writer/lock state.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileStatusSummary {
    pub authentication: String,
    pub identity_matches: bool,
    pub availability_state: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawProfileStatus {
    authentication: String,
    identity_matches: bool,
    availability: RawAvailability,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawAvailability {
    state: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LockStatusSummary {
    pub locked: bool,
    pub lease_owner: Option<String>,
    pub current_transaction: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawLockStatus {
    locked: bool,
    lease: Option<RawLease>,
    current_transaction: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawLease {
    owner_profile: String,
}

#[derive(Debug)]
pub struct AgentStatus {
    pub profile: ResolvedProfile,
    pub profile_status: ProfileStatusSummary,
    pub lock: LockStatusSummary,
}

/// Composite, read-only status for the pane's mapped profile: is it healthy, is it currently the
/// project's writer, and is there a live/pending transaction. Two `relay ... --json` calls, zero
/// mutation.
pub fn status<R: CommandRunner>(
    pane: &HerdrPaneContext,
    client: &RelayClient<R>,
) -> Result<AgentStatus, HerdrIntegrationError> {
    let profile = mapping::resolve_profile(pane, client)?;

    let raw: RawProfileStatus = client.run_json(&["profile", "status", &profile.name])?;
    let profile_status = ProfileStatusSummary {
        authentication: raw.authentication,
        identity_matches: raw.identity_matches,
        availability_state: raw.availability.state,
    };

    let dir = project_dir(pane);
    let raw_lock: RawLockStatus = client.run_json(&["lock", "status", "--project-dir", &dir])?;
    let lock = LockStatusSummary {
        locked: raw_lock.locked,
        lease_owner: raw_lock.lease.map(|lease| lease.owner_profile),
        current_transaction: raw_lock.current_transaction,
    };

    Ok(AgentStatus {
        profile,
        profile_status,
        lock,
    })
}

// ---------------------------------------------------------------------------------------------
// `relay doctor`
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorCheckView {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorReportView {
    pub healthy: bool,
    pub checks: Vec<DoctorCheckView>,
}

pub fn doctor<R: CommandRunner>(
    pane: &HerdrPaneContext,
    client: &RelayClient<R>,
) -> Result<DoctorReportView, HerdrIntegrationError> {
    let profile = mapping::resolve_profile(pane, client)?;
    client.run_json(&["profile", "doctor", &profile.name])
}

// ---------------------------------------------------------------------------------------------
// Recovery status: project-scoped only, never requires a resolved profile, since it must remain
// usable even when the pane's `relay_profile` token is missing or stale.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffStateView {
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffJournalView {
    pub transaction_id: String,
    pub state: HandoffStateView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryStatus {
    pub locked: bool,
    pub lease_owner: Option<String>,
    pub transaction_id: Option<String>,
    pub transaction_state: Option<String>,
    pub transaction_reason: Option<String>,
    pub is_terminal: bool,
}

const TERMINAL_STATES: [&str; 2] = ["COMPLETE", "FAILED"];

/// Reads the project's orchestration lock and, if a transaction is on record, its journal state.
/// Purely diagnostic: whether the transaction actually needs `relay recover` run is Relay's own
/// call, reported verbatim, never re-derived here.
pub fn recovery_status<R: CommandRunner>(
    pane: &HerdrPaneContext,
    client: &RelayClient<R>,
) -> Result<RecoveryStatus, HerdrIntegrationError> {
    let dir = project_dir(pane);
    let lock: RawLockStatus = client.run_json(&["lock", "status", "--project-dir", &dir])?;

    let (transaction_state, transaction_reason, is_terminal) = match &lock.current_transaction {
        Some(id) => {
            let journal: HandoffJournalView =
                client.run_json(&["handoff", "status", id, "--project-dir", &dir])?;
            let terminal = TERMINAL_STATES.contains(&journal.state.state.as_str());
            (Some(journal.state.state), journal.state.reason, terminal)
        }
        None => (None, None, true),
    };

    Ok(RecoveryStatus {
        locked: lock.locked,
        lease_owner: lock.lease.map(|lease| lease.owner_profile),
        transaction_id: lock.current_transaction,
        transaction_state,
        transaction_reason,
        is_terminal,
    })
}

// ---------------------------------------------------------------------------------------------
// Automatic watch/evaluate: the M2C.1 usage-triggered handoff machinery, unchanged, invoked with
// a mapped source profile and the pane's own session id.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WatchRunView {
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
        target: String,
    },
    Handoff {
        target: String,
        journal: HandoffJournalView,
    },
    Recovered {
        transactions: Vec<String>,
    },
    TransactionInFlight {
        transaction_id: String,
    },
}

pub struct WatchEvaluateRequest<'a> {
    pub fallback_profiles: &'a [String],
    pub dry_run: bool,
    pub workload_model: Option<&'a str>,
}

/// Evaluates the mapped source profile's usage state once and, only if genuinely exhausted,
/// performs the existing transactional handoff to the first eligible fallback. This function
/// does not decide exhaustion itself — `relay watch run` does, using Relay's own usage policy —
/// it only supplies the profile/session/project Relay needs to evaluate against.
pub fn watch_evaluate<R: CommandRunner>(
    pane: &HerdrPaneContext,
    client: &RelayClient<R>,
    request: &WatchEvaluateRequest<'_>,
) -> Result<WatchRunView, HerdrIntegrationError> {
    let profile = mapping::resolve_profile(pane, client)?;
    let session_id = require_session_id(pane)?;
    if request.fallback_profiles.is_empty() {
        return Err(HerdrIntegrationError::ProfileMappingUnknown);
    }

    let dir = project_dir(pane);
    let mut args: Vec<&str> = vec!["watch", "run", "--profile", &profile.name];
    for fallback in request.fallback_profiles {
        args.push("--fallback");
        args.push(fallback);
    }
    args.push("--project");
    args.push(&dir);
    args.push("--session");
    args.push(session_id);
    if request.dry_run {
        args.push("--dry-run");
    }
    if let Some(model) = request.workload_model {
        args.push("--workload-model");
        args.push(model);
    }
    client.run_json(&args)
}

// ---------------------------------------------------------------------------------------------
// Manual handoff: explicit target, still driven entirely by `relay handoff run`.
// ---------------------------------------------------------------------------------------------

pub fn handoff_manual<R: CommandRunner>(
    pane: &HerdrPaneContext,
    client: &RelayClient<R>,
    target_profile: &str,
) -> Result<HandoffJournalView, HerdrIntegrationError> {
    let profile = mapping::resolve_profile(pane, client)?;
    let session_id = require_session_id(pane)?;
    let dir = project_dir(pane);
    client.run_json(&[
        "handoff",
        "run",
        "--from",
        &profile.name,
        "--to",
        target_profile,
        "--project",
        &dir,
        "--session",
        session_id,
    ])
}
