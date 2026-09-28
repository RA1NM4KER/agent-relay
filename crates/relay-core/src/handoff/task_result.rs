//! Durable, explicit task-result evidence for a claimed GitHub Issue.
//!
//! This is deliberately evidence only. It is never read by provider supervision, ownership,
//! handoff, or queue selection, and it cannot itself mutate GitHub. A later explicit lifecycle
//! command may consume a verified record; absence, corruption, or a mismatched binding must fail
//! closed rather than suggesting that a task was completed from terminal or conversation state.

use std::{fs, path::PathBuf, thread, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    AtomicWrite, Error, FsAtomicWriter, Result,
    handoff::{OrchestrationLock, RelaySessionId},
};

const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "task_result.json";
const LOCK_WAIT_MS: u64 = 2_000;
const LOCK_POLL_MS: u64 = 25;
/// A result is a short operator/agent attestation, never a transcript or an unbounded report.
pub const MAX_SUMMARY_CHARS: usize = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultKind {
    Completed,
    Blocked,
    Continuing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResult {
    pub schema_version: u32,
    pub issue: u64,
    pub claim_ref: String,
    pub relay_session_id: RelaySessionId,
    pub result: TaskResultKind,
    pub summary: String,
    pub recorded_unix_ms: u64,
}

impl TaskResult {
    pub fn new(
        issue: u64,
        claim_ref: String,
        relay_session_id: RelaySessionId,
        result: TaskResultKind,
        summary: String,
        recorded_unix_ms: u64,
    ) -> Result<Self> {
        if issue == 0 {
            return Err(Error::WorkingStateInvalid(
                "task issue must be positive".into(),
            ));
        }
        if summary.is_empty() || summary.chars().count() > MAX_SUMMARY_CHARS {
            return Err(Error::WorkingStateInvalid(format!(
                "task result summary must contain 1..={MAX_SUMMARY_CHARS} characters"
            )));
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            issue,
            claim_ref,
            relay_session_id,
            result,
            summary,
            recorded_unix_ms,
        })
    }
}

/// Per-session store. A single session can update its explicit result only for the same claimed
/// Issue/ref binding; this permits an explicit `continuing` record to later become `completed`
/// without ever retargeting the session to another task.
pub struct TaskResultStore {
    session_dir: PathBuf,
}

impl TaskResultStore {
    #[must_use]
    pub const fn at_session_dir(session_dir: PathBuf) -> Self {
        Self { session_dir }
    }

    fn file_path(&self) -> PathBuf {
        self.session_dir.join(FILE_NAME)
    }

    fn lock_path(&self) -> PathBuf {
        self.session_dir.join("orchestration.lock")
    }

    /// `None` is normal only before an explicit result has been recorded. A consumer that needs
    /// task-result evidence must reject it; malformed or unknown-schema state is corruption.
    pub fn load(&self) -> Result<Option<TaskResult>> {
        let path = self.file_path();
        match fs::read(&path) {
            Ok(bytes) => {
                let record: TaskResult =
                    serde_json::from_slice(&bytes).map_err(|_| Error::CorruptedState)?;
                (record.schema_version == SCHEMA_VERSION)
                    .then_some(record)
                    .map(Some)
                    .ok_or(Error::CorruptedState)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    /// Writes only a record bound to the same Issue/ref/session as any existing record. The
    /// caller must verify current remote claim state before calling this; this local store cannot
    /// create, renew, release, or otherwise interpret a GitHub claim.
    pub fn write(&self, record: TaskResult) -> Result<TaskResult> {
        fs::create_dir_all(&self.session_dir).map_err(|source| Error::Io {
            path: self.session_dir.clone(),
            source,
        })?;
        let lock = OrchestrationLock::at_path(self.lock_path());
        let started = std::time::Instant::now();
        loop {
            match lock.try_with(|| {
                if let Some(existing) = self.load()?
                    && (existing.issue != record.issue
                        || existing.claim_ref != record.claim_ref
                        || existing.relay_session_id != record.relay_session_id)
                {
                    return Err(Error::WorkingStateInvalid(
                        "existing task result is bound to a different issue, claim ref, or Relay Session".into(),
                    ));
                }
                let bytes = serde_json::to_vec_pretty(&record).map_err(|_| Error::SerializationFailed)?;
                FsAtomicWriter.write_atomic(&self.file_path(), &bytes)?;
                Ok(record.clone())
            }) {
                Err(Error::OrchestrationLockHeld)
                    if started.elapsed().as_millis() < u128::from(LOCK_WAIT_MS) =>
                {
                    thread::sleep(Duration::from_millis(LOCK_POLL_MS));
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> RelaySessionId {
        RelaySessionId::parse("11111111-1111-4111-8111-111111111111").unwrap()
    }

    fn record(kind: TaskResultKind) -> TaskResult {
        TaskResult::new(
            21,
            "refs/heads/relay/claims/21".into(),
            session(),
            kind,
            "explicit result".into(),
            7,
        )
        .unwrap()
    }

    #[test]
    fn result_round_trips_and_can_explicitly_change_the_same_claim() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskResultStore::at_session_dir(dir.path().to_path_buf());
        store.write(record(TaskResultKind::Continuing)).unwrap();
        let completed = store.write(record(TaskResultKind::Completed)).unwrap();
        assert_eq!(completed.result, TaskResultKind::Completed);
        assert_eq!(store.load().unwrap().unwrap(), completed);
    }

    #[test]
    fn corrupt_or_retargeted_state_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskResultStore::at_session_dir(dir.path().to_path_buf());
        fs::write(dir.path().join(FILE_NAME), "not json").unwrap();
        assert!(matches!(store.load(), Err(Error::CorruptedState)));
        fs::remove_file(dir.path().join(FILE_NAME)).unwrap();
        store.write(record(TaskResultKind::Blocked)).unwrap();
        let wrong = TaskResult::new(
            22,
            "refs/heads/relay/claims/22".into(),
            session(),
            TaskResultKind::Blocked,
            "other".into(),
            8,
        )
        .unwrap();
        assert!(matches!(
            store.write(wrong),
            Err(Error::WorkingStateInvalid(_))
        ));
    }
}
