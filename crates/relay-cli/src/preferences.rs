//! M4: a small, Relay-owned, non-secret preference layer (`<config_root>/preferences.toml`) so
//! `relay claude`/`relay setup`/`relay status` can act without the user repeating `--profile`/
//! `--fallback` on every invocation.
//!
//! **Never holds credentials.** Only profile *names* (already-registered `ProfileName`s — the
//! actual authentication material stays exactly where it already lived: provider-owned config
//! directories, inspected and referenced by Relay, never copied). This file is written with the
//! same private-directory conventions Relay already uses elsewhere (mode 0600, atomic write via a
//! temp file + rename), but it is not itself sensitive — losing or reading it reveals only which
//! profile *names* exist and their relative order, never anything that authenticates as them.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use relay_core::{Error, ProfileName, Result};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "preferences.toml";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    pub primary_profile: Option<ProfileName>,
    #[serde(default)]
    pub fallback_profiles: Vec<ProfileName>,
    #[serde(default)]
    pub herdr_enabled: Option<bool>,
    #[serde(default)]
    pub usage_integration_enabled: Option<bool>,
}

impl Preferences {
    #[must_use]
    pub fn path(config_root: &Path) -> PathBuf {
        config_root.join(FILE_NAME)
    }

    /// Missing file is not an error: a first-time user simply has no preferences yet, and every
    /// caller (`relay claude`, `relay status`, ...) needs to handle "not configured" as a normal,
    /// expected state pointing the user at `relay setup` rather than a failure.
    pub fn load(config_root: &Path) -> Result<Option<Self>> {
        let path = Self::path(config_root);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path, source }),
        };
        let preferences: Self =
            toml::from_str(&raw).map_err(|_| Error::UnsupportedStateVersion(0))?;
        Ok(Some(preferences))
    }

    /// Atomic write (temp file + rename), mode 0600, matching every other piece of Relay state —
    /// this file names profiles, and while it is not itself a secret, there is no reason to hold
    /// it to a lower bar than the rest of `<config_root>`.
    pub fn save(&self, config_root: &Path) -> Result<()> {
        fs::create_dir_all(config_root).map_err(|source| Error::Io {
            path: config_root.to_path_buf(),
            source,
        })?;
        let path = Self::path(config_root);
        let serialized = toml::to_string_pretty(self).map_err(|_| Error::SerializationFailed)?;
        let temp_path = path.with_extension("toml.tmp");
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .map_err(|source| Error::Io {
                    path: temp_path.clone(),
                    source,
                })?;
            file.write_all(serialized.as_bytes())
                .map_err(|source| Error::Io {
                    path: temp_path.clone(),
                    source,
                })?;
            file.sync_all().map_err(|source| Error::Io {
                path: temp_path.clone(),
                source,
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600)).map_err(
                    |source| Error::Io {
                        path: temp_path.clone(),
                        source,
                    },
                )?;
            }
        }
        fs::rename(&temp_path, &path).map_err(|source| Error::Io { path, source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Preferences;
    use relay_core::ProfileName;

    #[test]
    fn missing_file_loads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(Preferences::load(dir.path()).expect("load"), None);
    }

    #[test]
    fn round_trips_through_save_and_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let preferences = Preferences {
            primary_profile: Some(ProfileName::new("work").expect("name")),
            fallback_profiles: vec![ProfileName::new("backup").expect("name")],
            herdr_enabled: Some(true),
            usage_integration_enabled: Some(true),
        };
        preferences.save(dir.path()).expect("save");
        let loaded = Preferences::load(dir.path())
            .expect("load")
            .expect("present");
        assert_eq!(loaded, preferences);
    }

    #[test]
    fn saved_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        Preferences::default().save(dir.path()).expect("save");
        let metadata = std::fs::metadata(Preferences::path(dir.path())).expect("metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn corrupted_file_fails_closed_rather_than_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(Preferences::path(dir.path()), b"not valid toml {{{").expect("write");
        assert!(Preferences::load(dir.path()).is_err());
    }
}
