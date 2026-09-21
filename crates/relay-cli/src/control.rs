//! The local control channel between an in-agent `/relay …` command and the Relay process that
//! supervises that agent's terminal.
//!
//! It is deliberately the smallest thing that works: a private directory inside the project's own
//! Relay state (`…/control/`, mode 0700, same user only) holding
//!
//! - `supervisor.json` — written by the supervising `relay` process while it runs: its pid and
//!   process fingerprint, plus the owner profile and session it currently supervises;
//! - `request-<id>.json` — one request from the in-agent hook (`switch <profile>`), atomically
//!   written;
//! - `response-<id>.json` — the supervisor's answer (accepted / refused, with the reason);
//! - `last.json` — the outcome of the most recent executed request, for `/relay status`.
//!
//! No socket, no daemon, no network and no shared credentials. A request is only honoured when it
//! names the current lease's session and owner profile *and* the process that proved (from Claude's
//! own registry, see [`crate::live`]) that it is that session; anything stale or mismatched is
//! refused. The agent process never runs the switch itself — the supervisor does, through the same
//! `relay switch` transaction, and its terminal follows the new owner.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use relay_core::handoff::ProcessIdentity;
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
/// A request older than this is stale and is discarded unanswered.
pub const REQUEST_MAX_AGE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct SupervisorRecord {
    pub version: u32,
    pub pid: u32,
    pub fingerprint: Option<String>,
    pub owner_profile: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestKind {
    Switch { target: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub request: RequestKind,
    /// The lease session and owner the requester believes it belongs to.
    pub session_id: String,
    pub owner_profile: String,
    /// The Claude process that proved itself as that session.
    pub caller_pid: u32,
    pub requested_unix_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Response {
    pub id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct LastResult {
    pub ok: bool,
    pub message: String,
    pub unix_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Whether a recorded action is recent enough to still be worth reporting (15 minutes).
#[must_use]
pub fn is_recent(unix_ms: u64) -> bool {
    now_ms().saturating_sub(unix_ms) < 15 * 60 * 1000
}

pub struct ControlDir {
    dir: PathBuf,
}

impl ControlDir {
    #[must_use]
    pub fn for_project(project_state_dir: &Path) -> Self {
        Self {
            dir: project_state_dir.join("control"),
        }
    }

    fn ensure(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.ensure()?;
        let temporary = self.dir.join(format!(".{name}.tmp"));
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, self.dir.join(name))
    }

    // ---- supervisor side -------------------------------------------------------------------

    pub fn publish_supervisor(&self, owner_profile: &str, session_id: &str) {
        let record = SupervisorRecord {
            version: VERSION,
            pid: std::process::id(),
            fingerprint: ProcessIdentity::query(std::process::id()).start_time_fingerprint,
            owner_profile: owner_profile.to_owned(),
            session_id: session_id.to_owned(),
        };
        if let Ok(bytes) = serde_json::to_vec(&record) {
            let _ignored = self.write_atomic("supervisor.json", &bytes);
        }
    }

    /// Removes the record, but only if it is still this process's.
    pub fn clear_supervisor(&self) {
        let path = self.dir.join("supervisor.json");
        let ours = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SupervisorRecord>(&bytes).ok())
            .is_some_and(|record| record.pid == std::process::id());
        if ours {
            let _ignored = fs::remove_file(path);
        }
    }

    /// The oldest pending request, removed from disk (so it is handled at most once). Stale or
    /// unreadable request files are discarded.
    pub fn take_request(&self) -> Option<Request> {
        let mut names: Vec<PathBuf> = fs::read_dir(&self.dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("request-") && name.ends_with(".json"))
            })
            .collect();
        names.sort();
        for path in names {
            let bytes = fs::read(&path).ok();
            let _ignored = fs::remove_file(&path);
            let Some(request) =
                bytes.and_then(|bytes| serde_json::from_slice::<Request>(&bytes).ok())
            else {
                continue;
            };
            let age = now_ms().saturating_sub(request.requested_unix_ms);
            if request.version == VERSION
                && age <= u64::try_from(REQUEST_MAX_AGE.as_millis()).unwrap_or(0)
            {
                return Some(request);
            }
        }
        None
    }

    pub fn respond(&self, response: &Response) {
        if let Ok(bytes) = serde_json::to_vec(response) {
            let _ignored = self.write_atomic(&format!("response-{}.json", response.id), &bytes);
        }
    }

    pub fn record_last(&self, ok: bool, message: &str) {
        let last = LastResult {
            ok,
            message: message.to_owned(),
            unix_ms: now_ms(),
        };
        if let Ok(bytes) = serde_json::to_vec(&last) {
            let _ignored = self.write_atomic("last.json", &bytes);
        }
    }

    // ---- requester side --------------------------------------------------------------------

    /// The live supervisor, if one is running: its recorded process must still exist with the
    /// same start-time fingerprint.
    #[must_use]
    pub fn live_supervisor(&self) -> Option<SupervisorRecord> {
        let record: SupervisorRecord =
            serde_json::from_slice(&fs::read(self.dir.join("supervisor.json")).ok()?).ok()?;
        let running = ProcessIdentity::query(record.pid);
        (record.version == VERSION
            && record.fingerprint.is_some()
            && running.start_time_fingerprint == record.fingerprint)
            .then_some(record)
    }

    pub fn submit(&self, request: &Request) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        self.write_atomic(&format!("request-{}.json", request.id), &bytes)
    }

    /// Waits for the answer to `id`, removing it once read.
    pub fn await_response(&self, id: &str, timeout: Duration) -> Option<Response> {
        let path = self.dir.join(format!("response-{id}.json"));
        let started = std::time::Instant::now();
        while started.elapsed() < timeout {
            if let Ok(bytes) = fs::read(&path)
                && let Ok(response) = serde_json::from_slice::<Response>(&bytes)
            {
                let _ignored = fs::remove_file(&path);
                return Some(response);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    #[must_use]
    pub fn last_result(&self) -> Option<LastResult> {
        serde_json::from_slice(&fs::read(self.dir.join("last.json")).ok()?).ok()
    }
}

#[must_use]
pub fn request(
    id: String,
    request: RequestKind,
    session_id: &str,
    owner_profile: &str,
    caller_pid: u32,
) -> Request {
    Request {
        version: VERSION,
        id,
        request,
        session_id: session_id.to_owned(),
        owner_profile: owner_profile.to_owned(),
        caller_pid,
        requested_unix_ms: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str) -> Request {
        request(
            id.to_owned(),
            RequestKind::Switch {
                target: "megan".to_owned(),
            },
            "s",
            "erika",
            7,
        )
    }

    #[test]
    fn a_request_is_taken_once_and_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::for_project(dir.path());
        control.submit(&req("a")).unwrap();
        control.submit(&req("b")).unwrap();
        assert_eq!(control.take_request().unwrap().id, "a");
        assert_eq!(control.take_request().unwrap().id, "b");
        assert!(control.take_request().is_none());
    }

    #[test]
    fn a_stale_request_is_discarded_unanswered() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::for_project(dir.path());
        let mut old = req("old");
        old.requested_unix_ms = now_ms() - 10 * 60 * 1000;
        control.submit(&old).unwrap();
        assert!(control.take_request().is_none());
        assert!(control.take_request().is_none(), "and it is gone");
    }

    #[test]
    fn a_response_round_trips_and_is_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::for_project(dir.path());
        control.respond(&Response {
            id: "x".to_owned(),
            ok: true,
            message: "ok".to_owned(),
        });
        let got = control.await_response("x", Duration::from_secs(1)).unwrap();
        assert!(got.ok);
        assert!(
            control
                .await_response("x", Duration::from_millis(100))
                .is_none()
        );
    }

    #[test]
    fn only_a_running_supervisor_with_a_matching_fingerprint_counts() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::for_project(dir.path());
        assert!(control.live_supervisor().is_none());
        control.publish_supervisor("erika", "s");
        assert_eq!(control.live_supervisor().unwrap().owner_profile, "erika");
        control.clear_supervisor();
        assert!(control.live_supervisor().is_none());
        // A recorded pid whose fingerprint differs (a recycled pid) is not a supervisor.
        let bogus = SupervisorRecord {
            version: VERSION,
            pid: std::process::id(),
            fingerprint: Some("not-the-real-start-time".to_owned()),
            owner_profile: "erika".to_owned(),
            session_id: "s".to_owned(),
        };
        control
            .write_atomic("supervisor.json", &serde_json::to_vec(&bogus).unwrap())
            .unwrap();
        assert!(control.live_supervisor().is_none());
    }
}
