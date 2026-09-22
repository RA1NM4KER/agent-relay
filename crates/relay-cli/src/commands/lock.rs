//! `relay lock status`: advanced, read-only inspection of a project's Relay Sessions and their
//! orchestration locks/leases.

use relay_core::{
    Error, RelayPaths,
    handoff::{OrchestrationLock, ProjectId},
};
use serde_json::{Value, json};

use crate::{
    cli::{LockArgs, LockCommand},
    output::{CommandOutput, success},
    sessions,
};

pub(crate) fn run(paths: &RelayPaths, lock: &LockArgs) -> Result<CommandOutput, Error> {
    match &lock.command {
        LockCommand::Status {
            project_dir,
            native_session,
        } => {
            let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                path: project_dir.clone(),
                source,
            })?;
            let project_id = ProjectId::for_canonical_path(&canonical)?;
            let store = sessions::open_store(paths, &canonical)?;
            let views = store.list()?;
            let mut rows = Vec::new();
            for view in &views {
                let dir = store.session_dir(&view.record.relay_session_id);
                let held =
                    OrchestrationLock::at_path(dir.join("orchestration.lock")).is_currently_held();
                let current_transaction =
                    std::fs::read_to_string(dir.join("current_transaction.json")).ok();
                rows.push(json!({
                    "relay_session_id": view.record.relay_session_id.as_str(),
                    "state": view.state(),
                    "locked": held,
                    "lease": view.lease,
                    "current_transaction": current_transaction,
                }));
            }
            let human = format!(
                "Project: {}\n{}",
                canonical.display(),
                if views.is_empty() {
                    "No Relay sessions.".to_owned()
                } else {
                    views
                        .iter()
                        .zip(&rows)
                        .map(|(view, row)| {
                            format!(
                                "  {} {:?} owner/last: {} lock held: {}",
                                view.record.relay_session_id.short(),
                                view.state(),
                                view.profile(),
                                row["locked"]
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            );
            // The top-level `locked` / `lease` / `current_transaction` keep their old shape for
            // consumers (the Herdr plugin): they describe the session named by
            // `--native-session`, else the only session, else nothing in particular.
            let focus = match native_session {
                Some(native) => views
                    .iter()
                    .position(|view| view.native_session_id() == Some(native.as_str())),
                None if views.len() == 1 => Some(0),
                None => None,
            };
            let (locked, lease, current_transaction) = match focus {
                Some(index) => (
                    rows[index]["locked"].clone(),
                    rows[index]["lease"].clone(),
                    rows[index]["current_transaction"].clone(),
                ),
                None => (
                    json!(rows.iter().any(|row| row["locked"] == true)),
                    Value::Null,
                    Value::Null,
                ),
            };
            success(
                "lock.status",
                human,
                json!({
                    "project_id": project_id.as_str(),
                    "locked": locked,
                    "lease": lease,
                    "current_transaction": current_transaction,
                    "sessions": rows,
                }),
            )
        }
    }
}
