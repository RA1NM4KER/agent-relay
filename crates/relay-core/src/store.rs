use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AtomicWrite, Error, FsAtomicWriter, Profile, ProfileName, Result};

const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileState {
    pub version: u32,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Default for ProfileState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            profiles: Vec::new(),
        }
    }
}

pub struct ProfileStore<W = FsAtomicWriter> {
    path: PathBuf,
    writer: W,
}

impl ProfileStore<FsAtomicWriter> {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            writer: FsAtomicWriter,
        }
    }
}

impl<W: AtomicWrite> ProfileStore<W> {
    #[must_use]
    pub fn with_writer(path: PathBuf, writer: W) -> Self {
        Self { path, writer }
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn load(&self) -> Result<ProfileState> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProfileState::default());
            }
            Err(source) => {
                return Err(Error::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::CorruptedState)?;
        let state: ProfileState = toml::from_str(text).map_err(|_| Error::CorruptedState)?;
        if state.version != STATE_VERSION {
            return Err(Error::UnsupportedStateVersion(state.version));
        }
        validate_unique_profiles(&state)?;
        Ok(state)
    }

    pub fn save(&self, state: &ProfileState) -> Result<()> {
        if state.version != STATE_VERSION {
            return Err(Error::UnsupportedStateVersion(state.version));
        }
        validate_unique_profiles(state)?;
        let text = toml::to_string_pretty(state).map_err(|_| Error::CorruptedState)?;
        self.writer.write_atomic(&self.path, text.as_bytes())
    }
}

fn validate_unique_profiles(state: &ProfileState) -> Result<()> {
    let mut names = std::collections::BTreeSet::<&ProfileName>::new();
    for profile in &state.profiles {
        if !names.insert(&profile.name) {
            return Err(Error::DuplicateProfile(profile.name.to_string()));
        }
    }
    Ok(())
}
