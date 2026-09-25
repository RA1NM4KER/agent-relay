//! `relay why`: explain, in plain language, Relay's current automatic-handoff decision — built
//! on the exact same pure [`relay_core::automation::explain`] function automatic handoff itself
//! evaluates with, over the same durable ledger/usage state. Never guesses beyond what that
//! state actually shows.

use std::path::Path;

use relay_core::{
    Error, Profile, ProfileService, ProviderKind, RelayPaths,
    automation::{
        AutomationDecision, AutomationLedger, AutomationPolicy, CandidateExplanation, Explanation,
        LedgerStore, ProfileCandidate, WhyCategory, explain, recovery_pending,
    },
    handoff::LeaseStore,
};
use serde_json::json;

use crate::{
    auth::{apply_provider_exhaustion, doctor_is_healthy},
    auto_handoff,
    cli::WhyArgs,
    output::{CommandOutput, success},
    preferences, providers, sessions,
    util::current_unix_ms,
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    args: &WhyArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    let project_dir = match &args.project_dir {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|source| Error::Io {
            path: std::path::PathBuf::from("."),
            source,
        })?,
    };
    let canonical_project = std::fs::canonicalize(&project_dir).map_err(|source| Error::Io {
        path: project_dir.clone(),
        source,
    })?;
    let registered = service.list()?;
    let executables = providers::ExecutableOverrides {
        claude: args.claude_executable.clone(),
        codex: args.codex_executable.clone(),
    };
    let preferences = preferences::Preferences::load(paths.config_root())?.unwrap_or_default();

    let store = sessions::open_store(paths, &canonical_project)?;
    let views = sessions::reconcile(paths, &canonical_project, &registered, &executables)?;
    let chosen = sessions::choose_session(
        &store,
        &views,
        &registered,
        sessions::Want::Switchable,
        args.session.as_deref(),
        None,
        json_mode,
    )?;
    let source_name = chosen.profile().clone();
    let source = registered
        .iter()
        .find(|profile| profile.name == source_name)
        .ok_or_else(|| Error::ProfileNotFound(source_name.to_string()))?;
    let session_id = chosen
        .native_session_id()
        .map(str::to_owned)
        .unwrap_or_default();
    let session_dir = store.session_dir(&chosen.record.relay_session_id);
    let lease_held = LeaseStore::at_path(session_dir.join("lease.json"))
        .load()
        .ok()
        .flatten()
        .is_some();

    let fallback_names = auto_handoff::hierarchy_without(&preferences, &source.name, |name| {
        registered.iter().any(|profile| &profile.name == name)
    });
    let fallback_profiles: Vec<&Profile> = fallback_names
        .into_iter()
        .filter_map(|name| registered.iter().find(|profile| &profile.name == name))
        .collect();

    let signal_for = |profile: &Profile| {
        providers::usage_signal_for(
            profile.provider,
            &executables,
            false,
            None,
            profile.effective_claude_config_mode(),
        )
    };
    let now = current_unix_ms();
    let source_usage = apply_provider_exhaustion(
        paths,
        source,
        &executables,
        signal_for(source).detect(&source.config_dir, &canonical_project, &session_id)?,
        now,
    )?;
    let source_candidate = ProfileCandidate {
        name: source.name.clone(),
        provider: source.provider,
        config_dir: source.config_dir.clone(),
        claude_config_mode: Some(source.effective_claude_config_mode()),
        identity_stable_id: Some(source.expected_identity.stable_id.clone()),
        enabled: source.enabled,
        healthy: doctor_is_healthy(service, source, &executables)?,
        usage: source_usage,
    };
    let fallback_candidates = fallback_profiles
        .iter()
        .map(|profile| -> Result<ProfileCandidate, Error> {
            let usage = apply_provider_exhaustion(
                paths,
                profile,
                &executables,
                signal_for(profile).detect(&profile.config_dir, &canonical_project, &session_id)?,
                now,
            )?;
            Ok(ProfileCandidate {
                name: profile.name.clone(),
                provider: profile.provider,
                config_dir: profile.config_dir.clone(),
                claude_config_mode: Some(profile.effective_claude_config_mode()),
                identity_stable_id: Some(profile.expected_identity.stable_id.clone()),
                enabled: profile.enabled,
                healthy: doctor_is_healthy(service, profile, &executables)?,
                usage,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let ledger = LedgerStore::at_path(session_dir.join("automation_state.json")).load()?;
    let automatic_handoff_enabled = preferences.usage_integration_enabled == Some(true);

    let explanation = explain(
        now,
        &source_candidate,
        &fallback_candidates,
        &ledger,
        &AutomationPolicy::default(),
    );
    let category = resolve_category(
        &explanation,
        &ledger,
        &session_dir,
        automatic_handoff_enabled,
    );

    let human = render_human(
        source,
        &session_id,
        now,
        &source_candidate,
        &explanation.decision,
        category,
        &explanation.candidates,
        lease_held,
        automatic_handoff_enabled,
    );
    let data = json!({
        "relay_session_id": chosen.record.relay_session_id.as_str(),
        "current_owner": source.name.as_str(),
        "current_provider": provider_label(source.provider),
        "source_usage": format!("{:?}", source_candidate.usage.state),
        "reset_unix_ms": source_candidate.usage.reset_unix_ms,
        "category": category,
        "decision": explanation.decision,
        "automatic_handoff_enabled": automatic_handoff_enabled,
        "candidates": explanation.candidates,
    });
    success("why", human, data)
}

/// Grounds `explain()`'s category in durable state it doesn't itself see: a still-open handoff
/// transaction (which blocks everything until `relay recover` runs) and a cooldown that was
/// opened by a failed attempt rather than a successful one, before falling back to the
/// preference-driven `AutomaticHandoffDisabled` override.
///
/// Also used by the in-agent `/relay:why` (and legacy) hook, so the terminal and in-session
/// answers can never disagree on which category applies.
pub(crate) fn resolve_category(
    explanation: &Explanation,
    ledger: &AutomationLedger,
    session_dir: &Path,
    automatic_handoff_enabled: bool,
) -> WhyCategory {
    if recovery_pending(session_dir) {
        return WhyCategory::RecoveryRequired;
    }
    if matches!(
        explanation.decision,
        AutomationDecision::CooldownActive { .. }
    ) && ledger
        .recent_handoffs
        .last()
        .is_some_and(|event| event.transaction_id.is_none())
    {
        return WhyCategory::HandoffFailed;
    }
    if !automatic_handoff_enabled && explanation.category != WhyCategory::ReadyToHandoff {
        return WhyCategory::AutomaticHandoffDisabled;
    }
    explanation.category
}

fn provider_label(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude | ProviderKind::Fake => "Claude",
    }
}

/// A short, dependency-free relative-time phrase (no calendar/timezone crate in this workspace).
fn format_relative(now_ms: u64, target_ms: u64) -> String {
    if target_ms <= now_ms {
        return "any moment now".to_owned();
    }
    let secs = (target_ms - now_ms) / 1000;
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("in {days}d {hours}h")
    } else if hours > 0 {
        format!("in {hours}h {minutes}m")
    } else {
        format!("in {minutes}m")
    }
}

/// Also used by the in-agent `/relay:why` (and legacy) hook, so the terminal and in-session
/// answers can never read differently for the same durable state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_human(
    source: &Profile,
    session_id: &str,
    now: u64,
    source_candidate: &ProfileCandidate,
    decision: &AutomationDecision,
    category: WhyCategory,
    candidates: &[CandidateExplanation],
    lease_held: bool,
    automatic_handoff_enabled: bool,
) -> String {
    let short_session = session_id.get(..8).unwrap_or(session_id);
    let mut lines = vec![
        "Why hasn't Relay switched?".to_owned(),
        String::new(),
        format!(
            "Current owner: {} · {}{}",
            source.name,
            provider_label(source.provider),
            if lease_held { "" } else { " (dormant)" }
        ),
        format!("Session: {short_session}"),
    ];
    let usage_line = |state_desc: &str| {
        source_candidate.usage.reset_unix_ms.map_or_else(
            || format!("Usage: {state_desc}"),
            |reset| format!("Usage: {state_desc} — {}", format_relative(now, reset)),
        )
    };
    match category {
        WhyCategory::ReadyToHandoff => {
            lines.push(usage_line("exhausted"));
            if let AutomationDecision::Handoff { target } = decision {
                lines.push(format!("Next eligible profile: {target}"));
                lines.push("Relay will hand off to it on the next evaluation.".to_owned());
            } else {
                lines.push("Nothing is blocking a handoff right now.".to_owned());
            }
        }
        WhyCategory::SourceNotExhausted => {
            lines.push("Usage: available".to_owned());
            lines.push(format!(
                "{} still has capacity, so there is nothing to hand off.",
                source.name
            ));
        }
        WhyCategory::SourceUsageUnknown => {
            lines.push("Usage: could not be verified".to_owned());
            lines.push(
                "Relay never moves work on an unverifiable usage reading — it fails closed."
                    .to_owned(),
            );
        }
        WhyCategory::CooldownActive => {
            lines.push(usage_line("exhausted"));
            lines
                .push("A recent automatic handoff is still inside its cooldown window.".to_owned());
            lines.push("Relay will re-evaluate once it passes.".to_owned());
        }
        WhyCategory::NoEligibleFallback => {
            lines.push(usage_line("exhausted"));
            lines.push("No eligible fallback profile right now:".to_owned());
            for candidate in candidates {
                if let Some(reason) = &candidate.reason {
                    lines.push(format!(
                        "  {} — {}",
                        candidate.name,
                        phrase(*reason, &candidate.detail)
                    ));
                }
            }
            if candidates.is_empty() {
                lines.push("  (no fallback profiles are configured)".to_owned());
            }
        }
        WhyCategory::AutomaticHandoffDisabled => {
            lines.push(usage_line(&format!("{:?}", source_candidate.usage.state)));
            lines.push(
                "Automatic handoff is not enabled for this profile (see `relay doctor`)."
                    .to_owned(),
            );
        }
        WhyCategory::StickyCurrentOwner => {
            lines.push("Usage: available".to_owned());
            lines.push(format!(
                "{} has capacity again, but Relay does not automatically fail back.",
                source.name
            ));
            lines.push(
                "The current owner stays sticky until another handoff is needed or you switch \
                 manually."
                    .to_owned(),
            );
        }
        WhyCategory::HandoffFailed => {
            lines.push(usage_line("exhausted"));
            lines.push("Automatic handoff failed".to_owned());
            lines.push(
                "The most recent automatic attempt did not complete; Relay is inside its \
                 cooldown before it will try again."
                    .to_owned(),
            );
        }
        WhyCategory::RecoveryRequired => {
            lines.push(usage_line("exhausted"));
            lines.push(
                "A prior handoff transaction is still unresolved. Run `relay recover` before \
                 automatic handoff can continue."
                    .to_owned(),
            );
        }
        // Reached only via a candidate's own per-target reason (`candidates`), never as the
        // top-level category `explain` returns — a single candidate being exhausted/unhealthy/an
        // identity conflict is folded into `NoEligibleFallback` above when no candidate is left.
        WhyCategory::TargetExhausted
        | WhyCategory::TargetUnhealthy
        | WhyCategory::TargetIdentityConflict => {
            lines.push(usage_line("exhausted"));
            lines.push(format!("{category:?}"));
        }
    }
    if !automatic_handoff_enabled && category != WhyCategory::AutomaticHandoffDisabled {
        lines.push(String::new());
        lines.push(
            "Note: automatic handoff is disabled in preferences; only a manual `relay switch` \
             will move this conversation."
                .to_owned(),
        );
    }
    lines.join("\n")
}

fn phrase(reason: WhyCategory, detail: &str) -> String {
    match reason {
        WhyCategory::TargetExhausted => "exhausted".to_owned(),
        WhyCategory::TargetUnhealthy => detail.to_owned(),
        WhyCategory::TargetIdentityConflict => "same account as the current owner".to_owned(),
        _ => detail.to_owned(),
    }
}
