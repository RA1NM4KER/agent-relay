use std::path::PathBuf;

use crate::{
    AtomicWrite, AuthenticationState, Availability, ClaudeConfigMode, DoctorCheck, DoctorReport,
    Error, FsAtomicWriter, IdentityMetadata, Profile, ProfileDirectory, ProfileName, ProfileOrigin,
    ProfileSetupMode, ProfileSetupRequest, ProfileStatus, ProfileStore, Provider, ProviderKind,
    RelayPaths, Result,
};

#[derive(Clone, Debug)]
pub struct AddProfileRequest {
    pub name: ProfileName,
    pub provider: ProviderKind,
    pub config_dir: Option<PathBuf>,
    pub mode: ProfileSetupMode,
    pub expected_identity: Option<IdentityMetadata>,
    /// Only meaningful for Claude; see [`crate::ClaudeConfigMode`].
    pub claude_config_mode: Option<ClaudeConfigMode>,
}

pub struct ProfileService<W = FsAtomicWriter> {
    paths: RelayPaths,
    store: ProfileStore<W>,
}

impl ProfileService<FsAtomicWriter> {
    #[must_use]
    pub fn new(paths: RelayPaths) -> Self {
        let store = ProfileStore::new(paths.profile_state_file());
        Self { paths, store }
    }
}

impl<W: AtomicWrite> ProfileService<W> {
    #[must_use]
    pub fn with_store(paths: RelayPaths, store: ProfileStore<W>) -> Self {
        Self { paths, store }
    }

    pub fn add(&self, request: AddProfileRequest, provider: &dyn Provider) -> Result<Profile> {
        if provider.kind() != request.provider {
            return Err(Error::ProviderMismatch {
                expected: request.provider.to_string(),
                observed: provider.kind().to_string(),
            });
        }
        let mut state = self.store.load()?;
        if state
            .profiles
            .iter()
            .any(|profile| profile.name == request.name)
        {
            return Err(Error::DuplicateProfile(request.name.to_string()));
        }

        let requested_config_dir = request.config_dir.unwrap_or_else(|| {
            self.paths
                .default_profile_dir(&request.name, request.provider)
        });
        let config_dir = crate::paths::normalize_profile_path(&requested_config_dir)?;
        let directories = ProfileDirectory::new(self.paths.profiles_root())?;
        directories.prepare_root()?;
        match request.mode {
            ProfileSetupMode::Create => directories.create_managed(&config_dir)?,
            ProfileSetupMode::AdoptExisting => {
                directories.validate_existing(&config_dir)?;
                if request.expected_identity.is_none() {
                    return Err(Error::AdoptionIdentityRequired);
                }
            }
        }

        let observation = provider.setup_profile(&ProfileSetupRequest {
            name: request.name.clone(),
            config_dir: config_dir.clone(),
            mode: request.mode,
            claude_config_mode: request.claude_config_mode,
        })?;
        validate_authentication(observation.authentication)?;
        let observed_identity = observation
            .identity
            .ok_or(Error::AuthenticationInspectionFailed)?;
        let expected_identity = request
            .expected_identity
            .unwrap_or_else(|| observed_identity.clone());
        if expected_identity.stable_id != observed_identity.stable_id {
            return Err(Error::IdentityMismatch);
        }
        if state.profiles.iter().any(|profile| {
            profile.provider == request.provider
                && profile.expected_identity.stable_id == expected_identity.stable_id
        }) {
            return Err(Error::DuplicateIdentity);
        }
        let profile = Profile {
            name: request.name,
            provider: request.provider,
            config_dir,
            enabled: true,
            origin: match request.mode {
                ProfileSetupMode::Create => ProfileOrigin::Created,
                ProfileSetupMode::AdoptExisting => ProfileOrigin::Adopted,
            },
            expected_identity,
            last_availability: observation.availability,
            claude_config_mode: request.claude_config_mode,
        };
        state.profiles.push(profile.clone());
        state
            .profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.store.save(&state)?;
        Ok(profile)
    }

    pub fn list(&self) -> Result<Vec<Profile>> {
        Ok(self.store.load()?.profiles)
    }

    /// Renames a profile's registered label only — never its `config_dir`, authentication, or
    /// identity pin, so the provider-owned config home this profile references is untouched.
    /// Callers own everything else a rename must also update (preferences, session/ledger/journal
    /// history referencing the old name) — this only makes the label change atomically in the
    /// profile registry itself, the one piece of state this crate is authoritative for.
    pub fn rename(&self, old: &ProfileName, new: &ProfileName) -> Result<Profile> {
        let mut state = self.store.load()?;
        if state.profiles.iter().any(|profile| &profile.name == new) {
            return Err(Error::DuplicateProfile(new.to_string()));
        }
        let index = state
            .profiles
            .iter()
            .position(|profile| &profile.name == old)
            .ok_or_else(|| Error::ProfileNotFound(old.to_string()))?;
        state.profiles[index].name = new.clone();
        state
            .profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.store.save(&state)?;
        Ok(state
            .profiles
            .into_iter()
            .find(|profile| &profile.name == new)
            .expect("just inserted"))
    }

    pub fn status(&self, name: &ProfileName, provider: &dyn Provider) -> Result<ProfileStatus> {
        let profile = self.find(name)?;
        validate_provider(&profile, provider)?;
        self.validate_profile_directory(&profile)?;
        let observation =
            provider.inspect_profile(&profile.config_dir, profile.claude_config_mode)?;
        let identity_matches = observation
            .identity
            .as_ref()
            .is_some_and(|identity| identity.stable_id == profile.expected_identity.stable_id);
        Ok(ProfileStatus {
            profile,
            authentication: observation.authentication,
            observed_identity: observation.identity,
            availability: observation.availability,
            identity_matches,
        })
    }

    pub fn doctor(&self, name: &ProfileName, provider: &dyn Provider) -> Result<DoctorReport> {
        let profile = self.find(name)?;
        validate_provider(&profile, provider)?;
        let directory_result = self.validate_profile_directory(&profile);
        let mut checks = vec![DoctorCheck {
            name: "directory_security".to_owned(),
            passed: directory_result.is_ok(),
            message: match &directory_result {
                Ok(()) => {
                    "profile directory is private and contains no symlink components".to_owned()
                }
                // The real error (e.g. `UnsafePermissions`'s `chmod 700 <path>` guidance) is
                // always actionable on its own; a generic "failed safety validation" here would
                // throw that away right where a dogfooding user is most likely to be looking.
                Err(error) => error.to_string(),
            },
        }];

        if directory_result.is_err() {
            checks.push(DoctorCheck {
                name: "provider_inspection".to_owned(),
                passed: false,
                message: "provider inspection was skipped because the directory is unsafe"
                    .to_owned(),
            });
            return Ok(DoctorReport {
                profile: profile.name,
                healthy: false,
                checks,
            });
        }

        let observation = provider.inspect_profile(&profile.config_dir, profile.claude_config_mode);
        match observation {
            Ok(observation) => {
                let auth_ok = observation.authentication == AuthenticationState::Authenticated;
                checks.push(DoctorCheck {
                    name: "authentication".to_owned(),
                    passed: auth_ok,
                    message: if auth_ok {
                        "provider reports an authenticated profile".to_owned()
                    } else {
                        "provider authentication is unavailable".to_owned()
                    },
                });
                let identity_ok = observation.identity.as_ref().is_some_and(|identity| {
                    identity.stable_id == profile.expected_identity.stable_id
                });
                checks.push(DoctorCheck {
                    name: "identity".to_owned(),
                    passed: identity_ok,
                    message: if identity_ok {
                        "observed identity matches the pinned identity".to_owned()
                    } else {
                        "observed identity does not match the pinned identity".to_owned()
                    },
                });
            }
            Err(_) => {
                checks.push(DoctorCheck {
                    name: "provider_inspection".to_owned(),
                    passed: false,
                    message: "provider inspection failed without exposing provider output"
                        .to_owned(),
                });
            }
        }
        Ok(DoctorReport {
            profile: profile.name,
            healthy: checks.iter().all(|check| check.passed),
            checks,
        })
    }

    pub fn remove(&self, name: &ProfileName) -> Result<Profile> {
        let mut state = self.store.load()?;
        let index = state
            .profiles
            .iter()
            .position(|profile| &profile.name == name)
            .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
        let removed = state.profiles.remove(index);
        self.store.save(&state)?;
        Ok(removed)
    }

    fn find(&self, name: &ProfileName) -> Result<Profile> {
        self.store
            .load()?
            .profiles
            .into_iter()
            .find(|profile| &profile.name == name)
            .ok_or_else(|| Error::ProfileNotFound(name.to_string()))
    }

    fn validate_profile_directory(&self, profile: &Profile) -> Result<()> {
        let directories = ProfileDirectory::new(self.paths.profiles_root())?;
        match profile.origin {
            ProfileOrigin::Created => directories.validate_managed_existing(&profile.config_dir),
            ProfileOrigin::Adopted => directories.validate_existing(&profile.config_dir),
        }
    }
}

fn validate_provider(profile: &Profile, provider: &dyn Provider) -> Result<()> {
    if profile.provider == provider.kind() {
        Ok(())
    } else {
        Err(Error::ProviderMismatch {
            expected: profile.provider.to_string(),
            observed: provider.kind().to_string(),
        })
    }
}

fn validate_authentication(authentication: AuthenticationState) -> Result<()> {
    match authentication {
        AuthenticationState::Authenticated => Ok(()),
        AuthenticationState::Required => Err(Error::AuthenticationRequired),
        AuthenticationState::InspectionFailed => Err(Error::AuthenticationInspectionFailed),
    }
}

#[allow(dead_code)]
const fn _availability_exhaustive(value: Availability) -> Availability {
    value
}
