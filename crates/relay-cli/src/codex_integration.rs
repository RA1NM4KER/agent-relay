//! Codex's native skill entry point. Kept separate from automatic usage polling, which does not
//! require an installed skill. A skill Relay did not write, or one it wrote and the user has since
//! edited, is never overwritten or removed.
//!
//! Ownership is tracked with a small sidecar marker (`.relay-managed.json`, next to `SKILL.md`)
//! recording the SHA-256 of exactly the bytes Relay itself last wrote. A later `install` (e.g.
//! after upgrading Relay to a version with a different skill body) compares the *current on-disk*
//! file against that recorded hash, not against the newest embedded skill text: a match proves the
//! file is unmodified since Relay wrote it — safe to update automatically, even though its content
//! differs from the version now embedded in this binary. Any other on-disk content (no marker, or
//! a hash that does not match — including a plain pre-existing file from before this marker
//! existed) is treated exactly as a hand-edit: refused, never guessed past.

use relay_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../assets/codex/relay/SKILL.md");

pub(crate) fn skill_path(config_dir: &Path) -> PathBuf {
    config_dir.join("skills/relay/SKILL.md")
}

fn manifest_path(config_dir: &Path) -> PathBuf {
    config_dir.join("skills/relay/.relay-managed.json")
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether the file on disk is byte-identical to the skill text *this* binary embeds — "fully up
/// to date", not merely "Relay-owned" (see [`is_relay_managed_and_unmodified`] for that).
pub(crate) fn installed(config_dir: &Path) -> bool {
    let path = skill_path(config_dir);
    std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.len() == SKILL.len() as u64
            && std::fs::read_to_string(path).is_ok_and(|text| text == SKILL)
    })
}

/// Whether the file on disk is exactly what Relay itself last wrote there — possibly an older
/// skill body than [`SKILL`], but proven unmodified since, via the sidecar marker's recorded hash.
/// `false` for a file with no marker at all (a pre-existing file of unknown origin, or an install
/// from before this marker existed) — that ambiguity is never resolved in Relay's favor.
fn is_relay_managed_and_unmodified(config_dir: &Path) -> bool {
    let Ok(current) = std::fs::read_to_string(skill_path(config_dir)) else {
        return false;
    };
    let Ok(manifest_text) = std::fs::read_to_string(manifest_path(config_dir)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Manifest>(&manifest_text) else {
        return false;
    };
    manifest.sha256 == sha256_hex(&current)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    version: u32,
    sha256: String,
}

fn write_skill_and_manifest(config_dir: &Path) -> Result<()> {
    let path = skill_path(config_dir);
    let parent = path.parent().expect("skill parent");
    std::fs::create_dir_all(parent).map_err(|source| Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    std::fs::write(&path, SKILL).map_err(|source| Error::Io { path, source })?;
    let manifest_path = manifest_path(config_dir);
    let manifest = serde_json::to_string_pretty(&Manifest {
        version: 1,
        sha256: sha256_hex(SKILL),
    })
    .map_err(|_| Error::SerializationFailed)?;
    std::fs::write(&manifest_path, manifest).map_err(|source| Error::Io {
        path: manifest_path,
        source,
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
        // Already exactly current — make sure the marker exists too (an install that predates
        // this marker feature has none yet), so a *future* upgrade can trust it.
        if !is_relay_managed_and_unmodified(config_dir) {
            write_skill_and_manifest(config_dir)?;
        }
        return Ok(());
    }
    let path = skill_path(config_dir);
    if path.exists() {
        return if is_relay_managed_and_unmodified(config_dir) {
            // Relay's own, unmodified — a real upgrade, not a stranger's file.
            write_skill_and_manifest(config_dir)
        } else {
            Err(Error::IntegrationRefused(
                "skills/relay/SKILL.md already exists and differs; preserving it".to_owned(),
            ))
        };
    }
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
            return if is_relay_managed_and_unmodified(config_dir) || installed(config_dir) {
                write_skill_and_manifest(config_dir)
            } else {
                Err(Error::IntegrationRefused(
                    "skills/relay/SKILL.md already exists and differs; preserving it".to_owned(),
                ))
            };
        }
        Err(source) => return Err(Error::Io { path, source }),
    };
    file.write_all(SKILL.as_bytes())
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    drop(file);
    let manifest_path = manifest_path(config_dir);
    let manifest = serde_json::to_string_pretty(&Manifest {
        version: 1,
        sha256: sha256_hex(SKILL),
    })
    .map_err(|_| Error::SerializationFailed)?;
    std::fs::write(&manifest_path, manifest).map_err(|source| Error::Io {
        path: manifest_path,
        source,
    })
}

pub(crate) fn uninstall(config_dir: &Path) -> Result<()> {
    validate_path(config_dir)?;
    let path = skill_path(config_dir);
    if !path.exists() {
        return Ok(());
    }
    if !installed(config_dir) && !is_relay_managed_and_unmodified(config_dir) {
        return Err(Error::IntegrationRefused(
            "the Relay skill was edited; preserving it".to_owned(),
        ));
    }
    std::fs::remove_file(&path).map_err(|source| Error::Io { path, source })?;
    let manifest_path = manifest_path(config_dir);
    if manifest_path.exists() {
        std::fs::remove_file(&manifest_path).map_err(|source| Error::Io {
            path: manifest_path,
            source,
        })?;
    }
    Ok(())
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

    /// The upgrade path this whole marker exists for: an older Relay version's own, untouched
    /// skill body is replaced automatically when a newer `install` runs — never treated as a
    /// stranger's file just because its bytes differ from what this binary now embeds.
    #[test]
    fn install_upgrades_its_own_unmodified_older_skill_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let older = "an older Relay version's skill body";
        std::fs::create_dir_all(dir.path().join("skills/relay")).unwrap();
        std::fs::write(skill_path(dir.path()), older).unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            serde_json::to_string(&Manifest {
                version: 1,
                sha256: sha256_hex(older),
            })
            .unwrap(),
        )
        .unwrap();

        install(dir.path()).unwrap();
        assert!(installed(dir.path()), "upgraded to the current skill text");
    }

    /// A file with no marker at all — a plain pre-existing file, or an install from before this
    /// marker feature existed — is never assumed to be Relay's own, even if a user might guess
    /// otherwise; `install` and `uninstall` both refuse rather than overwrite/remove it.
    #[test]
    fn install_and_uninstall_refuse_a_differing_file_with_no_ownership_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("skills/relay")).unwrap();
        std::fs::write(skill_path(dir.path()), "pre-existing content, no marker").unwrap();

        assert!(install(dir.path()).is_err());
        assert!(uninstall(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(skill_path(dir.path())).unwrap(),
            "pre-existing content, no marker"
        );
    }

    /// A marker whose recorded hash does not match the current on-disk content (the user edited a
    /// Relay-installed file after the fact) is exactly as refused as no marker at all.
    #[test]
    fn a_stale_marker_that_no_longer_matches_the_edited_file_still_refuses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("skills/relay")).unwrap();
        std::fs::write(
            skill_path(dir.path()),
            "user edited this after Relay wrote it",
        )
        .unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            serde_json::to_string(&Manifest {
                version: 1,
                sha256: sha256_hex("what Relay originally wrote, not the edited text"),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(install(dir.path()).is_err());
        assert!(uninstall(dir.path()).is_err());
    }
}
