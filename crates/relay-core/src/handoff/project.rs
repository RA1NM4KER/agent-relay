use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// A stable id derived from a canonical project path, used only to key Relay's own state
/// directories. Never derived from an uncanonicalized or relative path, so two spellings of the
/// same project can never collide with two different projects.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn for_canonical_path(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::PathNotAbsolute(path.to_path_buf()));
        }
        let mut hasher = Sha256::new();
        hasher.update(path.as_os_str().as_encoded_bytes());
        let digest = hasher.finalize();
        let hex: String = digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self(format!("proj-{hex}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::ProjectId;

    #[test]
    fn same_canonical_path_yields_same_id() {
        let first = ProjectId::for_canonical_path(Path::new("/Users/x/repos/y")).expect("id");
        let second = ProjectId::for_canonical_path(Path::new("/Users/x/repos/y")).expect("id");
        assert_eq!(first, second);
    }

    #[test]
    fn different_paths_yield_different_ids() {
        let first = ProjectId::for_canonical_path(Path::new("/Users/x/repos/y")).expect("id");
        let second = ProjectId::for_canonical_path(Path::new("/Users/x/repos/z")).expect("id");
        assert_ne!(first, second);
    }

    #[test]
    fn relative_paths_are_rejected() {
        assert!(ProjectId::for_canonical_path(Path::new("relative/path")).is_err());
    }
}
