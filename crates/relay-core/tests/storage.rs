use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use relay_core::{AtomicWrite, Error, ProfileState, ProfileStore, Result};
use tempfile::tempdir;

#[derive(Clone)]
struct FailingWriter {
    attempts: Arc<Mutex<usize>>,
}

impl AtomicWrite for FailingWriter {
    fn write_atomic(&self, _destination: &Path, _contents: &[u8]) -> Result<()> {
        *self.attempts.lock().expect("test mutex must be available") += 1;
        Err(Error::AtomicWriteFailed)
    }
}

#[test]
fn atomic_write_failure_preserves_existing_state() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("profiles.toml");
    let original = "version = 1\nprofiles = []\n";
    fs::write(&path, original).expect("seed state");
    let attempts = Arc::new(Mutex::new(0));
    let store = ProfileStore::with_writer(
        path.clone(),
        FailingWriter {
            attempts: Arc::clone(&attempts),
        },
    );

    let error = store
        .save(&ProfileState::default())
        .expect_err("write must fail");

    assert_eq!(error.code(), "atomic_write_failed");
    assert_eq!(fs::read_to_string(path).expect("state remains"), original);
    assert_eq!(*attempts.lock().expect("test mutex"), 1);
}

#[test]
fn corrupted_state_fails_closed_without_echoing_contents() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("profiles.toml");
    let planted_secret = "sk-ant-secret-canary";
    fs::write(&path, format!("not valid = [\"{planted_secret}\"")).expect("corrupt state");
    let store = ProfileStore::new(path);

    let error = store.load().expect_err("corrupt state must fail");
    let rendered = format!("{error:?} {error}");

    assert_eq!(error.code(), "corrupted_state");
    assert!(!rendered.contains(planted_secret));
}
