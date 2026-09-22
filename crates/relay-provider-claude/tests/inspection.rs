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
        stderr: Vec::new(),
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
    inspector.inspect(
        &config_dir,
        relay_core::ClaudeConfigMode::Explicit,
        environment,
    )
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
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            inspect_environment_with(&config_dir, |_| None),
        )
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
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            inspect_environment_with(&config_dir, |_| None),
        )
        .expect_err("directory mismatch must fail");

    assert_eq!(error.code(), "provider_profile_mismatch");
}

/// M4.1: NativeDefault must be pinned to exactly `~/.claude` — never any other directory, even
/// one that would otherwise pass every other check — and this is decided before any provider
/// command ever runs (no version probe, no `auth status`), so a misconfigured NativeDefault
/// profile can never leak a command against the wrong directory.
#[test]
fn native_default_mode_refuses_any_directory_that_is_not_the_real_native_default_one() {
    let root = tempdir().expect("temp directory");
    // A tempdir path is never, by construction, the real `$HOME/.claude`.
    let config_dir = root.path().join("not-the-real-home/.claude");
    fs::create_dir_all(&config_dir).expect("config directory");
    let runner = FakeRunner::new(Vec::new());
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner.clone()).expect("inspector");

    let error = inspector
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::NativeDefault,
            inspect_environment_with(&config_dir, |_| None),
        )
        .expect_err("a non-native-default directory must be refused");

    assert_eq!(error.code(), "provider_profile_mismatch");
    assert_eq!(
        runner.call_count(),
        0,
        "no provider command must run before the mode/directory check"
    );
}

/// The identical `CLAUDE_CONFIG_DIR`-unset-vs-explicit distinction this whole milestone rests on,
/// exercised at the inspection layer: authenticating the SAME real native-default directory
/// succeeds under `NativeDefault` (the only mode it is ever valid under) and is never silently
/// accepted under `Explicit`, matching `relay_provider_claude::config_mode`'s documented contract.
#[test]
fn native_default_identity_pinning_succeeds_only_in_native_default_mode() {
    let native_dir = relay_provider_claude::native_default_dir()
        .expect("HOME must resolve in a real test environment");
    let root = tempdir().expect("temp directory");
    let auth = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","accountUuid":"account-erika","email":"erika@example.com"}"#;

    // NativeDefault against the real native-default directory: succeeds, identity pinned.
    let runner = FakeRunner::new(vec![output("2.1.276 (Claude Code)"), output(auth)]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
    let report = inspector
        .inspect(
            &native_dir,
            relay_core::ClaudeConfigMode::NativeDefault,
            inspect_environment_with(&native_dir, |_| None),
        )
        .expect("native-default inspection against the real directory must succeed");
    assert!(report.authenticated);
    let pin = report.identity_pin.expect("identity pin");
    assert_eq!(pin.account_id.as_deref(), Some("account-erika"));

    // The identical directory under Explicit is never treated the same: Relay never special-cases
    // `config_dir == "~/.claude"` — the mode is what decides whether `CLAUDE_CONFIG_DIR` is set,
    // and Explicit setting it changes what the (real) `claude auth status` would itself report.
    // At this layer that means Explicit simply proceeds as an ordinary isolated-profile
    // inspection — it must NOT be short-circuited or refused the way NativeDefault's directory
    // check would refuse a foreign path.
    let runner2 = FakeRunner::new(vec![output("2.1.276 (Claude Code)"), output(auth)]);
    let inspector2 =
        ClaudeInspector::with_runner(executable(root.path()), runner2.clone()).expect("inspector");
    let explicit_report = inspector2
        .inspect(
            &native_dir,
            relay_core::ClaudeConfigMode::Explicit,
            inspect_environment_with(&native_dir, |_| None),
        )
        .expect("Explicit mode runs the ordinary path, not the NativeDefault directory check");
    assert!(explicit_report.authenticated);
    // The calls made were identical in count (version + auth) — proving mode alone, not an
    // env-value special case, is what changed between the two runs.
    assert_eq!(runner2.call_count(), 2);
}

/// NativeDefault's own auth failure (the account is not logged in at all) must be reported
/// exactly like Explicit's — not authenticated, not safe to adopt — never treated as an
/// inspection error just because it is the native-default account.
#[test]
fn native_default_auth_failure_is_reported_not_authenticated() {
    let native_dir = relay_provider_claude::native_default_dir()
        .expect("HOME must resolve in a real test environment");
    let root = tempdir().expect("temp directory");
    let runner = FakeRunner::new(vec![
        output("2.1.276 (Claude Code)"),
        output(r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
    let report = inspector
        .inspect(
            &native_dir,
            relay_core::ClaudeConfigMode::NativeDefault,
            inspect_environment_with(&native_dir, |_| None),
        )
        .expect("inspection itself must succeed on an expected unauthenticated state");
    assert!(!report.authenticated);
    assert!(!report.safe_to_adopt);
    assert!(report.identity_pin.is_none());
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
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            environment.clone(),
        )
        .expect_err("override must block");
    let serialized = serde_json::to_string(&environment).expect("serialize environment report");

    assert_eq!(error.code(), "environment_override_conflict");
    assert_eq!(runner.call_count(), 0);
    assert!(!serialized.contains("sk-ant-secret-canary"));
    assert!(serialized.contains("ANTHROPIC_API_KEY"));
}

/// Reproduces the exact scenario a live `/relay:doctor` (or any readiness check run from a
/// terminal Claude itself spawned) sees: `CLAUDE_CONFIG_DIR` ambiently points at the *calling*
/// process's own profile, not the different profile being inspected here. This must never be
/// treated as a conflict - every Relay-controlled invocation always explicitly overrides
/// `CLAUDE_CONFIG_DIR` for its actual target regardless of what was ambiently inherited, so the
/// mismatch reflects nothing more than "a different profile is being checked."
#[test]
fn an_ambient_claude_config_dir_for_a_different_profile_is_never_a_conflict() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile-being-checked");
    fs::create_dir(&config_dir).expect("profile directory");
    let other_profile = root.path().join("the-calling-sessions-own-profile");
    let runner = FakeRunner::new(vec![
        output("2.1.276 (Claude Code)"),
        output(
            r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"acct","email":"a@example.com"}"#,
        ),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
    let other_profile_text = OsString::from(other_profile.as_os_str());
    let environment = inspect_environment_with(&config_dir, |name| {
        (name == "CLAUDE_CONFIG_DIR").then(|| other_profile_text.clone())
    });
    assert!(
        environment.safe,
        "an ambient CLAUDE_CONFIG_DIR for a different profile must not mark the report unsafe"
    );

    let report = inspector
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            environment,
        )
        .expect("inspection must proceed despite the ambient mismatch");
    assert!(report.authenticated);
    assert!(report.safe_to_adopt);
}

/// Reproduces the other half of the same live in-session scenario: the running Claude Code CLI's
/// own instance bookkeeping variable, inherited purely because Relay is a child of the currently
/// supervised session. Must never block a *different* profile's readiness check, and must never
/// print a false "log back in" remedy for a profile that is genuinely authenticated.
#[test]
fn an_inherited_claude_code_messaging_token_is_never_a_conflict() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(vec![
        output("2.1.276 (Claude Code)"),
        output(
            r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"acct","email":"a@example.com"}"#,
        ),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
    let token = OsString::from("6053b7e95eedfd452239c466c8720498");
    let environment = inspect_environment_with(&config_dir, |name| {
        (name == "CLAUDE_CODE_MESSAGING_TOKEN").then(|| token.clone())
    });
    assert!(
        environment.safe,
        "an inherited CLAUDE_CODE_MESSAGING_TOKEN must not mark the report unsafe"
    );

    let report = inspector
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            environment,
        )
        .expect("inspection must proceed despite the inherited messaging token");
    assert!(
        report.authenticated,
        "a genuinely authenticated profile must report so"
    );
}

/// The full in-session reproduction: both benign, inherited variables present together, exactly
/// as a live `/relay:doctor` sees them - still safe, still authenticated.
#[test]
fn both_benign_inherited_variables_together_still_report_authenticated() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("erika");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(vec![
        output("2.1.280 (Claude Code)"),
        output(
            r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"acct-erika","email":"erika@example.com"}"#,
        ),
    ]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
    let megan_dir = root.path().join("megan");
    let megan_dir_text = OsString::from(megan_dir.as_os_str());
    let token = OsString::from("6053b7e95eedfd452239c466c8720498");
    let environment = inspect_environment_with(&config_dir, |name| match name {
        "CLAUDE_CONFIG_DIR" => Some(megan_dir_text.clone()),
        "CLAUDE_CODE_MESSAGING_TOKEN" => Some(token.clone()),
        _ => None,
    });
    assert!(environment.safe);

    let report = inspector
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            environment,
        )
        .expect("inspection must succeed");
    assert!(report.authenticated);
}

/// A genuine credential override remains blocking even alongside the two benign, session-scoped
/// variables above - the fix narrows detection, it does not disable it.
#[test]
fn a_real_credential_override_still_blocks_even_alongside_benign_session_variables() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(Vec::new());
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner.clone()).expect("inspector");
    let megan_dir = root.path().join("megan");
    let megan_dir_text = OsString::from(megan_dir.as_os_str());
    let token = OsString::from("6053b7e95eedfd452239c466c8720498");
    let api_key = OsString::from("sk-ant-secret-canary");
    let environment = inspect_environment_with(&config_dir, |name| match name {
        "CLAUDE_CONFIG_DIR" => Some(megan_dir_text.clone()),
        "CLAUDE_CODE_MESSAGING_TOKEN" => Some(token.clone()),
        "ANTHROPIC_API_KEY" => Some(api_key.clone()),
        _ => None,
    });
    assert!(!environment.safe);

    let error = inspector
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            environment,
        )
        .expect_err("a genuine credential override must still block");
    assert_eq!(error.code(), "environment_override_conflict");
    assert_eq!(runner.call_count(), 0);
}

#[test]
fn provider_command_failure_is_closed() {
    let root = tempdir().expect("temp directory");
    let config_dir = root.path().join("profile");
    fs::create_dir(&config_dir).expect("profile directory");
    let runner = FakeRunner::new(vec![Ok(ProcessResult {
        success: false,
        stdout: b"raw secret output".to_vec(),
        stderr: b"error: some-long-token-abcdefghijklmnopqrstuvwxyz1234567890 is invalid".to_vec(),
    })]);
    let inspector =
        ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");

    let error = inspector
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            inspect_environment_with(&config_dir, |_| None),
        )
        .expect_err("command must fail");

    // Actionable: names the operation, and a sanitized excerpt — but stdout (which could hold
    // conversation/credential-adjacent content) and long token-shaped words are never included.
    assert_eq!(error.code(), "provider_diagnostic");
    let text = format!("{error:?} {error}");
    assert!(
        text.contains("claude --version") || text.contains("claude auth status"),
        "{text}"
    );
    assert!(!text.contains("raw secret output"));
    assert!(
        !text.contains("abcdefghijklmnopqrstuvwxyz1234567890"),
        "{text}"
    );
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
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            inspect_environment_with(&config_dir, |_| None),
        )
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
        .inspect(
            &config_dir,
            relay_core::ClaudeConfigMode::Explicit,
            inspect_environment_with(&config_dir, |_| None),
        )
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
