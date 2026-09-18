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
