//! `relay update` (agent-relay#6), against the real compiled `relay` binary. Never exercises the
//! real Homebrew delegation path or a real network fetch: this process's own `current_exe()` is
//! never a Homebrew Cellar path, so `relay update` always takes the manual-instructions branch
//! here, and `relay update --check` is only ever exercised with a pre-seeded *fresh* cache file so
//! it never needs to reach the network either.

use std::{path::Path, process::Command, time::SystemTime};

use serde_json::{Value, json};

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

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_millis() as u64
}

fn seed_fresh_cache(root: &Path, latest_version: &str) {
    let state = root.join("state");
    std::fs::create_dir_all(&state).expect("create state root");
    std::fs::write(
        state.join("update_check.json"),
        json!({
            "checked_at_unix_ms": current_unix_ms(),
            "latest_version": latest_version,
        })
        .to_string(),
    )
    .expect("seed update-check cache");
}

#[test]
fn update_on_a_non_homebrew_install_prints_manual_instructions_and_never_touches_homebrew() {
    let root = tempfile::tempdir().expect("tempdir");
    let output = relay(root.path(), &["update"]);
    assert!(output.status.success(), "{output:?}");
    let body = json_stdout(&output);
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["method"], "manual");
    assert!(
        body["data"]["release_url"]
            .as_str()
            .expect("release_url")
            .contains("github.com/RA1NM4KER/agent-relay/releases"),
    );
}

#[test]
fn update_check_reports_up_to_date_from_a_fresh_cache_with_no_network_call() {
    let root = tempfile::tempdir().expect("tempdir");
    // Deliberately not numerically newer than any real build: this must compare as up to date
    // without this test needing to know the exact running version.
    seed_fresh_cache(root.path(), "0.0.1");
    let output = relay(root.path(), &["update", "--check"]);
    assert!(output.status.success(), "{output:?}");
    let body = json_stdout(&output);
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["update_available"], false);
    assert!(body["data"]["latest_version"].is_null());
}

#[test]
fn update_check_reports_available_from_a_fresh_cache_with_no_network_call() {
    let root = tempfile::tempdir().expect("tempdir");
    seed_fresh_cache(root.path(), "99.0.0");
    let output = relay(root.path(), &["update", "--check"]);
    assert!(output.status.success(), "{output:?}");
    let body = json_stdout(&output);
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["update_available"], true);
    assert_eq!(body["data"]["latest_version"], "99.0.0");
}

#[test]
fn update_check_never_installs_anything() {
    let root = tempfile::tempdir().expect("tempdir");
    seed_fresh_cache(root.path(), "99.0.0");
    let output = relay(root.path(), &["update", "--check"]);
    assert!(output.status.success(), "{output:?}");
    // `--check` must only ever report; confirm the state root gained nothing beyond the cache
    // file this test itself seeded (in particular, no Homebrew invocation artifact of any kind).
    let mut entries: Vec<_> = std::fs::read_dir(root.path().join("state"))
        .expect("read state root")
        .map(|entry| entry.expect("dir entry").file_name())
        .collect();
    entries.sort();
    assert_eq!(entries, vec![std::ffi::OsString::from("update_check.json")]);
}
