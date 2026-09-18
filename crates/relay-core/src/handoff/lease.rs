use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AtomicWrite, Error, FsAtomicWriter, ProfileName, Result,
    handoff::{ProcessIdentity, ProjectId, TransactionId},
};

const LEASE_VERSION: u32 = 1;

/// Durable record of which profile currently owns write access to a project. This is metadata,
/// not the lock itself: it describes ownership for humans and for cross-checking, while
/// [`crate::handoff::OrchestrationLock`] and a real process-liveness check are what actually
/// prevent two writers. A lease is never silently discarded — only replaced by a transaction that
/// proved the prior owner's process is gone (or was never live), inside the orchestration lock.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterLease {
    pub version: u32,
    pub project_id: ProjectId,
    pub owner_profile: ProfileName,
    pub owner_process: ProcessIdentity,
    pub session_id: String,
    pub transaction_id: TransactionId,
    pub acquired_unix_ms: u64,
    /// An opaque provider-specific handle for the launched writer (e.g. Claude's own background
    /// job id), so an operator or a future command can reference it without Relay needing to
    /// know provider-specific semantics. Absent for leases established via a handoff rather than
    /// a direct `relay launch`.
    #[serde(default)]
    pub provider_handle: Option<String>,
}

pub struct LeaseStore {
    path: PathBuf,
}

impl LeaseStore {
    #[must_use]
    pub const fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<WriterLease>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(Error::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let lease: WriterLease =
            serde_json::from_slice(&bytes).map_err(|_| Error::CorruptedLease)?;
        if lease.version != LEASE_VERSION {
            return Err(Error::CorruptedLease);
        }
        Ok(Some(lease))
    }

    pub fn save(&self, lease: &WriterLease) -> Result<()> {
        let text = serde_json::to_string_pretty(lease).map_err(|_| Error::SerializationFailed)?;
        FsAtomicWriter.write_atomic(&self.path, text.as_bytes())
    }

    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

impl WriterLease {
    #[must_use]
    pub const fn new(
        project_id: ProjectId,
        owner_profile: ProfileName,
        owner_process: ProcessIdentity,
        session_id: String,
        transaction_id: TransactionId,
        acquired_unix_ms: u64,
    ) -> Self {
        Self {
            version: LEASE_VERSION,
            project_id,
            owner_profile,
            owner_process,
            session_id,
            transaction_id,
            acquired_unix_ms,
            provider_handle: None,
        }
    }

    #[must_use]
    pub fn with_provider_handle(mut self, provider_handle: Option<String>) -> Self {
        self.provider_handle = provider_handle;
        self
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{LeaseStore, ProcessIdentity, WriterLease};
    use crate::{
        ProfileName,
        handoff::{ProjectId, TransactionId},
    };

    fn sample_lease() -> WriterLease {
        WriterLease::new(
            ProjectId::for_canonical_path(std::path::Path::new("/tmp/proj")).expect("id"),
            ProfileName::new("erika").expect("name"),
            ProcessIdentity {
                pid: 1234,
                start_time_fingerprint: Some("fingerprint".to_owned()),
            },
            "8586fe71-395b-4449-b973-78011d561fed".to_owned(),
            TransactionId::generate(),
            0,
        )
    }

    #[test]
    fn missing_lease_file_loads_as_none() {
        let root = tempdir().expect("temp dir");
        let store = LeaseStore::at_path(root.path().join("lease.json"));
        assert_eq!(store.load().expect("load"), None);
    }

    #[test]
    fn round_trips_through_atomic_save_and_load() {
        let root = tempdir().expect("temp dir");
        let store = LeaseStore::at_path(root.path().join("lease.json"));
        let lease = sample_lease();
        store.save(&lease).expect("save");
        assert_eq!(store.load().expect("load"), Some(lease));
    }

    #[test]
    fn corrupted_lease_fails_closed() {
        let root = tempdir().expect("temp dir");
        let path = root.path().join("lease.json");
        std::fs::write(&path, "not json").expect("write garbage");
        let store = LeaseStore::at_path(path);
        let error = store.load().expect_err("must fail closed");
        assert_eq!(error.code(), "corrupted_lease");
    }

    #[test]
    fn clear_is_idempotent() {
        let root = tempdir().expect("temp dir");
        let store = LeaseStore::at_path(root.path().join("lease.json"));
        store.save(&sample_lease()).expect("save");
        store.clear().expect("first clear");
        store
            .clear()
            .expect("second clear is a no-op, not an error");
        assert_eq!(store.load().expect("load"), None);
    }
}
