//! Adopting a Claude conversation that is *already running* (or that Claude is resuming right
//! now) under Relay, plus the structural identification it rests on.
//!
//! Identity comes from Claude itself and is never typed, guessed or read from model output:
//! - the hook payload Claude writes to the hook's stdin (`session_id`, `cwd`, `transcript_path`);
//! - the environment of the Claude process the hook was started by (`CLAUDE_CONFIG_DIR`,
//!   `CLAUDE_PID`, `CLAUDE_PROJECT_DIR`);
//! - Claude's live-session registry (`<config dir>/sessions/<pid>.json`), which must agree with
//!   the hook about the session id, and whose process must still be running.
//!
//! Anything that does not line up is a refusal; nothing is created until every proof holds, and the
//! lease is written in one atomic step under the orchestration lock, so an interruption can never
//! leave a half-adopted project.

use std::path::{Path, PathBuf};

use relay_core::{
    Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
    handoff::{
        LeaseStore, OrchestrationLock, ProcessIdentity, ProjectId, TransactionId, WriterLease,
    },
};
use serde::Deserialize;
use serde_json::Value;

use crate::{auto_handoff, preferences::Preferences, provider_args, providers};

/// What a hook payload says, and nothing else is trusted from it.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    pub prompt: Option<String>,
    /// `SessionStart` only: `startup`, `resume`, `clear` or `compact`.
    pub source: Option<String>,
}

impl HookInput {
    #[must_use]
    pub fn parse(stdin: &[u8]) -> Option<Self> {
        serde_json::from_slice(stdin).ok()
    }
}

/// A running Claude conversation, identified structurally.
#[derive(Clone, Debug)]
pub struct LiveSession {
    pub config_dir: PathBuf,
    pub session_id: String,
    pub project: PathBuf,
    pub pid: u32,
}

/// The parts of the process environment identification reads (injected so tests need not touch
/// the real one).
pub struct HookEnv {
    pub claude_pid: Option<u32>,
    pub claude_config_dir: Option<PathBuf>,
    pub claude_project_dir: Option<PathBuf>,
}

impl HookEnv {
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            claude_pid: std::env::var("CLAUDE_PID")
                .ok()
                .and_then(|v| v.parse().ok()),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            claude_project_dir: std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from),
        }
    }
}

/// `8-4-4-4-12` hex, the shape of every Claude session id.
#[must_use]
pub fn is_session_uuid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(len, part)| part.len() == *len && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

fn refuse(reason: impl Into<String>) -> Error {
    Error::AdoptionRefused(reason.into())
}

struct RegistryEntry {
    pid: u32,
    session_id: String,
}

/// Claude's own list of running interactive sessions for one config dir.
fn registry_entries(config_dir: &Path) -> Vec<RegistryEntry> {
    let Ok(dir) = std::fs::read_dir(config_dir.join("sessions")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let value: Value = serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()?;
            Some(RegistryEntry {
                pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
                session_id: value.get("sessionId")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

fn process_alive(pid: u32) -> bool {
    ProcessIdentity::query(pid).start_time_fingerprint.is_some()
}

/// Proves which Claude session a hook is running in. `config_dir` is the profile directory the
/// hook was installed for (never the model's word for it).
pub fn identify(input: &HookInput, config_dir: &Path, env: &HookEnv) -> Result<LiveSession, Error> {
    let config = canonical(config_dir).ok_or_else(|| refuse("the profile directory is missing"))?;
    if let Some(from_env) = env.claude_config_dir.as_deref().and_then(canonical)
        && from_env != config
    {
        return Err(refuse(
            "the running Claude uses a different profile directory than this hook was installed for",
        ));
    }
    let session_id = input
        .session_id
        .clone()
        .filter(|id| is_session_uuid(id))
        .ok_or_else(|| refuse("Claude did not report a session id"))?;
    let cwd = input
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .and_then(|path| canonical(&path))
        .ok_or_else(|| refuse("Claude did not report a working directory"))?;
    let project = env
        .claude_project_dir
        .as_deref()
        .and_then(canonical)
        .unwrap_or_else(|| cwd.clone());
    if !cwd.starts_with(&project) {
        return Err(refuse(
            "the working directory is outside the project Claude was started in",
        ));
    }

    // The registry must know this exact session, running, and agree with CLAUDE_PID if given.
    let matches: Vec<RegistryEntry> = registry_entries(&config)
        .into_iter()
        .filter(|entry| entry.session_id == session_id && process_alive(entry.pid))
        .collect();
    let pid = match (matches.as_slice(), env.claude_pid) {
        ([only], Some(reported)) if only.pid == reported => only.pid,
        ([only], None) => only.pid,
        ([], _) => {
            return Err(refuse(
                "Claude's live-session registry has no running process for this session",
            ));
        }
        _ => {
            return Err(refuse(
                "the running process for this session could not be identified unambiguously",
            ));
        }
    };

    // The transcript is what `relay resume` will reopen: it must exist, be this session's, and
    // live inside this profile's own project storage.
    let transcript = input
        .transcript_path
        .as_deref()
        .map(PathBuf::from)
        .and_then(|path| canonical(&path))
        .ok_or_else(|| {
            refuse("the conversation has no saved transcript yet (send a message first)")
        })?;
    let stem = transcript.file_stem().and_then(|stem| stem.to_str());
    if !transcript.starts_with(config.join("projects")) || stem != Some(session_id.as_str()) {
        return Err(refuse(
            "the conversation's transcript does not belong to this profile and session",
        ));
    }
    Ok(LiveSession {
        config_dir: config,
        session_id,
        project,
        pid,
    })
}

#[derive(Debug, Eq, PartialEq)]
pub enum AdoptionOutcome {
    Adopted {
        profile: ProfileName,
        automatic_handoff: bool,
    },
    AlreadyManaged {
        profile: ProfileName,
    },
}

/// The registered Claude profile whose config directory *is* `config_dir`: exactly one, or a
/// refusal. Never inferred from account text.
fn resolve_profile<'a>(registered: &'a [Profile], config_dir: &Path) -> Result<&'a Profile, Error> {
    let mut matching = registered.iter().filter(|profile| {
        profile.provider == ProviderKind::Claude
            && canonical(&profile.config_dir).is_some_and(|dir| dir == config_dir)
    });
    match (matching.next(), matching.next()) {
        (Some(profile), None) => Ok(profile),
        (None, _) => Err(refuse(
            "this Claude profile is not registered with Relay (register it with \
             `relay login <name>`, or start through `relay claude`)",
        )),
        (Some(_), Some(_)) => Err(refuse(
            "more than one Relay profile points at this Claude directory, so which one owns \
             it is ambiguous",
        )),
    }
}

/// Brings `live` under Relay in place. Every proof runs first; then one atomic lease write under
/// the orchestration lock. `expect_profile` (set by `relay claude --resume`) additionally pins the
/// profile the terminal was launched for.
pub fn adopt_claude(
    service: &ProfileService,
    paths: &RelayPaths,
    live: &LiveSession,
    expect_profile: Option<&ProfileName>,
    executables: &providers::ExecutableOverrides,
) -> Result<AdoptionOutcome, Error> {
    let registered = service.list()?;
    let profile = resolve_profile(&registered, &live.config_dir)?;
    if expect_profile.is_some_and(|expected| expected != &profile.name) {
        return Err(refuse(
            "the conversation belongs to a different profile than the one requested",
        ));
    }
    // Identity pin: the account behind this directory must still be the registered one.
    if let Some(reason) = crate::target::identity_refusal_scrubbed(paths, profile, executables) {
        return Err(refuse(reason));
    }

    let project_id = ProjectId::for_canonical_path(&live.project)?;
    let state_dir = paths.project_state_dir(&project_id);
    std::fs::create_dir_all(&state_dir).map_err(|source| Error::Io {
        path: state_dir.clone(),
        source,
    })?;
    let lock = OrchestrationLock::at_path(state_dir.join("orchestration.lock"));
    let store = LeaseStore::at_path(state_dir.join("lease.json"));

    let outcome = lock.try_with(|| -> Result<AdoptionOutcome, Error> {
        if let Some(existing) = store.load()? {
            let same_conversation =
                existing.session_id == live.session_id && existing.owner_profile == profile.name;
            if same_conversation && existing.owner_process.pid == live.pid {
                return Ok(AdoptionOutcome::AlreadyManaged {
                    profile: profile.name.clone(),
                });
            }
            // The same conversation on the same profile whose recorded process is *proven gone*
            // (or was only ever a placeholder) is simply re-bound to the live process now running
            // it: Relay already managed this conversation, its terminal just ended.
            let rebinding = same_conversation
                && (existing.owner_process.pid == 0
                    || existing.owner_process.is_still_the_same_process() == Some(false));
            if !rebinding {
                // Any other Relay writer that is (or cannot be proven not) still running blocks.
                let owner = registered
                    .iter()
                    .find(|candidate| candidate.name == existing.owner_profile);
                let still_active = match owner {
                    Some(owner) => crate::confirm_not_active(
                        owner,
                        &live.project,
                        &existing.session_id,
                        &existing.owner_process,
                        executables,
                    )
                    .map(|inactive| !inactive)?,
                    None => true,
                };
                if still_active {
                    return Err(Error::WriterAlreadyActive(
                        existing.owner_profile.to_string(),
                    ));
                }
            }
        }
        let lease = WriterLease::new(
            project_id.clone(),
            profile.name.clone(),
            ProcessIdentity::query(live.pid),
            live.session_id.clone(),
            TransactionId::generate(),
            crate::current_unix_ms(),
        );
        store.save(&lease)?;
        Ok(AdoptionOutcome::Adopted {
            profile: profile.name.clone(),
            automatic_handoff: relay_provider_claude::integration_status(&live.config_dir)
                .is_ok_and(|status| status.installed),
        })
    })?;

    if matches!(outcome, AdoptionOutcome::Adopted { .. }) {
        // Adoption starts a Relay-managed conversation: no provider arguments are known for it.
        let _ignored = provider_args::ProviderArgs::fresh_for(ProviderKind::Claude, Vec::new())
            .save(&state_dir);
        let preferences = Preferences::load(paths.config_root())
            .ok()
            .flatten()
            .unwrap_or_default();
        let fallback: Vec<ProfileName> =
            auto_handoff::hierarchy_without(&preferences, &profile.name, |_| true)
                .into_iter()
                .cloned()
                .collect();
        crate::bind_herdr_pane(&profile.name, &fallback, &live.session_id);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_must_be_uuid_shaped() {
        assert!(is_session_uuid("c10cf1e1-e830-4d8d-b7a5-ca4135a895ee"));
        assert!(!is_session_uuid("c10cf1e1e8304d8db7a5ca4135a895ee"));
        assert!(!is_session_uuid("not-a-uuid"));
        assert!(!is_session_uuid("c10cf1e1-e830-4d8d-b7a5-ca4135a895ez"));
    }
}
