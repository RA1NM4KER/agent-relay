use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{Error, Result};

pub trait AtomicWrite: Send + Sync {
    fn write_atomic(&self, destination: &Path, contents: &[u8]) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FsAtomicWriter;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl AtomicWrite for FsAtomicWriter {
    fn write_atomic(&self, destination: &Path, contents: &[u8]) -> Result<()> {
        let parent = destination.parent().ok_or(Error::AtomicWriteFailed)?;
        let temp_path = temporary_path(destination)?;
        let result = write_and_replace(&temp_path, destination, contents);
        if result.is_err() {
            let _ignored = fs::remove_file(&temp_path);
        }
        result?;
        sync_directory(parent)?;
        Ok(())
    }
}

fn temporary_path(destination: &Path) -> Result<PathBuf> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::AtomicWriteFailed)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::AtomicWriteFailed)?
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(destination.with_file_name(format!(
        ".{file_name}.tmp.{}.{}.{}",
        std::process::id(),
        timestamp,
        sequence
    )))
}

fn write_and_replace(temp_path: &Path, destination: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp_path).map_err(|source| Error::Io {
        path: temp_path.to_path_buf(),
        source,
    })?;
    file.write_all(contents).map_err(|source| Error::Io {
        path: temp_path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| Error::Io {
        path: temp_path.to_path_buf(),
        source,
    })?;
    drop(file);
    fs::rename(temp_path, destination).map_err(|source| Error::Io {
        path: destination.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::{AtomicWrite, FsAtomicWriter};

    #[test]
    fn a_failed_write_preserves_existing_content_and_leaves_no_temp_file() {
        let root = tempdir().expect("temp dir");
        let destination = root.path().join("state.toml");
        std::fs::write(&destination, "original").expect("seed");

        // Make the directory unwritable so temp-file creation fails.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500))
            .expect("chmod read-only");
        let result = FsAtomicWriter.write_atomic(&destination, b"replacement");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod restore");

        assert!(
            result.is_err(),
            "write into a read-only directory must fail"
        );
        assert_eq!(
            std::fs::read_to_string(&destination).expect("destination unchanged"),
            "original"
        );
        let stray: Vec<_> = std::fs::read_dir(root.path())
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .filter(|name| name != "state.toml")
            .collect();
        assert!(stray.is_empty(), "no temp file must remain: {stray:?}");
    }

    #[test]
    fn a_successful_write_atomically_replaces_the_destination() {
        let root = tempdir().expect("temp dir");
        let destination = root.path().join("state.toml");
        std::fs::write(&destination, "original").expect("seed");

        FsAtomicWriter
            .write_atomic(&destination, b"replacement")
            .expect("write");

        assert_eq!(
            std::fs::read_to_string(&destination).expect("destination updated"),
            "replacement"
        );
    }
}
