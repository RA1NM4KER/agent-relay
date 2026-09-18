//! M2C: explicit, opt-in, usage-triggered automatic handoff. Reuses M2B's transactional
//! `HandoffCoordinator` unchanged for the actual transfer — this module only decides *whether*
//! and *to whom* to hand off, and durably remembers recent automatic activity so a noisy or
//! flapping usage signal can never thrash between profiles.
//!
//! Nothing here runs unless a caller explicitly invokes [`WatchCoordinator::evaluate`] — no
//! background polling, no implicit monitoring. `relay-cli`'s `relay watch run` is the only
//! caller, and only when the operator explicitly runs it.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    AtomicWrite, Error, FsAtomicWriter, ProfileName, Result,
    handoff::{HandoffCoordinator, HandoffJournal, HandoffRequest, OrchestrationLock, ProjectId},
    usage::{UsageEvidence, UsageObservation, UsageState},
};

/// One profile under consideration, with the caller-supplied health/identity facts and a fresh
/// usage observation. The caller (provider-aware code) is responsible for gathering these; this
/// module's decision logic is pure and provider-neutral.
#[derive(Clone, Debug)]
pub struct ProfileCandidate {
    pub name: ProfileName,
    pub config_dir: PathBuf,
    /// `None` when identity could not be established; never treated as a safe non-match.
    pub identity_stable_id: Option<String>,
    pub enabled: bool,
    pub healthy: bool,
    pub usage: UsageObservation,
}

#[derive(Clone, Copy, Debug)]
pub struct AutomationPolicy {
    pub cooldown_ms: u64,
    pub max_handoffs_per_window: usize,
    pub window_ms: u64,
}

impl Default for AutomationPolicy {
    fn default() -> Self {
        Self {
            cooldown_ms: 30_000,
            max_handoffs_per_window: 5,
            window_ms: 60 * 60 * 1000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomationDecision {
    NoActionNeeded,
    Handoff { target: ProfileName },
    WaitingForCapacity { reason: String },
    CooldownActive { retry_after_unix_ms: u64 },
    LoopPrevented { reason: String },
}

const LEDGER_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationEvent {
    pub unix_ms: u64,
    pub source: ProfileName,
    pub target: ProfileName,
    pub transaction_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExhaustedRecord {
    pub profile: ProfileName,
    pub observed_unix_ms: u64,
    pub reset_unix_ms: Option<u64>,
    /// Evidence category and a short secret-free description of the detecting signal — never the
    /// raw provider output.
    pub evidence: UsageEvidence,
    pub detected_via: String,
}

/// Durable, per-project record of recent automatic activity. This is thrash protection, not the
/// safety-critical path (the orchestration lock and writer lease remain that) — its only job is
/// to keep a noisy or flapping usage signal from bouncing the project between profiles.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationLedger {
    pub version: u32,
    pub recent_handoffs: Vec<AutomationEvent>,
    /// Profiles observed exhausted. A record with no reset time blocks its profile as a target
    /// until the operator runs `relay watch clear`. A record with a reset time blocks it only
    /// until that time passes — after which it may again be chosen as a *failover target* when
    /// the current writer is exhausted. Reset never triggers an automatic fail-back on its own:
    /// handoffs still only start when the current writer is itself exhausted.
    pub known_exhausted: Vec<ExhaustedRecord>,
}

impl Default for AutomationLedger {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            recent_handoffs: Vec::new(),
            known_exhausted: Vec::new(),
        }
    }
}

impl AutomationLedger {
    fn mark_exhausted(&mut self, profile: ProfileName, usage: &UsageObservation) {
        let record = ExhaustedRecord {
            profile,
            observed_unix_ms: usage.observed_unix_ms,
            reset_unix_ms: usage.reset_unix_ms,
            evidence: usage.evidence,
            detected_via: usage.detected_via.clone(),
        };
        if let Some(existing) = self
            .known_exhausted
            .iter_mut()
            .find(|existing| existing.profile == record.profile)
        {
            *existing = record;
        } else {
            self.known_exhausted.push(record);
        }
    }

    /// True while the profile is blocked as a target: recorded exhausted and either no reset
    /// time is known (fail closed) or the recorded reset time has not yet passed.
    #[must_use]
    pub fn is_known_exhausted(&self, profile: &ProfileName, now_unix_ms: u64) -> bool {
        self.known_exhausted.iter().any(|record| {
            &record.profile == profile
                && record.reset_unix_ms.is_none_or(|reset| now_unix_ms < reset)
        })
    }

    fn record_handoff(&mut self, event: AutomationEvent) {
        self.recent_handoffs.push(event);
    }

    fn prune_older_than(&mut self, now_unix_ms: u64, window_ms: u64) {
        let cutoff = now_unix_ms.saturating_sub(window_ms);
        self.recent_handoffs.retain(|event| event.unix_ms >= cutoff);
    }

    #[must_use]
    pub fn last_handoff_unix_ms(&self) -> Option<u64> {
        self.recent_handoffs.iter().map(|event| event.unix_ms).max()
    }
}

pub struct LedgerStore {
    path: PathBuf,
}

impl LedgerStore {
    #[must_use]
    pub const fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AutomationLedger> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AutomationLedger::default());
            }
            Err(source) => {
                return Err(Error::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let ledger: AutomationLedger =
            serde_json::from_slice(&bytes).map_err(|_| Error::CorruptedState)?;
        if ledger.version != LEDGER_VERSION {
            return Err(Error::CorruptedState);
        }
        Ok(ledger)
    }

    pub fn save(&self, ledger: &AutomationLedger) -> Result<()> {
        let text = serde_json::to_string_pretty(ledger).map_err(|_| Error::SerializationFailed)?;
        FsAtomicWriter.write_atomic(&self.path, text.as_bytes())
    }

    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

/// Pure decision function: given the current owner's fresh usage observation, the fallback
/// candidates (in priority order), and the durable ledger, decide what (if anything) to do.
/// Never touches the filesystem or a provider — fully unit-testable.
#[must_use]
pub fn decide(
    now_unix_ms: u64,
    source: &ProfileCandidate,
    fallbacks: &[ProfileCandidate],
    ledger: &AutomationLedger,
    policy: &AutomationPolicy,
) -> AutomationDecision {
    if !source.usage.state.is_blocking() {
        // Unknown must fail closed exactly like Available/NearLimit: no automatic action.
        return AutomationDecision::NoActionNeeded;
    }

    if let Some(last) = ledger.last_handoff_unix_ms() {
        let earliest_next = last.saturating_add(policy.cooldown_ms);
        if now_unix_ms < earliest_next {
            return AutomationDecision::CooldownActive {
                retry_after_unix_ms: earliest_next,
            };
        }
    }

    let recent_count = ledger
        .recent_handoffs
        .iter()
        .filter(|event| event.unix_ms + policy.window_ms >= now_unix_ms)
        .count();
    if recent_count >= policy.max_handoffs_per_window {
        return AutomationDecision::LoopPrevented {
            reason: format!(
                "{recent_count} automatic handoffs already occurred within the last {}ms window",
                policy.window_ms
            ),
        };
    }

    let eligible = fallbacks.iter().find(|candidate| {
        candidate.name != source.name
            && candidate.enabled
            && candidate.healthy
            && !candidate.usage.state.is_blocking()
            && !ledger.is_known_exhausted(&candidate.name, now_unix_ms)
            && match (&candidate.identity_stable_id, &source.identity_stable_id) {
                (Some(candidate_id), Some(source_id)) => candidate_id != source_id,
                _ => true,
            }
    });

    match eligible {
        Some(candidate) => AutomationDecision::Handoff {
            target: candidate.name.clone(),
        },
        None => AutomationDecision::WaitingForCapacity {
            reason: "no eligible fallback profile: all are exhausted, unhealthy, disabled, or \
                     share the source's identity"
                .to_owned(),
        },
    }
}

pub struct WatchRequest {
    pub project_dir: PathBuf,
    pub source_profile: ProfileName,
    pub source_config_dir: PathBuf,
    pub source_identity_stable_id: Option<String>,
    pub session_id: String,
    pub source_usage: UsageObservation,
    pub fallbacks: Vec<ProfileCandidate>,
    pub dry_run: bool,
}

#[derive(Debug)]
pub enum WatchOutcome {
    NoActionNeeded {
        source_usage: UsageState,
    },
    WaitingForCapacity {
        reason: String,
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
        journal: Box<HandoffJournal>,
        target: ProfileName,
    },
}

pub struct WatchCoordinator<'a> {
    pub paths: &'a crate::RelayPaths,
    pub handoff: &'a HandoffCoordinator<'a>,
    pub policy: AutomationPolicy,
}

impl WatchCoordinator<'_> {
    pub fn evaluate(&self, request: WatchRequest, now_unix_ms: u64) -> Result<WatchOutcome> {
        require_absolute(&request.project_dir)?;
        let canonical_project =
            fs::canonicalize(&request.project_dir).map_err(|source| Error::Io {
                path: request.project_dir.clone(),
                source,
            })?;
        let project_id = ProjectId::for_canonical_path(&canonical_project)?;
        let project_state_dir = self.paths.project_state_dir(&project_id);
        fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
            path: project_state_dir.clone(),
            source,
        })?;
        let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
        let ledger_store = LedgerStore::at_path(project_state_dir.join("automation_state.json"));

        let source_candidate = ProfileCandidate {
            name: request.source_profile.clone(),
            config_dir: request.source_config_dir.clone(),
            identity_stable_id: request.source_identity_stable_id.clone(),
            enabled: true,
            healthy: true,
            usage: request.source_usage.clone(),
        };

        let decision = lock.try_with(|| {
            let mut ledger = ledger_store.load()?;
            for candidate in std::iter::once(&source_candidate).chain(request.fallbacks.iter()) {
                if candidate.usage.state.is_blocking() {
                    ledger.mark_exhausted(candidate.name.clone(), &candidate.usage);
                }
            }
            ledger.prune_older_than(now_unix_ms, self.policy.window_ms);
            let decision = decide(
                now_unix_ms,
                &source_candidate,
                &request.fallbacks,
                &ledger,
                &self.policy,
            );
            if !request.dry_run {
                ledger_store.save(&ledger)?;
            }
            Ok(decision)
        })?;

        match decision {
            AutomationDecision::NoActionNeeded => Ok(WatchOutcome::NoActionNeeded {
                source_usage: request.source_usage.state,
            }),
            AutomationDecision::WaitingForCapacity { reason } => {
                Ok(WatchOutcome::WaitingForCapacity { reason })
            }
            AutomationDecision::CooldownActive {
                retry_after_unix_ms,
            } => Ok(WatchOutcome::CooldownActive {
                retry_after_unix_ms,
            }),
            AutomationDecision::LoopPrevented { reason } => {
                Ok(WatchOutcome::LoopPrevented { reason })
            }
            AutomationDecision::Handoff { target } => {
                if request.dry_run {
                    return Ok(WatchOutcome::DryRunWouldHandoff { target });
                }
                let target_candidate = request
                    .fallbacks
                    .iter()
                    .find(|candidate| candidate.name == target)
                    .expect("decide() only selects a name present in fallbacks");
                let result = self.handoff.run(HandoffRequest {
                    project_dir: canonical_project.clone(),
                    source_profile: request.source_profile.clone(),
                    source_config_dir: request.source_config_dir.clone(),
                    target_profile: target.clone(),
                    target_config_dir: target_candidate.config_dir.clone(),
                    session_id: request.session_id.clone(),
                });
                // Every attempt counts toward the cooldown and the bounded-handoff guard, whether
                // it completed or failed: a failing target must not be retried in a tight loop
                // (each retry can spend real API usage).
                lock.try_with(|| {
                    let mut ledger = ledger_store.load()?;
                    ledger.record_handoff(AutomationEvent {
                        unix_ms: now_unix_ms,
                        source: request.source_profile.clone(),
                        target: target.clone(),
                        transaction_id: result
                            .as_ref()
                            .ok()
                            .map(|journal| journal.transaction_id.to_string()),
                    });
                    ledger.prune_older_than(now_unix_ms, self.policy.window_ms);
                    ledger_store.save(&ledger)
                })?;
                let journal = result?;
                Ok(WatchOutcome::Handoff {
                    journal: Box::new(journal),
                    target,
                })
            }
        }
    }
}

fn require_absolute(path: &Path) -> Result<&Path> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Error::PathNotAbsolute(path.to_path_buf()))
    }
}

#[must_use]
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{AutomationDecision, AutomationLedger, AutomationPolicy, ProfileCandidate, decide};
    use crate::{
        ProfileName,
        usage::{UsageEvidence, UsageObservation, UsageState},
    };
    use std::path::PathBuf;

    fn observation(state: UsageState) -> UsageObservation {
        UsageObservation {
            state,
            evidence: UsageEvidence::Simulated,
            detected_via: "test".to_owned(),
            observed_unix_ms: 1_000,
            reset_unix_ms: None,
        }
    }

    fn candidate(name: &str, state: UsageState) -> ProfileCandidate {
        ProfileCandidate {
            name: ProfileName::new(name).expect("name"),
            config_dir: PathBuf::from(format!("/tmp/{name}")),
            identity_stable_id: Some(format!("identity-{name}")),
            enabled: true,
            healthy: true,
            usage: observation(state),
        }
    }

    #[test]
    fn available_source_needs_no_action() {
        let source = candidate("erika", UsageState::Available);
        let fallback = candidate("megan", UsageState::Available);
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(decision, AutomationDecision::NoActionNeeded);
    }

    #[test]
    fn unknown_source_fails_closed_to_no_action() {
        let source = candidate("erika", UsageState::Unknown);
        let fallback = candidate("megan", UsageState::Available);
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(decision, AutomationDecision::NoActionNeeded);
    }

    #[test]
    fn exhausted_source_with_healthy_fallback_triggers_handoff() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(
            decision,
            AutomationDecision::Handoff {
                target: ProfileName::new("megan").expect("name")
            }
        );
    }

    #[test]
    fn both_profiles_exhausted_waits_for_capacity() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Exhausted);
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert!(matches!(
            decision,
            AutomationDecision::WaitingForCapacity { .. }
        ));
    }

    #[test]
    fn no_fallback_configured_waits_for_capacity() {
        let source = candidate("erika", UsageState::Exhausted);
        let decision = decide(
            10_000,
            &source,
            &[],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert!(matches!(
            decision,
            AutomationDecision::WaitingForCapacity { .. }
        ));
    }

    #[test]
    fn unhealthy_fallback_is_never_selected() {
        let source = candidate("erika", UsageState::Exhausted);
        let mut fallback = candidate("megan", UsageState::Available);
        fallback.healthy = false;
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert!(matches!(
            decision,
            AutomationDecision::WaitingForCapacity { .. }
        ));
    }

    #[test]
    fn disabled_fallback_is_never_selected() {
        let source = candidate("erika", UsageState::Exhausted);
        let mut fallback = candidate("megan", UsageState::Available);
        fallback.enabled = false;
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert!(matches!(
            decision,
            AutomationDecision::WaitingForCapacity { .. }
        ));
    }

    #[test]
    fn a_fallback_sharing_the_source_identity_is_never_selected() {
        let source = candidate("erika", UsageState::Exhausted);
        let mut fallback = candidate("erika-alias", UsageState::Available);
        fallback.identity_stable_id = source.identity_stable_id.clone();
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert!(matches!(
            decision,
            AutomationDecision::WaitingForCapacity { .. }
        ));
    }

    #[test]
    fn a_known_exhausted_fallback_is_never_reselected_even_if_freshly_available() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        ledger.mark_exhausted(fallback.name.clone(), &observation(UsageState::Exhausted));
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &ledger,
            &AutomationPolicy::default(),
        );
        assert!(matches!(
            decision,
            AutomationDecision::WaitingForCapacity { .. }
        ));
    }

    #[test]
    fn cooldown_blocks_a_second_handoff_immediately_after_the_first() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        ledger.record_handoff(super::AutomationEvent {
            unix_ms: 10_000,
            source: source.name.clone(),
            target: fallback.name.clone(),
            transaction_id: None,
        });
        let policy = AutomationPolicy {
            cooldown_ms: 30_000,
            ..AutomationPolicy::default()
        };
        let decision = decide(10_500, &source, &[fallback], &ledger, &policy);
        assert!(matches!(
            decision,
            AutomationDecision::CooldownActive { .. }
        ));
    }

    #[test]
    fn cooldown_clears_once_enough_time_has_passed() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        ledger.record_handoff(super::AutomationEvent {
            unix_ms: 10_000,
            source: source.name.clone(),
            target: fallback.name.clone(),
            transaction_id: None,
        });
        let policy = AutomationPolicy {
            cooldown_ms: 30_000,
            ..AutomationPolicy::default()
        };
        let decision = decide(41_000, &source, &[fallback], &ledger, &policy);
        assert!(matches!(decision, AutomationDecision::Handoff { .. }));
    }

    #[test]
    fn repeated_exhaustion_beyond_the_max_window_count_prevents_the_loop() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let policy = AutomationPolicy {
            cooldown_ms: 0,
            max_handoffs_per_window: 2,
            window_ms: 3_600_000,
        };
        let mut ledger = AutomationLedger::default();
        ledger.record_handoff(super::AutomationEvent {
            unix_ms: 1_000,
            source: source.name.clone(),
            target: fallback.name.clone(),
            transaction_id: None,
        });
        ledger.record_handoff(super::AutomationEvent {
            unix_ms: 2_000,
            source: fallback.name.clone(),
            target: source.name.clone(),
            transaction_id: None,
        });
        let decision = decide(3_000, &source, &[fallback], &ledger, &policy);
        assert!(matches!(decision, AutomationDecision::LoopPrevented { .. }));
    }

    #[test]
    fn reset_pending_source_is_treated_the_same_as_exhausted() {
        let source = candidate("erika", UsageState::ResetPending);
        let fallback = candidate("megan", UsageState::Available);
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert!(matches!(decision, AutomationDecision::Handoff { .. }));
    }

    #[test]
    fn near_limit_source_does_not_trigger_automatic_handoff() {
        let source = candidate("erika", UsageState::NearLimit);
        let fallback = candidate("megan", UsageState::Available);
        let decision = decide(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(decision, AutomationDecision::NoActionNeeded);
    }

    #[test]
    fn a_recorded_reset_time_blocks_the_profile_only_until_it_passes() {
        let mut ledger = AutomationLedger::default();
        let mut usage = observation(UsageState::Exhausted);
        usage.reset_unix_ms = Some(5_000);
        let profile = ProfileName::new("megan").expect("name");
        ledger.mark_exhausted(profile.clone(), &usage);
        assert!(ledger.is_known_exhausted(&profile, 4_999));
        assert!(!ledger.is_known_exhausted(&profile, 5_000));
    }

    #[test]
    fn an_exhausted_record_without_a_reset_time_blocks_until_cleared() {
        let mut ledger = AutomationLedger::default();
        let profile = ProfileName::new("megan").expect("name");
        ledger.mark_exhausted(profile.clone(), &observation(UsageState::Exhausted));
        assert!(ledger.is_known_exhausted(&profile, u64::MAX));
    }
}
