use std::{path::Path, process::Command};

use serde_json::Value;
use tempfile::tempdir;

use relay_core::{
    Availability, AvailabilityObservation, IdentityMetadata, Profile, ProfileName, ProfileOrigin,
    ProfileState, ProfileStore, ProviderKind, RelayPaths,
    handoff::{
        ContinuityType, ExecutionIntent, HandoffJournal, HandoffStateTimestamp, JournalStore,
        ProjectId, RelaySessionId, RelaySessionRecord, SessionStore, TransactionId,
    },
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

fn relay_from(root: &Path, cwd: &Path, arguments: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .current_dir(cwd)
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

fn relay_human(root: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .output()
        .expect("run relay")
}

fn snapshot_tree(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)> {
    fn visit(
        root: &Path,
        path: &Path,
        out: &mut Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)>,
    ) {
        for entry in std::fs::read_dir(path).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                let metadata = entry.metadata().unwrap();
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                    metadata.modified().unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

fn history_record(
    paths: &RelayPaths,
    project: &Path,
    name: &str,
    activity: u64,
    intent: ExecutionIntent,
) -> RelaySessionRecord {
    let canonical = std::fs::canonicalize(project).expect("canonical project");
    let project_id = ProjectId::for_canonical_path(&canonical).expect("project id");
    let record = RelaySessionRecord::new(
        RelaySessionId::generate().expect("session id"),
        project_id.clone(),
        ProfileName::new(name).expect("profile"),
        Some("codex".into()),
        Some(format!("native-{name}")),
        false,
        activity,
    )
    .with_execution_intent(intent);
    SessionStore::new(paths, project_id)
        .save_record(&record)
        .expect("record");
    record
}

#[test]
fn history_cli_selects_sessions_and_preserves_safe_journal_evidence() {
    let root = tempdir().expect("root");
    let project = tempdir().expect("project");
    let paths =
        RelayPaths::new(root.path().join("config"), root.path().join("state")).expect("paths");
    let first = history_record(
        &paths,
        project.path(),
        "codex-main",
        100,
        ExecutionIntent::Interactive,
    );
    let second = history_record(
        &paths,
        project.path(),
        "claude-main",
        200,
        ExecutionIntent::Autonomous,
    );
    let canonical = std::fs::canonicalize(project.path()).unwrap();
    let store = SessionStore::new(&paths, ProjectId::for_canonical_path(&canonical).unwrap());
    let journal_path = store
        .session_dir(&second.relay_session_id)
        .join("handoffs/ho-0000000000000001-00001.json");
    std::fs::create_dir_all(journal_path.parent().unwrap()).unwrap();
    let mut failed = HandoffJournal::new(
        TransactionId::parse("ho-0000000000000001-00001").unwrap(),
        store.project_id().clone(),
        canonical.clone(),
        ProfileName::new("codex-main").unwrap(),
        ProfileName::new("claude-main").unwrap(),
        "/tmp/target".into(),
        "native".into(),
        ContinuityType::StateContinuation,
    );
    failed.created_unix_ms = 1_000;
    failed
        .notes
        .push("PROMPT_SECRET MODEL_OUTPUT email@example.test token=SECRET provider output".into());
    failed.state_timestamps = vec![
        HandoffStateTimestamp {
            state: "PREPARING".into(),
            unix_ms: 1_000,
        },
        HandoffStateTimestamp {
            state: "SOURCE_STOPPED".into(),
            unix_ms: 1_010,
        },
        HandoffStateTimestamp {
            state: "TARGET_STARTING".into(),
            unix_ms: 1_020,
        },
        HandoffStateTimestamp {
            state: "FAILED".into(),
            unix_ms: 1_030,
        },
    ];
    JournalStore::at_path(journal_path).save(&failed).unwrap();
    let mut complete = failed.clone();
    complete.transaction_id = TransactionId::parse("ho-0000000000000002-00002").unwrap();
    complete.created_unix_ms = 2_000;
    complete.state_timestamps = vec![
        HandoffStateTimestamp {
            state: "PREPARING".into(),
            unix_ms: 2_000,
        },
        HandoffStateTimestamp {
            state: "SOURCE_STOPPED".into(),
            unix_ms: 2_010,
        },
        HandoffStateTimestamp {
            state: "TARGET_STARTING".into(),
            unix_ms: 2_020,
        },
        HandoffStateTimestamp {
            state: "COMPLETE".into(),
            unix_ms: 2_030,
        },
    ];
    JournalStore::at_path(
        store
            .session_dir(&second.relay_session_id)
            .join("handoffs/ho-0000000000000002-00002.json"),
    )
    .save(&complete)
    .unwrap();
    let project_text = project.path().to_string_lossy().to_string();
    let out = relay(root.path(), &["history", "--project", &project_text]);
    assert!(out.status.success());
    let json = json_stdout(&out);
    assert_eq!(json["command"], "history");
    assert_eq!(
        json["data"]["session"]["id"],
        second.relay_session_id.to_string()
    );
    assert_eq!(json["data"]["session"]["execution_intent"], "autonomous");
    let serialized = serde_json::to_string(&json).unwrap();
    for secret in [
        "PROMPT_SECRET",
        "MODEL_OUTPUT",
        "email@example.test",
        "token=SECRET",
        "provider output",
    ] {
        assert!(!serialized.contains(secret));
    }
    let session_dir = store.session_dir(&second.relay_session_id);
    std::fs::write(session_dir.join("working_state.json"), "WORKING_STATE_GOAL WORKING_STATE_SUBTASK account-fingerprint-SECRET auth-token-SECRET provider-output-SECRET").unwrap();
    std::fs::write(
        session_dir.join("provider_output.json"),
        "MODEL_RESPONSE_SECRET provider-output-SECRET",
    )
    .unwrap();
    let before_tree = snapshot_tree(&session_dir);
    let human = relay_human(root.path(), &["history", "--project", &project_text]);
    assert!(human.status.success());
    let human_text = String::from_utf8_lossy(&human.stdout);
    for secret in [
        "WORKING_STATE_GOAL",
        "WORKING_STATE_SUBTASK",
        "account-fingerprint-SECRET",
        "auth-token-SECRET",
        "provider-output-SECRET",
        "MODEL_RESPONSE_SECRET",
        "PROMPT_SECRET",
        "email@example.test",
    ] {
        assert!(!human_text.contains(secret));
    }
    let hostile_json = relay(root.path(), &["history", "--project", &project_text]);
    let hostile_json_text = String::from_utf8_lossy(&hostile_json.stdout);
    for secret in [
        "WORKING_STATE_GOAL",
        "WORKING_STATE_SUBTASK",
        "account-fingerprint-SECRET",
        "auth-token-SECRET",
        "provider-output-SECRET",
        "MODEL_RESPONSE_SECRET",
        "PROMPT_SECRET",
        "email@example.test",
    ] {
        assert!(!hostile_json_text.contains(secret));
    }
    let after_tree = snapshot_tree(&session_dir);
    assert_eq!(
        before_tree, after_tree,
        "history must not mutate any session evidence"
    );
    let events = json["data"]["events"].as_array().unwrap();
    assert!(events.iter().any(|event| event["kind"] == "handoff_failed"));
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "handoff_complete")
    );
    let stopped = events
        .iter()
        .find(|event| event["state"] == "SOURCE_STOPPED")
        .unwrap()["timestamp_unix_ms"]
        .as_u64()
        .unwrap();
    let starting = events
        .iter()
        .find(|event| event["state"] == "TARGET_STARTING")
        .unwrap()["timestamp_unix_ms"]
        .as_u64()
        .unwrap();
    assert!(stopped < starting);
    let default = relay_from(root.path(), project.path(), &["history"]);
    assert!(default.status.success());
    assert_eq!(
        json_stdout(&default)["data"]["session"]["id"],
        second.relay_session_id.to_string()
    );
    let explicit = relay(
        root.path(),
        &[
            "history",
            "--project",
            &project_text,
            "--session",
            first.relay_session_id.as_str(),
        ],
    );
    assert!(explicit.status.success());
    assert_eq!(
        json_stdout(&explicit)["data"]["session"]["id"],
        first.relay_session_id.to_string()
    );
    let unknown = relay(
        root.path(),
        &[
            "history",
            "--project",
            &project_text,
            "--session",
            "deadbeef",
        ],
    );
    assert!(!unknown.status.success());

    let corrupt = store
        .session_dir(&second.relay_session_id)
        .join("handoffs/corrupt.json");
    std::fs::write(&corrupt, "{not json").unwrap();
    let before = std::fs::read(&corrupt).unwrap();
    let degraded = relay(root.path(), &["history", "--project", &project_text]);
    assert!(degraded.status.success());
    assert_eq!(
        std::fs::read(&corrupt).unwrap(),
        before,
        "history never repairs evidence"
    );
    assert!(
        json_stdout(&degraded)["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning == "a handoff journal could not be read")
    );
    let ledger_path = store
        .session_dir(&second.relay_session_id)
        .join("automation_state.json");
    std::fs::write(&ledger_path, "{not json").unwrap();
    let ledger_before = std::fs::read(&ledger_path).unwrap();
    let ledger_degraded = relay(root.path(), &["history", "--project", &project_text]);
    assert!(ledger_degraded.status.success());
    assert_eq!(std::fs::read(&ledger_path).unwrap(), ledger_before);
    assert!(
        json_stdout(&ledger_degraded)["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning == "automation ledger could not be read")
    );

    let record_path = store
        .session_dir(&first.relay_session_id)
        .join("session.json");
    let mut legacy: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("execution_intent");
    std::fs::write(&record_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let legacy_out = relay(
        root.path(),
        &[
            "history",
            "--project",
            &project_text,
            "--session",
            first.relay_session_id.as_str(),
        ],
    );
    assert!(legacy_out.status.success());
    assert_eq!(
        json_stdout(&legacy_out)["data"]["session"]["execution_intent"],
        "interactive"
    );
    let record_before = std::fs::read(&record_path).unwrap();
    let _ = relay(
        root.path(),
        &[
            "history",
            "--project",
            &project_text,
            "--session",
            first.relay_session_id.as_str(),
        ],
    );
    assert_eq!(
        std::fs::read(&record_path).unwrap(),
        record_before,
        "history is read-only"
    );
    std::fs::write(&record_path, "{not json").unwrap();
    let bad_record = relay(
        root.path(),
        &[
            "history",
            "--project",
            &project_text,
            "--session",
            first.relay_session_id.as_str(),
        ],
    );
    assert!(!bad_record.status.success());
    assert_eq!(std::fs::read(&record_path).unwrap(), b"{not json");
    let empty = tempdir().unwrap();
    assert!(
        !relay(
            root.path(),
            &["history", "--project", empty.path().to_str().unwrap()]
        )
        .status
        .success()
    );
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
            claude_config_mode: None,
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
#[test]
fn real_adoption_registers_the_profile_and_leaves_the_claude_directory_untouched() {
    let root = tempdir().expect("temp directory");
    let profile = root.path().join("config/profiles/erika/claude");
    create_private_dir(&profile);
    secure_relay_config_ancestors(root.path());
    std::fs::write(profile.join("sentinel"), "unchanged").expect("sentinel");
    let executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-erika","email":"erika@example.com","orgId":"org-1"}"#,
    );
    let profile_text = profile.to_string_lossy().to_string();
    let executable_text = executable.to_string_lossy().to_string();
    let claude_dir_before = snapshot(&profile);

    let adopt = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "erika",
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &executable_text,
        ],
    );

    assert!(
        adopt.status.success(),
        "{}",
        String::from_utf8_lossy(&adopt.stderr)
    );
    let adoption = json_stdout(&adopt);
    assert_eq!(adoption["command"], "profile.adopt");
    assert_eq!(adoption["data"]["name"], "erika");
    assert_eq!(adoption["data"]["origin"], "adopted");
    assert_eq!(snapshot(&profile), claude_dir_before);

    let list = relay(root.path(), &["profile", "list"]);
    assert!(list.status.success());
    assert_eq!(json_stdout(&list)["data"][0]["name"], "erika");

    let status = relay(
        root.path(),
        &[
            "profile",
            "status",
            "erika",
            "--claude-executable",
            &executable_text,
        ],
    );
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(json_stdout(&status)["data"]["identity_matches"], true);

    let doctor = relay(
        root.path(),
        &[
            "profile",
            "doctor",
            "erika",
            "--claude-executable",
            &executable_text,
        ],
    );
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert_eq!(json_stdout(&doctor)["data"]["healthy"], true);

    let config_entries: Vec<_> = std::fs::read_dir(root.path().join("config"))
        .expect("read config root")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert!(
        config_entries
            .iter()
            .all(|name| !name.to_string_lossy().contains(".tmp.")),
        "no transient atomic-write file must remain: {config_entries:?}"
    );
}

#[cfg(unix)]
#[test]
fn real_adoption_rejects_duplicate_profile_name_without_mutating_the_registry() {
    let root = tempdir().expect("temp directory");
    let first_profile = root.path().join("config/profiles/erika/claude");
    create_private_dir(&first_profile);
    secure_relay_config_ancestors(root.path());
    let first_executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"erika@example.com"}"#,
    );
    let adopt_first = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "erika",
            "--provider",
            "claude",
            "--config-dir",
            &first_profile.to_string_lossy(),
            "--claude-executable",
            &first_executable.to_string_lossy(),
        ],
    );
    assert!(adopt_first.status.success());
    let state_path = root.path().join("config/profiles.toml");
    let state_after_first = std::fs::read(&state_path).expect("state after first adoption");

    let second_profile = root.path().join("config/profiles/erika-again/claude");
    create_private_dir(&second_profile);
    let second_executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"someone-else@example.com"}"#,
    );
    let claude_dir_before = snapshot(&second_profile);

    let adopt_second = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "erika",
            "--provider",
            "claude",
            "--config-dir",
            &second_profile.to_string_lossy(),
            "--claude-executable",
            &second_executable.to_string_lossy(),
        ],
    );

    assert!(!adopt_second.status.success());
    let error: Value = serde_json::from_slice(&adopt_second.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "duplicate_profile");
    assert_eq!(
        std::fs::read(&state_path).expect("state unchanged"),
        state_after_first
    );
    assert_eq!(snapshot(&second_profile), claude_dir_before);
}

#[cfg(unix)]
#[test]
fn real_adoption_rejects_duplicate_identity_pin_across_profile_names() {
    let root = tempdir().expect("temp directory");
    let first_profile = root.path().join("config/profiles/erika/claude");
    create_private_dir(&first_profile);
    secure_relay_config_ancestors(root.path());
    let first_executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"same@example.com","orgId":"org-1"}"#,
    );
    let adopt_first = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "erika",
            "--provider",
            "claude",
            "--config-dir",
            &first_profile.to_string_lossy(),
            "--claude-executable",
            &first_executable.to_string_lossy(),
        ],
    );
    assert!(adopt_first.status.success());
    let state_path = root.path().join("config/profiles.toml");
    let state_after_first = std::fs::read(&state_path).expect("state after first adoption");

    let second_profile = root.path().join("config/profiles/megan/claude");
    create_private_dir(&second_profile);
    let second_executable = fake_claude(
        root.path(),
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"same@example.com","orgId":"org-1"}"#,
    );
    let claude_dir_before = snapshot(&second_profile);

    let adopt_second = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "megan",
            "--provider",
            "claude",
            "--config-dir",
            &second_profile.to_string_lossy(),
            "--claude-executable",
            &second_executable.to_string_lossy(),
        ],
    );

    assert!(!adopt_second.status.success());
    let error: Value = serde_json::from_slice(&adopt_second.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "duplicate_identity");
    assert_eq!(
        std::fs::read(&state_path).expect("state unchanged"),
        state_after_first
    );
    assert_eq!(snapshot(&second_profile), claude_dir_before);
}

#[cfg(unix)]
#[test]
fn real_adoption_fails_closed_when_not_authenticated_and_writes_nothing() {
    let root = tempdir().expect("temp directory");
    let profile = root.path().join("config/profiles/erika/claude");
    create_private_dir(&profile);
    let executable = fake_claude(
        root.path(),
        r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#,
    );
    let claude_dir_before = snapshot(&profile);

    let adopt = relay(
        root.path(),
        &[
            "profile",
            "adopt",
            "erika",
            "--provider",
            "claude",
            "--config-dir",
            &profile.to_string_lossy(),
            "--claude-executable",
            &executable.to_string_lossy(),
        ],
    );

    assert!(!adopt.status.success());
    let error: Value = serde_json::from_slice(&adopt.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "authentication_required");
    assert!(
        !root.path().join("config/profiles.toml").exists(),
        "no registry file must be created on a failed adoption"
    );
    assert_eq!(snapshot(&profile), claude_dir_before);
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

/// `create_private_dir` only hardens its leaf; real adoption also validates Relay's own
/// config/profiles roots (via `ProfileDirectory::prepare_root`), so tests that adopt beneath
/// `<root>/config/profiles/...` must secure those ancestor directories too, matching what a
/// directory tree created by Relay's own `profile add`/adopt flow would already have.
#[cfg(unix)]
fn secure_relay_config_ancestors(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    for relative in ["config", "config/profiles"] {
        std::fs::set_permissions(root.join(relative), std::fs::Permissions::from_mode(0o700))
            .expect("ancestor permissions");
    }
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

// ---- M2C.1: usage integration ----

fn fake_claude_version(root: &Path, version: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = root.join("fake-claude-version");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '{version} (Claude Code)' ;;\n  --help) printf '%s\\n' '--output-format <format> (choices: text, json, stream-json)' '--verbose' ;;\n  agents) printf '%s\\n' '[]' ;;\n  *) exit 2 ;;\nesac\n"
    );
    std::fs::write(&path, script).expect("fake Claude script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .expect("script permissions");
    path
}

fn hook(args: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_relay"))
        .args(["hook", "claude"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("hook output")
}

#[test]
fn integration_install_status_hooks_and_uninstall_round_trip_exactly() {
    let root = tempdir().expect("temp directory");
    let profile = root.path().join("erika-claude");
    std::fs::create_dir_all(&profile).expect("profile dir");
    let settings = profile.join("settings.json");
    let original = "{\n  \"theme\": \"dark\",\n  \"statusLine\": {\"type\": \"command\", \"command\": \"cat >/dev/null; echo MY-STATUS\"}\n}\n";
    std::fs::write(&settings, original).expect("settings");
    let claude = fake_claude_version(root.path(), "2.1.277");
    let claude_arg = claude.to_str().expect("utf8");
    let profile_arg = profile.to_str().expect("utf8");

    let dry = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--config-dir",
            profile_arg,
            "--dry-run",
            "--claude-executable",
            claude_arg,
        ],
    );
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(std::fs::read_to_string(&settings).expect("read"), original);
    assert!(!profile.join("relay-integration").exists());

    let install = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--config-dir",
            profile_arg,
            "--claude-executable",
            claude_arg,
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let installed: Value = serde_json::from_str(&std::fs::read_to_string(&settings).expect("read"))
        .expect("valid settings");
    assert_eq!(installed["theme"], "dark");
    assert_eq!(
        installed["hooks"]["StopFailure"][0]["matcher"],
        "rate_limit"
    );

    // The installed hook commands really work when Claude runs them via a shell.
    let hook_command = installed["hooks"]["StopFailure"][0]["hooks"][0]["command"]
        .as_str()
        .expect("hook command")
        .to_owned();
    let statusline_command = installed["statusLine"]["command"]
        .as_str()
        .expect("statusline command")
        .to_owned();
    let run_shell = |command: &str, stdin: &str| {
        use std::io::Write;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("sh");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write");
        child.wait_with_output().expect("output")
    };
    // The test binary path is what `current_exe` recorded, so these are the real hooks.
    let stop_payload = r#"{"hook_event_name":"StopFailure","session_id":"s1","cwd":"/p","error":"rate_limit","last_assistant_message":"secret text"}"#;
    assert!(run_shell(&hook_command, stop_payload).status.success());
    let far_future = 4_000_000_000_u64;
    let status_payload = format!(
        r#"{{"session_id":"s1","rate_limits":{{"five_hour":{{"used_percentage":100,"resets_at":{far_future}}}}}}}"#
    );
    let chained = run_shell(&statusline_command, &status_payload);
    assert_eq!(
        String::from_utf8_lossy(&chained.stdout).trim(),
        "MY-STATUS",
        "the original statusLine's output is passed through unchanged"
    );

    let recorded =
        std::fs::read_to_string(profile.join("relay-integration/signals/stop_failures.json"))
            .expect("stop failure record");
    assert!(recorded.contains("rate_limit") && !recorded.contains("secret text"));
    assert!(
        profile
            .join("relay-integration/signals/statusline.json")
            .exists()
    );

    let status = json_stdout(&relay(
        root.path(),
        &[
            "integration",
            "claude",
            "status",
            "--config-dir",
            profile_arg,
            "--claude-executable",
            claude_arg,
        ],
    ));
    assert_eq!(status["data"]["status"]["installed"], true);
    assert_eq!(status["data"]["status"]["statusline"], "relay_chained");
    assert_eq!(status["data"]["status"]["recorded_stop_failures"], 1);

    let uninstall = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "uninstall",
            "--config-dir",
            profile_arg,
        ],
    );
    assert!(
        uninstall.status.success(),
        "{}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    assert_eq!(std::fs::read_to_string(&settings).expect("read"), original);
}

#[test]
fn hooks_never_fail_the_calling_claude_session() {
    let root = tempdir().expect("temp directory");
    // No integration directory exists: nothing is recorded, nothing is printed, exit 0.
    let out = hook(
        &[
            "stop-failure",
            "--config-dir",
            root.path().to_str().expect("utf8"),
        ],
        "garbage that is not json",
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!root.path().join("relay-integration").exists());
}

#[test]
fn integration_install_fails_closed_on_unverified_or_unsupported_claude_versions() {
    let root = tempdir().expect("temp directory");
    let profile = root.path().join("p");
    std::fs::create_dir_all(&profile).expect("profile dir");
    let profile_arg = profile.to_str().expect("utf8");

    let newer = fake_claude_version(root.path(), "2.1.290");
    let refused = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--config-dir",
            profile_arg,
            "--claude-executable",
            newer.to_str().expect("utf8"),
        ],
    );
    assert!(!refused.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&refused.stderr).expect("json")["error"]["code"],
        "integration_refused"
    );
    assert!(!profile.join("settings.json").exists());

    let allowed = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--config-dir",
            profile_arg,
            "--allow-unverified-version",
            "--claude-executable",
            newer.to_str().expect("utf8"),
        ],
    );
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );

    let other = tempdir().expect("temp directory");
    let profile2 = other.path().join("p");
    std::fs::create_dir_all(&profile2).expect("profile dir");
    let major = fake_claude_version(other.path(), "2.2.0");
    let unsupported = relay(
        other.path(),
        &[
            "integration",
            "claude",
            "install",
            "--config-dir",
            profile2.to_str().expect("utf8"),
            "--allow-unverified-version",
            "--claude-executable",
            major.to_str().expect("utf8"),
        ],
    );
    assert!(
        !unsupported.status.success(),
        "a different release line is never accepted"
    );
}
