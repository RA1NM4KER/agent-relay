use std::path::PathBuf;

/// Errors are intentionally structured and must never embed secret values.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("profile name is invalid: {0}")]
    InvalidProfileName(String),
    #[error("profile already exists: {0}")]
    DuplicateProfile(String),
    #[error("profile was not found: {0}")]
    ProfileNotFound(String),
    #[error("provider mismatch: expected {expected}, observed {observed}")]
    ProviderMismatch { expected: String, observed: String },
    #[error("profile authentication is required")]
    AuthenticationRequired,
    #[error("provider authentication inspection failed")]
    AuthenticationInspectionFailed,
    #[error("profile identity does not match the pinned identity")]
    IdentityMismatch,
    #[error("adopting an existing profile requires an expected identity")]
    AdoptionIdentityRequired,
    #[error("path must be absolute: {0}")]
    PathNotAbsolute(PathBuf),
    #[error("path is outside the managed profile root: {0}")]
    PathOutsideManagedRoot(PathBuf),
    #[error("path contains or is a symbolic link: {0}")]
    SymbolicLink(PathBuf),
    #[error("path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("path has unsafe permissions: {0}")]
    UnsafePermissions(PathBuf),
    #[error("path is not owned by the current user: {0}")]
    WrongOwner(PathBuf),
    #[error("state file is corrupted or incompatible")]
    CorruptedState,
    #[error("state schema version {0} is unsupported")]
    UnsupportedStateVersion(u32),
    #[error("atomic state write failed")]
    AtomicWriteFailed,
    #[error("provider is unavailable")]
    ProviderUnavailable,
    #[error("provider-local state is corrupted or incompatible")]
    ProviderStateCorrupted,
    #[error("provider operation is unsupported")]
    ProviderUnsupported,
    #[error("serialization failed")]
    SerializationFailed,
    #[error("required environment path is unavailable: {0}")]
    MissingEnvironment(&'static str),
    #[error("I/O operation failed for {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidProfileName(_) => "invalid_profile_name",
            Self::DuplicateProfile(_) => "duplicate_profile",
            Self::ProfileNotFound(_) => "profile_not_found",
            Self::ProviderMismatch { .. } => "provider_mismatch",
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthenticationInspectionFailed => "authentication_inspection_failed",
            Self::IdentityMismatch => "identity_mismatch",
            Self::AdoptionIdentityRequired => "adoption_identity_required",
            Self::PathNotAbsolute(_) => "path_not_absolute",
            Self::PathOutsideManagedRoot(_) => "path_outside_managed_root",
            Self::SymbolicLink(_) => "symbolic_link",
            Self::NotDirectory(_) => "not_directory",
            Self::UnsafePermissions(_) => "unsafe_permissions",
            Self::WrongOwner(_) => "wrong_owner",
            Self::CorruptedState => "corrupted_state",
            Self::UnsupportedStateVersion(_) => "unsupported_state_version",
            Self::AtomicWriteFailed => "atomic_write_failed",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderStateCorrupted => "provider_state_corrupted",
            Self::ProviderUnsupported => "provider_unsupported",
            Self::SerializationFailed => "serialization_failed",
            Self::MissingEnvironment(_) => "missing_environment",
            Self::Io { .. } => "io_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
