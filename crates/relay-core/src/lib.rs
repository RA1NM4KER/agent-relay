//! Provider-neutral safety-critical primitives for Agent Relay.

mod atomic;
mod error;
mod model;
mod paths;
mod provider;
mod service;
mod store;

pub use atomic::{AtomicWrite, FsAtomicWriter};
pub use error::{Error, Result};
pub use model::{
    AuthenticationState, Availability, AvailabilityObservation, DoctorCheck, DoctorReport,
    IdentityMetadata, Profile, ProfileName, ProfileOrigin, ProfileStatus, ProviderKind,
};
pub use paths::{ProfileDirectory, RelayPaths};
pub use provider::{ProfileSetupMode, ProfileSetupRequest, Provider, ProviderObservation};
pub use service::{AddProfileRequest, ProfileService};
pub use store::{ProfileState, ProfileStore};
