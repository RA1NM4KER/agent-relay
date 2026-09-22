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
//! `badge_for` is plain text; [`styled`] wraps it in one restrained amber colour (256-colour, reset
//! immediately after so it can never bleed into the rest of the status line) unless `NO_COLOR` is
//! set. The colour exists only in what is printed to the status line — never in any Relay state.

use std::path::{Path, PathBuf};

use relay_core::{
    ProfileName, RelayPaths,
    handoff::{JournalStore, ProjectId, TransactionId},
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
    // The Relay session holding THIS provider conversation — never "the project's lease": with
    // several active sessions in one project each shows its own owner.
    let store = relay_core::handoff::SessionStore::new(
        paths,
        ProjectId::for_canonical_path(&project).ok()?,
    );
    let view = store.find_by_native(&session_id).ok()??;
    let project_state_dir = store.session_dir(&view.record.relay_session_id);
    let lease = view.lease?;
    if lease.session_id != session_id {
        return None;
    }
    let text = match switching_target(&project_state_dir, &session_id) {
        Some(target) => format!("switching → {target}"),
        None => lease.owner_profile.to_string(),
    };
    Some(format!("[Relay · {text}]"))
}

const AMBER: &str = "\u{1b}[38;5;172m";
const AMBER_SWITCHING: &str = "\u{1b}[38;5;179m";
const RESET: &str = "\u{1b}[0m";

/// The badge in muted amber (a lighter amber while switching), honouring `NO_COLOR`.
#[must_use]
pub fn styled(badge: &str) -> String {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    styled_with(badge, no_color)
}

#[must_use]
pub fn styled_with(badge: &str, no_color: bool) -> String {
    if no_color {
        return badge.to_owned();
    }
    let color = if badge.contains("switching") {
        AMBER_SWITCHING
    } else {
        AMBER
    };
    format!("{color}{badge}{RESET}")
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
        let name = |value: &str| ProfileName::new(value).expect("name");
        let sessions = relay_core::handoff::SessionStore::new(&paths, project_id.clone());
        let create = |profile: &str, native: &str| {
            let record = relay_core::handoff::RelaySessionRecord::new(
                relay_core::handoff::RelaySessionId::generate().expect("id"),
                project_id.clone(),
                name(profile),
                None,
                Some(native.to_owned()),
                false,
                1,
            );
            sessions
                .create_active(
                    &record,
                    &WriterLease::new(
                        project_id.clone(),
                        name(profile),
                        ProcessIdentity::current(),
                        native.to_owned(),
                        TransactionId::generate(),
                        1,
                    ),
                )
                .expect("session");
            sessions.session_dir(&record.relay_session_id)
        };
        let state_dir = create("claude-primary", "sess-1");
        std::fs::create_dir_all(state_dir.join("handoffs")).expect("dirs");
        // A second active session in the same project shows ITS OWN owner, never the first's.
        create("claude-other", "sess-2");
        let other = format!(
            r#"{{"session_id":"sess-2","workspace":{{"project_dir":"{}"}}}}"#,
            canonical.display()
        );
        assert_eq!(
            badge_for(&paths, other.as_bytes()).as_deref(),
            Some("[Relay · claude-other]")
        );
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
    fn colour_is_one_amber_span_that_is_always_reset_and_no_color_turns_it_off() {
        assert_eq!(
            styled_with("[Relay · a]", false),
            "\u{1b}[38;5;172m[Relay · a]\u{1b}[0m"
        );
        assert!(styled_with("[Relay · switching → b]", false).starts_with("\u{1b}[38;5;179m"));
        assert_eq!(styled_with("[Relay · a]", true), "[Relay · a]");
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
