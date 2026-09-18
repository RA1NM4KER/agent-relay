use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{Error, ProviderKind, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayPaths {
    config_root: PathBuf,
    state_root: PathBuf,
}

impl RelayPaths {
    pub fn discover() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(Error::MissingEnvironment("HOME"))?;
        let config_root = env::var_os("XDG_CONFIG_HOME")
            .map_or_else(|| home.join(".config"), PathBuf::from)
            .join("agent-relay");
        let state_root = env::var_os("XDG_STATE_HOME")
            .map_or_else(|| home.join(".local/state"), PathBuf::from)
            .join("agent-relay");
        Self::new(config_root, state_root)
    }

    pub fn new(config_root: PathBuf, state_root: PathBuf) -> Result<Self> {
        if !config_root.is_absolute() {
            return Err(Error::PathNotAbsolute(config_root));
        }
        if !state_root.is_absolute() {
            return Err(Error::PathNotAbsolute(state_root));
        }
        reject_terminal_symlink(&config_root)?;
        reject_terminal_symlink(&state_root)?;
        let config_root = normalize_absolute(&config_root)?;
        let state_root = normalize_absolute(&state_root)?;
        Ok(Self {
            config_root,
            state_root,
        })
    }

    #[must_use]
    pub fn config_root(&self) -> &Path {
        &self.config_root
    }

    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    #[must_use]
    pub fn profiles_root(&self) -> PathBuf {
        self.config_root.join("profiles")
    }

    #[must_use]
    pub fn profile_state_file(&self) -> PathBuf {
        self.config_root.join("profiles.toml")
    }

    #[must_use]
    pub fn default_profile_dir(
        &self,
        name: &crate::ProfileName,
        provider: ProviderKind,
    ) -> PathBuf {
        self.profiles_root()
            .join(name.as_str())
            .join(provider.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct ProfileDirectory {
    managed_root: PathBuf,
}

impl ProfileDirectory {
    pub fn new(managed_root: PathBuf) -> Result<Self> {
        if !managed_root.is_absolute() {
            return Err(Error::PathNotAbsolute(managed_root));
        }
        Ok(Self { managed_root })
    }

    pub fn create_managed(&self, path: &Path) -> Result<()> {
        if !path.is_absolute() {
            return Err(Error::PathNotAbsolute(path.to_path_buf()));
        }
        if !path.starts_with(&self.managed_root) || path == self.managed_root {
            return Err(Error::PathOutsideManagedRoot(path.to_path_buf()));
        }
        self.prepare_root()?;
        let relative = path
            .strip_prefix(&self.managed_root)
            .map_err(|_| Error::PathOutsideManagedRoot(path.to_path_buf()))?;
        let mut current = self.managed_root.clone();
        for component in relative.components() {
            use std::path::Component;
            let Component::Normal(component) = component else {
                return Err(Error::PathOutsideManagedRoot(path.to_path_buf()));
            };
            current.push(component);
            ensure_private_dir(&current)?;
        }
        self.validate_existing(path)
    }

    pub fn prepare_root(&self) -> Result<()> {
        let relay_config_root = self
            .managed_root
            .parent()
            .ok_or_else(|| Error::PathOutsideManagedRoot(self.managed_root.clone()))?;
        ensure_application_root(relay_config_root)?;
        ensure_private_dir(&self.managed_root)
    }

    pub fn validate_existing(&self, path: &Path) -> Result<()> {
        if !path.is_absolute() {
            return Err(Error::PathNotAbsolute(path.to_path_buf()));
        }
        reject_symlink_components(path)?;
        validate_private_dir(path)
    }

    pub fn validate_managed_existing(&self, path: &Path) -> Result<()> {
        if !path.starts_with(&self.managed_root) || path == self.managed_root {
            return Err(Error::PathOutsideManagedRoot(path.to_path_buf()));
        }
        self.validate_existing(path)
    }
}

fn reject_terminal_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        Err(Error::SymbolicLink(path.to_path_buf()))
    } else {
        Ok(())
    }
}

pub(crate) fn normalize_profile_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::PathNotAbsolute(path.to_path_buf()));
    }
    reject_terminal_symlink(path)?;
    normalize_absolute(path)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(Error::PathNotAbsolute(path.to_path_buf()));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::PathOutsideManagedRoot(path.to_path_buf()));
    }

    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| Error::PathNotAbsolute(path.to_path_buf()))?;
        missing.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| Error::PathNotAbsolute(path.to_path_buf()))?;
    }
    let mut normalized = fs::canonicalize(existing).map_err(|source| Error::Io {
        path: existing.to_path_buf(),
        source,
    })?;
    for component in missing.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(Error::SymbolicLink(path.to_path_buf()));
            }
            if !metadata.is_dir() {
                return Err(Error::NotDirectory(path.to_path_buf()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            set_private_directory_permissions(path)?;
        }
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    validate_private_dir(path)
}

fn ensure_application_root(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_dir(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| Error::PathNotAbsolute(path.to_path_buf()))?;
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
            reject_symlink_components(parent)?;
            fs::create_dir(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            set_private_directory_permissions(path)?;
            validate_private_dir(path)
        }
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn reject_symlink_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::SymbolicLink(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotDirectory(path.to_path_buf()));
            }
            Err(source) => {
                return Err(Error::Io {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn validate_private_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::SymbolicLink(path.to_path_buf()));
    }
    if !metadata.is_dir() {
        return Err(Error::NotDirectory(path.to_path_buf()));
    }
    validate_platform_permissions(path, &metadata)
}

#[cfg(unix)]
fn validate_platform_permissions(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("HOME"))?;
    let home_metadata = fs::metadata(&home).map_err(|source| Error::Io { path: home, source })?;
    if metadata.uid() != home_metadata.uid() {
        return Err(Error::WrongOwner(path.to_path_buf()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::UnsafePermissions(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_platform_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
