//! M2B: crash-safe transactional handoff — durable state machine, project-level writer lease,
//! and an orchestration lock with genuine OS-release-on-crash semantics.
//!
//! `relay-core` stays provider-neutral: this module knows nothing about Claude. It drives the
//! transaction through injected [`SourceLiveness`], [`SessionStager`], and [`TargetLauncher`]
//! ports; `relay-provider-claude` supplies the real Claude-aware implementations.

mod coordinator;
mod journal;
mod lease;
mod lock;
mod project;
mod state;

pub use coordinator::{
    HandoffCoordinator, HandoffRequest, LaunchOutcome, LivenessVerdict, SessionStager,
    SessionStopper, SourceLiveness, TargetLauncher, TargetVerification, TransferOutcome,
    TransferredArtifact,
};
pub use journal::{
    ArtifactRecord, Checkpoint, HandoffJournal, JournalStore, TargetLaunchRecord,
    VerificationRecord,
};
pub use lease::{LeaseStore, WriterLease};
pub use lock::{OrchestrationLock, ProcessIdentity};
pub use project::ProjectId;
pub use state::{FailedPhase, HandoffState, TransactionId};
