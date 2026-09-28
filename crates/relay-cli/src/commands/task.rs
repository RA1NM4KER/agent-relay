//! Deterministic GitHub Issue queue discovery.
//!
//! GitHub labels remain the source of task state; a deterministic remote Git ref is the exclusive
//! serialization point for claiming. Task-result consumption has only the explicitly documented
//! blocked transition; review/completion remains a fail-closed boundary until its GitHub contract
//! exists.

use std::process::{Command, Stdio};

use relay_core::{
    Error, RelayPaths,
    handoff::{SessionState, TaskResult, TaskResultKind},
};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{TaskCommand, TaskResultValue},
    output::{CommandOutput, header, success},
    sessions,
    util::current_unix_ms,
};

const REPOSITORY: &str = "RA1NM4KER/agent-relay";
const READY: &str = "relay:ready";
const CLAIMED: &str = "relay:claimed";
const BLOCKED: &str = "relay:blocked";

/// The remote, repository-wide serialization point for a task claim. A branch creation is the
/// only operation in this protocol that decides ownership; labels and comments are visibility
/// records written strictly after ownership exists.
fn claim_ref(number: u64) -> String {
    format!("refs/heads/relay/claims/{number}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefCreate {
    Created,
    Exists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimOutcome {
    Claimed,
    NotEligible,
    Contended,
    /// The remote source of truth could not be read or mutated. No worker may start.
    Unavailable,
    /// We own the remote ref but could not durably make that ownership visible. A worker must
    /// never start from this outcome; recovery may either complete visibility or release the ref.
    RecoveryRequired,
}

/// The only remote states a result consumer may act upon. Each variant has already proved the
/// deterministic ref and Relay-owned claim comment; anything else is ambiguous and fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteClaimState {
    Active,
    /// `relay:blocked` was added, but an interrupted earlier attempt did not remove `claimed`.
    BlockedPartial,
    Blocked,
}

/// Deliberately narrow boundary around the GitHub operations needed for an atomic claim. Its fake
/// implementation exercises concurrency and partial-failure behavior without any live account.
trait ClaimBackend {
    fn read_issue(&mut self, number: u64) -> Result<GitHubIssue, ()>;
    fn default_branch_sha(&mut self) -> Result<String, ()>;
    fn create_ref(&mut self, reference: &str, sha: &str) -> Result<RefCreate, ()>;
    fn mark_claimed(&mut self, number: u64, worker: &str, reference: &str) -> Result<(), ()>;
    fn verify_visible_claim(
        &mut self,
        number: u64,
        worker: &str,
        reference: &str,
    ) -> Result<bool, ()>;
    /// Reads only a claim whose ref, visible Relay comment, and Issue state all agree.
    fn claim_state(&mut self, number: u64, reference: &str) -> Result<RemoteClaimState, ()>;
    /// The sole defined result-consumer mutation: add Relay's blocked label and remove only its
    /// claimed label. The ref, comment, Issue state, and all foreign labels remain untouched.
    fn mark_blocked(&mut self, number: u64) -> Result<(), ()>;
}

fn eligible(issue: &GitHubIssue) -> bool {
    select_next(vec![issue.clone()]).is_some_and(|candidate| candidate.number == issue.number)
}

/// Claim transaction. The second eligibility read closes the discovery-to-claim race. Once the
/// deterministic ref exists, every contender gets `Contended` before it can mutate GitHub issue
/// state or launch work. A visibility failure intentionally retains the ref and returns
/// `RecoveryRequired`; deleting a ref that might be another worker's recovery record is unsafe.
fn claim<B: ClaimBackend>(backend: &mut B, number: u64, worker: &str) -> ClaimOutcome {
    let Ok(before) = backend.read_issue(number) else {
        return ClaimOutcome::Unavailable;
    };
    if !eligible(&before) {
        return ClaimOutcome::NotEligible;
    }
    let Ok(base) = backend.default_branch_sha() else {
        return ClaimOutcome::Unavailable;
    };
    let reference = claim_ref(number);
    match backend.create_ref(&reference, &base) {
        Ok(RefCreate::Exists) => return ClaimOutcome::Contended,
        Err(()) => return ClaimOutcome::Unavailable,
        Ok(RefCreate::Created) => {}
    }
    let Ok(after) = backend.read_issue(number) else {
        return ClaimOutcome::RecoveryRequired;
    };
    if !eligible(&after) {
        return ClaimOutcome::RecoveryRequired;
    }
    match backend.mark_claimed(number, worker, &reference) {
        Ok(())
            if backend
                .verify_visible_claim(number, worker, &reference)
                .unwrap_or(false) =>
        {
            ClaimOutcome::Claimed
        }
        Ok(()) | Err(()) => ClaimOutcome::RecoveryRequired,
    }
}

/// Production boundary. Every invocation uses structured `gh api` arguments (never a shell), and
/// throws away remote stderr except for the one documented duplicate-ref signal needed to make a
/// contender stop. This type is intentionally small so the transaction above remains fully fakeable.
struct GhClaimBackend;

impl GhClaimBackend {
    fn api(&self, args: &[&str]) -> Result<std::process::Output, ()> {
        Command::new("gh")
            .arg("api")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|_| ())
    }

    fn successful_json<T: for<'de> Deserialize<'de>>(&self, args: &[&str]) -> Result<T, ()> {
        let output = self.api(args)?;
        if !output.status.success() {
            return Err(());
        }
        serde_json::from_slice(&output.stdout).map_err(|_| ())
    }
}

impl ClaimBackend for GhClaimBackend {
    fn read_issue(&mut self, number: u64) -> Result<GitHubIssue, ()> {
        self.successful_json(&["-X", "GET", &format!("repos/{REPOSITORY}/issues/{number}")])
    }

    fn default_branch_sha(&mut self) -> Result<String, ()> {
        #[derive(Deserialize)]
        struct Repository {
            default_branch: String,
        }
        #[derive(Deserialize)]
        struct RefObject {
            object: Sha,
        }
        #[derive(Deserialize)]
        struct Sha {
            sha: String,
        }
        let repository: Repository =
            self.successful_json(&["-X", "GET", &format!("repos/{REPOSITORY}")])?;
        let reference: RefObject = self.successful_json(&[
            "-X",
            "GET",
            &format!(
                "repos/{REPOSITORY}/git/ref/heads/{}",
                repository.default_branch
            ),
        ])?;
        Ok(reference.object.sha)
    }

    fn create_ref(&mut self, reference: &str, sha: &str) -> Result<RefCreate, ()> {
        let output = self.api(&[
            "-X",
            "POST",
            &format!("repos/{REPOSITORY}/git/refs"),
            "-f",
            &format!("ref={reference}"),
            "-f",
            &format!("sha={sha}"),
        ])?;
        if output.status.success() {
            return Ok(RefCreate::Created);
        }
        // GitHub documents conflict for duplicate ref creation. `gh` exposes an already-existing
        // ref as a nonzero command; this narrowly recognizes only that loser case.
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        if diagnostic.contains("Reference already exists") || diagnostic.contains("already exists")
        {
            Ok(RefCreate::Exists)
        } else {
            Err(())
        }
    }

    fn mark_claimed(&mut self, number: u64, worker: &str, reference: &str) -> Result<(), ()> {
        let labels = self.api(&[
            "-X",
            "POST",
            &format!("repos/{REPOSITORY}/issues/{number}/labels"),
            "-f",
            &format!("labels[]={CLAIMED}"),
        ])?;
        if !labels.status.success() {
            return Err(());
        }
        let remove_ready = self.api(&[
            "-X",
            "DELETE",
            &format!("repos/{REPOSITORY}/issues/{number}/labels/relay%3Aready"),
        ])?;
        if !remove_ready.status.success() {
            return Err(());
        }
        let comment = self.api(&[
            "-X",
            "POST",
            &format!("repos/{REPOSITORY}/issues/{number}/comments"),
            "-f",
            &format!("body=Relay task claim: worker={worker}; ref={reference}"),
        ])?;
        comment.status.success().then_some(()).ok_or(())
    }

    fn verify_visible_claim(
        &mut self,
        number: u64,
        worker: &str,
        reference: &str,
    ) -> Result<bool, ()> {
        #[derive(Deserialize)]
        struct Comment {
            body: String,
        }
        let issue = self.read_issue(number)?;
        let labels = issue
            .labels
            .iter()
            .filter_map(|label| match label {
                GitHubLabel::Named { name } => Some(name.as_str()),
                GitHubLabel::Foreign { .. } => None,
            })
            .collect::<Vec<_>>();
        if !labels.contains(&CLAIMED) || labels.contains(&READY) {
            return Ok(false);
        }
        let comments: Vec<Comment> = self.successful_json(&[
            "-X",
            "GET",
            &format!("repos/{REPOSITORY}/issues/{number}/comments?per_page=100"),
        ])?;
        let expected = format!("Relay task claim: worker={worker}; ref={reference}");
        Ok(comments.iter().any(|comment| comment.body == expected))
    }

    fn claim_state(&mut self, number: u64, reference: &str) -> Result<RemoteClaimState, ()> {
        let issue = self.read_issue(number)?;
        if issue.number != number || issue.pull_request.is_some() {
            return Err(());
        }
        let labels = issue
            .labels
            .iter()
            .filter_map(|label| match label {
                GitHubLabel::Named { name } => Some(name.as_str()),
                GitHubLabel::Foreign { .. } => None,
            })
            .collect::<Vec<_>>();
        // A missing ref is stale claim state. Any GitHub read failure also fails closed: neither
        // case permits a result to be written or consumed.
        let ref_path = reference.strip_prefix("refs/").ok_or(())?;
        let ref_output = self.api(&[
            "-X",
            "GET",
            &format!("repos/{REPOSITORY}/git/ref/{ref_path}"),
        ])?;
        if !ref_output.status.success() {
            return Err(());
        }
        #[derive(Deserialize)]
        struct Comment {
            body: String,
        }
        let comments: Vec<Comment> = self.successful_json(&[
            "-X",
            "GET",
            &format!("repos/{REPOSITORY}/issues/{number}/comments?per_page=100"),
        ])?;
        if !comments.iter().any(|comment| {
            comment.body.starts_with("Relay task claim: worker=")
                && comment.body.ends_with(&format!("; ref={reference}"))
        }) {
            return Err(());
        }
        match (
            labels.contains(&READY),
            labels.contains(&CLAIMED),
            labels.contains(&BLOCKED),
        ) {
            (false, true, false) => Ok(RemoteClaimState::Active),
            (false, true, true) => Ok(RemoteClaimState::BlockedPartial),
            (false, false, true) => Ok(RemoteClaimState::Blocked),
            _ => Err(()),
        }
    }

    fn mark_blocked(&mut self, number: u64) -> Result<(), ()> {
        let add_blocked = self.api(&[
            "-X",
            "POST",
            &format!("repos/{REPOSITORY}/issues/{number}/labels"),
            "-f",
            &format!("labels[]={BLOCKED}"),
        ])?;
        if !add_blocked.status.success() {
            return Err(());
        }
        let remove_claimed = self.api(&[
            "-X",
            "DELETE",
            &format!("repos/{REPOSITORY}/issues/{number}/labels/relay%3Aclaimed"),
        ])?;
        remove_claimed.status.success().then_some(()).ok_or(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubIssue {
    number: u64,
    title: String,
    html_url: String,
    #[serde(default)]
    labels: Vec<GitHubLabel>,
    // GitHub's issues endpoint includes this field for pull requests.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
enum GitHubLabel {
    Named { name: String },
    // A foreign/malformed label is deliberately not queue state.
    Foreign { _value: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SelectedIssue {
    number: u64,
    title: String,
    url: String,
}

pub(crate) fn run(paths: &RelayPaths, command: &TaskCommand) -> Result<CommandOutput, Error> {
    match command {
        TaskCommand::Next => next(),
        TaskCommand::Claim { issue, worker } => claim_live(*issue, worker),
        TaskCommand::Result {
            issue,
            claim_ref: supplied_ref,
            session,
            project_dir,
            result,
            summary,
        } => record_result(
            paths,
            *issue,
            supplied_ref,
            session,
            project_dir,
            *result,
            summary,
        ),
        TaskCommand::ApplyResult {
            issue,
            claim_ref: supplied_ref,
            session,
            project_dir,
        } => apply_result(paths, *issue, supplied_ref, session, project_dir),
    }
}

fn task_result_kind(value: TaskResultValue) -> TaskResultKind {
    match value {
        TaskResultValue::Completed => TaskResultKind::Completed,
        TaskResultValue::Blocked => TaskResultKind::Blocked,
        TaskResultValue::Continuing => TaskResultKind::Continuing,
    }
}

fn record_result(
    paths: &RelayPaths,
    issue: u64,
    supplied_ref: &str,
    session: &str,
    project_dir: &std::path::Path,
    result: TaskResultValue,
    summary: &str,
) -> Result<CommandOutput, Error> {
    let expected_ref = claim_ref(issue);
    if supplied_ref != expected_ref {
        return Err(Error::WorkingStateInvalid(format!(
            "task claim ref must be exactly {expected_ref}"
        )));
    }
    let canonical_project = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.to_path_buf(),
        source,
    })?;
    let store = sessions::open_store(paths, &canonical_project)?;
    let view = store.resolve(session)?;
    if view.state() != SessionState::Active {
        return Err(Error::RelaySessionDormant(
            view.record.relay_session_id.short().to_owned(),
        ));
    }
    let mut backend = GhClaimBackend;
    let record = TaskResult::new(
        issue,
        supplied_ref.to_owned(),
        view.record.relay_session_id.clone(),
        task_result_kind(result),
        summary.to_owned(),
        current_unix_ms(),
    )?;
    record_verified_result(
        &mut backend,
        &store.task_result(&view.record.relay_session_id),
        record,
    )
    .and_then(|record| task_result_output(&record))
}

fn record_verified_result<B: ClaimBackend>(
    backend: &mut B,
    store: &relay_core::handoff::TaskResultStore,
    record: TaskResult,
) -> Result<TaskResult, Error> {
    if record.claim_ref != claim_ref(record.issue) {
        return Err(Error::WorkingStateInvalid(
            "task result claim ref does not match issue".into(),
        ));
    }
    if backend.claim_state(record.issue, &record.claim_ref).ok() != Some(RemoteClaimState::Active) {
        return Err(Error::WorkingStateInvalid(
            "task claim is missing, stale, or not visibly Relay-owned; no result was recorded"
                .into(),
        ));
    }
    store.write(record)
}

/// Explicitly consumes the exact, durable evidence bound to a claimed Issue. It intentionally
/// does not require an active provider: the record was only writable while active, and this
/// command does no provider work. Remote claim evidence remains authoritative.
fn apply_result(
    paths: &RelayPaths,
    issue: u64,
    supplied_ref: &str,
    session: &str,
    project_dir: &std::path::Path,
) -> Result<CommandOutput, Error> {
    if supplied_ref != claim_ref(issue) {
        return Err(Error::WorkingStateInvalid(format!(
            "task claim ref must be exactly {}",
            claim_ref(issue)
        )));
    }
    let canonical_project = std::fs::canonicalize(project_dir).map_err(|source| Error::Io {
        path: project_dir.to_path_buf(),
        source,
    })?;
    let sessions = sessions::open_store(paths, &canonical_project)?;
    let view = sessions.resolve(session)?;
    let evidence = validated_result_evidence(
        &sessions.task_result(&view.record.relay_session_id),
        issue,
        supplied_ref,
        &view.record.relay_session_id,
    )?;
    let mut backend = GhClaimBackend;
    consume_result(&mut backend, evidence)
        .and_then(|outcome| task_apply_output(issue, supplied_ref, &outcome))
}

/// Keep the durable result validation as a named, testable boundary. Every lifecycle consumer
/// must use [`TaskResultStore::require_for`], never reconstruct task state from other local data.
fn validated_result_evidence(
    store: &relay_core::handoff::TaskResultStore,
    issue: u64,
    claim_ref: &str,
    relay_session_id: &relay_core::handoff::RelaySessionId,
) -> Result<TaskResult, Error> {
    store.require_for(issue, claim_ref, relay_session_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultApplyOutcome {
    Continuing,
    Blocked,
    CompletionReviewContractMissing,
}

/// The result consumer's sole authority boundary. It reads [`TaskResultStore::require_for`] at
/// the CLI edge before reaching here; this function requires the remote ref/comment/labels to
/// agree as well. No branch, transcript, provider process, or CI fact participates.
fn consume_result<B: ClaimBackend>(
    backend: &mut B,
    evidence: TaskResult,
) -> Result<ResultApplyOutcome, Error> {
    let state = backend
        .claim_state(evidence.issue, &evidence.claim_ref)
        .map_err(|_| {
            Error::WorkingStateInvalid(
                "task claim is missing, stale, or ambiguous; result was not applied".into(),
            )
        })?;
    match evidence.result {
        TaskResultKind::Continuing => {
            if state != RemoteClaimState::Active {
                return Err(Error::WorkingStateInvalid(
                    "continuing evidence requires the exact active claimed state".into(),
                ));
            }
            Ok(ResultApplyOutcome::Continuing)
        }
        TaskResultKind::Blocked => match state {
            RemoteClaimState::Blocked => Ok(ResultApplyOutcome::Blocked),
            RemoteClaimState::Active | RemoteClaimState::BlockedPartial => {
                backend.mark_blocked(evidence.issue).map_err(|_| {
                    Error::ConflictRequiresResolution(
                        "blocked transition may be partially applied; rerun the same explicit command after verifying GitHub".into(),
                    )
                })?;
                if backend
                    .claim_state(evidence.issue, &evidence.claim_ref)
                    .ok()
                    != Some(RemoteClaimState::Blocked)
                {
                    return Err(Error::ConflictRequiresResolution(
                        "blocked transition did not reach its exact visible state; no further action was taken".into(),
                    ));
                }
                Ok(ResultApplyOutcome::Blocked)
            }
        },
        TaskResultKind::Completed => {
            if state != RemoteClaimState::Active {
                return Err(Error::WorkingStateInvalid(
                    "completed evidence requires the exact active claimed state".into(),
                ));
            }
            // #7 defines no review-ready label, PR identity proof, or claim-release contract.
            // Completing an Issue or releasing the ref here would invent lifecycle authority.
            Ok(ResultApplyOutcome::CompletionReviewContractMissing)
        }
    }
}

fn task_apply_output(
    issue: u64,
    claim_ref: &str,
    outcome: &ResultApplyOutcome,
) -> Result<CommandOutput, Error> {
    let (result, mutated, human) = match outcome {
        ResultApplyOutcome::Continuing => (
            "continuing",
            false,
            format!(
                "{}Same claimed task may continue; GitHub lifecycle state was not changed.",
                header("Relay task result")
            ),
        ),
        ResultApplyOutcome::Blocked => (
            "blocked",
            true,
            format!(
                "{}Applied Relay blocked state for GitHub Issue #{issue}; the claim ref remains intact.",
                header("Relay task result")
            ),
        ),
        ResultApplyOutcome::CompletionReviewContractMissing => (
            "completed",
            false,
            format!(
                "{}Validated completed evidence for GitHub Issue #{issue}, but no review-ready lifecycle contract exists; no GitHub state changed.",
                header("Relay task result")
            ),
        ),
    };
    success(
        "task.apply_result",
        human,
        serde_json::json!({"issue": issue, "claim_ref": claim_ref, "result": result, "lifecycle_mutated": mutated}),
    )
}

fn task_result_output(record: &TaskResult) -> Result<CommandOutput, Error> {
    let kind = match record.result {
        TaskResultKind::Completed => "completed",
        TaskResultKind::Blocked => "blocked",
        TaskResultKind::Continuing => "continuing",
    };
    success(
        "task.result",
        format!(
            "{}Recorded explicit {kind} result for GitHub Issue #{}.",
            header("Relay task result"),
            record.issue
        ),
        serde_json::json!({"issue": record.issue, "claim_ref": record.claim_ref, "session": record.relay_session_id, "result": kind, "summary": record.summary, "recorded_unix_ms": record.recorded_unix_ms}),
    )
}

fn claim_live(number: u64, worker: &str) -> Result<CommandOutput, Error> {
    if !valid_worker(worker) {
        return Err(Error::WorkingStateInvalid(
            "task worker identity must use only ASCII letters, digits, '-', '_', or '.'".into(),
        ));
    }
    let mut backend = GhClaimBackend;
    let outcome = claim(&mut backend, number, worker);
    let (human, claimed) = match outcome {
        ClaimOutcome::Claimed => (
            format!(
                "{}Claimed GitHub Issue #{number} as {worker}.",
                header("Relay task claim")
            ),
            true,
        ),
        ClaimOutcome::Contended => (
            format!(
                "{}Issue #{number} is already claimed; no work was started.",
                header("Relay task claim")
            ),
            false,
        ),
        ClaimOutcome::NotEligible => (
            format!(
                "{}Issue #{number} is not currently Relay-ready; no state changed.",
                header("Relay task claim")
            ),
            false,
        ),
        ClaimOutcome::Unavailable => return Err(Error::ProviderCommandFailed),
        ClaimOutcome::RecoveryRequired => {
            return Err(Error::ConflictRequiresResolution(format!(
                "claim ref {} exists but Relay could not verify its visible Issue state; run task recovery before any worker starts",
                claim_ref(number)
            )));
        }
    };
    success(
        "task.claim",
        human,
        serde_json::json!({
            "repository": REPOSITORY,
            "issue": number,
            "worker": worker,
            "claimed": claimed,
            "claim_ref": claim_ref(number)
        }),
    )
}

fn valid_worker(worker: &str) -> bool {
    !worker.is_empty()
        && worker.len() <= 128
        && worker
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn next() -> Result<CommandOutput, Error> {
    let output = Command::new("gh")
        .args([
            "api",
            "-X",
            "GET",
            "repos/RA1NM4KER/agent-relay/issues?state=open&per_page=100",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|_| Error::ProviderExecutableMissing)?;
    if !output.status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    let issues = decode_issues(&output.stdout)?;
    let selected = select_next(issues);
    let human = match &selected {
        Some(issue) => format!(
            "{}#{} {}\n{}",
            header("Next Relay task"),
            issue.number,
            issue.title,
            issue.url
        ),
        None => format!(
            "{}No open GitHub Issue is explicitly labelled `{READY}`.",
            header("Next Relay task")
        ),
    };
    success(
        "task.next",
        human,
        serde_json::json!({"repository": REPOSITORY, "issue": selected}),
    )
}

fn decode_issues(bytes: &[u8]) -> Result<Vec<GitHubIssue>, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::MalformedProviderOutput)
}

fn select_next(mut issues: Vec<GitHubIssue>) -> Option<SelectedIssue> {
    // The API's ordering is not a queue contract; issue number is stable across calls.
    issues.sort_by_key(|issue| issue.number);
    issues.into_iter().find_map(|issue| {
        if issue.pull_request.is_some() {
            return None;
        }
        let labels = issue
            .labels
            .iter()
            .filter_map(|label| match label {
                GitHubLabel::Named { name } => Some(name.as_str()),
                GitHubLabel::Foreign { .. } => None,
            })
            .collect::<Vec<_>>();
        (labels.contains(&READY) && !labels.contains(&CLAIMED) && !labels.contains(&BLOCKED))
            .then_some(SelectedIssue {
                number: issue.number,
                title: issue.title,
                url: issue.html_url,
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn issue(number: u64, labels: &[&str]) -> GitHubIssue {
        GitHubIssue {
            number,
            title: format!("issue {number}"),
            html_url: format!("https://example.test/{number}"),
            labels: labels
                .iter()
                .map(|name| GitHubLabel::Named {
                    name: (*name).into(),
                })
                .collect(),
            pull_request: None,
        }
    }

    #[test]
    fn selects_lowest_numbered_explicitly_ready_issue() {
        assert_eq!(
            select_next(vec![issue(9, &[READY]), issue(2, &[READY])])
                .unwrap()
                .number,
            2
        );
    }

    #[test]
    fn skips_claimed_blocked_pull_requests_and_foreign_labels() {
        let mut pull = issue(1, &[READY]);
        pull.pull_request = Some(serde_json::json!({}));
        let mut foreign = issue(2, &[READY]);
        foreign.labels.push(GitHubLabel::Foreign {
            _value: serde_json::json!({"unexpected": true}),
        });
        assert_eq!(
            select_next(vec![
                pull,
                issue(3, &[READY, CLAIMED]),
                issue(4, &[READY, BLOCKED]),
                foreign,
                issue(5, &[READY])
            ])
            .unwrap()
            .number,
            2
        );
    }

    #[test]
    fn returns_none_without_an_unclaimed_ready_issue() {
        assert!(
            select_next(vec![
                issue(1, &[]),
                issue(2, &[CLAIMED]),
                issue(3, &[BLOCKED])
            ])
            .is_none()
        );
    }

    #[test]
    fn rejects_malformed_github_api_response() {
        assert!(matches!(
            decode_issues(b"not json"),
            Err(Error::MalformedProviderOutput)
        ));
    }

    struct FakeClaims {
        issue: GitHubIssue,
        refs: BTreeSet<String>,
        reads: usize,
        becomes_ineligible_on_second_read: bool,
        visibility_fails: bool,
        verification_fails: bool,
        read_fails: bool,
        block_remove_failures: usize,
        block_calls: usize,
        visible_claims: usize,
        comments: Vec<String>,
    }

    impl ClaimBackend for FakeClaims {
        fn read_issue(&mut self, _: u64) -> Result<GitHubIssue, ()> {
            if self.read_fails {
                return Err(());
            }
            self.reads += 1;
            let mut result = self.issue.clone();
            if self.becomes_ineligible_on_second_read && self.reads == 2 {
                result.labels.push(GitHubLabel::Named {
                    name: CLAIMED.into(),
                });
            }
            Ok(result)
        }

        fn default_branch_sha(&mut self) -> Result<String, ()> {
            Ok("base".into())
        }

        fn create_ref(&mut self, reference: &str, _: &str) -> Result<RefCreate, ()> {
            Ok(if self.refs.insert(reference.into()) {
                RefCreate::Created
            } else {
                RefCreate::Exists
            })
        }

        fn mark_claimed(&mut self, _: u64, worker: &str, reference: &str) -> Result<(), ()> {
            if self.visibility_fails {
                return Err(());
            }
            self.issue
                .labels
                .retain(|label| !matches!(label, GitHubLabel::Named { name } if name == READY));
            self.issue.labels.push(GitHubLabel::Named {
                name: CLAIMED.into(),
            });
            self.comments.push(format!(
                "Relay task claim: worker={worker}; ref={reference}"
            ));
            self.visible_claims += 1;
            Ok(())
        }

        fn verify_visible_claim(
            &mut self,
            _: u64,
            worker: &str,
            reference: &str,
        ) -> Result<bool, ()> {
            let labels = self
                .issue
                .labels
                .iter()
                .filter_map(|label| match label {
                    GitHubLabel::Named { name } => Some(name.as_str()),
                    GitHubLabel::Foreign { .. } => None,
                })
                .collect::<Vec<_>>();
            Ok(!self.verification_fails
                && labels.contains(&CLAIMED)
                && !labels.contains(&READY)
                && self.comments.iter().any(|comment| {
                    comment == &format!("Relay task claim: worker={worker}; ref={reference}")
                }))
        }

        fn claim_state(&mut self, number: u64, reference: &str) -> Result<RemoteClaimState, ()> {
            if number != self.issue.number
                || !self.refs.contains(reference)
                || !self.comments.iter().any(|comment| {
                    comment.starts_with("Relay task claim: worker=")
                        && comment.ends_with(&format!("; ref={reference}"))
                })
            {
                return Err(());
            }
            let has = |expected| {
                self.issue
                    .labels
                    .iter()
                    .any(|label| matches!(label, GitHubLabel::Named { name } if name == expected))
            };
            match (has(READY), has(CLAIMED), has(BLOCKED)) {
                (false, true, false) => Ok(RemoteClaimState::Active),
                (false, true, true) => Ok(RemoteClaimState::BlockedPartial),
                (false, false, true) => Ok(RemoteClaimState::Blocked),
                _ => Err(()),
            }
        }

        fn mark_blocked(&mut self, number: u64) -> Result<(), ()> {
            if number != self.issue.number {
                return Err(());
            }
            self.block_calls += 1;
            if !self
                .issue
                .labels
                .iter()
                .any(|label| matches!(label, GitHubLabel::Named { name } if name == BLOCKED))
            {
                self.issue.labels.push(GitHubLabel::Named {
                    name: BLOCKED.into(),
                });
            }
            if self.block_remove_failures > 0 {
                self.block_remove_failures -= 1;
                return Err(());
            }
            self.issue
                .labels
                .retain(|label| !matches!(label, GitHubLabel::Named { name } if name == CLAIMED));
            Ok(())
        }
    }

    fn claims(issue: GitHubIssue) -> FakeClaims {
        FakeClaims {
            issue,
            refs: BTreeSet::new(),
            reads: 0,
            becomes_ineligible_on_second_read: false,
            visibility_fails: false,
            verification_fails: false,
            read_fails: false,
            block_remove_failures: 0,
            block_calls: 0,
            visible_claims: 0,
            comments: Vec::new(),
        }
    }

    #[test]
    fn only_one_worker_can_create_a_deterministic_claim_ref() {
        let mut backend = claims(issue(7, &[READY]));
        // Model the concurrent window after worker A acquired the ref but before its visibility
        // transaction completed. The Issue remains ready, so worker B reaches the remote ref
        // creation and is rejected there rather than merely observing a changed label.
        backend.visibility_fails = true;
        assert_eq!(
            claim(&mut backend, 7, "worker-a"),
            ClaimOutcome::RecoveryRequired
        );
        backend.visibility_fails = false;
        assert_eq!(claim(&mut backend, 7, "worker-b"), ClaimOutcome::Contended);
        assert_eq!(backend.visible_claims, 0);
    }

    #[test]
    fn claim_rechecks_eligibility_after_ref_creation() {
        let mut backend = claims(issue(7, &[READY]));
        backend.becomes_ineligible_on_second_read = true;
        assert_eq!(
            claim(&mut backend, 7, "worker-a"),
            ClaimOutcome::RecoveryRequired
        );
        assert!(backend.refs.contains(&claim_ref(7)));
        assert_eq!(backend.visible_claims, 0);
    }

    #[test]
    fn visibility_failure_keeps_a_recoverable_claim_ref_and_never_starts_work() {
        let mut backend = claims(issue(7, &[READY]));
        backend.visibility_fails = true;
        assert_eq!(
            claim(&mut backend, 7, "worker-a"),
            ClaimOutcome::RecoveryRequired
        );
        assert!(backend.refs.contains(&claim_ref(7)));
        assert_eq!(backend.visible_claims, 0);
    }

    #[test]
    fn unverifiable_visible_claim_is_recovery_required() {
        let mut backend = claims(issue(7, &[READY]));
        backend.verification_fails = true;
        assert_eq!(
            claim(&mut backend, 7, "worker-a"),
            ClaimOutcome::RecoveryRequired
        );
    }

    #[test]
    fn worker_identity_is_constrained_before_any_remote_operation() {
        assert!(valid_worker("relay-session_123"));
        assert!(!valid_worker("bad worker\ncomment"));
    }

    #[test]
    fn github_read_failure_fails_closed_without_creating_a_ref() {
        let mut backend = claims(issue(7, &[READY]));
        backend.read_fails = true;
        assert_eq!(
            claim(&mut backend, 7, "worker-a"),
            ClaimOutcome::Unavailable
        );
        assert!(backend.refs.is_empty());
    }

    #[test]
    fn already_claimed_or_foreignly_malformed_issue_is_not_claimable() {
        assert_eq!(
            claim(&mut claims(issue(7, &[READY, CLAIMED])), 7, "worker-a"),
            ClaimOutcome::NotEligible
        );
        let mut malformed = issue(7, &[READY]);
        malformed.labels.push(GitHubLabel::Foreign {
            _value: serde_json::json!({"untrusted": true}),
        });
        let mut backend = claims(malformed);
        assert_eq!(claim(&mut backend, 7, "worker-a"), ClaimOutcome::Claimed);
        assert!(backend.issue.labels.iter().any(|label| {
            matches!(label, GitHubLabel::Foreign { _value } if _value == &serde_json::json!({"untrusted": true}))
        }));
    }

    fn claimed_backend(number: u64) -> FakeClaims {
        let mut backend = claims(issue(number, &[CLAIMED]));
        let reference = claim_ref(number);
        backend.refs.insert(reference.clone());
        backend
            .comments
            .push(format!("Relay task claim: worker=worker; ref={reference}"));
        backend
    }

    fn result_store() -> (
        tempfile::TempDir,
        relay_core::handoff::TaskResultStore,
        relay_core::handoff::RelaySessionId,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let session =
            relay_core::handoff::RelaySessionId::parse("11111111-1111-4111-8111-111111111111")
                .unwrap();
        let store =
            relay_core::handoff::TaskResultStore::at_session_dir(dir.path().join("session"));
        (dir, store, session)
    }

    fn result_record(
        issue: u64,
        reference: String,
        session: relay_core::handoff::RelaySessionId,
        kind: TaskResultKind,
        summary: &str,
        now: u64,
    ) -> TaskResult {
        TaskResult::new(issue, reference, session, kind, summary.into(), now).unwrap()
    }

    #[test]
    fn explicit_continuing_blocked_and_completed_results_are_durable_and_deterministic() {
        for (kind, expected) in [
            (TaskResultKind::Continuing, "continuing"),
            (TaskResultKind::Blocked, "blocked"),
            (TaskResultKind::Completed, "completed"),
        ] {
            let (_dir, store, session) = result_store();
            let record = record_verified_result(
                &mut claimed_backend(21),
                &store,
                result_record(
                    21,
                    claim_ref(21),
                    session,
                    kind,
                    "explicit operator attestation",
                    42,
                ),
            )
            .unwrap();
            let output = task_result_output(&record).unwrap();
            assert_eq!(output.json["data"]["result"], expected);
            assert_eq!(output.json["data"]["recorded_unix_ms"], 42);
            assert!(output.human.contains(expected));
            assert_eq!(store.load().unwrap().unwrap(), record);
        }
    }

    #[test]
    fn result_refuses_wrong_issue_ref_session_or_stale_claim_without_writing() {
        let (_dir, store, session) = result_store();
        let mut backend = claimed_backend(21);
        assert!(
            record_verified_result(
                &mut backend,
                &store,
                result_record(
                    21,
                    "refs/heads/relay/claims/22".into(),
                    session.clone(),
                    TaskResultKind::Continuing,
                    "x",
                    1
                )
            )
            .is_err()
        );
        assert!(store.load().unwrap().is_none());

        let mut wrong_issue = claimed_backend(22);
        assert!(
            record_verified_result(
                &mut wrong_issue,
                &store,
                result_record(
                    21,
                    claim_ref(21),
                    session.clone(),
                    TaskResultKind::Continuing,
                    "x",
                    1
                )
            )
            .is_err()
        );
        assert!(store.load().unwrap().is_none());

        let other =
            relay_core::handoff::RelaySessionId::parse("22222222-2222-4222-8222-222222222222")
                .unwrap();
        store
            .write(
                TaskResult::new(
                    21,
                    claim_ref(21),
                    other,
                    TaskResultKind::Continuing,
                    "prior".into(),
                    1,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(
            record_verified_result(
                &mut claimed_backend(21),
                &store,
                result_record(
                    21,
                    claim_ref(21),
                    session,
                    TaskResultKind::Completed,
                    "x",
                    2
                )
            )
            .is_err()
        );

        let (_dir, empty, session) = result_store();
        assert!(
            record_verified_result(
                &mut claims(issue(21, &[CLAIMED])),
                &empty,
                result_record(21, claim_ref(21), session, TaskResultKind::Blocked, "x", 1)
            )
            .is_err()
        );
        assert!(empty.load().unwrap().is_none());
    }

    #[test]
    fn recording_a_result_never_mutates_the_github_claim_lifecycle() {
        let (_dir, store, session) = result_store();
        let mut backend = claimed_backend(21);
        let labels_before = backend
            .issue
            .labels
            .iter()
            .filter_map(|label| match label {
                GitHubLabel::Named { name } => Some(name.clone()),
                GitHubLabel::Foreign { .. } => None,
            })
            .collect::<Vec<_>>();
        let refs_before = backend.refs.clone();
        let comments_before = backend.comments.clone();
        record_verified_result(
            &mut backend,
            &store,
            result_record(
                21,
                claim_ref(21),
                session,
                TaskResultKind::Completed,
                "explicit completion evidence",
                7,
            ),
        )
        .unwrap();
        let labels_after = backend
            .issue
            .labels
            .iter()
            .filter_map(|label| match label {
                GitHubLabel::Named { name } => Some(name.clone()),
                GitHubLabel::Foreign { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels_after, labels_before);
        assert_eq!(backend.refs, refs_before);
        assert_eq!(backend.comments, comments_before);
    }

    fn stored_evidence(
        kind: TaskResultKind,
    ) -> (
        tempfile::TempDir,
        relay_core::handoff::TaskResultStore,
        relay_core::handoff::RelaySessionId,
    ) {
        let (dir, store, session) = result_store();
        store
            .write(result_record(
                21,
                claim_ref(21),
                session.clone(),
                kind,
                "explicit",
                1,
            ))
            .unwrap();
        (dir, store, session)
    }

    #[test]
    fn continuing_consumes_exact_evidence_without_a_lifecycle_mutation() {
        let (_dir, store, session) = stored_evidence(TaskResultKind::Continuing);
        let evidence = validated_result_evidence(&store, 21, &claim_ref(21), &session).unwrap();
        let mut backend = claimed_backend(21);
        let labels = backend.issue.labels.clone();
        let refs = backend.refs.clone();
        let comments = backend.comments.clone();
        assert_eq!(
            consume_result(&mut backend, evidence).unwrap(),
            ResultApplyOutcome::Continuing
        );
        assert_eq!(backend.issue.labels, labels);
        assert_eq!(backend.refs, refs);
        assert_eq!(backend.comments, comments);
        assert_eq!(backend.block_calls, 0);
        let output =
            task_apply_output(21, &claim_ref(21), &ResultApplyOutcome::Continuing).unwrap();
        assert_eq!(output.json["command"], "task.apply_result");
        assert_eq!(output.json["data"]["result"], "continuing");
        assert_eq!(output.json["data"]["lifecycle_mutated"], false);
    }

    #[test]
    fn blocked_requires_exact_blocked_evidence_and_preserves_foreign_labels() {
        let (_dir, store, session) = stored_evidence(TaskResultKind::Blocked);
        let evidence = validated_result_evidence(&store, 21, &claim_ref(21), &session).unwrap();
        let mut backend = claimed_backend(21);
        backend.issue.labels.push(GitHubLabel::Named {
            name: "foreign:keep".into(),
        });
        let refs = backend.refs.clone();
        let comments = backend.comments.clone();
        assert_eq!(
            consume_result(&mut backend, evidence).unwrap(),
            ResultApplyOutcome::Blocked
        );
        assert!(
            backend
                .issue
                .labels
                .iter()
                .any(|label| matches!(label, GitHubLabel::Named { name } if name == BLOCKED))
        );
        assert!(
            !backend
                .issue
                .labels
                .iter()
                .any(|label| matches!(label, GitHubLabel::Named { name } if name == CLAIMED))
        );
        assert!(
            backend.issue.labels.iter().any(
                |label| matches!(label, GitHubLabel::Named { name } if name == "foreign:keep")
            )
        );
        assert_eq!(backend.refs, refs);
        assert_eq!(backend.comments, comments);
    }

    #[test]
    fn completed_requires_exact_completed_evidence_and_stops_at_review_boundary() {
        let (_dir, store, session) = stored_evidence(TaskResultKind::Completed);
        let evidence = validated_result_evidence(&store, 21, &claim_ref(21), &session).unwrap();
        let mut backend = claimed_backend(21);
        let labels = backend.issue.labels.clone();
        let refs = backend.refs.clone();
        let comments = backend.comments.clone();
        assert_eq!(
            consume_result(&mut backend, evidence).unwrap(),
            ResultApplyOutcome::CompletionReviewContractMissing
        );
        assert_eq!(backend.issue.labels, labels);
        assert_eq!(backend.refs, refs);
        assert_eq!(backend.comments, comments);
        assert_eq!(backend.block_calls, 0);
    }

    #[test]
    fn result_consumer_rejects_wrong_binding_missing_or_corrupt_evidence() {
        let (_dir, store, session) = stored_evidence(TaskResultKind::Blocked);
        assert!(validated_result_evidence(&store, 22, &claim_ref(22), &session).is_err());
        assert!(validated_result_evidence(&store, 21, &claim_ref(22), &session).is_err());
        let other =
            relay_core::handoff::RelaySessionId::parse("22222222-2222-4222-8222-222222222222")
                .unwrap();
        assert!(validated_result_evidence(&store, 21, &claim_ref(21), &other).is_err());

        let (dir, empty, session) = result_store();
        assert!(validated_result_evidence(&empty, 21, &claim_ref(21), &session).is_err());
        std::fs::create_dir_all(dir.path().join("session")).unwrap();
        std::fs::write(dir.path().join("session/task_result.json"), b"not-json").unwrap();
        assert!(validated_result_evidence(&empty, 21, &claim_ref(21), &session).is_err());
    }

    #[test]
    fn result_consumer_rejects_stale_claim_and_wrong_result_state() {
        let (_dir, store, session) = stored_evidence(TaskResultKind::Blocked);
        let evidence = validated_result_evidence(&store, 21, &claim_ref(21), &session).unwrap();
        assert!(consume_result(&mut claims(issue(21, &[CLAIMED])), evidence).is_err());

        let (_dir, store, session) = stored_evidence(TaskResultKind::Continuing);
        let evidence = validated_result_evidence(&store, 21, &claim_ref(21), &session).unwrap();
        let mut backend = claimed_backend(21);
        assert_eq!(
            consume_result(&mut backend, evidence).unwrap(),
            ResultApplyOutcome::Continuing
        );
        assert!(
            !backend
                .issue
                .labels
                .iter()
                .any(|label| matches!(label, GitHubLabel::Named { name } if name == BLOCKED))
        );
    }

    #[test]
    fn blocked_transition_is_idempotent_and_recovers_only_its_partial_state() {
        let (_dir, store, session) = stored_evidence(TaskResultKind::Blocked);
        let evidence = validated_result_evidence(&store, 21, &claim_ref(21), &session).unwrap();
        let mut backend = claimed_backend(21);
        backend.block_remove_failures = 1;
        assert!(consume_result(&mut backend, evidence.clone()).is_err());
        assert_eq!(
            backend.claim_state(21, &claim_ref(21)).unwrap(),
            RemoteClaimState::BlockedPartial
        );
        assert_eq!(backend.block_calls, 1);

        assert_eq!(
            consume_result(&mut backend, evidence.clone()).unwrap(),
            ResultApplyOutcome::Blocked
        );
        assert_eq!(
            backend.claim_state(21, &claim_ref(21)).unwrap(),
            RemoteClaimState::Blocked
        );
        assert_eq!(backend.block_calls, 2);

        assert_eq!(
            consume_result(&mut backend, evidence).unwrap(),
            ResultApplyOutcome::Blocked
        );
        assert_eq!(backend.block_calls, 2);
    }
}
