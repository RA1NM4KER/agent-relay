//! Deterministic tests for `relay integration herdr install|status|doctor|uninstall`, against a
//! scripted `herdr` process — never a real Herdr server. Fixture JSON mirrors exactly what was
//! captured from a real Herdr 0.9.0 server during M3.2 live validation, including the per-command
//! `--json`-flag inconsistency that validation caught (`pane get`/`workspace get`/`plugin link`/
//! `plugin unlink` emit JSON-RPC-shaped output unconditionally and reject `--json`; `plugin list`
//! requires it explicitly and prints human text otherwise) — the fixtures below intentionally do
//! not encode that distinction (`ScriptedCommandRunner` ignores the argv it's given), so what
//! actually guards against regressing that bug is `herdr_client.rs`'s `run_json` call sites
//! themselves, not these tests. These tests instead lock in `install.rs`'s logic once the
//! flag is right.

use std::path::PathBuf;

use relay_herdr::client::{ScriptedCommandRunner, ScriptedResponse};
use relay_herdr::herdr_client::HerdrCliClient;
use relay_herdr::install;
use serde_json::json;

fn fixture_executable() -> PathBuf {
    PathBuf::from("/usr/bin/true")
}

fn client_with(responses: Vec<ScriptedResponse>) -> HerdrCliClient<ScriptedCommandRunner> {
    HerdrCliClient::with_runner(fixture_executable(), ScriptedCommandRunner::new(responses))
}

fn herdr_status_json(running: bool, compatible: bool) -> serde_json::Value {
    json!({
        "client": { "version": "0.9.0", "channel": "stable", "protocol": 22 },
        "server": {
            "status": if running { "running" } else { "stopped" },
            "running": running,
            "version": "0.9.0",
            "compatible": compatible
        },
        "update": { "restart_needed": false }
    })
}

fn plugin_record_json(enabled: bool, min_version: &str) -> serde_json::Value {
    json!({
        "plugin_id": "agent-relay",
        "name": "Agent Relay",
        "version": "0.1.0",
        "min_herdr_version": min_version,
        "enabled": enabled,
        "manifest_path": "/repo/plugins/herdr/herdr-plugin.toml"
    })
}

// Herdr's own JSON-RPC-ish envelope for a `plugin list` result (only meaningful with the
// `--json` flag this crate now knows to pass — see the module doc).
fn plugin_list_envelope(plugins: Vec<serde_json::Value>) -> serde_json::Value {
    json!({ "id": "cli:plugin", "result": { "plugins": plugins, "type": "plugin_list" } })
}

#[test]
fn status_reports_unregistered_plugin() {
    let client = client_with(vec![
        ScriptedResponse::Success(herdr_status_json(true, true)),
        ScriptedResponse::Success(plugin_list_envelope(vec![])),
    ]);
    let report = install::status(&client).expect("status composes");
    assert!(report.herdr_server_running);
    assert!(report.herdr_compatible);
    assert!(report.plugin.is_none());
}

#[test]
fn status_reports_registered_plugin() {
    let client = client_with(vec![
        ScriptedResponse::Success(herdr_status_json(true, true)),
        ScriptedResponse::Success(plugin_list_envelope(vec![plugin_record_json(
            true, "0.9.0",
        )])),
    ]);
    let report = install::status(&client).expect("status composes");
    let plugin = report.plugin.expect("plugin present");
    assert_eq!(plugin.plugin_id, "agent-relay");
    assert!(plugin.enabled);
}

#[test]
fn doctor_is_healthy_when_everything_matches() {
    let client = client_with(vec![
        ScriptedResponse::Success(herdr_status_json(true, true)),
        ScriptedResponse::Success(plugin_list_envelope(vec![plugin_record_json(
            true, "0.9.0",
        )])),
    ]);
    let report = install::doctor(&client).expect("doctor composes");
    assert!(report.healthy);
    assert!(report.checks.iter().all(|check| check.passed));
}

#[test]
fn doctor_flags_incompatible_herdr_server() {
    let client = client_with(vec![
        ScriptedResponse::Success(herdr_status_json(true, false)),
        ScriptedResponse::Success(plugin_list_envelope(vec![plugin_record_json(
            true, "0.9.0",
        )])),
    ]);
    let report = install::doctor(&client).expect("doctor composes even when unhealthy");
    assert!(!report.healthy);
    let server_check = report
        .checks
        .iter()
        .find(|check| check.name == "herdr_server")
        .expect("herdr_server check present");
    assert!(!server_check.passed);
}

#[test]
fn doctor_flags_disabled_plugin() {
    let client = client_with(vec![
        ScriptedResponse::Success(herdr_status_json(true, true)),
        ScriptedResponse::Success(plugin_list_envelope(vec![plugin_record_json(
            false, "0.9.0",
        )])),
    ]);
    let report = install::doctor(&client).expect("doctor composes");
    assert!(!report.healthy);
    let plugin_check = report
        .checks
        .iter()
        .find(|check| check.name == "plugin_registered")
        .expect("plugin_registered check present");
    assert!(!plugin_check.passed);
}

#[test]
fn doctor_flags_version_pin_drift() {
    let client = client_with(vec![
        ScriptedResponse::Success(herdr_status_json(true, true)),
        ScriptedResponse::Success(plugin_list_envelope(vec![plugin_record_json(
            true, "0.9.1",
        )])),
    ]);
    let report = install::doctor(&client).expect("doctor composes");
    assert!(!report.healthy);
    let pin_check = report
        .checks
        .iter()
        .find(|check| check.name == "manifest_version_pin")
        .expect("manifest_version_pin check present");
    assert!(!pin_check.passed);
}

#[test]
fn uninstall_is_idempotent_when_not_registered() {
    let client = client_with(vec![ScriptedResponse::Success(plugin_list_envelope(
        vec![],
    ))]);
    let removed = install::apply_uninstall(&client).expect("uninstall composes");
    assert!(
        !removed,
        "nothing to remove, no unlink call should even happen"
    );
}

#[test]
fn uninstall_removes_a_registered_plugin() {
    let client = client_with(vec![
        ScriptedResponse::Success(plugin_list_envelope(vec![plugin_record_json(
            true, "0.9.0",
        )])),
        ScriptedResponse::Success(json!({
            "id": "cli:plugin",
            "result": { "plugin_id": "agent-relay", "removed": true, "type": "plugin_unlinked" }
        })),
    ]);
    let removed = install::apply_uninstall(&client).expect("uninstall composes");
    assert!(removed);
}

#[test]
fn resolve_plugin_path_fails_closed_when_manifest_missing() {
    let error = install::resolve_plugin_path(Some(std::path::Path::new(
        "/definitely/not/a/real/plugin/dir",
    )))
    .expect_err("must fail closed");
    assert_eq!(
        error,
        relay_herdr::HerdrIntegrationError::HerdrMetadataUnavailable
    );
}

#[test]
fn plan_install_reports_already_linked() {
    let client = client_with(vec![ScriptedResponse::Success(plugin_list_envelope(vec![
        plugin_record_json(true, "0.9.0"),
    ]))]);
    let plan = install::plan_install(&client, std::path::Path::new("plugins/herdr"))
        .expect("plan composes");
    assert!(plan.already_linked);
}

#[test]
fn plan_install_reports_not_yet_linked() {
    let client = client_with(vec![ScriptedResponse::Success(plugin_list_envelope(
        vec![],
    ))]);
    let plan = install::plan_install(&client, std::path::Path::new("plugins/herdr"))
        .expect("plan composes");
    assert!(!plan.already_linked);
}
