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
    let cwd = std::env::current_dir().ok();
    let readiness = readiness::assess_for_project(
        service,
        &registered,
        &preferences,
        &executables,
        args.project_dir.as_deref().or(cwd.as_deref()),
    );
    success("doctor", render_human(&readiness), render_json(&readiness))
}

/// Plain-text rendering for a real terminal (`relay doctor`), which does not interpret Markdown.
pub(crate) fn render_human(readiness: &readiness::Readiness) -> String {
    render(readiness, |text| text.to_owned())
}

/// Markdown-emphasis rendering for the in-agent `/relay:doctor` (and legacy `/relay doctor`)
/// hook: Claude's own "blocked by hook" panel shows every line in one flat colour with no
/// symbol/text-weight distinction, so the only lever left to separate "problem" from "fine" is
/// **bold** on the title, on any blocking check's own label, and on the final verdict.
pub(crate) fn render_human_markdown(readiness: &readiness::Readiness) -> String {
    render(readiness, |text| format!("**{text}**"))
}

fn render(readiness: &readiness::Readiness, emphasize: impl Fn(&str) -> String) -> String {
    let mut lines = vec![emphasize("Agent Relay health"), String::new()];
    for check in &readiness.checks {
        let label = format!("{} {}", check.level.symbol(), check.label);
        lines.push(if check.level == Level::Blocking {
            emphasize(&label)
        } else {
            label
        });
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
    lines.push(emphasize(match readiness.overall() {
        Level::Ok => "Ready for automatic handoff.",
        Level::Warning => "Ready for automatic handoff (with warnings above).",
        Level::Blocking => "Not ready for automatic handoff — see above.",
    }));
    lines.join("\n")
}

fn render_json(readiness: &readiness::Readiness) -> serde_json::Value {
    serde_json::json!({
        "ready": readiness.ready(),
        "overall": readiness.overall(),
        "checks": readiness.checks,
    })
}
