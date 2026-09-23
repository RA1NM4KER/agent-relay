//! `relay profile rename`: relabels a profile everywhere Relay keeps a durable reference to it,
//! never its authentication, identity pin, or provider config directory. Fake-provider profiles
//! only (never a real account) — this is about the state migration, not provider authentication.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

fn relay(root: &Path, arguments: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--json")
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env_remove("CLAUDE_CONFIG_DIR");
    command.output().expect("run relay")
}

/// The `--json` envelope: on stdout for success, stderr for a refused command — either way, one
/// `{"schema_version":...}` object.
fn json_envelope(output: &std::process::Output) -> Value {
    let bytes = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        panic!(
            "valid JSON envelope, got stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn setup_two_fake_profiles(root: &Path) {
    assert!(
        relay(root, &["profile", "add", "alice", "--provider", "fake"])
            .status
            .success()
    );
    assert!(
        relay(root, &["profile", "add", "bob", "--provider", "fake"])
            .status
            .success()
    );
    assert!(
        relay(
            root,
            &[
                "setup",
                "--non-interactive",
                "--primary",
                "alice",
                "--fallback",
                "bob"
            ]
        )
        .status
        .success()
    );
}

fn write_session_state(root: &Path) {
    let dir = root.join("state/projects/proj-test/sessions/sess-1/handoffs");
    std::fs::create_dir_all(&dir).expect("session dir");
    let session_dir = dir.parent().unwrap();
    std::fs::write(
        session_dir.join("session.json"),
        json!({
            "version": 1, "relay_session_id": "sess-1", "project_id": "proj-test",
            "created_unix_ms": 1, "last_activity_unix_ms": 1, "last_profile": "alice",
            "provider": "fake", "native_session_id": null, "provisional": false
        })
        .to_string(),
    )
    .expect("write session.json");
    std::fs::write(
        session_dir.join("automation_state.json"),
        json!({
            "version": 1,
            "recent_handoffs": [{"unix_ms": 1, "source": "alice", "target": "bob", "transaction_id": "ho-x"}],
            "known_exhausted": [{"profile": "alice", "observed_unix_ms": 1, "reset_unix_ms": null, "evidence": "simulated", "detected_via": "--simulate-usage"}]
        })
        .to_string(),
    )
    .expect("write automation_state.json");
    std::fs::write(
        dir.join("ho-x.json"),
        json!({
            "version": 2, "transaction_id": "ho-x", "project_id": "proj-test",
            "project_dir": "/tmp/x", "source_profile": "alice", "target_profile": "bob",
            "target_config_dir": "/tmp/y", "session_id": "sess-1",
            "continuity_type": "SESSION_CONTINUATION", "state": {"state": "COMPLETE"},
            "revision": 1, "created_unix_ms": 1, "updated_unix_ms": 1, "checkpoint": null,
            "transferred_artifacts": [], "bundle_summary": null, "verification": null,
            "target_launch": null, "notes": []
        })
        .to_string(),
    )
    .expect("write handoff journal");
}

#[test]
fn rename_updates_registry_preferences_and_historical_state() {
    let root = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(root.path()).expect("canonical root");
    let root = root.as_path();
    setup_two_fake_profiles(root);
    write_session_state(root);

    let output = relay(root, &["profile", "rename", "alice", "claude-main"]);
    assert!(
        output.status.success(),
        "rename failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = json_envelope(&output);
    assert_eq!(data["data"]["preferences_updated"], true);
    assert_eq!(data["data"]["state_files_updated"], 3);
    // Never touches the provider config directory the old name pointed at.
    assert_eq!(
        data["data"]["profile"]["config_dir"]
            .as_str()
            .expect("config_dir"),
        root.join("config/profiles/alice/fake").to_str().unwrap()
    );

    let profiles = json_envelope(&relay(root, &["profiles"]));
    let names: Vec<&str> = profiles["data"]["profiles"]
        .as_array()
        .expect("profiles array")
        .iter()
        .map(|p| p["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"claude-main"));
    assert!(!names.contains(&"alice"));

    let session =
        std::fs::read_to_string(root.join("state/projects/proj-test/sessions/sess-1/session.json"))
            .expect("session.json");
    assert!(session.contains("\"last_profile\": \"claude-main\""));

    let ledger = std::fs::read_to_string(
        root.join("state/projects/proj-test/sessions/sess-1/automation_state.json"),
    )
    .expect("automation_state.json");
    assert!(ledger.contains("\"source\": \"claude-main\""));
    assert!(ledger.contains("\"profile\": \"claude-main\""));

    let journal = std::fs::read_to_string(
        root.join("state/projects/proj-test/sessions/sess-1/handoffs/ho-x.json"),
    )
    .expect("journal");
    assert!(journal.contains("\"source_profile\": \"claude-main\""));
}

#[test]
fn rename_refuses_and_changes_nothing_while_a_lease_names_the_profile_as_owner() {
    let root = tempfile::tempdir().expect("tempdir");
    let root = root.path();
    setup_two_fake_profiles(root);
    let session_dir = root.join("state/projects/proj-test/sessions/sess-1");
    std::fs::create_dir_all(&session_dir).expect("session dir");
    std::fs::write(
        session_dir.join("lease.json"),
        json!({
            "version": 1, "project_id": "proj-test", "owner_profile": "alice",
            "owner_process": {"pid": 99_999_999, "start_time_fingerprint": null},
            "session_id": "native-1", "transaction_id": "ho-x", "acquired_unix_ms": 1,
            "provider_handle": null
        })
        .to_string(),
    )
    .expect("write lease.json");

    let output = relay(root, &["profile", "rename", "alice", "claude-main"]);
    assert!(!output.status.success());
    let data = json_envelope(&output);
    assert_eq!(data["error"]["code"], "profile_has_active_session");

    let profiles = json_envelope(&relay(root, &["profiles"]));
    let names: Vec<&str> = profiles["data"]["profiles"]
        .as_array()
        .expect("profiles array")
        .iter()
        .map(|p| p["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"alice"), "nothing should have renamed");
}

#[test]
fn rename_refuses_a_name_already_taken_and_an_unregistered_source() {
    let root = tempfile::tempdir().expect("tempdir");
    let root = root.path();
    setup_two_fake_profiles(root);

    let taken = relay(root, &["profile", "rename", "alice", "bob"]);
    assert!(!taken.status.success());
    assert_eq!(json_envelope(&taken)["error"]["code"], "duplicate_profile");

    let missing = relay(root, &["profile", "rename", "nonexistent", "someone"]);
    assert!(!missing.status.success());
    assert_eq!(
        json_envelope(&missing)["error"]["code"],
        "profile_not_found"
    );
}

/// A lease whose recorded process is *confirmed* dead (not merely absent from the check, but
/// proven gone via a real pid+fingerprint that once matched a real process) does not block the
/// rename — it is stale history, not a live owner — and, since it is not excluded from the
/// historical-state walk the way a live lease is, its `owner_profile` is relabeled too, so no
/// stale reference to the old name is left behind even in a dead lease.
#[test]
fn rename_proceeds_through_a_confirmed_dead_lease_and_relabels_it_too() {
    let root = tempfile::tempdir().expect("tempdir");
    let root = root.path();
    setup_two_fake_profiles(root);

    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn a real short-lived process");
    let identity = relay_core::handoff::ProcessIdentity::query(child.id());
    child.kill().expect("kill it");
    child
        .wait()
        .expect("reap it — confirmed gone, not merely unqueryable");

    let session_dir = root.join("state/projects/proj-test/sessions/sess-1");
    std::fs::create_dir_all(&session_dir).expect("session dir");
    std::fs::write(
        session_dir.join("lease.json"),
        json!({
            "version": 1, "project_id": "proj-test", "owner_profile": "alice",
            "owner_process": identity, "session_id": "native-1", "transaction_id": "ho-x",
            "acquired_unix_ms": 1, "provider_handle": null
        })
        .to_string(),
    )
    .expect("write lease.json");

    let output = relay(root, &["profile", "rename", "alice", "claude-main"]);
    assert!(
        output.status.success(),
        "a confirmed-dead lease must not block a rename: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lease = std::fs::read_to_string(session_dir.join("lease.json")).expect("lease.json");
    assert!(
        lease.contains("\"owner_profile\": \"claude-main\""),
        "the dead lease's own owner_profile must be relabeled too: {lease}"
    );
}
