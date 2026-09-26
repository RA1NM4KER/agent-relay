//! Local, read-only operational provenance. Journals and session records stay authoritative.
use crate::{
    cli::HistoryArgs,
    output::{CommandOutput, success},
    sessions,
};
use relay_core::{
    Error, ProfileService, RelayPaths,
    automation::LedgerStore,
    handoff::{HandoffJournal, JournalStore, RelaySessionView},
};
use serde::Serialize;

#[derive(Clone, Serialize)]
struct Event {
    timestamp_unix_ms: u64,
    sequence: u64,
    kind: String,
    actor: String,
    profile: Option<String>,
    handoff_id: Option<String>,
    /// The coordinator state that supplied this observation, when applicable.  This is kept
    /// separate from the normalized kind so JSON consumers do not need to parse display prose.
    state: Option<String>,
    /// A wall-clock duration only when the journal supplies both endpoints.  It is diagnostic,
    /// not a scheduler or recovery input.
    handoff_elapsed_ms: Option<u64>,
}

pub(crate) fn run(
    _service: &ProfileService,
    paths: &RelayPaths,
    args: &HistoryArgs,
) -> Result<CommandOutput, Error> {
    let requested = args
        .project_dir
        .clone()
        .unwrap_or(std::env::current_dir().map_err(|source| Error::Io {
            path: ".".into(),
            source,
        })?);
    let project = std::fs::canonicalize(&requested).map_err(|source| Error::Io {
        path: requested,
        source,
    })?;
    let store = sessions::open_store(paths, &project)?;
    let view = if let Some(id) = &args.session {
        store.resolve_read_only(id)?
    } else {
        let mut views = store.list_read_only()?;
        views.sort_by_key(|v| std::cmp::Reverse(v.record.last_activity_unix_ms));
        views.into_iter().next().ok_or_else(|| {
            Error::RelaySessionNotFound("no Relay sessions for this project".into())
        })?
    };
    let dir = store.session_dir(&view.record.relay_session_id);
    let mut events = vec![Event {
        timestamp_unix_ms: view.record.created_unix_ms,
        sequence: 0,
        kind: "session_started".into(),
        actor: "relay".into(),
        profile: Some(view.record.last_profile.to_string()),
        handoff_id: None,
        state: None,
        handoff_elapsed_ms: None,
    }];
    let mut warnings = Vec::new();
    match LedgerStore::at_path(dir.join("automation_state.json")).load() {
        Ok(ledger) => {
            for exhausted in ledger.known_exhausted {
                events.push(Event {
                    timestamp_unix_ms: exhausted.observed_unix_ms,
                    sequence: 1,
                    kind: "exhaustion_detected".into(),
                    actor: exhausted.profile.to_string(),
                    profile: Some(exhausted.profile.to_string()),
                    handoff_id: None,
                    state: None,
                    handoff_elapsed_ms: None,
                });
            }
        }
        Err(Error::Io { .. }) => (),
        Err(_) => warnings.push("automation ledger could not be read"),
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("handoffs")) {
        for entry in entries.flatten() {
            if entry.path().extension().is_none_or(|x| x != "json") {
                continue;
            }
            match JournalStore::at_path(entry.path()).load() {
                Ok(journal) => add_journal(&mut events, &journal),
                Err(_) => warnings.push("a handoff journal could not be read"),
            }
        }
    }
    events.sort_by(|a, b| {
        (a.timestamp_unix_ms, a.sequence, &a.handoff_id, &a.kind).cmp(&(
            b.timestamp_unix_ms,
            b.sequence,
            &b.handoff_id,
            &b.kind,
        ))
    });
    // The command is a chronology, so a finite view keeps the newest observations.  A limit of
    // zero is useful to scripts that only need the session header.
    if events.len() > args.limit {
        let keep_from = events.len() - args.limit;
        events.drain(..keep_from);
    }
    let human = render(&view, &events, &project);
    success(
        "history",
        human,
        serde_json::json!({"session": {"id": view.record.relay_session_id, "execution_intent": view.record.execution_intent, "created_unix_ms": view.record.created_unix_ms, "last_profile": view.record.last_profile}, "events": events, "warnings": warnings}),
    )
}

fn add_journal(events: &mut Vec<Event>, journal: &HandoffJournal) {
    events.push(Event {
        timestamp_unix_ms: journal.created_unix_ms,
        sequence: 2,
        kind: "fallback_selected".into(),
        actor: journal.target_profile.to_string(),
        profile: Some(journal.target_profile.to_string()),
        handoff_id: Some(journal.transaction_id.to_string()),
        state: None,
        handoff_elapsed_ms: None,
    });
    for (index, point) in journal.state_timestamps.iter().enumerate() {
        let (kind, actor, profile) = match point.state.as_str() {
            "PREPARING" => ("handoff_preparing", "relay".to_owned(), None),
            "CHECKPOINTED" => ("working_state_captured", "relay".to_owned(), None),
            "SOURCE_STOPPING" => (
                "source_stopping",
                journal.source_profile.to_string(),
                Some(journal.source_profile.to_string()),
            ),
            "SOURCE_STOPPED" => (
                "source_stopped",
                journal.source_profile.to_string(),
                Some(journal.source_profile.to_string()),
            ),
            "SESSION_TRANSFERRING" => ("session_transferring", "relay".to_owned(), None),
            "SESSION_TRANSFERRED" => ("session_transferred", "relay".to_owned(), None),
            "TARGET_STARTING" => (
                "target_starting",
                journal.target_profile.to_string(),
                Some(journal.target_profile.to_string()),
            ),
            "TARGET_VERIFIED" => (
                "target_verified",
                journal.target_profile.to_string(),
                Some(journal.target_profile.to_string()),
            ),
            "COMPLETE" => ("handoff_complete", "relay".to_owned(), None),
            "FAILED" | "RECOVERY_REQUIRED" => ("handoff_failed", "relay".to_owned(), None),
            _ => continue,
        };
        events.push(Event {
            timestamp_unix_ms: point.unix_ms,
            sequence: 10 + index as u64,
            kind: kind.into(),
            actor,
            profile,
            handoff_id: Some(journal.transaction_id.to_string()),
            state: Some(point.state.clone()),
            handoff_elapsed_ms: (point.state == "COMPLETE")
                .then(|| point.unix_ms.saturating_sub(journal.created_unix_ms)),
        });
    }
}
fn render(view: &RelaySessionView, events: &[Event], project: &std::path::Path) -> String {
    let intent = match view.record.execution_intent {
        relay_core::handoff::ExecutionIntent::Autonomous => "autonomous",
        relay_core::handoff::ExecutionIntent::Interactive => "interactive",
    };
    let mut lines = vec![
        format!("Relay Session: {}", view.record.relay_session_id),
        format!(
            "Project: {}",
            project.file_name().unwrap_or_default().to_string_lossy()
        ),
        format!("Execution: {intent}"),
        format!(
            "Started: {} UTC (exact timestamp in --json)",
            clock(view.record.created_unix_ms)
        ),
        String::new(),
    ];
    let mut handoff = None;
    for event in events {
        if event.handoff_id.as_deref() != handoff {
            if let Some(id) = event.handoff_id.as_deref() {
                lines.push(String::new());
                lines.push(format!("Handoff: {id}"));
            }
            handoff = event.handoff_id.as_deref();
        }
        lines.push(format!(
            "{}  {:<14} {}{}",
            clock(event.timestamp_unix_ms),
            event.actor,
            human_kind(&event.kind),
            event
                .handoff_elapsed_ms
                .map(|duration| format!(" ({:.1}s)", duration as f64 / 1_000.0))
                .unwrap_or_default(),
        ));
    }
    lines.join("\n")
}

fn human_kind(kind: &str) -> String {
    let mut text = kind.replace('_', " ");
    if let Some(first) = text.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    text
}
fn clock(ms: u64) -> String {
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use relay_core::{
        ProfileName,
        handoff::{
            ContinuityType, HandoffJournal, HandoffStateTimestamp, ProjectId, TransactionId,
        },
    };

    use super::{Event, add_journal, clock};

    fn journal(source: &str, target: &str, id: &str, states: &[(&str, u64)]) -> HandoffJournal {
        let mut journal = HandoffJournal::new(
            TransactionId::parse(id).expect("transaction id"),
            ProjectId::for_canonical_path(std::path::Path::new("/tmp/history-test"))
                .expect("project id"),
            "/tmp/history-test".into(),
            ProfileName::new(source).expect("source"),
            ProfileName::new(target).expect("target"),
            "/tmp/target".into(),
            "native-session".into(),
            ContinuityType::StateContinuation,
        );
        journal.created_unix_ms = states[0].1;
        journal.state_timestamps = states
            .iter()
            .map(|(state, unix_ms)| HandoffStateTimestamp {
                state: (*state).into(),
                unix_ms: *unix_ms,
            })
            .collect();
        journal
    }

    #[test]
    fn equal_timestamps_order_by_durable_sequence_then_identity() {
        let mut events = [
            Event {
                timestamp_unix_ms: 10,
                sequence: 2,
                kind: "target_starting".into(),
                actor: "b".into(),
                profile: None,
                handoff_id: Some("b".into()),
                state: None,
                handoff_elapsed_ms: None,
            },
            Event {
                timestamp_unix_ms: 10,
                sequence: 1,
                kind: "source_stopped".into(),
                actor: "a".into(),
                profile: None,
                handoff_id: Some("a".into()),
                state: None,
                handoff_elapsed_ms: None,
            },
        ];
        events.sort_by(|a, b| {
            (a.timestamp_unix_ms, a.sequence, &a.handoff_id, &a.kind).cmp(&(
                b.timestamp_unix_ms,
                b.sequence,
                &b.handoff_id,
                &b.kind,
            ))
        });
        assert_eq!(events[0].kind, "source_stopped");
        assert_eq!(clock((15 * 3600 + 56 * 60 + 7) * 1000), "15:56:07");
    }

    #[test]
    fn preserves_two_handoffs_and_never_reorders_source_stop_after_target_start() {
        let first = journal(
            "codex-main",
            "claude-main",
            "ho-0000000000000001-00001",
            &[
                ("PREPARING", 100),
                ("SOURCE_STOPPED", 110),
                ("TARGET_STARTING", 120),
                ("COMPLETE", 130),
            ],
        );
        let second = journal(
            "claude-main",
            "codex-backup",
            "ho-0000000000000002-00002",
            &[("PREPARING", 200), ("COMPLETE", 210)],
        );
        let mut events = Vec::new();
        add_journal(&mut events, &second);
        add_journal(&mut events, &first);
        events.sort_by(|a, b| {
            (a.timestamp_unix_ms, a.sequence, &a.handoff_id, &a.kind).cmp(&(
                b.timestamp_unix_ms,
                b.sequence,
                &b.handoff_id,
                &b.kind,
            ))
        });

        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "handoff_complete")
                .count(),
            2
        );
        let stopped = events
            .iter()
            .position(|event| event.kind == "source_stopped")
            .expect("source stopped event");
        let starting = events
            .iter()
            .position(|event| event.kind == "target_starting")
            .expect("target starting event");
        assert!(stopped < starting);
    }

    #[test]
    fn failed_handoff_is_not_rendered_as_complete() {
        let failed = journal(
            "codex-main",
            "claude-main",
            "ho-0000000000000003-00003",
            &[
                ("PREPARING", 100),
                ("SOURCE_STOPPED", 110),
                ("TARGET_STARTING", 120),
                ("FAILED", 130),
            ],
        );
        let mut events = Vec::new();
        add_journal(&mut events, &failed);
        assert!(events.iter().any(|event| event.kind == "handoff_failed"));
        assert!(!events.iter().any(|event| event.kind == "handoff_complete"));
    }
}
