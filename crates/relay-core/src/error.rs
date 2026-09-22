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
    #[error("an untracked Claude session was detected for the source profile: {0}")]
    UntrackedWriterDetected(String),
    #[error("a writer is already active for this project, owned by: {0}")]
    WriterAlreadyActive(String),
    #[error(
        "A Relay-managed session is already active for this project.\n\nCurrent profile: {owner}\n\nRun:\n  relay resume\nto continue it.\n\nOr:\n  relay {entrypoint} --new\nto stop the existing managed session and start a fresh one."
    )]
    ManagedSessionAlreadyActive {
        owner: String,
        /// The provider entrypoint that was invoked (`claude` / `codex`), so the suggested
        /// replacement command is the one the user actually ran.
        entrypoint: &'static str,
    },
    #[error(
        "cannot safely determine whether profile '{0}''s managed session is still live; refusing \
         to guess between attaching to it and replaying it natively. Investigate manually (e.g. \
         `claude agents --json` under that profile), or use `relay claude --new` to safely \
         replace it."
    )]
    AmbiguousSessionLiveness(String),
    #[error(
        "Relay could not prove that Codex thread '{0}' exists in this profile's own Codex home \
         (for the expected project), so it did not resume it: an interactive `codex resume` with a \
         missing or stale id may silently start a different thread. Start a new session with \
         `relay codex --new` (or `relay claude --new`), or hand off explicitly with `relay switch`."
    )]
    CodexThreadNotVerified(String),
    /// A live/selected conversation could not be brought under Relay: the reason says exactly
    /// which proof was missing. Nothing was changed.
    #[error("cannot adopt this conversation: {0}. Nothing was changed.")]
    AdoptionRefused(String),
    #[error("cannot use the requested switch target: {0}")]
    SwitchTargetUnavailable(String),
    #[error(
        "no interactive terminal to choose a profile in; choose one explicitly with \
         `relay switch <profile>` ({0})"
    )]
    SwitchPickerNeedsTerminal(String),
    #[error("switch cancelled; nothing was changed")]
    SwitchCancelled,
    #[error(
        "Relay session {0} is dormant (no live conversation to move); continue it with \
         `relay resume --session {0}` first"
    )]
    RelaySessionDormant(String),
    #[error("{0}")]
    NoResumableSession(String),
    #[error("no Relay session '{0}' in this project (run `relay status` to list them)")]
    RelaySessionNotFound(String),
    #[error("'{0}' matches more than one Relay session; use a longer id")]
    RelaySessionAmbiguous(String),
    #[error(
        "Relay session {0} is already active (a live provider process owns it); it is never \
         started twice. Use it where it is running, or `relay switch` to move it."
    )]
    RelaySessionActive(String),
    #[error(
        "that exact conversation is already active in Relay session {0}; the same provider \
         conversation never gets two active owners"
    )]
    NativeSessionAlreadyActive(String),
    #[error(
        "this project has {0} Relay sessions and no way to choose between them here; pass \
         `--session <id>` (see `relay status`)"
    )]
    SessionAmbiguous(String),
    #[error(
        "both Claude Code and Codex are installed, so Relay will not guess which one the new \
         profile is for; pass `--provider claude` or `--provider codex`"
    )]
    ProviderChoiceRequired,
    #[error(
        "the provider argument '{0}' conflicts with something Agent Relay itself must own for a \
         managed session ({1}); Relay did not start anything"
    )]
    ProviderArgumentRejected(String, &'static str),
    #[error(
        "profile '{0}' is exhausted right now (Codex reports ordinary usage is not allowed); Relay \
         did not switch to it"
    )]
    TargetProfileExhausted(String),
    #[error(
        "Relay could not verify Codex usage for profile '{0}' (the structured usage interface was \
         unavailable or ambiguous), so it did not start or switch to it"
    )]
    CodexUsageUnverified(String),
    #[error(
        "Codex profile '{0}' is exhausted and no other configured profile is eligible right now"
    )]
    NoEligibleProfile(String),
    #[error("no {0} profile is configured (checked the primary and fallbacks in priority order)")]
    NoProfileForProvider(&'static str),
    #[error(
        "profile '{profile}' is a {actual} profile, but this command starts a {expected} session"
    )]
    ProfileProviderMismatch {
        profile: String,
        expected: &'static str,
        actual: String,
    },
    #[error(
        "a prior handoff transaction for this project requires explicit recovery before a new one can start: {0}"
    )]
    PendingRecoveryRequired(String),
    #[error("authoritative stop could not be verified quiescent within the bounded window: {0}")]
    StopNotVerified(String),
    #[error("target session artifact conflict requires explicit resolution: {0}")]
    ConflictRequiresResolution(String),
    #[error("no backup was found to roll back")]
    NoBackupToRestore,
    #[error("conflict-resolution metadata is corrupted or incompatible")]
    CorruptedConflictMetadata,
    #[error("usage integration refused: {0}")]
    IntegrationRefused(String),
    #[error("a prior handoff transaction needs recovery before automation can continue: {0}")]
    RecoveryRequired(String),
    #[error("serialization failed")]
    SerializationFailed,
    #[error("required handoff port is not configured for this continuity type: {0}")]
    MissingHandoffPort(String),
    #[error(
        "no writer currently owns this project; start a managed conversation with `relay claude` or \
         `relay codex` first"
    )]
    NoActiveWriterForProject,
    #[error("'{0}' is already the current writer for this project")]
    AlreadyCurrentWriter(String),
    #[error(
        "recovery cannot durably reconstruct a state-continuation bundle; the source's own \
         state may have changed since capture, so the target was not relaunched automatically: {0}"
    )]
    ContinuationBundleNotRecoverable(String),
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
            Self::UntrackedWriterDetected(_) => "untracked_writer_detected",
            Self::WriterAlreadyActive(_) => "writer_already_active",
            Self::ManagedSessionAlreadyActive { .. } => "managed_session_active",
            Self::AmbiguousSessionLiveness(_) => "ambiguous_session_liveness",
            Self::CodexThreadNotVerified(_) => "codex_thread_not_verified",
            Self::AdoptionRefused(_) => "adoption_refused",
            Self::SwitchTargetUnavailable(_) => "switch_target_unavailable",
            Self::SwitchPickerNeedsTerminal(_) => "switch_needs_terminal",
            Self::SwitchCancelled => "switch_cancelled",
            Self::RelaySessionDormant(_) => "relay_session_dormant",
            Self::NoResumableSession(_) => "no_resumable_session",
            Self::RelaySessionNotFound(_) => "relay_session_not_found",
            Self::RelaySessionAmbiguous(_) => "relay_session_ambiguous",
            Self::RelaySessionActive(_) => "relay_session_active",
            Self::NativeSessionAlreadyActive(_) => "native_session_already_active",
            Self::SessionAmbiguous(_) => "session_ambiguous",
            Self::ProviderChoiceRequired => "provider_choice_required",
            Self::ProviderArgumentRejected(..) => "provider_argument_rejected",
            Self::NoProfileForProvider(_) => "no_profile_for_provider",
            Self::TargetProfileExhausted(_) => "target_profile_exhausted",
            Self::CodexUsageUnverified(_) => "codex_usage_unverified",
            Self::NoEligibleProfile(_) => "no_eligible_profile",
            Self::ProfileProviderMismatch { .. } => "profile_provider_mismatch",
            Self::PendingRecoveryRequired(_) => "pending_recovery_required",
            Self::StopNotVerified(_) => "stop_not_verified",
            Self::ConflictRequiresResolution(_) => "conflict_requires_resolution",
            Self::NoBackupToRestore => "no_backup_to_restore",
            Self::CorruptedConflictMetadata => "corrupted_conflict_metadata",
            Self::IntegrationRefused(_) => "integration_refused",
            Self::RecoveryRequired(_) => "recovery_required",
            Self::SerializationFailed => "serialization_failed",
            Self::MissingHandoffPort(_) => "missing_handoff_port",
            Self::NoActiveWriterForProject => "no_active_writer_for_project",
            Self::AlreadyCurrentWriter(_) => "already_current_writer",
            Self::ContinuationBundleNotRecoverable(_) => "continuation_bundle_not_recoverable",
            Self::MissingEnvironment(_) => "missing_environment",
            Self::Io { .. } => "io_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
