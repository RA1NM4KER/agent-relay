//! `relay doctor`: "is Agent Relay actually ready to save me when my current account runs out?"
//! One concise, confidence-oriented answer, built entirely from the shared readiness model in
//! [`crate::readiness`] so this can never disagree with `relay setup`'s completion screen or
//! `relay status`'s "Automatic handoff" line.

use relay_core::{Error, ProfileService, RelayPaths};

use crate::{
    cli::DoctorArgs,
    output::{CommandOutput, success},
    preferences, presentation, progress, providers,
    readiness::{self, Level},
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &DoctorArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let progress = progress::Progress::start("Checking Relay configuration…", json_mode);
    let registered = service.list()?;
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };
    let cwd = std::env::current_dir().ok();
    let readiness = readiness::assess_for_project_reporting(
        service,
        &registered,
        &preferences,
        &executables,
        args.project_dir.as_deref().or(cwd.as_deref()),
        true,
        &|phase| progress.set_label(phase),
    );
    progress.finish();
    let project = args.project_dir.as_deref().or(cwd.as_deref());
    let mut human = render_human(&readiness);
    if let Some(project) = project {
        let title_end = human.find("\n\n").unwrap_or(human.len());
        human.insert_str(
            title_end + 2,
            &format!("Current project\n  {}\n\n", project.display()),
        );
    }
    success("doctor", human, render_json(&readiness))
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
    let title = emphasize("Agent Relay Doctor");
    let verdict = match readiness.overall() {
        Level::Ok => "✓ Ready for automatic handoff.",
        Level::Warning => "! Ready for automatic handoff (with warnings).",
        Level::Blocking => "✗ Not ready for automatic handoff — see below.",
    };
    let mut sections = vec![format!("Automatic handoff\n  {}", emphasize(verdict))];
    for (level, heading) in [
        (Level::Blocking, "Blockers"),
        (Level::Warning, "Warnings"),
        (Level::Ok, "Passed checks"),
    ] {
        let mut checks = Vec::new();
        for check in readiness.checks.iter().filter(|c| c.level == level) {
            let symbol = check.level.symbol();
            let label = format!("  {symbol} {}", check_label(check));
            checks.push(if level == Level::Blocking {
                format!("  {}", emphasize(label.trim_start()))
            } else {
                label
            });
            if level != Level::Ok
                && let Some(detail) = &check.detail
            {
                checks.push(format!("    {}", presentation::diagnostic(detail)));
            }
        }
        if !checks.is_empty() {
            sections.push(format!("{heading}\n{}", checks.join("\n")));
        }
    }
    let recovery = super::status::recovery_for(readiness);
    if !recovery.is_empty() {
        sections.push(format!("Recovery\n{}", recovery.join("\n\n")));
    }
    format!("{}{}", crate::output::header(&title), sections.join("\n\n"))
}

fn check_label(check: &readiness::Check) -> String {
    if let Some(detail) = &check.detail {
        let auth = if detail.contains("is logged out") {
            Some("Needs login")
        } else if detail.starts_with("authentication_revoked:") {
            Some("Credentials revoked")
        } else if detail.starts_with("authentication_invalid:") {
            Some("Credentials invalid")
        } else {
            None
        };
        if let Some(auth) = auth {
            let subject = check
                .label
                .strip_suffix(" authenticated")
                .or_else(|| check.label.strip_suffix(" ordinary usage verified"))
                .unwrap_or(&check.label);
            return format!("{subject} · {auth}");
        }
    }
    if check.level == Level::Warning
        && let Some(subject) = check.label.strip_suffix(" supported")
    {
        return format!("{subject} · Version unverified");
    }
    check.label.clone()
}

fn render_json(readiness: &readiness::Readiness) -> serde_json::Value {
    serde_json::json!({
        "ready": readiness.ready(),
        "overall": readiness.overall(),
        "checks": readiness.checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blockers_warnings_and_recovery_keep_separate_sections_and_levels() {
        let readiness = readiness::Readiness {
            checks: vec![
                readiness::Check {
                    label: "main authenticated".into(),
                    level: Level::Blocking,
                    detail: Some("logged out".into()),
                    remedy: Some("relay login main".into()),
                },
                readiness::Check {
                    label: "backup authenticated".into(),
                    level: Level::Warning,
                    detail: Some("logged out".into()),
                    remedy: Some("relay login backup".into()),
                },
                readiness::Check {
                    label: "Git available".into(),
                    level: Level::Ok,
                    detail: None,
                    remedy: None,
                },
            ],
        };
        let human = render_human(&readiness);
        assert!(human.contains("Blockers\n  ✗ main"));
        assert!(human.contains("Warnings\n  ! backup"));
        assert!(human.contains("Passed checks\n  ✓ Git"));
        assert!(human.contains("Recovery\n  main authenticated\n    relay login main"));
        let styled = presentation::render(
            &human,
            presentation::Terminal {
                color: true,
                width: 80,
            },
        );
        assert!(styled.contains("\x1b[31m  ✗") && styled.contains("\x1b[33m  !"));
        assert_eq!(readiness.overall(), Level::Blocking);
        let warning_only = readiness::Readiness {
            checks: readiness.checks[1..].to_vec(),
        };
        assert!(warning_only.ready());
        assert!(render_human(&warning_only).contains("Ready for automatic handoff"));
        assert!(!render_human_markdown(&readiness).contains('\x1b'));
    }
}
