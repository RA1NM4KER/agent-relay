//! Placeholder boundary for the optional Herdr adapter.
//!
//! Herdr is never required for core profile storage or safety decisions.

use std::path::PathBuf;

use relay_core::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HerdrPaneContext {
    pub pane_id: String,
    pub working_directory: PathBuf,
    pub agent_session_id: Option<String>,
}

pub trait HerdrAdapter: Send + Sync {
    fn focused_pane(&self) -> Result<HerdrPaneContext>;
}
