//! M2B: crash-safe transactional handoff — durable state machine, project-level writer lease,
//! and an orchestration lock with genuine OS-release-on-crash semantics.
//!
//! `relay-core` stays provider-neutral: this module knows nothing about Claude. It drives the
//! transaction through injected [`SourceLiveness`], [`SessionStager`], and [`TargetLauncher`]
//! ports; `relay-provider-claude` supplies the real Claude-aware implementations.

mod continuity;
mod coordinator;
mod journal;
mod lease;
mod lock;
mod project;
mod session;
mod state;

pub use continuity::{
    ContinuationBundle, ContinuityType, ConversationExcerpt, EXCERPT_BYTE_CAP, ExcerptRole,
    RECENT_CONTEXT_BYTE_BUDGET, RepoFacts, bound_recent_context, render_bootstrap_prompt,
};
pub use coordinator::{
    ContextCapturer, HandoffCoordinator, HandoffRequest, LaunchDirective, LaunchOutcome,
    LivenessVerdict, SessionStager, SessionStopper, SourceLiveness, TargetLauncher,
    TargetVerification, TransferOutcome, TransferredArtifact,
};
pub use journal::{
    ArtifactRecord, Checkpoint, HandoffJournal, JournalStore, TargetLaunchRecord,
    VerificationRecord,
};
pub use lease::{LeaseStore, WriterLease};
pub use lock::{OrchestrationLock, ProcessIdentity};
pub use project::ProjectId;
pub use session::{
    RelaySessionId, RelaySessionRecord, RelaySessionView, SessionState, SessionStore,
};
pub use state::{FailedPhase, HandoffState, TransactionId};
