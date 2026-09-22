//! `relay history`: a readable timeline of recent Relay activity for this project, derived
//! entirely from existing durable state (Relay Session records, the automation ledger, and
//! handoff journals) — no new event database, and nothing beyond what's already recorded ever
//! leaks here (no transcript contents, no provider output, no credentials).

use relay_core::{
    Error, ProfileService, RelayPaths,
    automation::LedgerStore,
    handoff::{HandoffState, JournalStore},
};
use serde_json::json;

use crate::{
    cli::HistoryArgs,
    output::{CommandOutput, success},
    sessions,
};

struct Event {
    unix_ms: u64,
    summary: String,
    detail: Option<String>,
}

pub(crate) fn run(
    _service: &ProfileService,
    paths: &RelayPaths,
    args: &HistoryArgs,
) -> Result<CommandOutput, Error> {
    let project_dir = match &args.project_dir {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: std::path::PathBuf::from("."),
            source,
        })?,
    };
    let canonical_project = std::fs::canonicalize(&project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;
    let store = sessions::open_store(paths, &canonical_project)?;
    let mut views = store.list()?;
    if let Some(selector) = &args.session {
        views.retain(|view| {
            view.record
                .relay_session_id
                .as_str()
                .starts_with(selector.as_str())
                || view
                    .record
                    .relay_session_id
                    .short()
                    .starts_with(selector.as_str())
        });
        if views.is_empty() {
            return Err(Error::RelaySessionNotFound(selector.clone()));
        }
    }

    let mut events: Vec<Event> = Vec::new();
    for view in &views {
        let dir = store.session_dir(&view.record.relay_session_id);
        let short = view.record.relay_session_id.short().to_owned();

        events.push(Event {
            unix_ms: view.record.created_unix_ms,
            summary: format!("Session started on {}", view.record.last_profile),
            detail: Some(format!("session {short}")),
        });

        if let Ok(ledger) = LedgerStore::at_path(dir.join("automation_state.json")).load() {
            for handoff in &ledger.recent_handoffs {
                events.push(Event {
                    unix_ms: handoff.unix_ms,
                    summary: format!("Conversation moved {} → {}", handoff.source, handoff.target),
                    detail: handoff
                        .transaction_id
                        .as_ref()
                        .map(|id| format!("transaction {id}")),
                });
            }
            for exhausted in &ledger.known_exhausted {
                events.push(Event {
                    unix_ms: exhausted.observed_unix_ms,
                    summary: format!("{} marked exhausted", exhausted.profile),
                    detail: Some(exhausted.detected_via.clone()),
                });
            }
        }

        if let Ok(entries) = std::fs::read_dir(dir.join("handoffs")) {
            for entry in entries.flatten() {
                if entry.path().extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                let Ok(journal) = JournalStore::at_path(entry.path()).load() else {
                    continue;
                };
                match &journal.state {
                    HandoffState::Complete => events.push(Event {
                        unix_ms: journal.updated_unix_ms,
                        summary: format!(
                            "Conversation moved {} → {}",
                            journal.source_profile, journal.target_profile
                        ),
                        detail: Some(format!("transaction {}", journal.transaction_id)),
                    }),
                    HandoffState::Failed { reason, .. } => events.push(Event {
                        unix_ms: journal.updated_unix_ms,
                        summary: "Automatic handoff failed".to_owned(),
                        detail: Some(reason.clone()),
                    }),
                    HandoffState::RecoveryRequired { reason } => events.push(Event {
                        unix_ms: journal.updated_unix_ms,
                        summary: "Handoff needs recovery".to_owned(),
                        detail: Some(reason.clone()),
                    }),
                    _ => {}
                }
            }
        }
    }

    events.sort_by_key(|event| std::cmp::Reverse(event.unix_ms));
    events.dedup_by(|left, right| left.unix_ms == right.unix_ms && left.summary == right.summary);
    events.truncate(args.limit);

    let human = render_human(&events);
    let data = json!({
        "events": events
            .iter()
            .map(|event| json!({
                "unix_ms": event.unix_ms,
                "summary": event.summary,
                "detail": event.detail,
            }))
            .collect::<Vec<_>>(),
    });
    success("history", human, data)
}

fn render_human(events: &[Event]) -> String {
    if events.is_empty() {
        return "Recent Relay activity\n\nNothing recorded yet.".to_owned();
    }
    let mut lines = vec!["Recent Relay activity".to_owned(), String::new()];
    for event in events {
        lines.push(format!(
            "{}  {}",
            format_clock(event.unix_ms),
            event.summary
        ));
        if let Some(detail) = &event.detail {
            lines.push(format!("       {detail}"));
        }
    }
    lines.join("\n")
}

/// `HH:MM` UTC (no calendar/timezone crate in this workspace, so this is deliberately not
/// converted to local time) — `--json`'s `unix_ms` is the source of truth for anything that needs
/// to be exact.
fn format_clock(unix_ms: u64) -> String {
    let secs_since_midnight_utc = (unix_ms / 1000) % 86400;
    let hours = secs_since_midnight_utc / 3600;
    let minutes = (secs_since_midnight_utc % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

#[cfg(test)]
mod tests {
    use super::{Event, format_clock, render_human};

    #[test]
    fn format_clock_renders_hh_mm_utc_and_wraps_at_midnight() {
        assert_eq!(format_clock(0), "00:00");
        assert_eq!(format_clock((15 * 3600 + 56 * 60) * 1000), "15:56");
        assert_eq!(format_clock((23 * 3600 + 59 * 60) * 1000), "23:59");
        // A second day's worth of ms still reads as a time of day, not an overflowed one.
        assert_eq!(format_clock((86_400 + 14 * 3600 + 27 * 60) * 1000), "14:27");
    }

    #[test]
    fn render_human_has_a_friendly_empty_state() {
        let human = render_human(&[]);
        assert_eq!(human, "Recent Relay activity\n\nNothing recorded yet.");
    }

    #[test]
    fn render_human_shows_clock_summary_and_an_indented_detail_line() {
        let events = vec![
            Event {
                unix_ms: (15 * 3600 + 56 * 60) * 1000,
                summary: "Conversation moved Erika \u{2192} Megan".to_owned(),
                detail: None,
            },
            Event {
                unix_ms: (14 * 3600 + 43 * 60) * 1000,
                summary: "Automatic handoff failed".to_owned(),
                detail: Some("Claude transcript not found".to_owned()),
            },
        ];
        let human = render_human(&events);
        assert_eq!(
            human,
            "Recent Relay activity\n\n\
             15:56  Conversation moved Erika \u{2192} Megan\n\
             14:43  Automatic handoff failed\n\
             \u{20}      Claude transcript not found"
        );
    }
}
