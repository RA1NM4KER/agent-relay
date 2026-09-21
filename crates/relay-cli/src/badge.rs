//! The small Relay badge in Claude's status line: `[Relay · <profile>]`.
//!
//! It answers one question — *is this Claude session Relay-managed, and who owns it right now?* —
//! and nothing else (no quota, no reset times, no fallback chain). It rides on the status-line
//! command Relay's usage integration already installs (`relay hook claude statusline`, which chains
//! the user's own status line rather than replacing it), so there is no second status-line system.
//!
//! The owner always comes from Relay's authoritative project state, read fresh on every render:
//!
//! * managed  ⇔ the project's writer lease names *this exact session id* — a plain Claude session
//!   that merely lives in the same directory (different id) never shows the badge, and neither does
//!   a Claude session whose conversation has since moved to another provider (the lease then
//!   names a different session id, so nothing stale is claimed);
//! * owner    = the lease's `owner_profile`, so after a Claude → Claude handoff (same session id)
//!   the badge changes owner without restarting anything;
//! * `switching → <target>` only while a handoff transaction for this session is genuinely in
//!   progress (its journal says so), never merely because an evaluation briefly held the lock.
//!
//! Output is plain text: no colour, no escape sequences.

use std::path::{Path, PathBuf};

use relay_core::{
    ProfileName, RelayPaths,
    handoff::{JournalStore, LeaseStore, ProjectId, TransactionId},
};
use serde_json::Value;

/// `(session_id, project_dir)` from the JSON Claude feeds a status-line command. The project is
/// Claude's original launch directory (`workspace.project_dir`) when present, else `cwd`, because
/// Relay keys its state on the directory the managed session was started in — not wherever the
/// session has since changed to.
#[must_use]
pub fn parse_input(stdin: &[u8]) -> Option<(String, PathBuf)> {
    let value: Value = serde_json::from_slice(stdin).ok()?;
    let session_id = value.get("session_id")?.as_str()?.to_owned();
    let dir = value
        .pointer("/workspace/project_dir")
        .and_then(Value::as_str)
        .or_else(|| value.get("cwd").and_then(Value::as_str))
        .or_else(|| {
            value
                .pointer("/workspace/current_dir")
                .and_then(Value::as_str)
        })?;
    Some((session_id, PathBuf::from(dir)))
}

/// The badge for this status-line invocation, or `None` when the session is not Relay-managed
/// (or anything cannot be established — the badge never guesses).
#[must_use]
pub fn badge_for(paths: &RelayPaths, stdin: &[u8]) -> Option<String> {
    let (session_id, dir) = parse_input(stdin)?;
    let project = std::fs::canonicalize(dir).ok()?;
    let project_state_dir = paths.project_state_dir(&ProjectId::for_canonical_path(&project).ok()?);
    let lease = LeaseStore::at_path(project_state_dir.join("lease.json"))
        .load()
        .ok()??;
    if lease.session_id != session_id {
        return None;
    }
    let text = match switching_target(&project_state_dir, &session_id) {
        Some(target) => format!("switching → {target}"),
        None => lease.owner_profile.to_string(),
    };
    Some(format!("[Relay · {text}]"))
}

/// The target profile of a handoff of *this session* that is still in progress.
fn switching_target(project_state_dir: &Path, session_id: &str) -> Option<ProfileName> {
    let pointer =
        std::fs::read_to_string(project_state_dir.join("current_transaction.json")).ok()?;
    let id = TransactionId::parse(pointer.trim()).ok()?;
    let journal = JournalStore::at_path(
        project_state_dir
            .join("handoffs")
            .join(format!("{id}.json")),
    )
    .load()
    .ok()?;
    (journal.session_id == session_id && journal.state.is_in_progress())
        .then_some(journal.target_profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_is_shown_only_while_this_sessions_handoff_is_in_progress() {
        use relay_core::handoff::{
            ContinuityType, HandoffJournal, HandoffState, ProcessIdentity, WriterLease,
        };
        let root = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("project");
        let paths =
            RelayPaths::new(root.path().join("config"), root.path().join("state")).expect("paths");
        let canonical = std::fs::canonicalize(project.path()).expect("canonical");
        let project_id = ProjectId::for_canonical_path(&canonical).expect("id");
        let state_dir = paths.project_state_dir(&project_id);
        std::fs::create_dir_all(state_dir.join("handoffs")).expect("dirs");
        let name = |value: &str| ProfileName::new(value).expect("name");
        LeaseStore::at_path(state_dir.join("lease.json"))
            .save(&WriterLease::new(
                project_id.clone(),
                name("claude-primary"),
                ProcessIdentity::current(),
                "sess-1".to_owned(),
                TransactionId::generate(),
                1,
            ))
            .expect("lease");
        let input = format!(
            r#"{{"session_id":"sess-1","workspace":{{"project_dir":"{}"}}}}"#,
            canonical.display()
        );
        assert_eq!(
            badge_for(&paths, input.as_bytes()).as_deref(),
            Some("[Relay · claude-primary]")
        );

        let id = TransactionId::generate();
        let mut journal = HandoffJournal::new(
            id.clone(),
            project_id,
            canonical,
            name("claude-primary"),
            name("codex-backup"),
            PathBuf::from("/tmp/codex-backup"),
            "sess-1".to_owned(),
            ContinuityType::StateContinuation,
        );
        let store = JournalStore::at_path(state_dir.join("handoffs").join(format!("{id}.json")));
        store.save(&journal).expect("journal");
        std::fs::write(state_dir.join("current_transaction.json"), id.to_string())
            .expect("pointer");
        assert_eq!(
            badge_for(&paths, input.as_bytes()).as_deref(),
            Some("[Relay · switching → codex-backup]")
        );

        journal.state = HandoffState::Complete;
        store.save(&journal).expect("journal");
        assert_eq!(
            badge_for(&paths, input.as_bytes()).as_deref(),
            Some("[Relay · claude-primary]"),
            "a finished transaction is no longer 'switching'"
        );
    }

    #[test]
    fn input_prefers_the_original_project_dir_and_needs_a_session_id() {
        let (session, dir) = parse_input(
            br#"{"session_id":"s1","cwd":"/a/b","workspace":{"project_dir":"/a","current_dir":"/a/b"}}"#,
        )
        .expect("parsed");
        assert_eq!(session, "s1");
        assert_eq!(dir, PathBuf::from("/a"));
        let (_, dir) = parse_input(br#"{"session_id":"s1","cwd":"/a/b"}"#).expect("cwd fallback");
        assert_eq!(dir, PathBuf::from("/a/b"));
        assert!(parse_input(br#"{"cwd":"/a"}"#).is_none());
        assert!(parse_input(b"not json").is_none());
    }
}
