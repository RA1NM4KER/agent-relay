//! M3.1 test harness: exercises the Herdr adapter end to end against a scripted `relay` process,
//! never a real Claude account, real Herdr socket, or real profile registry. `@example.com`
//! identities and synthetic profile names only (`alice`/`bob`), per the M3 safety boundary.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use relay_herdr::actions::{self, WatchEvaluateRequest};
use relay_herdr::client::{RelayClient, ScriptedCommandRunner, ScriptedResponse};
use relay_herdr::mapping;
use relay_herdr::{HerdrAdapter, HerdrIntegrationError, HerdrPaneContext};
use serde_json::json;

/// `/usr/bin/true` exists, is root-owned, and is not group/other-writable on every macOS/Linux CI
/// runner, so it satisfies `RelayClient`'s executable safety check without needing a real `relay`
/// binary — the `ScriptedCommandRunner` never actually executes it.
fn fixture_executable() -> PathBuf {
    PathBuf::from("/usr/bin/true")
}

fn client_with(responses: Vec<ScriptedResponse>) -> RelayClient<ScriptedCommandRunner> {
    RelayClient::with_runner(fixture_executable(), ScriptedCommandRunner::new(responses))
        .expect("fixture executable must validate")
}

fn claude_pane(
    pane_tokens: &[(&str, &str)],
    workspace_tokens: &[(&str, &str)],
) -> HerdrPaneContext {
    HerdrPaneContext {
        pane_id: "pane-1".to_owned(),
        working_directory: PathBuf::from("/home/alice/project"),
        agent: Some("claude".to_owned()),
        agent_session_id: Some("11111111-1111-4111-8111-111111111111".to_owned()),
        pane_tokens: pane_tokens
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        workspace_tokens: workspace_tokens
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

fn healthy_profile_status_json(config_dir: &str) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "ok": true,
        "command": "profile.status",
        "data": {
            "profile": {
                "name": "alice",
                "provider": "claude",
                "config_dir": config_dir,
                "enabled": true,
                "origin": "adopted",
                "expected_identity": { "stable_id": "email=alice@example.com", "display_label": null },
                "last_availability": { "state": "AVAILABLE", "source": "test", "observed_unix_ms": 0, "reset_unix_ms": null }
            },
            "authentication": "authenticated",
            "observed_identity": { "stable_id": "email=alice@example.com", "display_label": null },
            "availability": { "state": "AVAILABLE", "source": "test", "observed_unix_ms": 0, "reset_unix_ms": null },
            "identity_matches": true
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Mapping scenarios
// ---------------------------------------------------------------------------------------------

#[test]
fn happy_path_mapping_resolves_the_pane_token() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![ScriptedResponse::Success(
        healthy_profile_status_json("/home/alice/.config/agent-relay/profiles/alice/claude"),
    )]);

    let resolved = mapping::resolve_profile(&pane, &client).expect("mapping succeeds");
    assert_eq!(resolved.name, "alice");
}

#[test]
fn unknown_relay_profile_fails_closed() {
    let pane = claude_pane(&[("relay_profile", "ghost")], &[]);
    let client = client_with(vec![ScriptedResponse::RelayError {
        code: "profile_not_found".to_owned(),
        message: "profile was not found: ghost".to_owned(),
    }]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::ProfileMappingUnknown);
}

#[test]
fn disabled_or_identity_mismatched_profile_is_treated_as_unknown() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let mut stale = healthy_profile_status_json("/x");
    stale["data"]["identity_matches"] = json!(false);
    let client = client_with(vec![ScriptedResponse::Success(stale)]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::ProfileMappingUnknown);
}

#[test]
fn ambiguous_pane_and_workspace_tokens_fail_closed() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[("relay_profile", "bob")]);
    // No relay invocation should even happen: ambiguity is caught before any subprocess call.
    let client = client_with(vec![]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must fail closed");
    match error {
        HerdrIntegrationError::ProfileMappingAmbiguous { candidates } => {
            assert_eq!(candidates, vec!["alice".to_owned(), "bob".to_owned()]);
        }
        other => panic!("expected ProfileMappingAmbiguous, got {other:?}"),
    }
}

#[test]
fn agreeing_pane_and_workspace_tokens_are_not_ambiguous() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[("relay_profile", "alice")]);
    let client = client_with(vec![ScriptedResponse::Success(
        healthy_profile_status_json("/x"),
    )]);

    let resolved = mapping::resolve_profile(&pane, &client).expect("agreement resolves cleanly");
    assert_eq!(resolved.name, "alice");
}

#[test]
fn workspace_token_is_the_fallback_default() {
    let pane = claude_pane(&[], &[("relay_profile", "bob")]);
    let client = client_with(vec![ScriptedResponse::Success(
        healthy_profile_status_json("/x"),
    )]);

    // The resolved name comes from the token itself (mapping never trusts a name embedded in the
    // JSON body over the token it looked up), so this also proves the *workspace* token was the
    // one consulted when no pane-level token exists.
    let resolved = mapping::resolve_profile(&pane, &client).expect("workspace token used");
    assert_eq!(resolved.name, "bob");
}

#[test]
fn no_token_at_all_is_profile_mapping_unknown() {
    let pane = claude_pane(&[], &[]);
    let client = client_with(vec![]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::ProfileMappingUnknown);
}

#[test]
fn non_claude_pane_is_refused_before_any_relay_call() {
    let mut pane = claude_pane(&[("relay_profile", "alice")], &[]);
    pane.agent = Some("cursor".to_owned());
    let client = client_with(vec![]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must refuse");
    assert_eq!(error, HerdrIntegrationError::NonClaudePane);
}

#[test]
fn pane_with_no_detected_agent_is_also_non_claude() {
    let mut pane = claude_pane(&[("relay_profile", "alice")], &[]);
    pane.agent = None;
    let client = client_with(vec![]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must refuse");
    assert_eq!(error, HerdrIntegrationError::NonClaudePane);
}

// ---------------------------------------------------------------------------------------------
// Action scenarios: status/doctor
// ---------------------------------------------------------------------------------------------

#[test]
fn status_action_composes_profile_and_lock_state_when_healthy() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "lock.status",
            "data": {
                "project_id": "proj-1",
                "locked": false,
                "lease": { "owner_profile": "alice" },
                "current_transaction": null
            }
        })),
    ]);

    let status = actions::status(&pane, &client).expect("status composes");
    assert_eq!(status.profile.name, "alice");
    assert_eq!(status.profile_status.authentication, "authenticated");
    assert!(!status.lock.locked);
    assert_eq!(status.lock.lease_owner.as_deref(), Some("alice"));
}

#[test]
fn doctor_action_reports_relay_reported_health() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "profile.doctor",
            "data": {
                "profile": "alice",
                "healthy": true,
                "checks": [{ "name": "directory_security", "passed": true, "message": "ok" }]
            }
        })),
    ]);

    let report = actions::doctor(&pane, &client).expect("doctor composes");
    assert!(report.healthy);
    assert_eq!(report.checks.len(), 1);
}

#[test]
fn doctor_action_propagates_mapping_failure_without_calling_relay_doctor() {
    let pane = claude_pane(&[], &[]);
    let client = client_with(vec![]);

    let error = actions::doctor(&pane, &client).expect_err("mapping fails first");
    assert_eq!(error, HerdrIntegrationError::ProfileMappingUnknown);
}

// ---------------------------------------------------------------------------------------------
// Usage-state passthrough (Relay healthy / UNKNOWN / NEAR_LIMIT / EXHAUSTED / WAITING_FOR_CAPACITY)
// via `watch_evaluate`'s typed outcome — Relay's own policy decides these, the adapter only
// reports them.
// ---------------------------------------------------------------------------------------------

fn watch_request<'a>(fallback: &'a [String]) -> WatchEvaluateRequest<'a> {
    WatchEvaluateRequest {
        fallback_profiles: fallback,
        dry_run: true,
        workload_model: None,
        simulate_usage: None,
    }
}

#[test]
fn watch_evaluate_reports_no_action_needed_when_healthy_or_unknown() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let fallback = vec!["bob".to_owned()];
    for usage in ["AVAILABLE", "UNKNOWN"] {
        let client = client_with(vec![
            ScriptedResponse::Success(healthy_profile_status_json("/x")),
            ScriptedResponse::Success(json!({
                "schema_version": 1, "ok": true, "command": "watch.run",
                "data": { "outcome": "no_action_needed", "source_usage": usage }
            })),
        ]);
        let outcome = actions::watch_evaluate(&pane, &client, &watch_request(&fallback))
            .expect("no-action outcome parses");
        match outcome {
            actions::WatchRunView::NoActionNeeded { source_usage } => {
                assert_eq!(source_usage, usage);
            }
            other => panic!("expected NoActionNeeded, got {other:?}"),
        }
    }
}

#[test]
fn watch_evaluate_reports_near_limit_via_no_action_needed() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let fallback = vec!["bob".to_owned()];
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "watch.run",
            "data": { "outcome": "no_action_needed", "source_usage": "NEAR_LIMIT" }
        })),
    ]);
    let outcome = actions::watch_evaluate(&pane, &client, &watch_request(&fallback)).unwrap();
    match outcome {
        actions::WatchRunView::NoActionNeeded { source_usage } => {
            assert_eq!(source_usage, "NEAR_LIMIT");
            assert_eq!(
                relay_herdr::usage_interop::acting_party(
                    relay_herdr::usage_interop::ReportedUsageState::NearLimit
                ),
                relay_herdr::usage_interop::ActingParty::InformationalOnly
            );
        }
        other => panic!("expected NoActionNeeded, got {other:?}"),
    }
}

#[test]
fn watch_evaluate_reports_exhausted_handoff() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let fallback = vec!["bob".to_owned()];
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "watch.run",
            "data": {
                "outcome": "handoff",
                "target": "bob",
                "journal": { "transaction_id": "ho-1", "state": { "state": "COMPLETE" } }
            }
        })),
    ]);
    let outcome = actions::watch_evaluate(&pane, &client, &watch_request(&fallback)).unwrap();
    match outcome {
        actions::WatchRunView::Handoff { target, journal } => {
            assert_eq!(target, "bob");
            assert_eq!(journal.state.state, "COMPLETE");
        }
        other => panic!("expected Handoff, got {other:?}"),
    }
}

#[test]
fn watch_evaluate_reports_waiting_for_capacity() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let fallback = vec!["bob".to_owned()];
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "watch.run",
            "data": { "outcome": "waiting_for_capacity", "reason": "no eligible target" }
        })),
    ]);
    let outcome = actions::watch_evaluate(&pane, &client, &watch_request(&fallback)).unwrap();
    assert!(matches!(
        outcome,
        actions::WatchRunView::WaitingForCapacity { .. }
    ));
}

#[test]
fn watch_evaluate_requires_a_session_id() {
    let mut pane = claude_pane(&[("relay_profile", "alice")], &[]);
    pane.agent_session_id = None;
    let fallback = vec!["bob".to_owned()];
    let client = client_with(vec![ScriptedResponse::Success(
        healthy_profile_status_json("/x"),
    )]);

    let error = actions::watch_evaluate(&pane, &client, &watch_request(&fallback))
        .expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::MissingSessionIdentity);
}

#[test]
fn watch_evaluate_requires_at_least_one_fallback() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let fallback: Vec<String> = vec![];
    let client = client_with(vec![ScriptedResponse::Success(
        healthy_profile_status_json("/x"),
    )]);

    let error = actions::watch_evaluate(&pane, &client, &watch_request(&fallback))
        .expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::ProfileMappingUnknown);
}

// ---------------------------------------------------------------------------------------------
// Manual handoff / target conflict
// ---------------------------------------------------------------------------------------------

#[test]
fn manual_handoff_reports_a_completed_journal() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "handoff.run",
            "data": { "transaction_id": "ho-2", "state": { "state": "COMPLETE" } }
        })),
    ]);

    let journal = actions::handoff_manual(&pane, &client, "bob").expect("handoff completes");
    assert_eq!(journal.transaction_id, "ho-2");
    assert_eq!(journal.state.state, "COMPLETE");
}

#[test]
fn manual_handoff_surfaces_target_artifact_conflict_verbatim() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::RelayError {
            code: "target_artifact_diverges".to_owned(),
            message: "target already holds a diverging transcript".to_owned(),
        },
    ]);

    let error = actions::handoff_manual(&pane, &client, "bob").expect_err("must surface refusal");
    match error {
        HerdrIntegrationError::RelayRefused { code, .. } => {
            assert_eq!(code, "target_artifact_diverges");
        }
        other => panic!("expected RelayRefused, got {other:?}"),
    }
}

#[test]
fn manual_handoff_requires_a_session_id() {
    let mut pane = claude_pane(&[("relay_profile", "alice")], &[]);
    pane.agent_session_id = None;
    let client = client_with(vec![ScriptedResponse::Success(
        healthy_profile_status_json("/x"),
    )]);

    let error = actions::handoff_manual(&pane, &client, "bob").expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::MissingSessionIdentity);
}

// ---------------------------------------------------------------------------------------------
// Recovery status
// ---------------------------------------------------------------------------------------------

#[test]
fn recovery_status_reports_recovery_required_state() {
    let pane = claude_pane(&[], &[]); // deliberately no profile token: must not be required
    let client = client_with(vec![
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "lock.status",
            "data": {
                "project_id": "proj-1",
                "locked": false,
                "lease": { "owner_profile": "alice" },
                "current_transaction": "ho-3"
            }
        })),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "handoff.status",
            "data": {
                "transaction_id": "ho-3",
                "state": { "state": "RECOVERY_REQUIRED", "reason": "target starting interrupted" }
            }
        })),
    ]);

    let recovery = actions::recovery_status(&pane, &client).expect("recovery status composes");
    assert_eq!(
        recovery.transaction_state.as_deref(),
        Some("RECOVERY_REQUIRED")
    );
    assert!(!recovery.is_terminal);
    assert_eq!(
        recovery.transaction_reason.as_deref(),
        Some("target starting interrupted")
    );
}

#[test]
fn recovery_status_with_no_transaction_on_record_is_terminal() {
    let pane = claude_pane(&[], &[]);
    let client = client_with(vec![ScriptedResponse::Success(json!({
        "schema_version": 1, "ok": true, "command": "lock.status",
        "data": {
            "project_id": "proj-1",
            "locked": false,
            "lease": null,
            "current_transaction": null
        }
    }))]);

    let recovery = actions::recovery_status(&pane, &client).expect("recovery status composes");
    assert!(recovery.is_terminal);
    assert!(recovery.transaction_id.is_none());
    assert!(recovery.lease_owner.is_none());
}

// ---------------------------------------------------------------------------------------------
// Transport failure modes
// ---------------------------------------------------------------------------------------------

#[test]
fn malformed_relay_json_fails_closed() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![ScriptedResponse::Success(json!("not an envelope"))]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::RelayMalformedOutput);
}

#[test]
fn malformed_relay_error_stderr_fails_closed() {
    let pane = claude_pane(&[("relay_profile", "alice")], &[]);
    let client = client_with(vec![ScriptedResponse::MalformedFailure(
        b"panic: index out of bounds".to_vec(),
    )]);

    let error = mapping::resolve_profile(&pane, &client).expect_err("must fail closed");
    assert_eq!(error, HerdrIntegrationError::RelayMalformedOutput);
}

#[test]
fn relay_executable_absent_fails_closed() {
    let missing = PathBuf::from("/this/path/does/not/exist/relay");
    let error = RelayClient::with_runner(missing, ScriptedCommandRunner::new(vec![]))
        .expect_err("nonexistent executable must be refused");
    assert_eq!(error, HerdrIntegrationError::RelayExecutableMissing);
}

#[test]
fn herdr_metadata_unavailable_is_a_distinct_error_from_relay_errors() {
    // No RelayClient call happens for this scenario at all: a `HerdrAdapter::focused_pane`
    // implementation is expected to return this directly when Herdr's own socket/CLI is
    // unreachable, before any pane/mapping/action code runs.
    struct AlwaysUnavailable;
    impl relay_herdr::HerdrAdapter for AlwaysUnavailable {
        fn focused_pane(&self) -> Result<HerdrPaneContext, HerdrIntegrationError> {
            Err(HerdrIntegrationError::HerdrMetadataUnavailable)
        }
    }
    let adapter = AlwaysUnavailable;
    let error = adapter
        .focused_pane()
        .expect_err("must surface Herdr-side unavailability");
    assert_eq!(error, HerdrIntegrationError::HerdrMetadataUnavailable);
}

// ---------------------------------------------------------------------------------------------
// Concurrency: the adapter must never bypass Relay's own writer authority. Two simultaneous
// evaluate attempts each just invoke `relay watch run` independently; whichever one Relay's own
// orchestration lock admits wins, and the adapter must report the loser's outcome verbatim
// rather than retrying around it or racing a second attempt itself.
// ---------------------------------------------------------------------------------------------

#[test]
fn two_simultaneous_evaluate_attempts_each_defer_to_relays_own_lock_outcome() {
    let pane = Arc::new(claude_pane(&[("relay_profile", "alice")], &[]));
    let fallback = Arc::new(vec!["bob".to_owned()]);
    let barrier = Arc::new(Barrier::new(2));

    // Attempt A: Relay reports it actually performed the handoff.
    let client_a = Arc::new(client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "watch.run",
            "data": {
                "outcome": "handoff",
                "target": "bob",
                "journal": { "transaction_id": "ho-4", "state": { "state": "COMPLETE" } }
            }
        })),
    ]));
    // Attempt B: Relay's own orchestration lock reports the other one is already in flight.
    let client_b = Arc::new(client_with(vec![
        ScriptedResponse::Success(healthy_profile_status_json("/x")),
        ScriptedResponse::Success(json!({
            "schema_version": 1, "ok": true, "command": "watch.run",
            "data": { "outcome": "transaction_in_flight", "transaction_id": "ho-4" }
        })),
    ]));

    let handles: Vec<_> = [client_a, client_b]
        .into_iter()
        .map(|client| {
            let pane = Arc::clone(&pane);
            let fallback = Arc::clone(&fallback);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                actions::watch_evaluate(
                    &pane,
                    &client,
                    &WatchEvaluateRequest {
                        fallback_profiles: &fallback,
                        dry_run: false,
                        workload_model: None,
                        simulate_usage: None,
                    },
                )
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let outcomes: Vec<&str> = results
        .iter()
        .map(|result| {
            match result
                .as_ref()
                .expect("both calls succeed at the transport level")
            {
                actions::WatchRunView::Handoff { .. } => "handoff",
                actions::WatchRunView::TransactionInFlight { .. } => "in_flight",
                other => panic!("unexpected outcome: {other:?}"),
            }
        })
        .collect();

    // The adapter performed exactly the two independent calls it was told to and reported each
    // outcome verbatim — one handoff, one deferral — never suppressing, merging, or retrying
    // around Relay's own serialization of the two attempts.
    assert!(outcomes.contains(&"handoff"));
    assert!(outcomes.contains(&"in_flight"));
}

/// Sanity check that the token map type itself is what `HerdrPaneContext` documents.
#[test]
fn pane_tokens_are_a_plain_string_map() {
    let mut tokens: BTreeMap<String, String> = BTreeMap::new();
    tokens.insert("relay_profile".to_owned(), "alice".to_owned());
    let pane = HerdrPaneContext {
        pane_id: "p".to_owned(),
        working_directory: PathBuf::from("/tmp/project"),
        agent: Some("claude".to_owned()),
        agent_session_id: None,
        pane_tokens: tokens,
        workspace_tokens: BTreeMap::new(),
    };
    assert!(pane.is_claude_pane());
}
