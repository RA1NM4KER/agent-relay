//! `relay launch`: advanced, scripting-oriented form of `relay claude` (M2B.5) — creates a
//! background writer lease directly rather than opening an interactive terminal.

use std::path::{Path, PathBuf};

use relay_core::{Error, ProfileName, ProfileService, RelayPaths};
use serde_json::json;

use crate::{
    launch::perform_launch,
    output::{CommandOutput, success},
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    profile: &ProfileName,
    project_dir: &Path,
    prompt: &str,
    claude_executable: &Option<PathBuf>,
) -> Result<CommandOutput, Error> {
    let (session, lease) = perform_launch(
        service,
        paths,
        profile,
        project_dir,
        prompt,
        claude_executable.as_deref(),
        &[],
    )?;
    let human = format!(
        "Launched '{}' for {} as Relay session {}\nSession: {}\nPid: {}\nBackground job: {}",
        profile,
        project_dir.display(),
        session.id.short(),
        lease.session_id,
        lease.owner_process.pid,
        lease.provider_handle.clone().unwrap_or_default()
    );
    // The lease's own fields stay at the top level (as before); the Relay session is added.
    let mut data = serde_json::to_value(&lease).map_err(|_| Error::SerializationFailed)?;
    data["relay_session_id"] = json!(session.id.as_str());
    success("launch", human, data)
}
