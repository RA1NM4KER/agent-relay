use std::path::PathBuf;

/// Errors are intentionally structured and must never embed secret values.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("profile name is invalid: {0}")]
    InvalidProfileName(String),
    #[error("profile already exists: {0}")]
    DuplicateProfile(String),
    #[error("provider identity is already registered to another profile")]
    DuplicateIdentity,
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
    #[error("provider executable was not found")]
    ProviderExecutableMissing,
    #[error("provider executable failed safety validation")]
    UnsafeProviderExecutable,
    #[error("provider command failed")]
    ProviderCommandFailed,
    #[error("provider command timed out")]
    ProviderCommandTimeout,
    #[error("provider output was malformed")]
    MalformedProviderOutput,
    #[error("provider output schema is unsupported")]
    UnsupportedProviderSchema,
    #[error("provider version is unsupported")]
    UnsupportedProviderVersion,
    #[error("provider identity could not be established safely")]
    IdentityUnavailable,
    #[error("provider reported a different profile directory")]
    ProviderProfileMismatch,
    #[error("authentication environment contains conflicting overrides")]
    EnvironmentOverrideConflict,
    #[error("session id is not a valid UUID")]
    InvalidSessionId,
    #[error("no session artifact was found for the requested project and session id")]
    SessionNotFound,
    #[error("source profile has an active Claude process; stop it before transferring")]
    SourceProfileActive,
    #[error("a target session artifact already exists and does not match the source")]
    TargetArtifactDiverges,
    #[error("a staged session artifact did not verify against its source hash after copy")]
    TransferVerificationFailed,
    #[error("another handoff is already in progress for this project")]
    OrchestrationLockHeld,
    #[error("writer lease is corrupted or incompatible")]
    CorruptedLease,
    #[error("handoff journal is corrupted or incompatible")]
    CorruptedJournal,
    #[error("no handoff transaction was found at: {0}")]
    TransactionNotFound(String),
    #[error("illegal handoff state transition from {from} to {to}")]
    IllegalStateTransition { from: String, to: String },
    #[error("transaction id is not in a Relay-generated shape: {0}")]
    InvalidTransactionId(String),
    #[error("this project's writer lease is owned by another profile: {0}")]
    WriterLeaseOwnedByAnotherProfile(String),
    #[error("target verification did not match the expected session or profile")]
    TargetVerificationMismatch,
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
            Self::DuplicateIdentity => "duplicate_identity",
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
            Self::ProviderExecutableMissing => "provider_executable_missing",
            Self::UnsafeProviderExecutable => "unsafe_provider_executable",
            Self::ProviderCommandFailed => "provider_command_failed",
            Self::ProviderCommandTimeout => "provider_command_timeout",
            Self::MalformedProviderOutput => "malformed_provider_output",
            Self::UnsupportedProviderSchema => "unsupported_provider_schema",
            Self::UnsupportedProviderVersion => "unsupported_provider_version",
            Self::IdentityUnavailable => "identity_unavailable",
            Self::ProviderProfileMismatch => "provider_profile_mismatch",
            Self::EnvironmentOverrideConflict => "environment_override_conflict",
            Self::InvalidSessionId => "invalid_session_id",
            Self::SessionNotFound => "session_not_found",
            Self::SourceProfileActive => "source_profile_active",
            Self::TargetArtifactDiverges => "target_artifact_diverges",
            Self::TransferVerificationFailed => "transfer_verification_failed",
            Self::OrchestrationLockHeld => "orchestration_lock_held",
            Self::CorruptedLease => "corrupted_lease",
            Self::CorruptedJournal => "corrupted_journal",
            Self::TransactionNotFound(_) => "transaction_not_found",
            Self::IllegalStateTransition { .. } => "illegal_state_transition",
            Self::InvalidTransactionId(_) => "invalid_transaction_id",
            Self::WriterLeaseOwnedByAnotherProfile(_) => "writer_lease_owned_by_another_profile",
            Self::TargetVerificationMismatch => "target_verification_mismatch",
            Self::SerializationFailed => "serialization_failed",
            Self::MissingEnvironment(_) => "missing_environment",
            Self::Io { .. } => "io_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
