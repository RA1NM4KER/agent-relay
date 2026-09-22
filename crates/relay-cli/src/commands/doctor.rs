//! `relay doctor`: "is Agent Relay actually ready to save me when my current account runs out?"
//! One concise, confidence-oriented answer, built entirely from the shared readiness model in
//! [`crate::readiness`] so this can never disagree with `relay setup`'s completion screen or
//! `relay status`'s "Automatic handoff" line.

use relay_core::{Error, ProfileService, RelayPaths};

use crate::{
    cli::DoctorArgs,
    output::{CommandOutput, success},
    preferences, providers,
    readiness::{self, Level},
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &DoctorArgs,
) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };
    let readiness = readiness::assess(service, &registered, &preferences, &executables);
    success("doctor", render_human(&readiness), render_json(&readiness))
}

/// Also used by the in-agent `/relay:doctor` (and legacy `/relay doctor`) hook, so the terminal
/// and in-session answers can never read differently for the same readiness.
pub(crate) fn render_human(readiness: &readiness::Readiness) -> String {
    let mut lines = vec!["Agent Relay health".to_owned(), String::new()];
    for check in &readiness.checks {
        lines.push(format!("{} {}", check.level.symbol(), check.label));
        if check.level != Level::Ok
            && let Some(detail) = &check.detail
        {
            lines.push(format!("  {detail}"));
        }
        if let Some(remedy) = &check.remedy {
            lines.push(format!("  Run: {remedy}"));
        }
    }
    lines.push(String::new());
    lines.push(match readiness.overall() {
        Level::Ok => "Ready for automatic handoff.".to_owned(),
        Level::Warning => "Ready for automatic handoff (with warnings above).".to_owned(),
        Level::Blocking => "Not ready for automatic handoff — see above.".to_owned(),
    });
    lines.join("\n")
}

fn render_json(readiness: &readiness::Readiness) -> serde_json::Value {
    serde_json::json!({
        "ready": readiness.ready(),
        "overall": readiness.overall(),
        "checks": readiness.checks,
    })
}
