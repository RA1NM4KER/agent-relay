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
    AtomicWrite, Error, FsAtomicWriter, ProfileName, ProviderKind, Result,
    handoff::{
        ContinuityType, HandoffCoordinator, HandoffJournal, HandoffRequest, HandoffState,
        JournalStore, OrchestrationLock, ProjectId, TransactionId,
    },
    usage::{UsageEvidence, UsageObservation, UsageState},
};

/// One profile under consideration, with the caller-supplied health/identity facts and a fresh
/// usage observation. The caller (provider-aware code) is responsible for gathering these; this
/// module's decision logic is pure and provider-neutral — [`decide`] never reads `provider` at
/// all, only the execution step (in [`WatchCoordinator::evaluate`]) uses it, to pick the right
/// mix of provider adapters and the right [`ContinuityType`] for the chosen target.
#[derive(Clone, Debug)]
pub struct ProfileCandidate {
    pub name: ProfileName,
    pub provider: ProviderKind,
    pub config_dir: PathBuf,
    /// Only meaningful when `provider` is Claude; see [`crate::ClaudeConfigMode`].
    pub claude_config_mode: Option<crate::ClaudeConfigMode>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

    let recent_count = recent_handoff_count(ledger, now_unix_ms, policy.window_ms);
    if recent_count >= policy.max_handoffs_per_window {
        return AutomationDecision::LoopPrevented {
            reason: format!(
                "{recent_count} automatic handoffs already occurred within the last {}ms window",
                policy.window_ms
            ),
        };
    }

    let eligible = fallbacks
        .iter()
        .find(|candidate| candidate_is_eligible(candidate, source, ledger, now_unix_ms));

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

fn recent_handoff_count(ledger: &AutomationLedger, now_unix_ms: u64, window_ms: u64) -> usize {
    ledger
        .recent_handoffs
        .iter()
        .filter(|event| event.unix_ms + window_ms >= now_unix_ms)
        .count()
}

fn candidate_is_eligible(
    candidate: &ProfileCandidate,
    source: &ProfileCandidate,
    ledger: &AutomationLedger,
    now_unix_ms: u64,
) -> bool {
    candidate.name != source.name
        && candidate.enabled
        && candidate.healthy
        && !candidate.usage.state.is_blocking()
        && !ledger.is_known_exhausted(&candidate.name, now_unix_ms)
        && match (&candidate.identity_stable_id, &source.identity_stable_id) {
            (Some(candidate_id), Some(source_id)) => candidate_id != source_id,
            _ => true,
        }
}

/// Stable, `relay why`-facing reason categories. Each is grounded in durable state `explain`
/// actually inspected — never an invented narrative. Serialized as the exact strings the request
/// for this feature specified, so `relay why --json` is a stable automation surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WhyCategory {
    /// The source is not exhausted (Available/NearLimit/ResetPending): nothing to hand off yet.
    SourceNotExhausted,
    /// The source's usage could not be determined at all: fails closed, never treated as a match.
    SourceUsageUnknown,
    /// A prior automatic handoff happened recently enough that the cooldown window is still open.
    CooldownActive,
    /// Every fallback was checked and none is currently eligible.
    NoEligibleFallback,
    /// This specific candidate is recorded exhausted (or its own usage is itself blocking).
    TargetExhausted,
    /// This specific candidate is disabled or unhealthy (failed `relay profile doctor`-style
    /// checks).
    TargetUnhealthy,
    /// This specific candidate shares the source's own provider identity (would be an alias, not
    /// a real second account).
    TargetIdentityConflict,
    /// The most recent automatic handoff attempt did not complete successfully.
    HandoffFailed,
    /// A prior handoff transaction is still unresolved; `relay recover` must run before another
    /// can start.
    RecoveryRequired,
    /// The source has capacity again (or always did); Relay never fails back on its own, so the
    /// current owner stays the owner until another handoff is actually needed or you switch by
    /// hand.
    StickyCurrentOwner,
    /// The usage integration / automatic handoff preference is not enabled for this profile.
    AutomaticHandoffDisabled,
    /// Nothing is blocking a handoff; if the source were exhausted right now, it would proceed.
    ReadyToHandoff,
}

/// Why one specific fallback candidate is or is not eligible right now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateExplanation {
    pub name: ProfileName,
    pub eligible: bool,
    pub reason: Option<WhyCategory>,
    pub detail: String,
}

/// The full explanation `relay why` renders: the same [`AutomationDecision`] `decide` would reach
/// right now, a single top-level category summarizing it in `relay why`'s vocabulary, and (when
/// relevant) a per-candidate breakdown of every configured fallback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Explanation {
    pub decision: AutomationDecision,
    pub category: WhyCategory,
    pub candidates: Vec<CandidateExplanation>,
}

/// Pure, read-only counterpart to [`decide`]: same eligibility logic, but never collapses *why*
/// down to one aggregate string. Never mutates the ledger and never performs a handoff.
#[must_use]
pub fn explain(
    now_unix_ms: u64,
    source: &ProfileCandidate,
    fallbacks: &[ProfileCandidate],
    ledger: &AutomationLedger,
    policy: &AutomationPolicy,
) -> Explanation {
    let decision = decide(now_unix_ms, source, fallbacks, ledger, policy);

    let candidates: Vec<CandidateExplanation> = fallbacks
        .iter()
        .filter(|candidate| candidate.name != source.name)
        .map(|candidate| explain_candidate(candidate, source, ledger, now_unix_ms))
        .collect();

    let category = match &decision {
        AutomationDecision::Handoff { .. } => WhyCategory::ReadyToHandoff,
        AutomationDecision::CooldownActive { .. } => WhyCategory::CooldownActive,
        AutomationDecision::LoopPrevented { .. } => WhyCategory::CooldownActive,
        AutomationDecision::WaitingForCapacity { .. } => WhyCategory::NoEligibleFallback,
        AutomationDecision::NoActionNeeded => {
            if source.usage.state == UsageState::Unknown {
                WhyCategory::SourceUsageUnknown
            } else if ledger.last_handoff_unix_ms().is_some() {
                // A handoff has happened in this project before and the source is not exhausted
                // now: staying here is stickiness, not "nothing has ever happened".
                WhyCategory::StickyCurrentOwner
            } else {
                WhyCategory::SourceNotExhausted
            }
        }
    };

    Explanation {
        decision,
        category,
        candidates,
    }
}

fn explain_candidate(
    candidate: &ProfileCandidate,
    source: &ProfileCandidate,
    ledger: &AutomationLedger,
    now_unix_ms: u64,
) -> CandidateExplanation {
    let name = candidate.name.clone();
    if !candidate.enabled {
        return CandidateExplanation {
            name,
            eligible: false,
            reason: Some(WhyCategory::TargetUnhealthy),
            detail: "disabled".to_owned(),
        };
    }
    if !candidate.healthy {
        return CandidateExplanation {
            name,
            eligible: false,
            reason: Some(WhyCategory::TargetUnhealthy),
            detail: "failed a profile health check".to_owned(),
        };
    }
    if candidate.usage.state.is_blocking()
        || ledger.is_known_exhausted(&candidate.name, now_unix_ms)
    {
        return CandidateExplanation {
            name,
            eligible: false,
            reason: Some(WhyCategory::TargetExhausted),
            detail: "exhausted".to_owned(),
        };
    }
    if let (Some(candidate_id), Some(source_id)) =
        (&candidate.identity_stable_id, &source.identity_stable_id)
        && candidate_id == source_id
    {
        return CandidateExplanation {
            name,
            eligible: false,
            reason: Some(WhyCategory::TargetIdentityConflict),
            detail: "same account as the current owner".to_owned(),
        };
    }
    CandidateExplanation {
        name,
        eligible: true,
        reason: None,
        detail: "eligible".to_owned(),
    }
}

pub struct WatchRequest {
    pub project_dir: PathBuf,
    pub source_profile: ProfileName,
    pub source_provider: ProviderKind,
    pub source_config_dir: PathBuf,
    /// Only meaningful when `source_provider` is Claude; see [`crate::ClaudeConfigMode`].
    pub source_claude_mode: Option<crate::ClaudeConfigMode>,
    pub source_identity_stable_id: Option<String>,
    pub session_id: String,
    pub source_usage: UsageObservation,
    pub fallbacks: Vec<ProfileCandidate>,
    pub dry_run: bool,
    /// The Relay Session's own state directory; `None` keeps the legacy project directory.
    pub state_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredTransaction {
    pub transaction_id: String,
    pub final_state: String,
}

#[derive(Debug)]
pub enum WatchOutcome {
    /// One or more incomplete transactions from an earlier (crashed) run were recovered. No new
    /// work is started in the same round: the recovery may have moved the writer lease, so the
    /// next invocation re-evaluates from the recovered state.
    Recovered {
        transactions: Vec<RecoveredTransaction>,
    },
    /// Recovery could not be completed safely (or the journal is unreadable). Nothing new is
    /// started; an operator must run `relay recover` / `relay recover --acknowledge`.
    RecoveryRequired {
        transaction_id: String,
        reason: String,
    },
    /// Another live orchestrator holds the project's lock mid-transaction.
    TransactionInFlight {
        transaction_id: String,
    },
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
    /// M6: a single fixed [`HandoffCoordinator`] cannot serve every (source, target) provider
    /// pairing — its `stager`/`launcher` are provider-specific. The caller (which owns the
    /// concrete provider adapters and the profile registry) supplies a resolver keyed by the
    /// two profile names involved, so automatic handoff can pick the right mix of adapters
    /// (and, indirectly, the right [`ContinuityType`]) for whichever target [`decide`] selects —
    /// without `relay-core` itself ever branching on provider identity.
    pub handoff_for: &'a dyn Fn(&ProfileName, &ProfileName) -> &'a HandoffCoordinator<'a>,
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
        let project_state_dir = request
            .state_dir
            .clone()
            .unwrap_or_else(|| self.paths.project_state_dir(&project_id));
        fs::create_dir_all(&project_state_dir).map_err(|source| Error::Io {
            path: project_state_dir.clone(),
            source,
        })?;
        let lock = OrchestrationLock::at_path(project_state_dir.join("orchestration.lock"));
        let ledger_store = LedgerStore::at_path(project_state_dir.join("automation_state.json"));

        // Startup/re-entry recovery: never begin new work while an earlier transaction for this
        // project is unresolved (a crashed run can leave a live orphan target process).
        if let Some(outcome) = self.recover_pending(&project_state_dir, request.dry_run)? {
            return Ok(outcome);
        }

        let mut source_candidate = ProfileCandidate {
            name: request.source_profile.clone(),
            provider: request.source_provider,
            config_dir: request.source_config_dir.clone(),
            claude_config_mode: request.source_claude_mode,
            identity_stable_id: request.source_identity_stable_id.clone(),
            enabled: true,
            healthy: true,
            usage: request.source_usage.clone(),
        };

        let mut fallbacks = request.fallbacks.clone();
        let (decision, eligible_targets) = lock.try_with(|| {
            let mut ledger = ledger_store.load()?;
            for candidate in std::iter::once(&source_candidate).chain(fallbacks.iter()) {
                if candidate.usage.state.is_blocking() {
                    ledger.mark_exhausted(candidate.name.clone(), &candidate.usage);
                }
            }
            // RESET_PENDING: a profile previously observed exhausted whose reset time is still in
            // the future stays blocking even when the fresh reading is merely UNKNOWN (for example
            // a stale statusline). A fresh AVAILABLE/NEAR_LIMIT reading is never overridden.
            for candidate in std::iter::once(&mut source_candidate).chain(fallbacks.iter_mut()) {
                if candidate.usage.state == UsageState::Unknown
                    && let Some(record) = ledger.known_exhausted.iter().find(|record| {
                        record.profile == candidate.name
                            && record
                                .reset_unix_ms
                                .is_some_and(|reset| now_unix_ms < reset)
                    })
                {
                    candidate.usage = UsageObservation {
                        state: UsageState::ResetPending,
                        evidence: record.evidence,
                        detected_via: format!("previously exhausted; {}", record.detected_via),
                        observed_unix_ms: now_unix_ms,
                        reset_unix_ms: record.reset_unix_ms,
                    };
                }
            }
            ledger.prune_older_than(now_unix_ms, self.policy.window_ms);
            let decision = decide(
                now_unix_ms,
                &source_candidate,
                &fallbacks,
                &ledger,
                &self.policy,
            );
            // Preserve the complete ordered candidate chain from this one decision snapshot.
            // If the first target's provider command fails after the source has already stopped,
            // automation may continue to the next one without re-entering `decide` (the failed
            // attempt deliberately starts cooldown protection).
            let eligible_targets = fallbacks
                .iter()
                .filter(|candidate| {
                    candidate_is_eligible(candidate, &source_candidate, &ledger, now_unix_ms)
                })
                .map(|candidate| candidate.name.clone())
                .take(
                    self.policy
                        .max_handoffs_per_window
                        .saturating_sub(recent_handoff_count(
                            &ledger,
                            now_unix_ms,
                            self.policy.window_ms,
                        )),
                )
                .collect::<Vec<_>>();
            if !request.dry_run {
                ledger_store.save(&ledger)?;
            }
            Ok((decision, eligible_targets))
        })?;

        match decision {
            AutomationDecision::NoActionNeeded => Ok(WatchOutcome::NoActionNeeded {
                source_usage: source_candidate.usage.state,
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
                debug_assert_eq!(eligible_targets.first(), Some(&target));
                let mut last_error = None;
                for target in eligible_targets {
                    let target_candidate = fallbacks
                        .iter()
                        .find(|candidate| candidate.name == target)
                        .expect("eligible target came from fallbacks");
                    let coordinator = (self.handoff_for)(&request.source_profile, &target);
                    let result = coordinator.run_detailed(HandoffRequest {
                        project_dir: canonical_project.clone(),
                        source_profile: request.source_profile.clone(),
                        source_provider: request.source_provider,
                        source_config_dir: request.source_config_dir.clone(),
                        source_claude_mode: request.source_claude_mode.unwrap_or_default(),
                        target_claude_mode: target_candidate.claude_config_mode.unwrap_or_default(),
                        target_profile: target.clone(),
                        target_provider: target_candidate.provider,
                        target_config_dir: target_candidate.config_dir.clone(),
                        session_id: request.session_id.clone(),
                        continuity_type: ContinuityType::for_transition(
                            request.source_provider,
                            target_candidate.provider,
                        ),
                        state_dir: request.state_dir.clone(),
                    });
                    // Every attempted destination counts toward the cooldown and bounded-handoff
                    // guard, including a failed one. The remaining candidates in this already-
                    // bounded cascade are the sole exception to that cooldown.
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
                    match result {
                        Ok(journal) => {
                            return Ok(WatchOutcome::Handoff {
                                journal: Box::new(journal),
                                target,
                            });
                        }
                        Err(failure) => {
                            let safe_to_continue =
                                failure.journal.as_ref().is_some_and(|journal| {
                                    matches!(
                                        journal.state,
                                        HandoffState::Failed {
                                            phase: crate::handoff::FailedPhase::TargetStart,
                                            ..
                                        }
                                    ) && journal.target_launch.as_ref().is_none_or(|launch| {
                                        launch.process.is_still_the_same_process() == Some(false)
                                    })
                                });
                            last_error = Some(failure.error);
                            if !safe_to_continue {
                                break;
                            }
                        }
                    }
                }
                Err(last_error.expect("at least the selected target was attempted"))
            }
        }
    }
}

impl WatchCoordinator<'_> {
    /// Inspects every incomplete transaction journal for the project and runs the existing safe
    /// recovery logic on each, under the orchestration lock, before anything new starts. Returns
    /// `None` when there is nothing pending.
    pub fn recover_pending(
        &self,
        project_state_dir: &Path,
        dry_run: bool,
    ) -> Result<Option<WatchOutcome>> {
        let pending = match scan_pending_transactions(project_state_dir) {
            PendingScan::None => return Ok(None),
            PendingScan::Unreadable { id, reason } => {
                return Ok(Some(WatchOutcome::RecoveryRequired {
                    transaction_id: id.to_string(),
                    reason,
                }));
            }
            PendingScan::Some(pending) => pending,
        };
        if dry_run {
            return Ok(Some(WatchOutcome::RecoveryRequired {
                transaction_id: pending[0].0.to_string(),
                reason: format!(
                    "dry run: {} incomplete transaction(s) would be recovered before any new work",
                    pending.len()
                ),
            }));
        }
        let mut recovered = Vec::new();
        for (id, source_profile, target_profile) in pending {
            let coordinator = (self.handoff_for)(&source_profile, &target_profile);
            match coordinator.recover(project_state_dir, &id) {
                Err(Error::OrchestrationLockHeld) => {
                    return Ok(Some(WatchOutcome::TransactionInFlight {
                        transaction_id: id.to_string(),
                    }));
                }
                Err(error) => {
                    return Ok(Some(WatchOutcome::RecoveryRequired {
                        transaction_id: id.to_string(),
                        reason: format!("recovery failed ({})", error.code()),
                    }));
                }
                Ok(journal) => {
                    if let HandoffState::RecoveryRequired { reason } = &journal.state {
                        return Ok(Some(WatchOutcome::RecoveryRequired {
                            transaction_id: id.to_string(),
                            reason: reason.clone(),
                        }));
                    }
                    recovered.push(RecoveredTransaction {
                        transaction_id: id.to_string(),
                        final_state: format!("{:?}", journal.state),
                    });
                }
            }
        }
        Ok(Some(WatchOutcome::Recovered {
            transactions: recovered,
        }))
    }
}

/// What a read-only scan of `<project_state_dir>/handoffs` for still-open transactions found.
enum PendingScan {
    /// Nothing pending.
    None,
    /// One or more journals not yet in a terminal state, oldest first.
    Some(Vec<(TransactionId, ProfileName, ProfileName)>),
    /// A journal exists but could not be parsed, and its raw `state.state` field is not terminal
    /// either — recovery must be run by hand.
    Unreadable { id: TransactionId, reason: String },
}

/// Read-only counterpart to the scan [`WatchCoordinator::recover_pending`] performs before it acts
/// — shared so `relay why`'s `RecoveryRequired` explanation can never disagree with what a real
/// recovery pass would find, without duplicating the journal-reading logic.
fn scan_pending_transactions(project_state_dir: &Path) -> PendingScan {
    let handoffs_dir = project_state_dir.join("handoffs");
    let Ok(entries) = fs::read_dir(&handoffs_dir) else {
        return PendingScan::None;
    };
    // Only the transaction the project's current pointer names (and anything newer, from a
    // crash between the first journal write and the pointer write) can still be live. Older
    // non-terminal journals were superseded by later transactions and are history, not work:
    // treating them as pending would block a project forever on a stale record.
    let current = fs::read_to_string(project_state_dir.join("current_transaction.json"))
        .ok()
        .and_then(|text| TransactionId::parse(text.trim()).ok());
    let mut pending = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        let Ok(id) = TransactionId::parse(stem) else {
            continue;
        };
        if current.as_ref().is_none_or(|current| id < *current) {
            continue;
        }
        match JournalStore::at_path(path.clone()).load() {
            Ok(journal) if journal.state.is_terminal() => {}
            Ok(journal) => {
                pending.push((id, journal.source_profile, journal.target_profile));
            }
            Err(error) => {
                // A journal written by an older Relay may not match today's schema; if its raw
                // state is terminal it is history, otherwise it is ambiguous.
                if raw_state_is_terminal(&path) {
                    continue;
                }
                return PendingScan::Unreadable {
                    id,
                    reason: format!("journal could not be read ({})", error.code()),
                };
            }
        }
    }
    if pending.is_empty() {
        return PendingScan::None;
    }
    pending.sort_by(|left, right| left.0.cmp(&right.0));
    PendingScan::Some(pending)
}

/// Whether an incomplete handoff transaction is waiting on `relay recover`, without attempting to
/// recover it. `relay why`'s `RecoveryRequired` category is grounded in this, the same durable
/// state a real recovery pass reads.
#[must_use]
pub fn recovery_pending(project_state_dir: &Path) -> bool {
    !matches!(
        scan_pending_transactions(project_state_dir),
        PendingScan::None
    )
}

fn raw_state_is_terminal(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("state")
                .and_then(|state| state.get("state"))
                .and_then(|state| state.as_str().map(str::to_owned))
        })
        .is_some_and(|state| state == "COMPLETE" || state == "FAILED")
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
            provider: crate::ProviderKind::Claude,
            config_dir: PathBuf::from(format!("/tmp/{name}")),
            identity_stable_id: Some(format!("identity-{name}")),
            enabled: true,
            healthy: true,
            usage: observation(state),
            claude_config_mode: None,
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

    fn exhausted_until(name: &str, reset_unix_ms: u64) -> (ProfileName, UsageObservation) {
        (
            ProfileName::new(name).expect("name"),
            UsageObservation {
                reset_unix_ms: Some(reset_unix_ms),
                ..observation(UsageState::Exhausted)
            },
        )
    }

    /// erika -> megan happened; megan now exhausts while erika's window has not reset: the full
    /// ordered hierarchy is reconsidered and erika is skipped, so the next healthy one (codex) wins.
    #[test]
    fn after_erika_to_megan_a_megan_limit_skips_still_blocked_erika_for_codex() {
        let source = candidate("megan", UsageState::Exhausted);
        let erika = candidate("erika", UsageState::Unknown);
        let codex = candidate("codex", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        let (name, usage) = exhausted_until("erika", 50_000);
        ledger.mark_exhausted(name, &usage);
        let decision = decide(
            10_000,
            &source,
            &[erika, codex],
            &ledger,
            &AutomationPolicy::default(),
        );
        assert_eq!(
            decision,
            AutomationDecision::Handoff {
                target: ProfileName::new("codex").expect("name")
            }
        );
    }

    /// Same situation once erika's reset has passed: she is first in the hierarchy and eligible
    /// again, so she is chosen ahead of codex (sticky writer, but no permanent demotion).
    #[test]
    fn once_erikas_reset_has_passed_she_is_first_eligible_again_ahead_of_codex() {
        let source = candidate("megan", UsageState::Exhausted);
        let erika = candidate("erika", UsageState::Available);
        let codex = candidate("codex", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        let (name, usage) = exhausted_until("erika", 5_000);
        ledger.mark_exhausted(name, &usage);
        let decision = decide(
            10_000,
            &source,
            &[erika, codex],
            &ledger,
            &AutomationPolicy::default(),
        );
        assert_eq!(
            decision,
            AutomationDecision::Handoff {
                target: ProfileName::new("erika").expect("name")
            }
        );
    }

    /// Codex is the writer and genuinely exhausted; the first Claude profile is still
    /// reset-pending, so the next eligible one in the global order is chosen.
    #[test]
    fn an_exhausted_codex_writer_skips_a_reset_pending_claude_for_the_next_eligible_one() {
        let mut source = candidate("codex", UsageState::Exhausted);
        source.provider = crate::ProviderKind::Codex;
        let primary = candidate("claude-primary", UsageState::Unknown);
        let backup = candidate("claude-backup", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        let (name, usage) = exhausted_until("claude-primary", 50_000);
        ledger.mark_exhausted(name, &usage);
        let decision = decide(
            10_000,
            &source,
            &[primary, backup],
            &ledger,
            &AutomationPolicy::default(),
        );
        assert_eq!(
            decision,
            AutomationDecision::Handoff {
                target: ProfileName::new("claude-backup").expect("name")
            }
        );
    }

    /// The same Codex writer once the primary Claude has reset: the primary is chosen first.
    #[test]
    fn an_exhausted_codex_writer_prefers_the_primary_claude_once_it_has_reset() {
        let mut source = candidate("codex", UsageState::Exhausted);
        source.provider = crate::ProviderKind::Codex;
        let primary = candidate("claude-primary", UsageState::Available);
        let backup = candidate("claude-backup", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        let (name, usage) = exhausted_until("claude-primary", 5_000);
        ledger.mark_exhausted(name, &usage);
        let decision = decide(
            10_000,
            &source,
            &[primary, backup],
            &ledger,
            &AutomationPolicy::default(),
        );
        assert_eq!(
            decision,
            AutomationDecision::Handoff {
                target: ProfileName::new("claude-primary").expect("name")
            }
        );
    }

    /// A healthy Codex writer never moves, however ready the higher-priority Claude profile is.
    #[test]
    fn a_healthy_codex_writer_is_sticky_even_when_the_primary_is_ready() {
        let mut source = candidate("codex", UsageState::Available);
        source.provider = crate::ProviderKind::Codex;
        let primary = candidate("claude-primary", UsageState::Available);
        assert_eq!(
            decide(
                10_000,
                &source,
                &[primary],
                &AutomationLedger::default(),
                &AutomationPolicy::default()
            ),
            AutomationDecision::NoActionNeeded
        );
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

    // =============================================================================================
    // `explain` (`relay why`'s pure engine) — one test per stable `WhyCategory` it can itself
    // reach. `HandoffFailed`/`RecoveryRequired` are grounded in state `explain` doesn't see (the
    // ledger's last event, and the handoffs directory); those are covered by
    // `relay-cli`'s `commands::why::resolve_category`, not here.
    // =============================================================================================

    use super::{Explanation, WhyCategory, explain};

    #[test]
    fn near_limit_source_explains_as_not_exhausted() {
        let source = candidate("erika", UsageState::NearLimit);
        let fallback = candidate("megan", UsageState::Available);
        let explanation = explain(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(explanation.category, WhyCategory::SourceNotExhausted);
    }

    #[test]
    fn exhausted_source_with_an_eligible_fallback_explains_as_ready() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let Explanation {
            category,
            candidates,
            ..
        } = explain(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(category, WhyCategory::ReadyToHandoff);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].eligible);
    }

    #[test]
    fn unknown_source_explains_as_usage_unknown() {
        let source = candidate("erika", UsageState::Unknown);
        let fallback = candidate("megan", UsageState::Available);
        let explanation = explain(
            10_000,
            &source,
            &[fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(explanation.category, WhyCategory::SourceUsageUnknown);
    }

    #[test]
    fn a_live_cooldown_explains_as_cooldown_active() {
        let source = candidate("erika", UsageState::Exhausted);
        let fallback = candidate("megan", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        ledger.record_handoff(super::AutomationEvent {
            unix_ms: 10_000,
            source: source.name.clone(),
            target: fallback.name.clone(),
            transaction_id: Some("ho-1".to_owned()),
        });
        let policy = AutomationPolicy {
            cooldown_ms: 30_000,
            ..AutomationPolicy::default()
        };
        let explanation = explain(10_500, &source, &[fallback], &ledger, &policy);
        assert_eq!(explanation.category, WhyCategory::CooldownActive);
    }

    #[test]
    fn no_eligible_fallback_lists_why_each_candidate_was_skipped() {
        let source = candidate("erika", UsageState::Exhausted);
        let mut unhealthy = candidate("megan", UsageState::Available);
        unhealthy.healthy = false;
        let exhausted_fallback = candidate("codex", UsageState::Exhausted);
        let Explanation {
            category,
            candidates,
            ..
        } = explain(
            10_000,
            &source,
            &[unhealthy, exhausted_fallback],
            &AutomationLedger::default(),
            &AutomationPolicy::default(),
        );
        assert_eq!(category, WhyCategory::NoEligibleFallback);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| !candidate.eligible));
        assert_eq!(candidates[0].reason, Some(WhyCategory::TargetUnhealthy));
        assert_eq!(candidates[1].reason, Some(WhyCategory::TargetExhausted));
    }

    #[test]
    fn source_with_capacity_after_a_prior_handoff_explains_as_sticky_not_never_happened() {
        let source = candidate("erika", UsageState::Available);
        let fallback = candidate("megan", UsageState::Available);
        let mut ledger = AutomationLedger::default();
        ledger.record_handoff(super::AutomationEvent {
            unix_ms: 1_000,
            source: fallback.name.clone(),
            target: source.name.clone(),
            transaction_id: Some("ho-1".to_owned()),
        });
        let explanation = explain(
            50_000,
            &source,
            &[fallback],
            &ledger,
            &AutomationPolicy::default(),
        );
        assert_eq!(explanation.category, WhyCategory::StickyCurrentOwner);
    }

    #[test]
    fn recovery_pending_is_false_with_no_handoffs_directory_and_true_for_a_non_terminal_journal() {
        use crate::handoff::{
            ContinuityType, HandoffJournal, JournalStore, ProjectId, TransactionId,
        };

        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!super::recovery_pending(dir.path()));

        let journal = HandoffJournal::new(
            TransactionId::generate(),
            ProjectId::for_canonical_path(dir.path()).expect("project id"),
            dir.path().to_path_buf(),
            ProfileName::new("erika").expect("name"),
            ProfileName::new("megan").expect("name"),
            dir.path().join("megan-config"),
            "s1".to_owned(),
            ContinuityType::SessionContinuation,
        );
        let id = journal.transaction_id.clone();
        std::fs::create_dir_all(dir.path().join("handoffs")).expect("handoffs dir");
        JournalStore::at_path(dir.path().join("handoffs").join(format!("{id}.json")))
            .save(&journal)
            .expect("save journal");
        std::fs::write(dir.path().join("current_transaction.json"), id.to_string())
            .expect("current transaction pointer");

        assert!(super::recovery_pending(dir.path()));
    }
}
