use std::{path::Path, process::Command};

use serde_json::Value;
use tempfile::tempdir;

fn relay(root: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--json")
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .output()
        .expect("run relay")
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

#[test]
fn profile_commands_have_machine_readable_output() {
    let root = tempdir().expect("temp directory");

    let add = relay(root.path(), &["profile", "add", "megan"]);
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert_eq!(json_stdout(&add)["command"], "profile.add");

    let list = relay(root.path(), &["profile", "list"]);
    assert!(list.status.success());
    assert_eq!(json_stdout(&list)["data"][0]["name"], "megan");

    let status = relay(root.path(), &["profile", "status", "megan"]);
    assert!(status.status.success());
    assert_eq!(json_stdout(&status)["data"]["identity_matches"], true);

    let doctor = relay(root.path(), &["profile", "doctor", "megan"]);
    assert!(doctor.status.success());
    assert_eq!(json_stdout(&doctor)["data"]["healthy"], true);

    let remove = relay(root.path(), &["profile", "remove", "megan"]);
    assert!(remove.status.success());
    assert_eq!(json_stdout(&remove)["data"]["directory_retained"], true);
}

#[test]
fn duplicate_error_is_structured_and_contains_no_state_contents() {
    let root = tempdir().expect("temp directory");
    let first = relay(root.path(), &["profile", "add", "megan"]);
    assert!(first.status.success());

    let duplicate = relay(root.path(), &["profile", "add", "megan"]);
    assert!(!duplicate.status.success());
    let error: Value = serde_json::from_slice(&duplicate.stderr).expect("valid JSON stderr");
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "duplicate_profile");
}

#[test]
fn corrupted_state_returns_stable_error() {
    let root = tempdir().expect("temp directory");
    let config = root.path().join("config");
    std::fs::create_dir(&config).expect("config root");
    std::fs::write(config.join("profiles.toml"), "definitely not toml = [").expect("corrupt state");

    let output = relay(root.path(), &["profile", "list"]);

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("valid JSON stderr");
    assert_eq!(error["error"]["code"], "corrupted_state");
}
