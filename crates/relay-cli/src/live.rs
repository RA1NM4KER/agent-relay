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
    ClaudeConfigMode, Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
    handoff::{ProcessIdentity, RelaySessionId, RelaySessionRecord, TransactionId, WriterLease},
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

/// Identifies a LIVE Claude conversation from OUTSIDE it — no hook stdin, no inherited
/// `CLAUDE_PID`/`CLAUDE_CONFIG_DIR` environment — for `relay adopt --session <id>`. That command
/// exists precisely because a Claude process started before Relay's `/relay` command was
/// installed never sees it (Claude only loads custom commands at session start), so the
/// in-session `identify()` path is unreachable for it. Every fact still comes from Claude's own
/// structural, provider-owned metadata: `claude agents --json` (the live pid, and the cwd it
/// itself reports for that pid) and the on-disk transcript layout (proves the conversation is
/// really saved under this exact profile and session) — never a guess from a profile name.
pub fn identify_external(
    config_dir: &Path,
    mode: ClaudeConfigMode,
    claude_executable: Option<&Path>,
    session_id: &str,
) -> Result<LiveSession, Error> {
    if !is_session_uuid(session_id) {
        return Err(refuse(format!("'{session_id}' is not a Claude session id")));
    }
    let config = canonical(config_dir).ok_or_else(|| refuse("the profile directory is missing"))?;
    let identity = relay_provider_claude::find_live_pid_for_session(
        &config,
        mode,
        claude_executable,
        session_id,
    )
    .ok_or_else(|| {
        refuse(
            "Claude's own live-session registry has no running process for this session \
                     under this profile",
        )
    })?;
    let sessions = relay_provider_claude::query_active_sessions(&config, mode, claude_executable)
        .map_err(|_| refuse("could not query Claude's live session registry"))?;
    let record = sessions
        .into_iter()
        .find(|record| record.session_id == session_id && record.pid == Some(identity.pid))
        .ok_or_else(|| {
            refuse("the running process for this session could not be identified unambiguously")
        })?;
    let cwd = record
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .and_then(|path| canonical(&path))
        .ok_or_else(|| refuse("Claude did not report a working directory for this session"))?;

    // The transcript is what `relay resume` will reopen: it must exist under this exact profile's
    // own project storage, named exactly for this session id.
    let escaped = relay_provider_claude::escape_project_path(&cwd);
    let transcript = config
        .join("projects")
        .join(&escaped)
        .join(format!("{session_id}.jsonl"));
    if !transcript.is_file() {
        return Err(refuse(
            "the conversation has no saved transcript yet under this profile (send a message \
             first)",
        ));
    }

    Ok(LiveSession {
        config_dir: config,
        session_id: session_id.to_owned(),
        project: cwd,
        pid: identity.pid,
    })
}

#[derive(Debug, Eq, PartialEq)]
pub enum AdoptionOutcome {
    /// A new Relay session was created for the conversation.
    Adopted {
        profile: ProfileName,
        relay_session_id: RelaySessionId,
        automatic_handoff: bool,
    },
    /// The conversation was already a (dormant) Relay session: the same session is active again.
    Reactivated {
        profile: ProfileName,
        relay_session_id: RelaySessionId,
        automatic_handoff: bool,
    },
    AlreadyManaged {
        profile: ProfileName,
        relay_session_id: RelaySessionId,
    },
}

impl AdoptionOutcome {
    #[must_use]
    pub fn relay_session_id(&self) -> &RelaySessionId {
        match self {
            Self::Adopted {
                relay_session_id, ..
            }
            | Self::Reactivated {
                relay_session_id, ..
            }
            | Self::AlreadyManaged {
                relay_session_id, ..
            } => relay_session_id,
        }
    }
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
    preassigned: Option<RelaySessionId>,
    user_args: Vec<String>,
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

    // The unit of ownership is the conversation, not the repository: other Relay sessions in this
    // project (under any profile, this one included) are irrelevant. What must never happen is the
    // SAME native conversation having two active owners.
    let store = crate::sessions::open_store(paths, &live.project)?;
    let now = crate::util::current_unix_ms();
    let automatic_handoff = relay_provider_claude::integration_status(&live.config_dir)
        .is_ok_and(|status| status.installed);
    let lease = WriterLease::new(
        store.project_id().clone(),
        profile.name.clone(),
        ProcessIdentity::query(live.pid),
        live.session_id.clone(),
        TransactionId::generate(),
        now,
    );
    let mut existing = store.find_by_native(&live.session_id)?;
    if let Some(view) = &existing
        && let Some(held) = &view.lease
    {
        let same_process = held.owner_process.pid == live.pid
            && held.owner_process.is_still_the_same_process() != Some(false);
        if same_process && held.owner_profile == profile.name {
            return Ok(AdoptionOutcome::AlreadyManaged {
                profile: profile.name.clone(),
                relay_session_id: view.record.relay_session_id.clone(),
            });
        }
        // Another process holds it — unless that owner is provably gone (the terminal that
        // managed it ended), in which case the conversation is simply re-bound below.
        let owner = registered
            .iter()
            .find(|candidate| candidate.name == held.owner_profile);
        if crate::sessions::lease_is_stale(held, owner, &live.project, executables, now) {
            crate::sessions::release_session(&store, view, &registered, now)?;
            existing = store.find_by_native(&live.session_id)?;
        } else {
            return Err(Error::NativeSessionAlreadyActive(
                view.record.relay_session_id.short().to_owned(),
            ));
        }
    }
    let (outcome, session_dir) = match existing {
        Some(view) => {
            let id = view.record.relay_session_id.clone();
            store.activate(&id, &lease)?;
            (
                AdoptionOutcome::Reactivated {
                    profile: profile.name.clone(),
                    relay_session_id: id.clone(),
                    automatic_handoff,
                },
                store.session_dir(&id),
            )
        }
        None => {
            let id = match preassigned {
                Some(id) => id,
                None => RelaySessionId::generate()?,
            };
            let record = RelaySessionRecord::new(
                id.clone(),
                store.project_id().clone(),
                profile.name.clone(),
                Some("claude".to_owned()),
                Some(live.session_id.clone()),
                false,
                now,
            );
            store.create_active(&record, &lease)?;
            (
                AdoptionOutcome::Adopted {
                    profile: profile.name.clone(),
                    relay_session_id: id.clone(),
                    automatic_handoff,
                },
                store.session_dir(&id),
            )
        }
    };

    // Only the arguments the user asked for on this launch belong to the session; Relay's own
    // launch flags (the adoption hook) are never stored.
    let _ignored =
        provider_args::ProviderArgs::fresh_for(ProviderKind::Claude, user_args).save(&session_dir);
    let preferences = Preferences::load(paths.config_root())
        .ok()
        .flatten()
        .unwrap_or_default();
    let fallback: Vec<ProfileName> =
        auto_handoff::hierarchy_without(&preferences, &profile.name, |_| true)
            .into_iter()
            .cloned()
            .collect();
    crate::util::bind_herdr_pane(&profile.name, &fallback, &live.session_id);
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
