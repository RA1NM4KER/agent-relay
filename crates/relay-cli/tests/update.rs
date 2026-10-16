//! `relay update` (agent-relay#6), against the real compiled `relay` binary. Never exercises the
//! real Homebrew or a real network fetch. Homebrew fixtures live entirely in a temporary directory
//! with a child-local PATH; they exercise binary ownership, delegation order, and failure paths
//! without changing the user's installation or global environment.

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

#[cfg(unix)]
mod homebrew {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    struct Fixture {
        root: tempfile::TempDir,
        exe: PathBuf,
        prefix: PathBuf,
        formula: &'static str,
    }

    impl Fixture {
        fn new(binary: &str, formula: &'static str) -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            // A custom prefix also verifies that detection is not tied to /opt/homebrew.
            let prefix = root.path().join("custom-prefix").join(formula);
            fs::create_dir_all(prefix.join("bin")).expect("create prefix");
            let exe = prefix.join("bin").join(binary);
            fs::copy(env!("CARGO_BIN_EXE_relay"), &exe).expect("copy executable");
            let tools = root.path().join("tools");
            fs::create_dir(&tools).expect("create tools");
            let brew = tools.join("brew");
            fs::write(
                &brew,
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$RELAY_TEST_BREW_LOG"
if [ "$1" = "$RELAY_TEST_BREW_FAIL" ]; then
    printf '%s\n' 'fixture failure' >&2
    exit 1
fi
case "$1" in
    --prefix) printf '%s\n' "$RELAY_TEST_BREW_PREFIX" ;;
    info) printf '{"formulae":[{"name":"%s","linked_keg":"9.8.7","installed":[{"version":"9.8.6"},{"version":"9.8.7"}]}]}\n' "$3" ;;
    update|upgrade) ;;
    *) exit 2 ;;
esac
"#,
            )
            .expect("write fake brew");
            fs::set_permissions(&brew, fs::Permissions::from_mode(0o755)).expect("chmod");
            Self {
                root,
                exe,
                prefix,
                formula,
            }
        }

        fn command(&self) -> Command {
            let mut command = Command::new(&self.exe);
            command
                .args(["--json", "--config-root"])
                .arg(self.root.path().join("config"))
                .arg("--state-root")
                .arg(self.root.path().join("state"))
                .arg("update")
                .env("PATH", self.root.path().join("tools"))
                .env("RELAY_TEST_BREW_PREFIX", &self.prefix)
                .env("RELAY_TEST_BREW_LOG", self.root.path().join("brew.log"))
                .env("RELAY_TEST_BREW_FAIL", "");
            command
        }

        fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.root.path().join("brew.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[test]
    fn owned_stable_and_dev_binaries_delegate_in_order_and_report_the_installed_version() {
        for (binary, formula) in [("relay", "agent-relay"), ("relay-dev", "agent-relay-dev")] {
            let fixture = Fixture::new(binary, formula);
            let output = fixture.command().output().expect("run");
            assert!(output.status.success(), "{output:?}");
            let body = json_stdout(&output);
            assert_eq!(body["data"]["method"], "homebrew");
            assert_eq!(body["data"]["formula"], fixture.formula);
            assert_eq!(body["data"]["version"], "9.8.7");
            assert_eq!(
                fixture.calls(),
                [
                    format!("--prefix {formula}"),
                    "update".to_owned(),
                    format!("upgrade {formula}"),
                    format!("info --json=v2 {formula}"),
                ]
            );
        }
    }

    #[test]
    fn failed_homebrew_steps_stop_the_sequence_and_report_failure() {
        for (step, count) in [("update", 2), ("upgrade", 3), ("info", 4)] {
            let fixture = Fixture::new("relay", "agent-relay");
            let output = fixture
                .command()
                .env("RELAY_TEST_BREW_FAIL", step)
                .output()
                .expect("run");
            assert!(!output.status.success(), "{output:?}");
            let body: Value = serde_json::from_slice(&output.stderr).expect("JSON error on stderr");
            assert_eq!(body["error"]["code"], "update_command_failed", "{body}");
            assert_eq!(fixture.calls().len(), count);
        }
    }

    #[test]
    fn a_homebrew_looking_copy_does_not_upgrade_a_different_installation() {
        let fixture = Fixture::new("relay", "agent-relay");
        let copied = fixture
            .root
            .path()
            .join("homebrew/Cellar/agent-relay/1/bin/relay");
        fs::create_dir_all(copied.parent().expect("parent")).expect("mkdir");
        fs::copy(&fixture.exe, &copied).expect("copy");
        let mut other = Fixture {
            exe: copied,
            ..fixture
        };
        let output = other.command().output().expect("run");
        assert_eq!(json_stdout(&output)["data"]["method"], "manual");
        assert_eq!(other.calls(), ["--prefix agent-relay"]);
        // A missing Homebrew installation is also a manual install, never guessed from the path.
        other.prefix = other.root.path().join("missing");
        let output = other.command().output().expect("run");
        assert_eq!(json_stdout(&output)["data"]["method"], "manual");
    }

    #[test]
    fn check_on_an_owned_install_never_invokes_homebrew() {
        let fixture = Fixture::new("relay", "agent-relay");
        seed_fresh_cache(fixture.root.path(), "99.0.0");
        let output = fixture.command().arg("--check").output().expect("run");
        assert_eq!(json_stdout(&output)["data"]["update_available"], true);
        assert!(fixture.calls().is_empty());
    }
}
