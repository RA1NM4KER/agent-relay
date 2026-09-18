use std::{
    collections::VecDeque,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use relay_provider_claude::{
    ClaudeIdentityPin, ClaudeInspector, CommandRunner, ProcessResult, ProcessSpec,
    inspect_environment_with,
};
use tempfile::tempdir;

#[derive(Clone)]
struct FakeRunner {
    results: Arc<Mutex<VecDeque<relay_core::Result<ProcessResult>>>>,
    calls: Arc<Mutex<Vec<ProcessSpec>>>,
}

impl FakeRunner {
    fn new(results: Vec<relay_core::Result<ProcessResult>>) -> Self {
        Self {
            results: Arc::new(Mutex::new(results.into())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("calls lock").len()
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, spec: &ProcessSpec) -> relay_core::Result<ProcessResult> {
        self.calls.lock().expect("calls lock").push(spec.clone());
        self.results
            .lock()
            .expect("results lock")
            .pop_front()
            .expect("fake result")
    }
}

fn output(contents: &str) -> relay_core::Result<ProcessResult> {
    Ok(ProcessResult {
        success: true,
        stdout: contents.as_bytes().to_vec(),
    })
}

fn executable(root: &Path) -> PathBuf {
    let executable = root.join("claude-fixture");
    fs::write(&executable, "fixture").expect("write executable fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
    }
    executable
}

fn inspect(auth_json: &str) -> relay_core::Result<relay_provider_claude::ClaudeInspectionReport> {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile with spaces 日本語");
    fs::create_dir(&config_dir).expect("config directory");
    let runner = FakeRunner::new(vec![output("2.1.276 (Claude Code)"), output(auth_json)]);
    let inspector = ClaudeInspector::with_runner(executable(root.path()), runner)?;
    let environment = inspect_environment_with(&config_dir, |_| None);
    inspector.inspect(&config_dir, environment)
}

#[test]
fn authenticated_known_schema_produces_identity_pin() {
    let report = inspect(
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-1","email":"Person@Example.com","orgId":"org-1","subscriptionType":"max"}"#,
    )
    .expect("inspection");

    assert!(report.authenticated);
    assert!(report.safe_to_adopt);
    let pin = report.identity_pin.expect("identity pin");
    assert_eq!(pin.account_id.as_deref(), Some("account-1"));
    assert_eq!(pin.email.as_deref(), Some("person@example.com"));
    assert_eq!(pin.organization_id.as_deref(), Some("org-1"));
}

#[test]
fn observed_2_1_276_schema_is_accepted_and_paths_are_verified() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile with spaces 日本語");
    fs::create_dir(&config_dir).expect("config directory");
    let auth = serde_json::json!({
        "loggedIn": true,
        "authMethod": "oauth",
        "apiProvider": "firstParty",
        "email": "person@example.com",
        "orgId": "org-1",
        "orgName": "Example",
        "subscriptionType": "max",
        "analyticsDisabled": false,
        "configDirectory": config_dir,
        "projectsDirectory": config_dir.join("projects"),
    });
    let runner = FakeRunner::new(vec![
        output("2.1.276 (Claude Code)"),
        output(&auth.to_string()),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");

    let report = inspector
        .inspect(&config_dir, inspect_environment_with(&config_dir, |_| None))
        .expect("inspection");

    assert!(report.safe_to_adopt);
    assert_eq!(
        report.identity_pin.and_then(|pin| pin.email),
        Some("person@example.com".to_owned())
    );
}

#[test]
fn reported_config_directory_mismatch_fails_closed() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("config directory");
    let auth = serde_json::json!({
        "loggedIn": true,
        "authMethod": "oauth",
        "apiProvider": "firstParty",
        "email": "person@example.com",
        "configDirectory": root.path().join("different-profile"),
    });
    let runner = FakeRunner::new(vec![
        output("2.1.276 (Claude Code)"),
        output(&auth.to_string()),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");

    let error = inspector
        .inspect(&config_dir, inspect_environment_with(&config_dir, |_| None))
        .expect_err("directory mismatch must fail");

    assert_eq!(error.code(), "provider_profile_mismatch");
}

#[test]
fn unauthenticated_profile_is_not_safe_to_adopt() {
    let report = inspect(r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#)
        .expect("inspection");

    assert!(!report.authenticated);
    assert!(!report.safe_to_adopt);
    assert!(report.identity_pin.is_none());
}

#[test]
fn authenticated_profile_without_identity_fails_closed() {
    let report = inspect(r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty"}"#)
        .expect("inspection");

    assert!(report.authenticated);
    assert!(!report.safe_to_adopt);
    assert!(report.identity_pin.is_none());
}

#[test]
fn malformed_auth_json_has_closed_error() {
    let error = inspect("not-json oauth-secret-canary").expect_err("malformed JSON must fail");
    let rendered = format!("{error:?} {error}");

    assert_eq!(error.code(), "malformed_provider_output");
    assert!(!rendered.contains("oauth-secret-canary"));
}

#[test]
fn unexpected_extra_fields_fail_as_unknown_schema() {
    let error = inspect(
        r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"a@example.com","futureField":1}"#,
    )
    .expect_err("unknown schema must fail");

    assert_eq!(error.code(), "unsupported_provider_schema");
}

#[test]
fn planted_secret_field_is_never_exposed() {
    let canary = "oauth-secret-canary";
    let error = inspect(&format!(
        r#"{{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"a@example.com","accessToken":"{canary}"}}"#
    ))
    .expect_err("secret-like extra field must fail");
    let rendered = format!("{error:?} {error}");

    assert_eq!(error.code(), "unsupported_provider_schema");
    assert!(!rendered.contains(canary));
}

#[test]
fn environment_override_blocks_commands_without_reading_value() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(Vec::new());
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner.clone()).expect("inspector");
    let canary = OsString::from("sk-ant-secret-canary");
    let environment = inspect_environment_with(&config_dir, |name| {
        (name == "ANTHROPIC_API_KEY").then(|| canary.clone())
    });

    let error = inspector
        .inspect(&config_dir, environment.clone())
        .expect_err("override must block");
    let serialized = serde_json::to_string(&environment).expect("serialize environment report");

    assert_eq!(error.code(), "environment_override_conflict");
    assert_eq!(runner.call_count(), 0);
    assert!(!serialized.contains("sk-ant-secret-canary"));
    assert!(serialized.contains("ANTHROPIC_API_KEY"));
}

#[test]
fn provider_command_failure_is_closed() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(vec![Ok(ProcessResult {
        success: false,
        stdout: b"raw secret output".to_vec(),
    })]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");

    let error = inspector
        .inspect(&config_dir, inspect_environment_with(&config_dir, |_| None))
        .expect_err("command must fail");

    assert_eq!(error.code(), "provider_command_failed");
    assert!(!format!("{error:?} {error}").contains("raw secret output"));
}

#[test]
fn unsupported_version_fails_before_auth_command() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(vec![output("2.2.0 (Claude Code)")]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner.clone()).expect("inspector");

    let error = inspector
        .inspect(&config_dir, inspect_environment_with(&config_dir, |_| None))
        .expect_err("unsupported version");

    assert_eq!(error.code(), "unsupported_provider_version");
    assert_eq!(runner.call_count(), 1);
}

#[test]
fn missing_executable_has_closed_error() {
    let root = tempdir().expect("temp directory");
    let error = ClaudeInspector::discover(Some(&root.path().join("missing")))
        .expect_err("missing executable");

    assert_eq!(error.code(), "provider_executable_missing");
}

#[test]
fn wrong_identity_pin_does_not_match() {
    let expected = ClaudeIdentityPin {
        schema_version: 1,
        account_id: Some("account-a".to_owned()),
        email: Some("same@example.com".to_owned()),
        organization_id: Some("org-1".to_owned()),
        auth_method: "oauth".to_owned(),
        api_provider: "firstParty".to_owned(),
    };
    let observed = ClaudeIdentityPin {
        account_id: Some("account-b".to_owned()),
        ..expected.clone()
    };

    assert!(!expected.matches(&observed));
}

#[test]
fn inspection_makes_no_profile_filesystem_changes() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    fs::write(config_dir.join("sentinel"), "unchanged").expect("sentinel");
    let before = snapshot(&config_dir);
    let runner = FakeRunner::new(vec![
        output("2.1.276 (Claude Code)"),
        output(
            r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","email":"a@example.com"}"#,
        ),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");

    let report = inspector
        .inspect(&config_dir, inspect_environment_with(&config_dir, |_| None))
        .expect("inspection");

    assert!(report.safe_to_adopt);
    assert_eq!(snapshot(&config_dir), before);
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = fs::read_dir(root)
        .expect("read snapshot")
        .map(|entry| {
            let path = entry.expect("directory entry").path();
            let bytes = if path.is_file() {
                fs::read(&path).expect("read snapshot file")
            } else {
                Vec::new()
            };
            (path, bytes)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}
