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
    let tmp = tempfile::tempdir().expect("tempdir");
    let error = install::resolve_plugin_path(
        Some(std::path::Path::new("/definitely/not/a/real/plugin/dir")),
        tmp.path(),
    )
    .expect_err("must fail closed");
    assert_eq!(
        error,
        relay_herdr::HerdrIntegrationError::HerdrMetadataUnavailable
    );
}

/// M5.5: with no explicit `--plugin-path`, the manifest is materialized from the binary's own
/// embedded copy into `<config_root>/herdr-plugin`, with the override parameter standing in for
/// the sibling-binary discovery a real packaged install performs (`resolve_plugin_path_with_override`
/// takes it directly, instead of `resolve_plugin_path`'s `RELAY_HERDR_PLUGIN_BIN` env read, so this
/// test never mutates process-global env — this workspace forbids `unsafe`, which
/// `std::env::set_var` requires since Rust 2024). No source checkout, no reliance on the current
/// working directory.
#[test]
fn resolve_plugin_path_with_override_materializes_the_embedded_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_root = tmp.path().join("config");
    let resolved = install::resolve_plugin_path_with_override(
        None,
        &config_root,
        Some(std::path::Path::new(
            "/opt/agent-relay/bin/relay-herdr-plugin",
        )),
        None,
    )
    .expect("materializes without a checkout");
    assert_eq!(resolved, config_root.join("herdr-plugin"));
    let manifest = std::fs::read_to_string(resolved.join("herdr-plugin.toml"))
        .expect("manifest file was written");
    assert!(manifest.contains("/opt/agent-relay/bin/relay-herdr-plugin"));
    assert!(!manifest.contains("{{RELAY_HERDR_PLUGIN_BIN}}"));
    assert!(!manifest.contains("../../target/release"));
    let parsed: toml::Value = toml::from_str(&manifest).expect("materialized manifest parses");
    assert!(
        parsed.get("build").is_none(),
        "embedded manifest must not declare a [[build]] step (it is only ever `herdr plugin link`ed, never built from this directory)"
    );
}

/// With no override, no `relay-herdr-plugin` next to the running test binary, and an explicitly
/// empty `PATH` (passed as a parameter rather than the real `PATH`, so this stays deterministic
/// even on a machine — such as this repository's own dev machine — that has a real
/// `relay-herdr-plugin` installed via Homebrew), discovery must fail closed rather than guess.
#[test]
fn resolve_plugin_path_with_override_fails_closed_when_the_plugin_binary_cannot_be_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_root = tmp.path().join("config");
    let empty_path = std::ffi::OsStr::new("");
    let error =
        install::resolve_plugin_path_with_override(None, &config_root, None, Some(empty_path));
    assert_eq!(
        error,
        Err(relay_herdr::HerdrIntegrationError::HerdrMetadataUnavailable)
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

/// M5.5: the embedded manifest (linked for a packaged install) and the repository's own
/// `plugins/herdr/herdr-plugin.toml` (linked directly for local `herdr plugin link` development)
/// must never silently drift apart on anything but `command`/`[[build]]`, which necessarily
/// differ (absolute installed-binary path vs. relative in-repo path, and the repo copy alone
/// keeps `[[build]]` for `herdr plugin install` from a checkout).
#[test]
fn embedded_manifest_matches_the_repository_manifest_except_command_and_build() {
    let repo_manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/herdr/herdr-plugin.toml");
    let repo_manifest = std::fs::read_to_string(&repo_manifest_path)
        .expect("repository plugins/herdr/herdr-plugin.toml is present");

    let repo: toml::Value = toml::from_str(&repo_manifest).expect("repo manifest parses");
    let embedded: toml::Value =
        toml::from_str(install::PLUGIN_MANIFEST_TEMPLATE).expect("embedded template parses");

    for key in [
        "id",
        "name",
        "version",
        "description",
        "min_herdr_version",
        "platforms",
    ] {
        assert_eq!(
            repo.get(key),
            embedded.get(key),
            "top-level field '{key}' drifted between the repo manifest and the embedded template"
        );
    }

    let repo_actions = repo["actions"].as_array().expect("repo actions array");
    let embedded_actions = embedded["actions"]
        .as_array()
        .expect("embedded actions array");
    assert_eq!(repo_actions.len(), embedded_actions.len());
    for (repo_action, embedded_action) in repo_actions.iter().zip(embedded_actions) {
        for key in ["id", "title", "contexts"] {
            assert_eq!(
                repo_action.get(key),
                embedded_action.get(key),
                "action field '{key}' drifted for action {:?}",
                repo_action.get("id")
            );
        }
    }

    let repo_events = repo["events"].as_array().expect("repo events array");
    let embedded_events = embedded["events"]
        .as_array()
        .expect("embedded events array");
    assert_eq!(repo_events.len(), embedded_events.len());
    for (repo_event, embedded_event) in repo_events.iter().zip(embedded_events) {
        assert_eq!(repo_event.get("on"), embedded_event.get("on"));
    }
}
