use std::{path::Path, process::Command};

use serde_json::Value;
use tempfile::tempdir;

use relay_core::{
    Availability, AvailabilityObservation, IdentityMetadata, Profile, ProfileName, ProfileOrigin,
    ProfileState, ProfileStore, ProviderKind,
};
use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES;
use relay_provider_claude::ClaudeIdentityPin;

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
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    command.output().expect("run relay")
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

#[cfg(unix)]
#[test]
fn inspect_and_adoption_dry_run_make_zero_writes() {
    let root = tempdir().expect("temp directory");
    let profile = root
        .path()
        .join("config/profiles/megan profile 日本語/claude");
    create_private_dir(&profile);
    std::fs::write(profile.join("sentinel"), "unchanged").expect("sentinel");
    let executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-megan","email":"megan@example.com","orgId":"org-1"}"#,
    );
    let profile_text = profile.to_string_lossy().to_string();
    let executable_text = executable.to_string_lossy().to_string();
    let before = snapshot(root.path());

    let inspect = relay(
        root.path(),
        &[
            "profile",
            "inspect-existing",
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &executable_text,
        ],
    );
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspection = json_stdout(&inspect);
    assert_eq!(inspection["data"]["safe_to_adopt"], true);
    assert_eq!(
        inspection["data"]["identity_pin"]["account_id"],
        "account-megan"
    );
    assert_eq!(snapshot(root.path()), before);

    let dry_run = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "megan",
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &executable_text,
            "--dry-run",
        ],
    );
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let adoption = json_stdout(&dry_run);
    assert_eq!(adoption["data"]["would_succeed"], true);
    assert_eq!(
        adoption["data"]["claude_profile_changes"],
        Value::Array(Vec::new())
    );
    assert_eq!(
        adoption["data"]["relay_owned_writes"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(snapshot(root.path()), before);
}

#[cfg(unix)]
#[test]
fn dry_run_reports_duplicate_without_writing() {
    let root = tempdir().expect("temp directory");
    let add = relay(root.path(), &["profile", "add", "megan"]);
    assert!(add.status.success());
    let profile = root.path().join("config/profiles/megan/claude");
    create_private_dir(&profile);
    let executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"megan@example.com"}"#,
    );
    let profile_text = profile.to_string_lossy().to_string();
    let executable_text = executable.to_string_lossy().to_string();
    let state_before = std::fs::read(root.path().join("config/profiles.toml")).expect("state");

    let output = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "megan",
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &executable_text,
            "--dry-run",
        ],
    );

    assert!(output.status.success());
    let adoption = json_stdout(&output);
    assert_eq!(adoption["data"]["would_succeed"], false);
    assert!(
        adoption["data"]["reasons"][0]
            .as_str()
            .is_some_and(|reason| reason.contains("already registered"))
    );
    assert_eq!(
        std::fs::read(root.path().join("config/profiles.toml")).expect("state"),
        state_before
    );
}

#[cfg(unix)]
#[test]
fn dry_run_rejects_an_identity_pin_registered_under_another_name() {
    let root = tempdir().expect("temp directory");
    let profile = root.path().join("config/profiles/megan/claude");
    create_private_dir(&profile);
    let pin = ClaudeIdentityPin {
        schema_version: 1,
        account_id: None,
        email: Some("same@example.com".to_owned()),
        organization_id: Some("org-1".to_owned()),
        auth_method: "claude.ai".to_owned(),
        api_provider: "firstParty".to_owned(),
    };
    let state = ProfileState {
        version: 1,
        profiles: vec![Profile {
            name: ProfileName::new("erika").expect("profile name"),
            provider: ProviderKind::Claude,
            config_dir: root.path().join("config/profiles/erika/claude"),
            enabled: true,
            origin: ProfileOrigin::Adopted,
            expected_identity: IdentityMetadata {
                stable_id: pin.stable_id(),
                display_label: Some("same@example.com".to_owned()),
            },
            last_availability: AvailabilityObservation {
                state: Availability::Available,
                source: "claude_auth_status".to_owned(),
                observed_unix_ms: 0,
                reset_unix_ms: None,
            },
        }],
    };
    let state_path = root.path().join("config/profiles.toml");
    ProfileStore::new(state_path.clone())
        .save(&state)
        .expect("seed profile registry");
    let executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"same@example.com","orgId":"org-1"}"#,
    );
    let profile_text = profile.to_string_lossy().to_string();
    let executable_text = executable.to_string_lossy().to_string();
    let state_before = std::fs::read(&state_path).expect("state");

    let output = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "megan",
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &executable_text,
            "--dry-run",
        ],
    );

    assert!(output.status.success());
    let adoption = json_stdout(&output);
    assert_eq!(adoption["data"]["would_succeed"], false);
    assert!(
        adoption["data"]["reasons"]
            .as_array()
            .is_some_and(|reasons| {
                reasons.iter().any(|reason| {
                    reason
                        .as_str()
                        .is_some_and(|reason| reason.contains("aliases are not allowed"))
                })
            })
    );
    assert_eq!(std::fs::read(state_path).expect("state"), state_before);
}

#[cfg(unix)]
#[test]
fn dry_run_fails_closed_on_corrupt_relay_state() {
    let root = tempdir().expect("temp directory");
    let profile = root.path().join("config/profiles/megan/claude");
    create_private_dir(&profile);
    std::fs::write(root.path().join("config/profiles.toml"), "broken = [").expect("corrupt state");
    let executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"megan@example.com"}"#,
    );
    let profile_text = profile.to_string_lossy().to_string();
    let executable_text = executable.to_string_lossy().to_string();

    let output = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "megan",
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &executable_text,
            "--dry-run",
        ],
    );

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "corrupted_state");
}

#[cfg(unix)]
fn fake_claude(root: &Path, auth_json: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = root.join("fake-claude");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' '2.1.276 (Claude Code)'\nelif [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ] && [ \"$3\" = \"--json\" ]; then\n  printf '%s\\n' '{auth_json}'\nelse\n  exit 2\nfi\n"
    );
    std::fs::write(&path, script).expect("fake Claude script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .expect("script permissions");
    path
}

#[cfg(unix)]
fn create_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path).expect("profile directory");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .expect("profile permissions");
}

fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(root: &Path, entries: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        let mut children = std::fs::read_dir(root)
            .expect("snapshot directory")
            .map(|entry| entry.expect("snapshot entry").path())
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            if path.is_dir() {
                entries.push((path.clone(), Vec::new()));
                visit(&path, entries);
            } else {
                entries.push((path.clone(), std::fs::read(path).expect("snapshot file")));
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, &mut entries);
    entries
}
