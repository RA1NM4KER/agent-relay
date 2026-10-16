//! `relay status`/`relay profiles`: read-only, plain-language summaries of a project's Relay
//! Sessions and of every registered profile's login state.

use std::path::{Path, PathBuf};

use relay_core::{
    Error, Profile, ProfileName, ProfileService, RelayPaths,
    automation::ProviderIdentityExhaustionStore, handoff::OrchestrationLock,
};
use serde_json::json;

use crate::{
    auth::friendly_auth_state,
    auto_handoff,
    output::{CommandOutput, success},
    preferences, presentation, progress, providers, readiness, sessions, target,
    util::current_unix_ms,
};

/// GitHub #11: default `relay status` human-readable output shows only this many of the most
/// recently active dormant sessions — older ones are summarized, never deleted, forgotten, or
/// hidden from JSON/`relay resume`'s own complete picker. Chosen as a small number consistent
/// with this file's other "only what's decision-relevant" defaults (e.g. the single-session
/// `Current` focus above); not a flag-configurable value because `relay resume` already exposes
/// the complete set for anyone who needs it.
const RECENT_DORMANT_DISPLAY_LIMIT: usize = 3;

pub(crate) fn run_profiles(
    service: &ProfileService,
    paths: &RelayPaths,
) -> Result<CommandOutput, Error> {
    let registered = service.list()?;
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();

    let mut rows = Vec::new();
    for profile in &registered {
        let role = if preferences.primary_profile.as_ref() == Some(&profile.name) {
            "primary"
        } else if preferences.fallback_profiles.contains(&profile.name) {
            "fallback"
        } else {
            "unassigned"
        };
        let (auth_state, detail) =
            friendly_auth_state(profile, &providers::ExecutableOverrides::default());
        let recovery = (auth_state == "needs login").then(|| {
            if profile.provider == relay_core::ProviderKind::Codex {
                format!(
                    "relay logout {}\nrelay login {}",
                    profile.name, profile.name
                )
            } else {
                format!("relay login {}", profile.name)
            }
        });
        rows.push(json!({
            "name": profile.name.as_str(),
            "provider": profile.provider.to_string(),
            "role": role,
            "authentication": auth_state,
            "detail": detail,
            "recovery_command": recovery,
        }));
    }
    let ordered = target::priority_order(&registered, &preferences);
    let human_rows: Vec<_> = ordered
        .iter()
        .filter_map(|profile| rows.iter().find(|row| row["name"] == profile.name.as_str()))
        .collect();
    success(
        "profiles",
        profiles_human(&human_rows),
        json!({ "profiles": rows }),
    )
}

fn profiles_human(rows: &[&serde_json::Value]) -> String {
    profiles_human_at_width(rows, presentation::Terminal::stdout().width)
}

fn profiles_human_at_width(rows: &[&serde_json::Value], width: usize) -> String {
    let mut human = crate::output::header("Agent Relay Profiles");
    if rows.is_empty() {
        human.push_str("No profiles registered.\n\nGet started\n  relay setup");
        return human;
    }
    let name_cap = width
        .saturating_sub(if width < 60 { 27 } else { 49 })
        .clamp(8, 24);
    let mut recovery = Vec::new();
    let mut details = Vec::new();
    let table: Vec<_> = rows
        .iter()
        .map(|row| {
            let name = row["name"].as_str().unwrap_or("");
            let detail = row["detail"].as_str();
            if let Some(detail) = detail {
                details.push(presentation::section(
                    name,
                    &presentation::diagnostic(detail),
                ));
            } else if name.len() > name_cap {
                details.push(presentation::section(
                    name,
                    "Full profile name (abbreviated in table).",
                ));
            }
            if let Some(commands) = row["recovery_command"].as_str() {
                recovery.push(presentation::recovery(name, commands));
            }
            vec![
                name.to_owned(),
                match row["provider"].as_str() {
                    Some("claude") => "Claude",
                    Some("codex") => "Codex",
                    _ => "Fake",
                }
                .to_owned(),
                row["role"].as_str().unwrap_or("").to_owned(),
                presentation::auth_label(row["authentication"].as_str().unwrap_or(""), detail)
                    .to_owned(),
            ]
        })
        .collect();
    if width < 60 {
        let compact: Vec<_> = table
            .iter()
            .map(|row| vec![row[0].clone(), row[3].clone()])
            .collect();
        human.push_str(&presentation::table(
            &["PROFILE", "STATUS"],
            &compact,
            &[name_cap, 25],
        ));
        for row in &table {
            details.push(format!("{}\n  {} · {}", row[0], row[1], row[2]));
        }
    } else {
        human.push_str(&presentation::table(
            &["PROFILE", "PROVIDER", "ROLE", "STATUS"],
            &table,
            &[name_cap, 8, 10, 25],
        ));
    }
    human.push_str("\n\nAuthentication only; project trust and handoff readiness: relay doctor");
    if !details.is_empty() {
        human.push_str(&format!(
            "\n\nDetails\n{}",
            details
                .iter()
                .map(|d| d
                    .lines()
                    .map(|line| format!("  {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"))
                .collect::<Vec<_>>()
                .join("\n\n")
        ));
    }
    if !recovery.is_empty() {
        human.push_str(&format!("\n\nRecovery\n{}", recovery.join("\n\n")));
    }
    human
}

pub(crate) fn run_status(
    service: &ProfileService,
    paths: &RelayPaths,
    project_dir: Option<&Path>,
    json_mode: bool,
    live: bool,
) -> Result<CommandOutput, Error> {
    // Purely local reads (no provider process ever spawned): never animated, since neither is
    // ever slow enough to need it.
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

    // Only `--live` ever spawns a provider CLI (auth checks, version checks, a fresh usage read)
    // — on this machine that used to cost ~14s of `relay status`'s ~14s total, almost entirely
    // Codex's own `codex doctor --json`. The default path below reads only local Relay state, so
    // the indicator here virtually never actually draws (see `Progress`'s own start-delay).
    let progress = progress::Progress::start("Checking provider status…", json_mode);

    let registered = service.list()?;
    let executables = providers::ExecutableOverrides::default();
    let primary_profile = registered.iter().find(|profile| profile.name == primary);
    let (primary_auth, primary_auth_source) = if live {
        let (state, _) = primary_profile.map_or(("not registered", None), |profile| {
            friendly_auth_state(profile, &executables)
        });
        (state.to_owned(), "live")
    } else {
        ("not checked this run".to_owned(), "not_checked")
    };

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
    // GitHub #11: every dormant session is still counted (and still fully present in
    // `session_rows`/`relay resume`'s own picker) — only the human-readable `lines_dormant`
    // display is capped, below.
    let mut dormant_total = 0usize;
    // The session decision-oriented "Current" focuses on: the most recently active session, so
    // repeated `relay status` calls track whichever conversation you actually touched last.
    let mut focus: Option<&relay_core::handoff::RelaySessionView> = None;
    // `views` is already sorted newest-activity-first (see `SessionStore::list`'s own doc
    // comment) — never filesystem order — so both `lines_active`/`lines_dormant` and the
    // dormant-display cap below inherit that same deterministic recency ordering for free.
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
                dormant_total += 1;
                if lines_dormant.len() < RECENT_DORMANT_DISPLAY_LIMIT {
                    lines_dormant.push(format!("  {line}  DORMANT"));
                }
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
            // GitHub Issue #3: additive (every other field here already existed) — dormant
            // sessions carry an execution intent too, set by `relay resume --autonomous`/`relay
            // mode` while they were last active, and it is local metadata already in `view.record`
            // either way, so there is no reason to expose it only for the one focused/active
            // session `current_session` already covers.
            "execution_intent": view.record.execution_intent,
            // GitHub #11: additive, optional provider-native display title — never identity,
            // never consulted by resume/ownership/handoff. `null` when none was ever captured.
            "title": view.record.title,
        }));
    }

    let herdr_connected =
        std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
    let fallback_order: Vec<String> =
        auto_handoff::hierarchy_without(&preferences, &primary, |_| true)
            .into_iter()
            .map(ToString::to_string)
            .collect();
    let listing = |title: &str, lines: &[String]| {
        if lines.is_empty() {
            format!("{title}: none")
        } else {
            format!("{title}\n{}", lines.join("\n"))
        }
    };
    // GitHub #11: `lines_dormant` already holds at most `RECENT_DORMANT_DISPLAY_LIMIT` entries,
    // newest first; `dormant_total` is the real count. Nothing here deletes, prunes, or forgets
    // the rest — `relay resume` and the JSON `sessions` list above still see every one of them.
    let dormant_hidden = dormant_total.saturating_sub(lines_dormant.len());
    let dormant_section = if dormant_hidden == 0 {
        listing("Dormant sessions", &lines_dormant)
    } else {
        format!(
            "Recent dormant sessions\n{}\n\n  + {dormant_hidden} more dormant session{}\n  Run: relay resume",
            lines_dormant.join("\n"),
            if dormant_hidden == 1 { "" } else { "s" },
        )
    };

    let readiness = readiness::assess_for_project_reporting(
        service,
        &registered,
        &preferences,
        &executables,
        Some(&canonical),
        live,
        &|phase| progress.set_label(phase),
    );
    let (handoff_ready, handoff_message, handoff_next, handoff_then) = automatic_handoff_summary(
        &registered,
        &preferences,
        &executables,
        &canonical,
        paths,
        focus,
        &readiness,
        &progress,
        live,
    );
    progress.finish();

    let decision_section = focus.map_or_else(
        || "Current\n  No active session in this project.".to_owned(),
        |view| {
            let owner = view.profile();
            let provider = provider_of(view).map(provider_label).unwrap_or("unknown");
            let mut section = format!(
                "Current\n  {owner} · {provider}\n  Session {}",
                view.record.relay_session_id.short()
            );
            if live {
                section.push_str(&format!(
                    "\n  {}",
                    profile_health(owner.as_str(), &readiness, true)
                ));
            }
            // Not noisy for the common case: only shown when it deviates from the default
            // (Interactive), same as everything else in this local-only summary — see
            // `readiness.rs`'s post-perf-fix "only claim what's known" convention.
            if view.record.execution_intent == relay_core::handoff::ExecutionIntent::Autonomous {
                section.push_str("\n  Execution: autonomous");
            }

            section
        },
    );
    let handoff_display = if live {
        format!(
            "{} {}",
            if handoff_ready { "✓" } else { "✗" },
            if handoff_message == "Waiting — current usage is not exhausted." {
                "Configuration checks passed; no verified handoff trigger."
            } else {
                &handoff_message
            }
        )
    } else {
        "! Not checked live — run relay status --live".to_owned()
    };
    let mut human = format!(
        "{}{decision_section}\n\n{}",
        crate::output::header("Agent Relay"),
        presentation::section("Automatic handoff", &handoff_display)
    );
    if !live {
        human.push_str(&format!("\n  {handoff_message}"));
    }
    let owner = focus.map(|view| view.profile()).unwrap_or(&primary);
    let configured = auto_handoff::hierarchy_without(&preferences, owner, |_| true);
    let chain: Vec<_> = configured
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let health = profile_health(name.as_str(), &readiness, live);
            format!("  {}. {name}  {health}", i + 1)
        })
        .collect();
    human.push_str(&format!(
        "\n\nConfigured fallback chain\n{}",
        if chain.is_empty() {
            "  None configured".to_owned()
        } else {
            chain.join("\n")
        }
    ));
    let other: Vec<_> = registered
        .iter()
        .filter(|profile| {
            profile.name != primary && !preferences.fallback_profiles.contains(&profile.name)
        })
        .map(|profile| {
            format!(
                "  {} · {} · unassigned; not checked",
                profile.name,
                presentation::provider(profile.provider)
            )
        })
        .collect();
    if !other.is_empty() {
        human.push_str(&format!("\n\nOther profiles\n{}", other.join("\n")));
    }
    human.push_str(&format!(
        "\n\n{}\n\n{}",
        listing("Active sessions", &lines_active),
        dormant_section
    ));
    if live {
        let actions = recovery_for(&readiness);
        if !actions.is_empty() {
            human.push_str(&format!("\n\nRecovery\n{}", actions.join("\n\n")));
        }
    }
    human.push_str(&format!(
        "\n\nIntegration: {} · Herdr: {}",
        if preferences.usage_integration_enabled == Some(true) {
            "enabled"
        } else {
            "disabled"
        },
        if herdr_connected {
            "connected"
        } else {
            "not connected"
        }
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
            "primary_authenticated_source": primary_auth_source,
            "live": live,
            "fallback_profiles": fallback_order,
            "usage_integration_enabled": preferences.usage_integration_enabled.unwrap_or(false),
            "herdr_connected": herdr_connected,
            // Additive (M-UX): decision-oriented fields alongside the original ones above.
            "current_session": focus.map(|view| json!({
                "relay_session_id": view.record.relay_session_id.as_str(),
                "profile": view.profile().as_str(),
                "provider": provider_of(view).map(|provider| provider.to_string()),
                "execution_intent": view.record.execution_intent,
            })),
            "next_target": handoff_next,
            "then_target": handoff_then,
            "automatic_handoff_ready": handoff_ready,
            "automatic_handoff_message": handoff_message,
            "readiness_overall": readiness.overall(),
        }),
    )
}

/// Describe only evidence already collected by readiness. Never turn local configuration into
/// live eligibility, and never let this display helper decide candidate selection.
fn profile_health(name: &str, readiness: &readiness::Readiness, live: bool) -> String {
    if !live {
        return "not checked".to_owned();
    }
    let prefix = format!("{name} (");
    let checks: Vec<_> = readiness
        .checks
        .iter()
        .filter(|c| c.label.starts_with(&prefix))
        .collect();
    if checks.is_empty() {
        return "not checked".to_owned();
    }
    for check in &checks {
        if let Some(detail) = &check.detail {
            if detail.starts_with("authentication_revoked:") {
                return "✗ Credentials revoked".to_owned();
            }
            if detail.starts_with("authentication_invalid:") {
                return "✗ Credentials invalid".to_owned();
            }
            if detail.contains("is logged out") {
                return "! Needs login".to_owned();
            }
        }
    }
    if checks.iter().any(|c| c.level == readiness::Level::Blocking) {
        "✗ Unavailable — see relay doctor".to_owned()
    } else if checks.iter().any(|c| c.level == readiness::Level::Warning) {
        "! Warning — see relay doctor".to_owned()
    } else {
        "✓ Checks passed".to_owned()
    }
}

pub(crate) fn recovery_for(readiness: &readiness::Readiness) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for check in &readiness.checks {
        let Some(remedy) = check.remedy.as_deref() else {
            continue;
        };
        if !seen.insert(remedy) {
            continue;
        }
        let label = check.label.split(" (").next().unwrap_or(&check.label);
        if let Some((_, commands)) = groups.iter_mut().find(|(name, _)| *name == label) {
            commands.push(remedy);
        } else {
            groups.push((label, vec![remedy]));
        }
    }
    groups
        .iter()
        .map(|(label, commands)| presentation::recovery(label, &commands.join("\n")))
        .collect()
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
    paths: &RelayPaths,
    focus: Option<&relay_core::handoff::RelaySessionView>,
    readiness: &readiness::Readiness,
    progress: &progress::Progress,
    live: bool,
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

    if !live {
        return local_only_handoff_summary(registered, preferences, &owner, paths, readiness);
    }

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

    progress.set_label(&format!("Checking {owner} usage…"));
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

/// `relay status`'s fast default path: the same "Next"/"Then" shape as the live path above, but
/// eligibility comes only from local state — the configured priority order, whether a profile is
/// enabled, and the durable provider-account exhaustion ledger (already timestamped, already
/// written by real handoff activity — see `docs/automatic-handoff.md`) — never a fresh provider
/// call. The message says plainly that this is last-known information, never presenting it as a
/// live query.
fn local_only_handoff_summary(
    registered: &[Profile],
    preferences: &preferences::Preferences,
    owner: &ProfileName,
    paths: &RelayPaths,
    readiness: &readiness::Readiness,
) -> (bool, String, Option<String>, Option<String>) {
    let now = current_unix_ms();
    let store = ProviderIdentityExhaustionStore::at_paths(
        paths.provider_identity_exhaustion_file(),
        paths.provider_identity_exhaustion_lock_file(),
    );
    let ledger = store.load().unwrap_or_default();
    let last_known_exhausted_at = |name: &ProfileName| -> Option<u64> {
        registered
            .iter()
            .find(|profile| &profile.name == name)
            .and_then(|profile| {
                ledger
                    .record_for(profile.provider, &profile.expected_identity.stable_id, now)
                    .map(|record| record.observed_unix_ms)
            })
    };

    let candidates = auto_handoff::hierarchy_without(preferences, owner, |name| {
        registered.iter().any(|profile| &profile.name == name)
    });
    let mut eligible = candidates.into_iter().filter(|name| {
        registered
            .iter()
            .find(|profile| &profile.name == *name)
            .is_some_and(|profile| profile.enabled)
            && last_known_exhausted_at(name).is_none()
    });
    let next = eligible
        .next()
        .and_then(|name| registered.iter().find(|profile| &profile.name == name))
        .map(|profile| format!("{} · {}", profile.name, provider_label(profile.provider)));
    let then = eligible
        .next()
        .and_then(|name| registered.iter().find(|profile| &profile.name == name))
        .map(|profile| format!("{} · {}", profile.name, provider_label(profile.provider)));

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

    let message = match last_known_exhausted_at(owner) {
        Some(observed_unix_ms) => format!(
            "Last known: exhausted (observed {}) — run `relay status --live` to check for a fresh reset.",
            sessions::ago(observed_unix_ms, now)
        ),
        None => "Last known: not exhausted — run `relay status --live` to verify current usage."
            .to_owned(),
    };
    (true, message, next, then)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core::{
        Availability, AvailabilityObservation, IdentityMetadata, ProfileOrigin, ProviderKind,
        automation::{ProviderIdentityExhaustionLedger, ProviderIdentityExhaustionRecord},
        usage::UsageEvidence,
    };

    #[test]
    fn profile_table_and_recovery_are_separate_and_keep_machine_states() {
        let rows = [
            json!({"name":"main","provider":"claude","role":"primary","authentication":"needs login","detail":null,"recovery_command":"relay login main"}),
            json!({"name":"codex-main","provider":"codex","role":"fallback","authentication":"needs login","detail":"authentication_revoked: HTTP 401; token_revoked\nRun:\nrelay logout codex-main\nrelay login codex-main","recovery_command":"relay logout codex-main\nrelay login codex-main"}),
            json!({"name":"cx","provider":"codex","role":"unassigned","authentication":"authenticated","detail":null,"recovery_command":null}),
        ];
        let refs: Vec<_> = rows.iter().collect();
        let human = profiles_human_at_width(&refs, 80);
        let table = human.split("\n\nAuthentication only").next().unwrap();
        assert_eq!(
            table
                .lines()
                .filter(|l| l.starts_with("main ")
                    || l.starts_with("codex-main ")
                    || l.starts_with("cx "))
                .count(),
            3
        );
        assert!(!table.contains("relay login") && !table.contains("HTTP 401"));
        assert!(
            table.contains("Credentials revoked")
                && table.contains("unassigned")
                && table.contains("Authenticated")
        );
        assert!(human.contains("Recovery\n  main\n    relay login main"));
        assert!(
            human.contains("  codex-main\n    relay logout codex-main\n    relay login codex-main")
        );
        assert_eq!(rows[2]["authentication"], "authenticated");
        assert!(profiles_human_at_width(&[], 80).contains("relay setup"));
        for width in [40, 60, 80] {
            let text = profiles_human_at_width(&refs, width);
            let table = text.split("\n\nAuthentication only").next().unwrap();
            assert!(table.lines().all(|l| l.chars().count() <= width));
        }
    }

    #[test]
    fn profile_health_never_promotes_unchecked_or_warning_evidence() {
        let readiness = readiness::Readiness {
            checks: vec![readiness::Check {
                label: "main (Claude) authenticated".to_owned(),
                level: readiness::Level::Warning,
                detail: Some("main is logged out".to_owned()),
                remedy: Some("relay login main".to_owned()),
            }],
        };
        assert_eq!(profile_health("main", &readiness, false), "not checked");
        assert_eq!(profile_health("main", &readiness, true), "! Needs login");
        assert_eq!(profile_health("cx", &readiness, true), "not checked");
    }

    fn fake_profile(name: &str, provider: ProviderKind, stable_id: &str) -> Profile {
        Profile {
            name: ProfileName::new(name).expect("name"),
            provider,
            config_dir: PathBuf::from("/tmp/does-not-need-to-exist"),
            enabled: true,
            origin: ProfileOrigin::Created,
            expected_identity: IdentityMetadata {
                stable_id: stable_id.to_owned(),
                display_label: None,
            },
            last_availability: AvailabilityObservation {
                state: Availability::Unknown,
                source: "test".to_owned(),
                observed_unix_ms: 0,
                reset_unix_ms: None,
            },
            claude_config_mode: None,
        }
    }

    fn paths_with_ledger(
        records: Vec<ProviderIdentityExhaustionRecord>,
    ) -> (tempfile::TempDir, RelayPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RelayPaths::new(dir.path().join("config"), dir.path().join("state"))
            .expect("relay paths");
        std::fs::create_dir_all(paths.state_root()).expect("state root");
        let ledger = ProviderIdentityExhaustionLedger {
            version: 1,
            records,
        };
        std::fs::write(
            paths.provider_identity_exhaustion_file(),
            serde_json::to_string_pretty(&ledger).expect("serialize ledger"),
        )
        .expect("write ledger");
        (dir, paths)
    }

    fn exhausted_record(
        profile: &Profile,
        observed_unix_ms: u64,
    ) -> ProviderIdentityExhaustionRecord {
        ProviderIdentityExhaustionRecord {
            provider: profile.provider,
            stable_identity: profile.expected_identity.stable_id.clone(),
            observed_unix_ms,
            exhausted_until_unix_ms: u64::MAX, // never expires within this test's lifetime
            evidence: UsageEvidence::Simulated,
            detected_via: "test".to_owned(),
        }
    }

    /// `relay status`'s fast default path must never claim a fresh check happened: with no
    /// durable exhaustion record for the owner, it says plainly that this is last-known,
    /// unverified state, not "available" or "ready" as though a live check confirmed it.
    #[test]
    fn local_only_summary_is_honest_about_no_known_exhaustion() {
        let owner = fake_profile("owner", ProviderKind::Fake, "id-owner");
        let fallback = fake_profile("fallback", ProviderKind::Fake, "id-fallback");
        let (_dir, paths) = paths_with_ledger(vec![]);
        let preferences = preferences::Preferences {
            primary_profile: Some(owner.name.clone()),
            fallback_profiles: vec![fallback.name.clone()],
            ..Default::default()
        };
        let registered = vec![owner.clone(), fallback.clone()];

        let (ready, message, next, then) = local_only_handoff_summary(
            &registered,
            &preferences,
            &owner.name,
            &paths,
            &readiness::Readiness::default(),
        );

        assert!(ready);
        assert!(
            message.contains("Last known: not exhausted") && message.contains("--live"),
            "must not claim freshness it doesn't have: {message}"
        );
        assert_eq!(next, Some("fallback · Claude".to_owned()));
        assert_eq!(then, None);
    }

    /// A durable exhaustion record already on disk (written by real handoff activity, per
    /// `docs/automatic-handoff.md`) is exactly the kind of local, timestamped evidence the fast
    /// path is allowed to use — and it must label it as observed, not live.
    #[test]
    fn local_only_summary_surfaces_durable_exhaustion_as_last_known() {
        let owner = fake_profile("owner", ProviderKind::Fake, "id-owner");
        let fallback = fake_profile("fallback", ProviderKind::Fake, "id-fallback");
        let now = crate::util::current_unix_ms();
        let observed = now - 3 * 60 * 1000; // 3 minutes ago
        let (_dir, paths) = paths_with_ledger(vec![exhausted_record(&owner, observed)]);
        let preferences = preferences::Preferences {
            primary_profile: Some(owner.name.clone()),
            fallback_profiles: vec![fallback.name.clone()],
            ..Default::default()
        };
        let registered = vec![owner.clone(), fallback.clone()];

        let (ready, message, next, _then) = local_only_handoff_summary(
            &registered,
            &preferences,
            &owner.name,
            &paths,
            &readiness::Readiness::default(),
        );

        assert!(ready);
        assert!(
            message.contains("Last known: exhausted") && message.contains("observed"),
            "must surface the durable record, labeled as an observation: {message}"
        );
        // The fallback is not itself exhausted, so it remains eligible.
        assert_eq!(next, Some("fallback · Claude".to_owned()));
    }

    /// A candidate that is ITSELF durably known-exhausted must never be offered as "Next" — that
    /// would tell the user Relay would hand off to an account it already knows is out of quota.
    #[test]
    fn local_only_summary_never_offers_a_durably_exhausted_fallback() {
        let owner = fake_profile("owner", ProviderKind::Fake, "id-owner");
        let fallback = fake_profile("fallback", ProviderKind::Fake, "id-fallback");
        let healthy = fake_profile("healthy", ProviderKind::Fake, "id-healthy");
        let now = crate::util::current_unix_ms();
        let (_dir, paths) = paths_with_ledger(vec![exhausted_record(&fallback, now)]);
        let preferences = preferences::Preferences {
            primary_profile: Some(owner.name.clone()),
            fallback_profiles: vec![fallback.name.clone(), healthy.name.clone()],
            ..Default::default()
        };
        let registered = vec![owner.clone(), fallback.clone(), healthy.clone()];

        let (_ready, _message, next, then) = local_only_handoff_summary(
            &registered,
            &preferences,
            &owner.name,
            &paths,
            &readiness::Readiness::default(),
        );

        assert_eq!(
            next,
            Some("healthy · Claude".to_owned()),
            "the exhausted fallback must be skipped"
        );
        assert_eq!(then, None);
    }

    /// A blocking local readiness problem (e.g. project trust not accepted) must still be
    /// reported even on the fast path — the fast path trims *live provider calls*, not
    /// correctness.
    #[test]
    fn local_only_summary_still_reports_a_blocking_local_readiness_problem() {
        let owner = fake_profile("owner", ProviderKind::Fake, "id-owner");
        let (_dir, paths) = paths_with_ledger(vec![]);
        let preferences = preferences::Preferences {
            primary_profile: Some(owner.name.clone()),
            ..Default::default()
        };
        let registered = vec![owner.clone()];
        let mut blocking = readiness::Readiness::default();
        blocking.checks.push(readiness::Check {
            label: "project trust accepted".to_owned(),
            level: readiness::Level::Blocking,
            detail: Some("not recorded for this exact directory".to_owned()),
            remedy: None,
        });

        let (ready, message, _next, _then) =
            local_only_handoff_summary(&registered, &preferences, &owner.name, &paths, &blocking);

        assert!(!ready);
        assert!(message.contains("not recorded for this exact directory"));
    }
}
