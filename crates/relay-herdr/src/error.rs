/// Errors from the Herdr adapter boundary: invoking the `relay` binary as a subprocess,
/// mapping Herdr pane metadata to a registered Relay profile, and composing read-only status
/// views. Never embeds secret values, and never represents a *Relay-side* safety decision —
/// those are always reported verbatim from Relay's own stable JSON error codes via
/// [`HerdrIntegrationError::RelayRefused`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HerdrIntegrationError {
    #[error("the `relay` executable could not be found on PATH or at the given path")]
    RelayExecutableMissing,
    #[error(
        "the `relay` executable failed a basic safety check (world/group-writable, or not owned \
         by root or the current user)"
    )]
    RelayUnsafeExecutable,
    #[error("invoking the `relay` executable failed to start or exited abnormally")]
    RelayCommandFailed,
    #[error("invoking the `relay` executable did not complete within the allotted time")]
    RelayCommandTimeout,
    #[error("the `relay` executable's --json output was not the expected stable envelope shape")]
    RelayMalformedOutput,
    /// Relay's own CLI refused the request. `code` is Relay's stable machine-readable error
    /// code (see `relay_core::Error::code`); `message` is for display only and must never be
    /// pattern-matched on.
    #[error("relay refused ({code}): {message}")]
    RelayRefused { code: String, message: String },
    /// More than one registered Relay profile's `config_dir` matches the pane's
    /// `CLAUDE_CONFIG_DIR`. This should never happen for a healthy registry (Relay rejects
    /// duplicate identities), but a stale/manually-edited `profiles.toml` could produce it, so
    /// the adapter fails closed rather than guessing.
    #[error("more than one registered Relay profile matches this pane's config directory")]
    ProfileMappingAmbiguous { candidates: Vec<String> },
    /// No registered Relay profile's `config_dir` matches the pane's `CLAUDE_CONFIG_DIR`.
    #[error("no registered Relay profile matches this pane's config directory")]
    ProfileMappingUnknown,
    /// The pane has no `CLAUDE_CONFIG_DIR` at all, so no mapping can even be attempted.
    #[error("this pane has no Claude config directory to map to a Relay profile")]
    MissingConfigDir,
    /// The action requires a Claude session id (for staging/handoff/watch) and the pane did not
    /// report one — for example, a pane whose agent has not started a session yet.
    #[error("this pane has no known Claude session id")]
    MissingSessionIdentity,
    /// The focused pane is not running a Claude agent at all (or Herdr could not identify it as
    /// one). The adapter never guesses; it refuses rather than assuming a provider.
    #[error("the focused pane is not a recognized Claude agent pane")]
    NonClaudePane,
    /// Herdr's own pane/workspace metadata could not be read (socket unavailable, no focused
    /// pane, malformed response, etc.). This is a Herdr-side problem, not a Relay-side one.
    #[error("Herdr pane/workspace metadata is unavailable")]
    HerdrMetadataUnavailable,
}
