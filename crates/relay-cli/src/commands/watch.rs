//! `relay watch <subcommand>`: evaluate (and, only if exhausted, perform) automatic handoff
//! for the current writer, by hand or from the `StopFailure` hook's own retry loop
//! (`watch auto`), plus read-only ledger inspection (`status`/`clear`).

use std::{ffi::OsString, path::Path};

use clap::Parser as _;
use relay_core::{
    Error, Profile, ProfileName, ProfileService, ProviderKind, RelayPaths,
    automation::{
        AutomationPolicy, LedgerStore, ProfileCandidate, RejectedCandidate, WatchCoordinator,
        WatchOutcome, WatchRequest,
    },
    handoff::{HandoffCoordinator, ProjectId},
    usage::{UsageSignal, UsageState},
};
use relay_provider_claude::{CapabilityStatus, SimulatedUsageSignal, assess_installed};
use serde::Serialize;
use serde_json::json;

use crate::{
    auth::{apply_provider_exhaustion, fallback_is_healthy, verified_current_stable_identity},
    auto_handoff,
    cli::{Cli, WatchArgs, WatchCommand},
    output::{CommandOutput, success},
    progress, providers, sessions,
    util::current_unix_ms,
};

pub(crate) fn run(
    service: &ProfileService,
    paths: &RelayPaths,
    watch: &WatchArgs,
    cli: &Cli,
) -> Result<CommandOutput, Error> {
    match &watch.command {
        WatchCommand::Run {
            profile,
            fallback,
            project_dir,
            session_id,
            dry_run,
            probe,
            workload_model,
            simulate_usage,
            simulate_reset_unix_ms,
            claude_executable,
        } => {
            let registered = service.list()?;
            let source = registered
                .iter()
                .find(|candidate| &candidate.name == profile)
                .ok_or_else(|| Error::ProfileNotFound(profile.to_string()))?;
            let fallback_profiles: Vec<&Profile> = fallback
                .iter()
                .map(|name| {
                    registered
                        .iter()
                        .find(|candidate| &candidate.name == name)
                        .ok_or_else(|| Error::ProfileNotFound(name.to_string()))
                })
                .collect::<Result<_, Error>>()?;

            let executables = providers::ExecutableOverrides {
                claude: claude_executable.clone(),
                codex: None,
            };
            // M6: one coordinator per (source_profile, target_profile) pair this round could
            // possibly select, each built from its own two profiles' registered providers.
            // `decide()` itself stays fully provider-neutral (see relay_core::automation);
            // this is the only place that picks a concrete mix of adapters. Every
            // `ProviderPorts` used by a coordinator must outlive `watch.evaluate(..)` below,
            // so they are all collected up front rather than built lazily per decision.
            let mut all_ports: std::collections::BTreeMap<ProfileName, providers::ProviderPorts> =
                std::collections::BTreeMap::new();
            for profile in std::iter::once(source).chain(fallback_profiles.iter().copied()) {
                all_ports.entry(profile.name.clone()).or_insert_with(|| {
                    providers::ports_for(
                        profile.provider,
                        &executables,
                        profile.effective_claude_config_mode(),
                    )
                });
            }

            let coordinators: std::collections::BTreeMap<
                (ProfileName, ProfileName),
                HandoffCoordinator<'_>,
            > = {
                let mut map = std::collections::BTreeMap::new();
                for target_profile in
                    std::iter::once(source).chain(fallback_profiles.iter().copied())
                {
                    if target_profile.name == source.name {
                        continue;
                    }
                    let continuity_type = relay_core::handoff::ContinuityType::for_transition(
                        source.provider,
                        target_profile.provider,
                    );
                    let source_ports = all_ports.get(&source.name).expect("inserted above");
                    let target_ports = all_ports.get(&target_profile.name).expect("inserted above");
                    map.insert(
                        (source.name.clone(), target_profile.name.clone()),
                        HandoffCoordinator {
                            paths,
                            liveness: source_ports.liveness.as_ref(),
                            source_stopper: source_ports.stopper.as_ref(),
                            target_stopper: target_ports.stopper.as_ref(),
                            stager: match continuity_type {
                                relay_core::handoff::ContinuityType::SessionContinuation => {
                                    source_ports.stager.as_deref()
                                }
                                _ => None,
                            },
                            context_capturer: match continuity_type {
                                relay_core::handoff::ContinuityType::StateContinuation => {
                                    Some(source_ports.context_capturer.as_ref())
                                }
                                _ => None,
                            },
                            launcher: target_ports.launcher.as_ref(),
                        },
                    );
                }
                map
            };
            let resolve_coordinator = |from: &ProfileName, to: &ProfileName| {
                coordinators
                    .get(&(from.clone(), to.clone()))
                    .expect("decide() only selects a name present in fallbacks")
            };
            let watch = WatchCoordinator {
                paths,
                handoff_for: &resolve_coordinator,
                policy: AutomationPolicy::default(),
            };

            let canonical_project =
                std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                    path: project_dir.clone(),
                    source,
                })?;
            // The evaluation belongs to ONE Relay session: the one holding this native
            // conversation. Its ledger, cooldown, journals and lock are its own.
            let project_state_dir =
                sessions::session_dir_for_native(paths, &canonical_project, session_id)?
                    .ok_or_else(|| Error::RelaySessionNotFound(session_id.clone()))?;

            // Startup recovery comes first: nothing below (usage detection, and above all a
            // possible probe request) runs while an earlier transaction is unresolved.
            if let Some(outcome) = watch.recover_pending(&project_state_dir, *dry_run)? {
                return watch_run_output(outcome);
            }

            // Version/capability gate: fail closed when a capability the handoff machinery
            // depends on cannot be verified; merely-unverified newer versions only warn. Only
            // meaningful for a Claude source (this is Claude's own capability-probing
            // machinery); Codex's separate version gate lives in
            // relay_provider_codex::assess_version and is checked inside the adapter itself.
            if source.provider == ProviderKind::Claude {
                let capabilities = assess_installed(
                    claude_executable.as_deref(),
                    &source.config_dir,
                    source.effective_claude_config_mode(),
                )?;
                for required in [
                    relay_provider_claude::Capability::AgentsJsonShape,
                    relay_provider_claude::Capability::TranscriptLayout,
                    relay_provider_claude::Capability::AuthStatusSchema,
                ] {
                    if capabilities.status_of(required) == CapabilityStatus::Unsupported {
                        return Err(Error::UnsupportedProviderVersion);
                    }
                }
                if !cli.json
                    && capabilities
                        .entries
                        .iter()
                        .any(|entry| entry.status == CapabilityStatus::Unverified)
                {
                    // Live-found (M4.12): this must never print in --json mode. `--json`'s
                    // contract is that stderr on a failed invocation is exactly the stable
                    // error envelope and nothing else; an extra human-readable line ahead of
                    // it breaks every machine consumer that parses stderr as JSON on failure
                    // (relay-herdr's own client included — this is exactly what caught it).
                    eprintln!(
                        "warning: Claude Code {} has not been validated by Relay; usage \
                             detection stays fail-closed, but re-validate before trusting handoffs",
                        capabilities.version
                    );
                }
            }

            let ledger =
                LedgerStore::at_path(project_state_dir.join("automation_state.json")).load()?;
            let now = current_unix_ms();
            // The probe spends real API usage: never against a profile already known
            // exhausted, and only when the operator explicitly opted in. Only meaningful for
            // Claude candidates; Codex's usage signal ignores the flag (see
            // relay_provider_codex::usage).
            let signal_for = |profile: &Profile| {
                providers::usage_signal_for(
                    profile.provider,
                    &executables,
                    *probe && !ledger.is_known_exhausted(&profile.name, now),
                    workload_model.clone(),
                    profile.effective_claude_config_mode(),
                )
            };

            let usage_progress =
                progress::Progress::start("Checking usage and fallbacks…", cli.json);
            trace_preflight("source_usage_check_started", &source.name, source.provider);
            // `--simulate-usage` fault-injects the SOURCE's own usage state only; fallback
            // candidates are always checked for real. A real (non-simulated) Codex source is read
            // directly rather than through the generic `UsageSignal` trait object so this, the
            // one authoritative structured read `watch run` makes for it, can also capture GitHub
            // #13's sanitized scheduling hint — never a second provider call purely to learn it.
            let mut source_max_used_percent: Option<u32> = None;
            let source_observation = match simulate_usage {
                Some(state) => SimulatedUsageSignal {
                    state: UsageState::from(*state),
                    reset_unix_ms: *simulate_reset_unix_ms,
                }
                .detect(&source.config_dir, project_dir, session_id)?,
                None if source.provider == ProviderKind::Codex => {
                    let reading = providers::codex_usage_reading(&executables, &source.config_dir);
                    source_max_used_percent = reading.max_used_percent;
                    reading.observation
                }
                None => signal_for(source).detect(&source.config_dir, project_dir, session_id)?,
            };
            // GitHub #13: the supervised terminal cannot make this read itself (it must stay
            // detached from the slow ~0.7-1.7s round trip) — this tiny, sanitized, bounded
            // diagnostic is how it learns what this poll found, to schedule the next one. Only
            // ever written for a Codex source: Claude stays fully event-driven and never polls.
            if source.provider == ProviderKind::Codex {
                crate::codex_poll::CodexPollDiagnostic::new(
                    source_observation.observed_unix_ms,
                    source_observation.state,
                    source_max_used_percent,
                )
                .write(&project_state_dir);
            }
            let source_usage =
                apply_provider_exhaustion(paths, source, &executables, source_observation, now)?;
            trace_preflight(
                "source_usage_check_completed",
                &source.name,
                source.provider,
            );
            if source_usage.state.is_blocking()
                && std::env::var_os("RELAY_AUTO_HANDOFF_TRACE").is_some()
            {
                eprintln!(
                    "[trace unix_ms={}] exhaustion_corroborated",
                    current_unix_ms()
                );
            }
            let fallback_candidates = fallback_profiles
                .iter()
                .map(|candidate| -> Result<ProfileCandidate, Error> {
                    trace_preflight(
                        "fallback_usage_check_started",
                        &candidate.name,
                        candidate.provider,
                    );
                    let usage = apply_provider_exhaustion(
                        paths,
                        candidate,
                        &executables,
                        signal_for(candidate).detect(
                            &candidate.config_dir,
                            project_dir,
                            session_id,
                        )?,
                        now,
                    )?;
                    trace_preflight(
                        "fallback_usage_check_completed",
                        &candidate.name,
                        candidate.provider,
                    );
                    trace_preflight(
                        "fallback_health_check_started",
                        &candidate.name,
                        candidate.provider,
                    );
                    let healthy = fallback_is_healthy(service, candidate, &executables, &usage)?;
                    trace_preflight(
                        "fallback_health_check_completed",
                        &candidate.name,
                        candidate.provider,
                    );
                    Ok(ProfileCandidate {
                        name: candidate.name.clone(),
                        provider: candidate.provider,
                        config_dir: candidate.config_dir.clone(),
                        identity_stable_id: Some(candidate.expected_identity.stable_id.clone()),
                        enabled: candidate.enabled,
                        healthy,
                        usage,
                        claude_config_mode: Some(candidate.effective_claude_config_mode()),
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            usage_progress.set_label("Evaluating handoff…");
            trace_preflight(
                "candidate_filtering_target_selection_started",
                &source.name,
                source.provider,
            );

            let outcome = watch.evaluate(
                WatchRequest {
                    project_dir: project_dir.clone(),
                    source_profile: source.name.clone(),
                    source_provider: source.provider,
                    source_config_dir: source.config_dir.clone(),
                    source_identity_stable_id: Some(source.expected_identity.stable_id.clone()),
                    session_id: session_id.clone(),
                    source_usage,
                    fallbacks: fallback_candidates,
                    dry_run: *dry_run,
                    state_dir: Some(project_state_dir.clone()),
                    source_claude_mode: Some(source.effective_claude_config_mode()),
                },
                current_unix_ms(),
            )?;
            usage_progress.finish();

            watch_run_output(outcome)
        }
        WatchCommand::Auto {
            profile,
            fallback,
            project_dir,
            session_id,
            attempts,
            interval_ms,
        } => run_watch_auto(
            cli,
            profile,
            fallback,
            project_dir,
            session_id,
            *attempts,
            *interval_ms,
        ),
        WatchCommand::Status { project_dir } => {
            let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                path: project_dir.clone(),
                source,
            })?;
            let store = sessions::open_store(paths, &canonical)?;
            let views = store.list()?;
            let mut human = format!("Project: {}", canonical.display());
            let mut rows = Vec::new();
            for view in &views {
                let dir = store.session_dir(&view.record.relay_session_id);
                let ledger = LedgerStore::at_path(dir.join("automation_state.json")).load()?;
                human.push_str(&format!(
                    "\nSession {} ({:?}, {}): known-exhausted: {}; recent automatic handoffs: {}",
                    view.record.relay_session_id.short(),
                    view.state(),
                    view.profile(),
                    if ledger.known_exhausted.is_empty() {
                        "none".to_owned()
                    } else {
                        ledger
                            .known_exhausted
                            .iter()
                            .map(|record| record.profile.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    ledger.recent_handoffs.len()
                ));
                rows.push(json!({
                    "relay_session_id": view.record.relay_session_id.as_str(),
                    "state": view.state(),
                    "lease": view.lease,
                    "ledger": ledger,
                }));
            }
            // With exactly one session the old top-level `lease`/`ledger` keys stay populated.
            let only = (rows.len() == 1).then(|| rows[0].clone());
            success(
                "watch.status",
                human,
                json!({
                    "lease": only.as_ref().map(|row| row["lease"].clone()),
                    "ledger": only.as_ref().map(|row| row["ledger"].clone()),
                    "sessions": rows,
                }),
            )
        }
        WatchCommand::Clear {
            project_dir,
            provider_account,
        } => {
            let canonical = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
                path: project_dir.clone(),
                source,
            })?;
            let project_id = ProjectId::for_canonical_path(&canonical)?;
            let store = sessions::open_store(paths, &canonical)?;
            for view in store.list()? {
                let dir = store.session_dir(&view.record.relay_session_id);
                LedgerStore::at_path(dir.join("automation_state.json")).clear()?;
            }
            if let Some(name) = provider_account {
                let profile = service
                    .list()?
                    .into_iter()
                    .find(|profile| &profile.name == name)
                    .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
                let executables = providers::ExecutableOverrides::default();
                let stable_identity = verified_current_stable_identity(&profile, &executables)?
                    .ok_or(Error::IdentityUnavailable)?;
                relay_core::automation::ProviderIdentityExhaustionStore::at_paths(
                    paths.provider_identity_exhaustion_file(),
                    paths.provider_identity_exhaustion_lock_file(),
                )
                .clear_identity(
                    profile.provider,
                    &stable_identity,
                    current_unix_ms(),
                )?;
            }
            success(
                "watch.clear",
                format!(
                    "Cleared automation ledgers for {}{}",
                    canonical.display(),
                    provider_account
                        .as_ref()
                        .map_or_else(String::new, |name| format!(
                            " and the durable provider-account exhaustion record for '{name}'"
                        ))
                ),
                json!({
                    "project_id": project_id.as_str(),
                    "provider_account_cleared": provider_account.as_ref().map(ProfileName::as_str),
                }),
            )
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum WatchRunOutput {
    NoActionNeeded {
        source_usage: String,
    },
    WaitingForCapacity {
        reason: String,
        rejected: Vec<RejectedCandidate>,
        newly_reported: bool,
    },
    CooldownActive {
        retry_after_unix_ms: u64,
    },
    LoopPrevented {
        reason: String,
    },
    DryRunWouldHandoff {
        target: ProfileName,
    },
    Handoff {
        target: ProfileName,
        journal: Box<relay_core::handoff::HandoffJournal>,
    },
    Recovered {
        transactions: Vec<String>,
    },
    TransactionInFlight {
        transaction_id: String,
    },
}

/// `relay watch auto`: `watch run`'s evaluation, retried a bounded number of times while it keeps
/// answering "no action needed". Every attempt is a full, ordinary `watch run` (so recovery,
/// cooldown, the per-window cap, the known-exhausted ledger and the orchestration lock all apply
/// exactly as they do for a manual run); this only decides whether to ask again.
fn run_watch_auto(
    cli: &Cli,
    profile: &ProfileName,
    fallback: &[ProfileName],
    project_dir: &Path,
    session_id: &str,
    attempts: u32,
    interval_ms: u64,
) -> Result<CommandOutput, Error> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        if attempt == 1 && std::env::var_os("RELAY_AUTO_HANDOFF_TRACE").is_some() {
            eprintln!(
                "[trace unix_ms={}] first_auto_watch_evaluation",
                current_unix_ms()
            );
        }
        let mut argv: Vec<OsString> = vec!["relay".into(), "--json".into()];
        if let Some(root) = &cli.config_root {
            argv.extend(["--config-root".into(), root.clone().into_os_string()]);
        }
        if let Some(root) = &cli.state_root {
            argv.extend(["--state-root".into(), root.clone().into_os_string()]);
        }
        argv.extend([
            "watch".into(),
            "run".into(),
            "--profile".into(),
            profile.as_str().into(),
        ]);
        for name in fallback {
            argv.extend(["--fallback".into(), name.as_str().into()]);
        }
        argv.extend([
            "--project".into(),
            project_dir.as_os_str().to_owned(),
            "--session".into(),
            session_id.into(),
        ]);
        let inner = Cli::try_parse_from(argv).map_err(|_| Error::ProviderUnsupported)?;
        let output = crate::commands::dispatch(&inner)?;
        let outcome = output.json["data"]["outcome"].as_str().unwrap_or_default();
        // Each attempt is logged as it happens (stderr is the triggered run's log file), so
        // "did it fire and what did it decide" is answerable while the retries are still going —
        // except an unchanged "no eligible fallback" standoff, already reported once, which would
        // otherwise repeat this identical block on every detached poll (every `RELAY_CODEX_POLL_SECS`
        // for as long as the standoff lasts).
        if outcome == "waiting_for_capacity" && output.json["data"]["newly_reported"] == false {
            eprintln!("[attempt {attempt}/{attempts}] (unchanged) waiting for capacity");
        } else {
            eprintln!("[attempt {attempt}/{attempts}] {}", output.human);
        }
        let undecided = outcome == "no_action_needed";
        if !undecided || attempt >= attempts {
            return Ok(output);
        }
        if let Some(delay) = retry_delay_after(attempt, attempts, interval_ms) {
            std::thread::sleep(delay);
        }
    }
}

/// The first three retries are deliberately short: Claude's statusline evidence often arrives a
/// moment after its StopFailure hook. Every later retry retains the existing long cadence.
fn retry_delay_after(
    attempt: u32,
    attempts: u32,
    long_interval_ms: u64,
) -> Option<std::time::Duration> {
    if attempt >= attempts {
        return None;
    }
    let delay_ms = usize::try_from(attempt.saturating_sub(1))
        .ok()
        .and_then(|index| auto_handoff::INITIAL_RETRY_DELAYS_MS.get(index).copied())
        .unwrap_or(long_interval_ms);
    Some(std::time::Duration::from_millis(delay_ms))
}

fn trace_preflight(event: &str, profile: &ProfileName, provider: ProviderKind) {
    if std::env::var_os("RELAY_AUTO_HANDOFF_TRACE").is_some() {
        eprintln!(
            "[trace unix_ms={}] {event} profile={profile} provider={provider}",
            current_unix_ms()
        );
    }
}

fn watch_run_output(outcome: WatchOutcome) -> Result<CommandOutput, Error> {
    let (human, data) = match outcome {
        WatchOutcome::NoActionNeeded { source_usage } => (
            format!("No action needed: source usage is {source_usage:?}"),
            WatchRunOutput::NoActionNeeded {
                source_usage: format!("{source_usage:?}"),
            },
        ),
        WatchOutcome::WaitingForCapacity {
            reason,
            rejected,
            newly_reported,
        } => {
            // Only the first cycle of a given standoff traces/prints — the caller (a fire-and-
            // forget periodic poll) would otherwise repeat this identical line every cycle.
            if newly_reported && std::env::var_os("RELAY_AUTO_HANDOFF_TRACE").is_some() {
                eprintln!(
                    "[trace unix_ms={}] no_eligible_fallback {}",
                    current_unix_ms(),
                    rejected
                        .iter()
                        .map(|candidate| format!("{}={}", candidate.name, candidate.detail))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let human = if rejected.is_empty() {
                format!("Waiting for capacity: {reason}")
            } else {
                let breakdown = rejected
                    .iter()
                    .map(|candidate| format!("  {}: {}", candidate.name, candidate.detail))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("Waiting for capacity: {reason}\n{breakdown}")
            };
            (
                human,
                WatchRunOutput::WaitingForCapacity {
                    reason,
                    rejected,
                    newly_reported,
                },
            )
        }
        WatchOutcome::CooldownActive {
            retry_after_unix_ms,
        } => (
            format!("Cooldown active; retry after unix_ms={retry_after_unix_ms}"),
            WatchRunOutput::CooldownActive {
                retry_after_unix_ms,
            },
        ),
        WatchOutcome::LoopPrevented { reason } => (
            format!("Loop prevented: {reason}"),
            WatchRunOutput::LoopPrevented { reason },
        ),
        WatchOutcome::DryRunWouldHandoff { target } => (
            format!("Dry run: would hand off to '{target}' (no mutation performed)"),
            WatchRunOutput::DryRunWouldHandoff { target },
        ),
        WatchOutcome::Recovered { transactions } => (
            format!(
                "Recovered {} incomplete transaction(s) from an earlier run; no new work was \
                 started this round. Run again to re-evaluate.\n{}",
                transactions.len(),
                transactions
                    .iter()
                    .map(|item| format!("  {} -> {}", item.transaction_id, item.final_state))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            WatchRunOutput::Recovered {
                transactions: transactions
                    .iter()
                    .map(|item| format!("{} -> {}", item.transaction_id, item.final_state))
                    .collect(),
            },
        ),
        WatchOutcome::RecoveryRequired {
            transaction_id,
            reason,
        } => {
            // A non-zero exit so a cron/shell loop notices; nothing new was started.
            return Err(Error::RecoveryRequired(format!(
                "{transaction_id}: {reason}. Run `relay recover {transaction_id} --project-dir <dir>`"
            )));
        }
        WatchOutcome::TransactionInFlight { transaction_id } => (
            format!(
                "Transaction {transaction_id} is in progress in another process; not starting new work"
            ),
            WatchRunOutput::TransactionInFlight { transaction_id },
        ),
        WatchOutcome::Handoff { journal, target } => (
            format!(
                "Automatic handoff to '{target}': {:?}\nTransaction: {}",
                journal.state, journal.transaction_id
            ),
            WatchRunOutput::Handoff { target, journal },
        ),
    };
    success("watch.run", human, data)
}

#[cfg(test)]
mod tests {
    use super::retry_delay_after;

    #[test]
    fn automatic_watch_uses_a_short_burst_then_the_existing_long_cadence() {
        let delay = |attempt| retry_delay_after(attempt, 10, 20_000).map(|value| value.as_millis());
        assert_eq!(delay(1), Some(300));
        assert_eq!(delay(2), Some(1_000));
        assert_eq!(delay(3), Some(2_000));
        assert_eq!(delay(4), Some(20_000));
        assert_eq!(delay(9), Some(20_000));
        assert_eq!(delay(10), None);
    }

    #[test]
    fn automatic_watch_never_sleeps_after_a_decisive_or_final_attempt() {
        assert_eq!(retry_delay_after(1, 1, 20_000), None);
    }
}
