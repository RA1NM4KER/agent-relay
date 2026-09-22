//! Codex's native skill entry point. Kept separate from automatic usage polling, which does
//! not require an installed skill. Existing user-authored skills are never overwritten.

use relay_core::{Error, Result};
use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../assets/codex/relay/SKILL.md");

pub(crate) fn skill_path(config_dir: &Path) -> PathBuf {
    config_dir.join("skills/relay/SKILL.md")
}

pub(crate) fn installed(config_dir: &Path) -> bool {
    let path = skill_path(config_dir);
    std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.len() == SKILL.len() as u64
            && std::fs::read_to_string(path).is_ok_and(|text| text == SKILL)
    })
}

fn validate_path(config_dir: &Path) -> Result<()> {
    for path in [
        config_dir.to_path_buf(),
        config_dir.join("skills"),
        config_dir.join("skills/relay"),
        skill_path(config_dir),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::SymbolicLink(path));
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(source) => return Err(Error::Io { path, source }),
        }
    }
    Ok(())
}

pub(crate) fn install(config_dir: &Path) -> Result<()> {
    use std::io::Write as _;
    validate_path(config_dir)?;
    if installed(config_dir) {
        return Ok(());
    }
    let path = skill_path(config_dir);
    let parent = path.parent().expect("skill parent");
    std::fs::create_dir_all(parent).map_err(|source| Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    // create_new protects an existing skill even if it appeared since the check above.
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return if installed(config_dir) {
                Ok(())
            } else {
                Err(Error::IntegrationRefused(
                    "skills/relay/SKILL.md already exists and differs; preserving it".to_owned(),
                ))
            };
        }
        Err(source) => return Err(Error::Io { path, source }),
    };
    file.write_all(SKILL.as_bytes())
        .map_err(|source| Error::Io { path, source })
}

pub(crate) fn uninstall(config_dir: &Path) -> Result<()> {
    validate_path(config_dir)?;
    let path = skill_path(config_dir);
    if !path.exists() {
        return Ok(());
    }
    if !installed(config_dir) {
        return Err(Error::IntegrationRefused(
            "the Relay skill was edited; preserving it".to_owned(),
        ));
    }
    std::fs::remove_file(&path).map_err(|source| Error::Io { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_uninstall_preserves_edits_and_neighbors() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path()).unwrap();
        install(dir.path()).unwrap();
        assert!(installed(dir.path()));
        let neighbor = dir.path().join("skills/relay/notes.md");
        std::fs::write(&neighbor, "mine").unwrap();
        uninstall(dir.path()).unwrap();
        uninstall(dir.path()).unwrap();
        assert!(!installed(dir.path()));
        assert_eq!(std::fs::read_to_string(neighbor).unwrap(), "mine");
        std::fs::write(skill_path(dir.path()), "custom").unwrap();
        assert!(install(dir.path()).is_err());
        assert!(uninstall(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(skill_path(dir.path())).unwrap(),
            "custom"
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_does_not_follow_a_skills_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("skills")).unwrap();
        assert!(install(dir.path()).is_err());
        assert!(!outside.path().join("relay").exists());
    }
}
