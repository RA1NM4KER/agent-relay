//! `relay status`/`relay profiles`: read-only, plain-language summaries of a project's Relay
//! Sessions and of every registered profile's login state.

use std::path::{Path, PathBuf};

use relay_core::{Error, Profile, ProfileService, RelayPaths, handoff::OrchestrationLock};
use serde_json::json;

use crate::{
    auth::friendly_auth_state,
    auto_handoff,
    output::{CommandOutput, success},
    preferences, providers, readiness, sessions, target,
    util::current_unix_ms,
};

pub(crate) fn run_profiles(
    service: &ProfileService,
    paths: &RelayPaths,
) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for profile in &registered {
        let role = if preferences.primary_profile.as_ref() == Some(&profile.name) {
            "primary"
        } else if preferences.fallback_profiles.contains(&profile.name) {
            "fallback"
        } else {
            "unassigned"
        };
        let (auth_state, _detail) =
            friendly_auth_state(profile, &providers::ExecutableOverrides::default());
        lines.push(format!(
            "{:<12} {:<8} {:<10} {}",
            profile.name, profile.provider, role, auth_state
        ));
        rows.push(json!({
            "name": profile.name.as_str(),
            "provider": profile.provider.to_string(),
            "role": role,
            "authentication": auth_state,
        }));
    }
    if lines.is_empty() {
        lines.push("No profiles registered yet. Run `relay setup` to get started.".to_owned());
    } else {
        lines.insert(
            0,
            format!(
                "{:<12} {:<8} {:<10} {}",
                "PROFILE", "PROVIDER", "ROLE", "AUTH"
            ),
        );
    }
    success("profiles", lines.join("\n"), json!({ "profiles": rows }))
}

pub(crate) fn run_status(
    service: &ProfileService,
    paths: &RelayPaths,
    project_dir: Option<&Path>,
) -> Result<CommandOutput, Error> {
    let cwd = match project_dir {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let canonical =
        std::fs::canonicalize(&cwd).map_err(|source| Error::Io { path: cwd, source })?;

    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();
    let Some(primary) = preferences.primary_profile.clone() else {
        return success(
            "status",
            "Agent Relay is not set up yet.\n\nRun:\n    relay setup".to_owned(),
            json!({ "configured": false }),
        );
    };

    let registered = service.list()?;
    let executables = providers::ExecutableOverrides::default();
    let primary_profile = registered.iter().find(|profile| profile.name == primary);
    let (primary_auth, _) = primary_profile.map_or(("not registered", None), |profile| {
        friendly_auth_state(profile, &executables)
    });

    // Relay supervises conversations, not repositories: list this project's Relay sessions, each
    // active one with its owner and each dormant one with its last profile (history, not an owner).
    let store = sessions::open_store(paths, &canonical)?;
    let views = sessions::reconcile(paths, &canonical, &registered, &executables)?;
    let now = current_unix_ms();
    let provider_of = |view: &relay_core::handoff::RelaySessionView| {
        registered
            .iter()
            .find(|profile| &profile.name == view.profile())
            .map(|profile| profile.provider)
    };
    let mut session_rows = Vec::new();
    let mut lines_active = Vec::new();
    let mut lines_dormant = Vec::new();
    // The session decision-oriented "Current" focuses on: the most recently active session, so
    // repeated `relay status` calls track whichever conversation you actually touched last.
    let mut focus: Option<&relay_core::handoff::RelaySessionView> = None;
    for view in &views {
        let dir = store.session_dir(&view.record.relay_session_id);
        let in_transaction =
            OrchestrationLock::at_path(dir.join("orchestration.lock")).is_currently_held();
        let state = view.state();
        let mut line = sessions::describe(view, &registered, now);
        if in_transaction {
            line.push_str("  (handoff in progress)");
        }
        match state {
            relay_core::handoff::SessionState::Active => {
                lines_active.push(format!("  {line}  ACTIVE"));
                let newer = focus.is_none_or(|current| {
                    view.record.last_activity_unix_ms > current.record.last_activity_unix_ms
                });
                if newer {
                    focus = Some(view);
                }
            }
            relay_core::handoff::SessionState::Dormant => {
                lines_dormant.push(format!("  {line}  DORMANT"));
            }
        }
        session_rows.push(json!({
            "relay_session_id": view.record.relay_session_id.as_str(),
            "state": state,
            "provider": provider_of(view).map(|provider| provider.to_string()),
            "profile": view.profile().as_str(),
            "owner": (state == relay_core::handoff::SessionState::Active)
                .then(|| view.profile().as_str()),
            "native_session_id": view.native_session_id(),
            "last_activity_unix_ms": view.record.last_activity_unix_ms,
            "handoff_in_progress": in_transaction,
        }));
    }

    let herdr_connected =
        std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
    let fallback_order: Vec<String> =
        auto_handoff::hierarchy_without(&preferences, &primary, |_| true)
            .into_iter()
            .map(ToString::to_string)
            .collect();
    let fallback_display = if fallback_order.is_empty() {
        "none".to_owned()
    } else {
        fallback_order.join(", ")
    };
    let listing = |title: &str, lines: &[String]| {
        if lines.is_empty() {
            format!("{title}: none")
        } else {
            format!("{title}\n{}", lines.join("\n"))
        }
    };

    let readiness = readiness::assess_for_project(
        service,
        &registered,
        &preferences,
        &executables,
        Some(&canonical),
    );
    let (handoff_ready, handoff_message, handoff_next, handoff_then) = automatic_handoff_summary(
        &registered,
        &preferences,
        &executables,
        &canonical,
        focus,
        &readiness,
    );

    let decision_section = focus.map_or_else(
        || "Current\n  No active session in this project.".to_owned(),
        |view| {
            let owner = view.profile();
            let provider = provider_of(view).map(provider_label).unwrap_or("unknown");
            let mut section = format!(
                "Current\n  {owner} · {provider}\n  Session {}",
                view.record.relay_session_id.short()
            );
            section.push_str("\n\nIf this profile runs out");
            match (&handoff_next, &handoff_then) {
                (Some(next), Some(then)) => {
                    section.push_str(&format!("\n  Next: {next}\n  Then: {then}"));
                }
                (Some(next), None) => section.push_str(&format!("\n  Next: {next}")),
                (None, _) => section.push_str("\n  Next: none configured or eligible"),
            }
            section
        },
    );
    let other_active: Vec<String> = views
        .iter()
        .filter(|view| {
            view.state() == relay_core::handoff::SessionState::Active
                && focus.is_none_or(|current| {
                    current.record.relay_session_id != view.record.relay_session_id
                })
        })
        .map(|view| {
            format!(
                "  {} · {} · {}",
                view.profile(),
                provider_of(view).map(provider_label).unwrap_or("unknown"),
                view.record.relay_session_id.short()
            )
        })
        .collect();

    let mut human =
        format!("Agent Relay\n\n{decision_section}\n\nAutomatic handoff\n  {handoff_message}");
    if !other_active.is_empty() {
        human.push_str(&format!(
            "\n\nOther active sessions\n{}",
            other_active.join("\n")
        ));
    }
    human.push_str(&format!(
        "\n\n{}\n\n{}\n\nPrimary profile: {} ({})\nFallback: {}\nAutomatic handoff installed: {}\nHerdr: {}",
        listing("Active sessions", &lines_active),
        listing("Dormant sessions", &lines_dormant),
        primary,
        primary_auth,
        fallback_display,
        if preferences.usage_integration_enabled == Some(true) {
            "enabled"
        } else {
            "not enabled"
        },
        if herdr_connected {
            "connected"
        } else {
            "not connected"
        },
    ));
    success(
        "status",
        human,
        json!({
            "configured": true,
            "project": canonical,
            "sessions": session_rows,
            "primary_profile": primary.as_str(),
            "primary_authenticated": primary_auth,
            "fallback_profiles": fallback_order,
            "usage_integration_enabled": preferences.usage_integration_enabled.unwrap_or(false),
            "herdr_connected": herdr_connected,
            // Additive (M-UX): decision-oriented fields alongside the original ones above.
            "current_session": focus.map(|view| json!({
                "relay_session_id": view.record.relay_session_id.as_str(),
                "profile": view.profile().as_str(),
                "provider": provider_of(view).map(|provider| provider.to_string()),
            })),
            "next_target": handoff_next,
            "then_target": handoff_then,
            "automatic_handoff_ready": handoff_ready,
            "automatic_handoff_message": handoff_message,
            "readiness_overall": readiness.overall(),
        }),
    )
}

fn provider_label(provider: relay_core::ProviderKind) -> &'static str {
    match provider {
        relay_core::ProviderKind::Codex => "Codex",
        relay_core::ProviderKind::Claude | relay_core::ProviderKind::Fake => "Claude",
    }
}

/// The "Automatic handoff" line `relay status` shows: reuses the exact same target ordering and
/// eligibility `relay switch`'s picker uses for "Next"/"Then" (never shows an ineligible target
/// as though it would actually be chosen), and the shared [`readiness`] model for whether
/// automatic handoff is actually configured to fire at all.
#[allow(clippy::too_many_arguments)]
fn automatic_handoff_summary(
    registered: &[Profile],
    preferences: &preferences::Preferences,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
    focus: Option<&relay_core::handoff::RelaySessionView>,
    readiness: &readiness::Readiness,
) -> (bool, String, Option<String>, Option<String>) {
    let Some(view) = focus else {
        let message = if readiness.ready() {
            "No active session to evaluate.".to_owned()
        } else {
            let reason = readiness
                .checks
                .iter()
                .find(|check| check.level == readiness::Level::Blocking)
                .map(|check| check.detail.as_deref().unwrap_or(&check.label))
                .unwrap_or("see relay doctor");
            format!("Not ready — {reason}")
        };
        return (readiness.ready(), message, None, None);
    };
    let owner = view.profile().clone();
    let targets = target::build_targets(
        registered,
        preferences,
        &owner,
        executables,
        project_dir,
        false,
    );
    let mut eligible = targets.iter().filter(|row| row.selectable());
    let next = eligible
        .next()
        .map(|row| format!("{} · {}", row.name, row.provider_label()));
    let then = eligible
        .next()
        .map(|row| format!("{} · {}", row.name, row.provider_label()));

    if !readiness.ready() {
        let reason = readiness
            .checks
            .iter()
            .find(|check| check.level == readiness::Level::Blocking)
            .map_or_else(
                || "not ready".to_owned(),
                |check| check.detail.clone().unwrap_or_else(|| check.label.clone()),
            );
        return (false, format!("Not ready — {reason}"), next, then);
    }

    let source_usage = registered
        .iter()
        .find(|profile| profile.name == owner)
        .map(|profile| {
            providers::usage_signal_for(
                profile.provider,
                executables,
                false,
                None,
                profile.effective_claude_config_mode(),
            )
            .detect(
                &profile.config_dir,
                project_dir,
                view.native_session_id().unwrap_or(""),
            )
        })
        .and_then(Result::ok);
    let waiting = source_usage.is_none_or(|usage| !usage.state.is_blocking());
    if waiting {
        (
            true,
            "Waiting — current usage is not exhausted.".to_owned(),
            next,
            then,
        )
    } else {
        (true, "Ready.".to_owned(), next, then)
    }
}
