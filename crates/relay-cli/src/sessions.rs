//! Relay Sessions from the CLI's side: creating them, finding the right one for a command,
//! reconciling stale leases into dormancy, and releasing the lease when a supervised provider
//! process exits. The model itself lives in `relay_core::handoff::session`.
//!
//! Relay supervises conversations, not repositories: a project holds many Relay Sessions, each
//! live one has exactly one owner (its lease), and a closed one has none.

use std::path::{Path, PathBuf};

use relay_core::{
    Error, Profile, ProfileName, ProviderKind, RelayPaths,
    handoff::{
        LeaseStore, OrchestrationLock, ProcessIdentity, ProjectId, RelaySessionId,
        RelaySessionRecord, RelaySessionView, SessionState, SessionStore, TransactionId,
        WriterLease,
    },
};

use crate::{control::ControlDir, providers, target};

/// A lease acquired but whose process was never recorded is stale after this long.
const PLACEHOLDER_GRACE_MS: u64 = 60_000;

/// Where one Relay Session's state lives.
#[derive(Clone, Debug)]
pub struct SessionCtx {
    pub id: RelaySessionId,
    pub dir: PathBuf,
    pub project_id: ProjectId,
}

impl SessionCtx {
    #[must_use]
    pub fn of(store: &SessionStore<'_>, id: &RelaySessionId) -> Self {
        Self {
            id: id.clone(),
            dir: store.session_dir(id),
            project_id: store.project_id().clone(),
        }
    }

    #[must_use]
    pub fn lease_store(&self) -> LeaseStore {
        LeaseStore::at_path(self.dir.join("lease.json"))
    }

    #[must_use]
    pub fn lock(&self) -> OrchestrationLock {
        OrchestrationLock::at_path(self.dir.join("orchestration.lock"))
    }

    #[must_use]
    pub fn control(&self) -> ControlDir {
        ControlDir::for_project(&self.dir)
    }
}

pub fn open_store<'a>(
    paths: &'a RelayPaths,
    canonical_project: &Path,
) -> Result<SessionStore<'a>, Error> {
    Ok(SessionStore::new(
        paths,
        ProjectId::for_canonical_path(canonical_project)?,
    ))
}

/// The state directory of the Relay session that holds this provider-native conversation, if any
/// is currently registered for it.
pub fn session_dir_for_native(
    paths: &RelayPaths,
    canonical_project: &Path,
    native: &str,
) -> Result<Option<PathBuf>, Error> {
    let store = open_store(paths, canonical_project)?;
    Ok(store
        .find_by_native(native)?
        .map(|view| store.session_dir(&view.record.relay_session_id)))
}

fn provider_label(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude | ProviderKind::Fake => "claude",
    }
}

/// Creates a new Relay Session with its first active lease (registry-locked; refuses when the
/// same native conversation is already active in another Relay Session).
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    paths: &RelayPaths,
    canonical_project: &Path,
    profile: &Profile,
    native_session_id: &str,
    owner_process: ProcessIdentity,
    provider_handle: Option<String>,
    provisional: bool,
    now_unix_ms: u64,
) -> Result<(SessionCtx, WriterLease), Error> {
    let store = open_store(paths, canonical_project)?;
    let id = RelaySessionId::generate()?;
    let lease = WriterLease::new(
        store.project_id().clone(),
        profile.name.clone(),
        owner_process,
        native_session_id.to_owned(),
        TransactionId::generate(),
        now_unix_ms,
    )
    .with_provider_handle(provider_handle);
    let record = RelaySessionRecord::new(
        id.clone(),
        store.project_id().clone(),
        profile.name.clone(),
        Some(provider_label(profile.provider).to_owned()),
        Some(native_session_id.to_owned()),
        provisional,
        now_unix_ms,
    );
    store.create_active(&record, &lease)?;
    Ok((SessionCtx::of(&store, &id), lease))
}

/// The identity of a process that has definitely ended (its start time was recorded while it ran).
/// A dormant session has no process, but the handoff machinery insists on a recorded source
/// process it can prove gone; this is that proof.
#[must_use]
pub fn gone_process_identity() -> ProcessIdentity {
    let Ok(mut child) = std::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .spawn()
    else {
        return ProcessIdentity {
            pid: 0,
            start_time_fingerprint: None,
        };
    };
    let identity = ProcessIdentity::query(child.id());
    let _ignored = child.kill();
    let _ignored = child.wait();
    identity
}

/// Gives a dormant session a fresh active lease on `profile` (a placeholder process until the
/// provider is spawned).
pub fn activate_session(
    paths: &RelayPaths,
    canonical_project: &Path,
    id: &RelaySessionId,
    profile: &Profile,
    native_session_id: &str,
    owner_process: ProcessIdentity,
    now_unix_ms: u64,
) -> Result<(SessionCtx, WriterLease), Error> {
    let store = open_store(paths, canonical_project)?;
    let lease = WriterLease::new(
        store.project_id().clone(),
        profile.name.clone(),
        owner_process,
        native_session_id.to_owned(),
        TransactionId::generate(),
        now_unix_ms,
    );
    store.activate(id, &lease)?;
    Ok((SessionCtx::of(&store, id), lease))
}

/// Whether Claude has persisted this conversation (the transcript exists in the profile).
fn claude_transcript_exists(config_dir: &Path, session_id: &str) -> bool {
    crate::commands::claude::claude_transcript_exists(config_dir, session_id)
}

/// Whether the lease's owner is provably gone. Anything uncertain is *not* stale.
pub fn lease_is_stale(
    lease: &WriterLease,
    owner: Option<&Profile>,
    canonical_project: &Path,
    executables: &providers::ExecutableOverrides,
    now_unix_ms: u64,
) -> bool {
    let provider_says_gone = || {
        owner.is_some_and(|owner| {
            crate::launch::confirm_not_active(
                owner,
                canonical_project,
                &lease.session_id,
                &lease.owner_process,
                executables,
            )
            .unwrap_or(false)
        })
    };
    // A background job is judged by the provider's own listing, not by a pid.
    if lease.provider_handle.is_some() {
        return provider_says_gone();
    }
    if lease.owner_process.pid == 0 {
        return now_unix_ms.saturating_sub(lease.acquired_unix_ms) > PLACEHOLDER_GRACE_MS;
    }
    match lease.owner_process.is_still_the_same_process() {
        Some(false) => true,
        Some(true) => false,
        None => provider_says_gone(),
    }
}

/// Ends the lease of a session whose provider process is gone: dormant with history, or removed
/// outright when it was provisional and no conversation was ever persisted. Skipped (returns
/// `false`) when a transaction holds the session lock.
pub fn release_session(
    store: &SessionStore<'_>,
    view: &RelaySessionView,
    registered: &[Profile],
    now_unix_ms: u64,
) -> Result<bool, Error> {
    let id = &view.record.relay_session_id;
    let last_owner = registered
        .iter()
        .find(|profile| &profile.name == view.profile());
    let never_persisted = view.record.provisional
        && match (last_owner, view.native_session_id()) {
            (Some(owner), Some(native)) if owner.provider == ProviderKind::Claude => {
                !claude_transcript_exists(&owner.config_dir, native)
            }
            _ => false,
        };
    match store
        .session_lock(id)
        .try_with(|| store.release(id, now_unix_ms, never_persisted))
    {
        Ok(()) => Ok(true),
        Err(Error::OrchestrationLockHeld) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Every session of the project with stale leases reconciled first: a lease whose recorded
/// process is confirmed gone is only folded to dormant if the exact same native conversation
/// cannot be found running under a NEW process right now (see [`providers::discover_live_owner`]);
/// when it can, the lease is rebound to that process in place and the session STAYS active. A
/// session must never be marked dormant merely because Relay itself lost track of its process —
/// only because the conversation is genuinely not running anywhere.
pub fn reconcile(
    paths: &RelayPaths,
    canonical_project: &Path,
    registered: &[Profile],
    executables: &providers::ExecutableOverrides,
) -> Result<Vec<RelaySessionView>, Error> {
    let store = open_store(paths, canonical_project)?;
    let now = crate::util::current_unix_ms();
    for view in store.list()? {
        let Some(lease) = &view.lease else {
            continue;
        };
        let owner = registered
            .iter()
            .find(|profile| profile.name == lease.owner_profile);
        if !lease_is_stale(lease, owner, canonical_project, executables, now) {
            continue;
        }
        let rebound = owner.and_then(|owner| {
            providers::discover_live_owner(
                owner.provider,
                executables,
                &owner.config_dir,
                &lease.session_id,
                owner.effective_claude_config_mode(),
            )
        });
        rebind_or_release(&store, &view, lease, rebound, registered, now)?;
    }
    store.list()
}

/// The two outcomes a stale-looking lease can have: reconnected to the process now genuinely
/// serving its conversation, or (nothing found) released to dormant as before. Best-effort under
/// the session's own lock: skipped (left as-is for the next reconciliation) if it is held.
fn rebind_or_release(
    store: &SessionStore<'_>,
    view: &RelaySessionView,
    lease: &WriterLease,
    rebound: Option<ProcessIdentity>,
    registered: &[Profile],
    now_unix_ms: u64,
) -> Result<(), Error> {
    let Some(identity) = rebound else {
        release_session(store, view, registered, now_unix_ms)?;
        return Ok(());
    };
    let id = &view.record.relay_session_id;
    let mut updated = lease.clone();
    updated.owner_process = identity;
    match store
        .session_lock(id)
        .try_with(|| store.lease_store(id).save(&updated))
    {
        Ok(()) | Err(Error::OrchestrationLockHeld) => Ok(()),
        Err(error) => Err(error),
    }
}

/// The supervised provider process ended: release the session's lease if (and only if) its
/// recorded process is gone, so the conversation is no longer claimed as actively managed.
pub fn release_after_exit(
    paths: &RelayPaths,
    canonical_project: &Path,
    id: &RelaySessionId,
    registered: &[Profile],
) {
    let Ok(store) = open_store(paths, canonical_project) else {
        return;
    };
    let Ok(Some(view)) = store.view(id) else {
        return;
    };
    let Some(lease) = &view.lease else {
        return;
    };
    let gone = lease.owner_process.pid == 0
        || lease.owner_process.is_still_the_same_process() == Some(false);
    if gone {
        let _ignored = release_session(&store, &view, registered, crate::util::current_unix_ms());
    }
}

// ---- choosing a session --------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Want {
    /// A live conversation (a lease): what `switch` moves.
    Active,
    /// A conversation with no live process.
    Dormant,
    /// What `switch` can move: a live conversation if there is one, else (when nothing is live)
    /// a dormant one, which is re-homed to the target profile for its next `relay resume`.
    Switchable,
    /// What `resume` can open: a dormant session, or an active one that is a Claude background
    /// job (which is attached to rather than started again).
    Resumable,
}

impl Want {
    fn accepts(self, view: &RelaySessionView) -> bool {
        let active = view.state() == SessionState::Active;
        match self {
            Self::Active => active,
            // Resolved to `Active` or `Dormant` before any filtering (see `choose_session`).
            Self::Switchable => true,
            Self::Dormant => !active,
            Self::Resumable => {
                !active
                    || view
                        .lease
                        .as_ref()
                        .is_some_and(|lease| lease.provider_handle.is_some())
            }
        }
    }
}

/// One row of the session picker.
pub struct SessionRow {
    pub label: String,
    pub selectable: bool,
    pub note: Option<String>,
}

impl target::PickRow for SessionRow {
    fn selectable(&self) -> bool {
        self.selectable
    }
    fn label(&self) -> String {
        self.label.clone()
    }
    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

fn ago(unix_ms: u64, now_unix_ms: u64) -> String {
    let seconds = now_unix_ms.saturating_sub(unix_ms) / 1000;
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn short(text: &str) -> &str {
    text.get(..8).unwrap_or(text)
}

/// `Claude  megan  8c91a3f0  native 11fa…  5m ago`
#[must_use]
pub fn describe(view: &RelaySessionView, registered: &[Profile], now_unix_ms: u64) -> String {
    let provider = registered
        .iter()
        .find(|profile| &profile.name == view.profile())
        .map_or_else(
            || {
                view.record
                    .provider
                    .clone()
                    .unwrap_or_else(|| "?".to_owned())
            },
            |profile| provider_label(profile.provider).to_owned(),
        );
    format!(
        "{}  {}  {}  native {}  {}",
        if provider == "codex" {
            "Codex "
        } else {
            "Claude"
        },
        view.profile(),
        view.record.relay_session_id.short(),
        view.native_session_id().map_or("-", short),
        ago(view.record.last_activity_unix_ms, now_unix_ms),
    )
}

fn rows_for(views: &[&RelaySessionView], registered: &[Profile], want: Want) -> Vec<SessionRow> {
    let now = crate::util::current_unix_ms();
    views
        .iter()
        .map(|view| {
            let active = view.state() == SessionState::Active;
            let selectable = want.accepts(view);
            SessionRow {
                label: describe(view, registered, now),
                selectable,
                note: if selectable {
                    None
                } else if active {
                    Some("active".to_owned())
                } else {
                    Some("dormant".to_owned())
                },
            }
        })
        .collect()
}

/// Picks the Relay Session a command acts on.
///
/// - an explicit `selector` (a full id or unambiguous prefix) wins and is validated;
/// - otherwise the sessions in the wanted state (optionally only those last/now on `profile`) are
///   candidates: exactly one is used directly; several open a picker in a terminal, and fail
///   deterministically (naming `--session`) without one. The newest is never chosen silently.
pub fn choose_session(
    store: &SessionStore<'_>,
    views: &[RelaySessionView],
    registered: &[Profile],
    want: Want,
    selector: Option<&str>,
    profile: Option<&ProfileName>,
    json_mode: bool,
) -> Result<RelaySessionView, Error> {
    let explicit_switch = want == Want::Switchable && selector.is_some();
    // `switch` moves a live conversation when there is one; only when nothing is live does it
    // re-home a dormant one.
    let want = if want == Want::Switchable {
        if views
            .iter()
            .any(|view| view.state() == SessionState::Active)
        {
            Want::Active
        } else {
            Want::Dormant
        }
    } else {
        want
    };
    if let Some(selector) = selector {
        let view = store.resolve(selector)?;
        // Re-read through the reconciled list so a stale lease is not mistaken for active.
        let view = views
            .iter()
            .find(|candidate| candidate.record.relay_session_id == view.record.relay_session_id)
            .cloned()
            .unwrap_or(view);
        return if explicit_switch || want.accepts(&view) {
            Ok(view)
        } else if want == Want::Active {
            Err(Error::RelaySessionDormant(
                view.record.relay_session_id.short().to_owned(),
            ))
        } else {
            Err(Error::RelaySessionActive(
                view.record.relay_session_id.short().to_owned(),
            ))
        };
    }
    let wanted_state = |view: &&RelaySessionView| want.accepts(view);
    let on_profile = |view: &&RelaySessionView| profile.is_none_or(|name| view.profile() == name);
    let candidates: Vec<&RelaySessionView> = views
        .iter()
        .filter(wanted_state)
        .filter(on_profile)
        .collect();
    match candidates.as_slice() {
        [only] => return Ok((*only).clone()),
        [] => {
            return Err(match want {
                Want::Active | Want::Switchable | Want::Dormant => Error::NoActiveWriterForProject,
                Want::Resumable => {
                    let active = views
                        .iter()
                        .filter(|view| view.state() == SessionState::Active)
                        .count();
                    Error::NoResumableSession(if active > 0 {
                        format!(
                            "there is nothing to resume: this project's {active} Relay session(s) \
                             are all active right now (see `relay status`). Start another with \
                             `relay claude` or `relay codex`."
                        )
                    } else {
                        "there is no Relay session to resume in this project: start one with \
                         `relay claude` or `relay codex`, or bring an old Claude conversation \
                         under Relay with `relay claude --resume`."
                            .to_owned()
                    })
                }
            });
        }
        _ => {}
    }
    // Ambiguous: show every session in the project so the state of each is visible.
    let shown: Vec<&RelaySessionView> = views.iter().filter(on_profile).collect();
    let now = crate::util::current_unix_ms();
    if json_mode || !target::interactive() {
        let listing = candidates
            .iter()
            .map(|view| {
                format!(
                    "{} ({})",
                    view.record.relay_session_id.short(),
                    describe(view, registered, now)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::SessionAmbiguous(format!(
            "{}: {listing}",
            candidates.len()
        )));
    }
    let rows = rows_for(&shown, registered, want);
    let title = match want {
        Want::Active | Want::Switchable | Want::Dormant => "Which conversation?",
        Want::Resumable => "Resume which conversation?",
    };
    match target::pick_rows(&rows, title) {
        Ok(Some(index)) => Ok(shown[index].clone()),
        Ok(None) => Err(Error::SwitchCancelled),
        Err(_) => Err(Error::SessionAmbiguous(format!("{}", candidates.len()))),
    }
}
