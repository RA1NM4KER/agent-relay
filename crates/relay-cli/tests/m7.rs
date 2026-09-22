//! M7 tests: the UX/polish pass — `relay doctor`, `relay why`, `relay history`, and `relay
//! status`'s decision-oriented additions. Against the real compiled `relay` binary (matching
//! `tests/m4.rs`'s existing pattern) with a fake `claude` executable standing in for the real one
//! — never a real Claude account. Synthetic `alice`/`bob` profile names and `@example.com`
//! identities only.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES;

/// Herdr env vars this test binary itself may have inherited (it is quite plausibly *run* from
/// inside a real Herdr pane during development) must never leak into a child `relay` process
/// under test, matching `tests/m4.rs`'s own precaution.
const HERDR_ENV_VARS: &[&str] = &[
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_TAB_ID",
    "HERDR_BIN_PATH",
    "HERDR_SOCKET_PATH",
];

/// `relay status` has no `--claude-executable` flag (by design — it's a quick-glance command):
/// its readiness check looks for a plain `claude` on `PATH`, same as a real install. `root/bin`
/// is prepended here so a test that needs `relay status`'s own readiness to see a *specific* fake
/// executable can drop it there under that name; harmless (and a no-op) when the directory does
/// not exist.
fn relay(root: &Path, arguments: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    let path_with_bin = std::env::join_paths(
        std::iter::once(root.join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH");
    command
        .arg("--json")
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env("PATH", path_with_bin)
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in HERDR_ENV_VARS {
        command.env_remove(variable);
    }
    command.output().expect("run relay")
}

/// Like [`relay`], but sets `extra_env` on top of the scrubbed baseline instead of leaving every
/// override variable removed - for reproducing exactly what a live, currently-supervised Claude
/// session's own environment (inherited by a child process such as the `/relay:doctor` hook)
/// looks like.
fn relay_with_env(
    root: &Path,
    arguments: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
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
    for variable in HERDR_ENV_VARS {
        command.env_remove(variable);
    }
    for (name, value) in extra_env {
        command.env(name, value);
    }
    command.output().expect("run relay")
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

#[cfg(unix)]
fn create_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).expect("profile directory");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("permissions");
}

#[cfg(unix)]
fn secure_relay_config_ancestors(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for relative in ["config", "config/profiles"] {
        std::fs::set_permissions(root.join(relative), std::fs::Permissions::from_mode(0o700))
            .expect("ancestor permissions");
    }
}

/// A fake `claude` covering every subcommand these tests' code paths call: `--version`, `--help`
/// (usage-integration capability probe), `auth status|login|logout`, `--bg` (launch), `agents
/// --json` (liveness/session lookup). `auth status` reads a marker file that `auth login`/`auth
/// logout` themselves flip, exactly like the real CLI, so a profile can be adopted authenticated
/// and later observed logged out through a real `relay logout` — never by rewriting the fixture.
struct FakeClaude {
    executable: std::path::PathBuf,
    logged_out_marker: std::path::PathBuf,
}

impl FakeClaude {
    fn new(root: &Path, name: &str, version: &str, bg_id: &str, session_id: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let executable = root.join(format!("fake-claude-{bg_id}"));
        let logged_out_marker = root.join(format!("logged-out-{bg_id}"));
        let logged_in_json = format!(
            r#"{{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-{name}","email":"{name}@example.com","orgId":"org-1"}}"#
        );
        // Real `claude auth status --json` still reports `authMethod`/`apiProvider` when logged
        // out; only the account fields and email are absent.
        let logged_out_json = r#"{"loggedIn":false,"authMethod":"none","apiProvider":"none"}"#;
        let script = format!(
            r#"#!/bin/sh
MARKER="{marker}"
case " $* " in *" --session-id "*)
  PREV=""; SID=""; for a in "$@"; do [ "$PREV" = "--session-id" ] && SID="$a"; PREV="$a"; done
  mkdir -p "$CLAUDE_CONFIG_DIR/projects/-fake" && echo '{{}}' > "$CLAUDE_CONFIG_DIR/projects/-fake/$SID.jsonl"
  exit 0 ;; esac
case "$1" in
  --version) printf '%s (Claude Code)\n' "{version}" ;;
  --help) printf -- '--output-format stream-json --verbose\n' ;;
  auth)
    case "$2" in
      status)
        if [ -f "$MARKER" ]; then printf '%s\n' '{logged_out_json}'; else printf '%s\n' '{logged_in_json}'; fi
        ;;
      login) rm -f "$MARKER"; exit 0 ;;
      logout) touch "$MARKER"; exit 0 ;;
      *) exit 2 ;;
    esac
    ;;
  --bg) printf 'backgrounded \302\267 %s\n' "{bg_id}" ;;
  agents)
    printf '[{{"pid":4242,"id":"{bg_id}","kind":"background","startedAt":1,"sessionId":"{session_id}","name":"x","status":"idle","state":"done"}}]\n'
    ;;
  attach) exit 0 ;;
  --resume) exit 0 ;;
  *) exit 2 ;;
esac
"#,
            marker = logged_out_marker.display(),
        );
        std::fs::write(&executable, script).expect("fake claude script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        Self {
            executable,
            logged_out_marker,
        }
    }

    fn path_text(&self) -> String {
        self.executable.to_string_lossy().into_owned()
    }

    fn log_out(&self) {
        std::fs::write(&self.logged_out_marker, b"1").expect("write logged-out marker");
    }
}

fn accept_project_trust(root: &Path, name: &str, project: &Path) {
    let path = root
        .join("config/profiles")
        .join(name)
        .join("claude/.claude.json");
    let mut value: Value = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({"projects": {}}));
    let project = std::fs::canonicalize(project).unwrap();
    value["projects"][project.to_str().unwrap()] =
        serde_json::json!({"hasTrustDialogAccepted": true});
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn adopt_profile(root: &Path, name: &str, claude: &FakeClaude) {
    let profile_dir = root.join("config/profiles").join(name).join("claude");
    create_private_dir(&profile_dir);
    secure_relay_config_ancestors(root);
    accept_project_trust(root, name, &std::env::current_dir().unwrap());
    let profile_text = profile_dir.to_string_lossy().to_string();
    let adopt = relay(
        root,
        &[
            "profile",
            "adopt",
            name,
            "--provider",
            "claude",
            "--config-dir",
            &profile_text,
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        adopt.status.success(),
        "adopt {name} failed: {}",
        String::from_utf8_lossy(&adopt.stderr)
    );
}

// =================================================================================================
// DOCTOR
// =================================================================================================

#[test]
fn doctor_reports_ready_and_exits_zero_when_everything_checks_out() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &claude);
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let output = relay(
        root.path(),
        &["doctor", "--claude-executable", &claude.path_text()],
    );
    assert!(
        output.status.success(),
        "doctor should exit 0 when ready: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    assert_eq!(payload["ok"], true);
    let data = &payload["data"];
    assert_eq!(data["ready"], true);
    assert_eq!(data["overall"], "ok");
    let labels: Vec<&str> = data["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .map(|check| check["label"].as_str().expect("label"))
        .collect();
    assert!(labels.contains(&"alice (Claude) authenticated"));
    assert!(labels.contains(&"alice (Claude) usage integration installed"));
    assert!(labels.contains(&"Automatic handoff enabled"));
    assert!(labels.iter().any(|label| label.contains("Claude Code")));
    // Herdr is only ever reported when actually running inside a Herdr pane; its absence here is
    // never a check at all, let alone a problem.
    assert!(!labels.iter().any(|label| label.contains("Herdr")));
}

#[test]
fn doctor_a_logged_out_fallback_is_blocking_and_exits_nonzero() {
    let root = tempdir().expect("tempdir");
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &alice);
    let bob = FakeClaude::new(root.path(), "bob", "2.1.276", "bbbb2222", "s2");
    adopt_profile(root.path(), "bob", &bob);
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            "bob",
            "--usage-integration",
            "true",
            "--claude-executable",
            &alice.path_text(),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    // bob was authenticated for adoption/setup, exactly like a real profile; now they log out,
    // just as `relay doctor` should actually be able to catch.
    bob.log_out();

    // `relay doctor` has one global `--claude-executable` override applied to every configured
    // Claude profile (a real install has exactly one `claude` binary for all of them); point it
    // at bob's own fixture so bob's check reflects bob's own (now logged-out) state rather than
    // whatever a bare `claude` on this machine's PATH would report.
    let output = relay(
        root.path(),
        &["doctor", "--claude-executable", &bob.path_text()],
    );
    assert!(
        !output.status.success(),
        "doctor must exit non-zero when genuinely not ready"
    );
    let payload = json_stdout(&output);
    // Not ready is a diagnosis, never a command *error*: the rich success envelope is unchanged.
    assert_eq!(payload["ok"], true);
    let data = &payload["data"];
    assert_eq!(data["ready"], false);
    assert_eq!(data["overall"], "blocking");
    let bob_check = data["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["label"] == "bob (Claude) authenticated")
        .expect("bob's authentication check is present");
    assert_eq!(bob_check["level"], "blocking");
    assert_eq!(bob_check["remedy"], "relay login bob");
}

#[test]
fn doctor_missing_usage_integration_for_a_fallback_is_blocking() {
    let root = tempdir().expect("tempdir");
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &alice);
    let bob = FakeClaude::new(root.path(), "bob", "2.1.276", "bbbb2222", "s2");
    adopt_profile(root.path(), "bob", &bob);
    // Install + enable for alice alone first...
    let setup1 = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &alice.path_text(),
        ],
    );
    assert!(setup1.status.success());
    // ...then add bob as a fallback without touching the usage-integration flag again, so bob
    // never gets the integration installed.
    let setup2 = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            "bob",
            "--claude-executable",
            &alice.path_text(),
        ],
    );
    assert!(setup2.status.success());

    let output = relay(
        root.path(),
        &["doctor", "--claude-executable", &alice.path_text()],
    );
    assert!(!output.status.success());
    let data = json_stdout(&output)["data"].clone();
    assert_eq!(data["ready"], false);
    let bob_integration = data["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["label"] == "bob (Claude) usage integration installed")
        .expect("bob's integration check is present");
    assert_eq!(bob_integration["level"], "blocking");
    assert_eq!(
        bob_integration["remedy"],
        "relay integration claude install --profile bob"
    );
}

#[test]
fn doctor_automatic_handoff_disabled_in_preferences_is_blocking() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    // The integration stays installed; only the preference is turned back off.
    let disable = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "false",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(disable.status.success());

    let output = relay(
        root.path(),
        &["doctor", "--claude-executable", &claude.path_text()],
    );
    assert!(!output.status.success());
    let data = json_stdout(&output)["data"].clone();
    let handoff_check = data["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["label"] == "Automatic handoff enabled")
        .expect("automatic handoff check is present");
    assert_eq!(handoff_check["level"], "blocking");
}

#[test]
fn doctor_an_unverified_claude_version_is_a_warning_not_a_blocker() {
    let root = tempdir().expect("tempdir");
    // Same release line as the validated versions, but not itself in the validated list.
    let claude = FakeClaude::new(root.path(), "alice", "2.1.279", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &claude);
    // Non-interactive setup's `--usage-integration true` refuses an unverified version outright
    // (it has no `--allow-unverified-version` escape hatch of its own); install directly with
    // that flag instead, then set just the primary/preference the way setup normally would.
    let install = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--profile",
            "alice",
            "--allow-unverified-version",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let setup = relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let preferences_path = root.path().join("config/preferences.toml");
    let mut preferences = std::fs::read_to_string(&preferences_path).expect("preferences");
    preferences.push_str("usage_integration_enabled = true\n");
    std::fs::write(&preferences_path, preferences).expect("enable usage integration");

    let output = relay(
        root.path(),
        &["doctor", "--claude-executable", &claude.path_text()],
    );
    // A warning never blocks readiness.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = json_stdout(&output)["data"].clone();
    assert_eq!(data["ready"], true);
    assert_eq!(data["overall"], "warning");
    let version_check = data["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["label"] == "Claude Code 2.1.279 supported")
        .expect("version check is present");
    assert_eq!(version_check["level"], "warning");
    assert!(
        version_check["detail"]
            .as_str()
            .expect("detail")
            .contains("fail-closed")
    );
}

#[test]
fn doctor_human_output_matches_the_documented_shape() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &claude.path_text(),
        ],
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("doctor")
        .arg("--claude-executable")
        .arg(claude.path_text())
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let output = command.output().expect("run relay doctor");
    assert!(output.status.success());
    let human = String::from_utf8_lossy(&output.stdout);
    assert!(human.starts_with("Agent Relay health"));
    assert!(human.contains("\u{2713} alice (Claude) authenticated"));
    assert!(human.contains("Ready for automatic handoff."));
}

/// Reproduces the exact false-positive a live `/relay:doctor` hit: `relay doctor` invoked as a
/// child of an *already-running, different* Claude Code session, inheriting that session's own
/// `CLAUDE_CONFIG_DIR` (pointing at a different profile entirely) and its
/// `CLAUDE_CODE_MESSAGING_TOKEN`. Neither is a real authentication override, so alice's check
/// must still report genuinely authenticated - never a false "Run: relay login alice".
#[test]
fn doctor_inside_a_live_claude_session_never_reports_a_false_login_remedy() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &claude);
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );

    // Simulates the ambient environment `/relay:doctor` actually runs in: a *different* Claude
    // Code session's own `CLAUDE_CONFIG_DIR` and its instance-scoped messaging token, both
    // inherited purely because this process is a child of that live session - not because
    // alice's own authentication is in any way in question.
    let output = relay_with_env(
        root.path(),
        &["doctor", "--claude-executable", &claude.path_text()],
        &[
            (
                "CLAUDE_CONFIG_DIR",
                &root
                    .path()
                    .join("some-other-live-sessions-profile")
                    .to_string_lossy(),
            ),
            (
                "CLAUDE_CODE_MESSAGING_TOKEN",
                "6053b7e95eedfd452239c466c8720498",
            ),
        ],
    );
    assert!(
        output.status.success(),
        "doctor must still report ready: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = json_stdout(&output)["data"].clone();
    assert_eq!(data["ready"], true);
    let alice_check = data["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["label"] == "alice (Claude) authenticated")
        .expect("alice's authentication check is present");
    assert_eq!(alice_check["level"], "ok");
    assert_eq!(alice_check["remedy"], Value::Null);
    assert_eq!(alice_check["detail"], Value::Null);
}

// =================================================================================================
// WHY / HISTORY / STATUS — against one live (non-attached) session.
// =================================================================================================

/// Sets up one adopted, set-up-with-usage-integration profile and launches one non-interactive,
/// non-attached session on it — enough durable state for `why`/`history`/`status` to have
/// something real to read. Returns the project directory the session was launched in.
fn live_session(root: &Path) -> (std::path::PathBuf, FakeClaude) {
    let project = tempdir().expect("project dir").keep();
    let claude = FakeClaude::new(
        root,
        "alice",
        "2.1.276",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
    );
    adopt_profile(root, "alice", &claude);
    accept_project_trust(root, "alice", &project);
    // `relay status` has no `--claude-executable` flag; it looks for a plain `claude` on PATH,
    // same as a real install, which `relay()`'s PATH-with-`root/bin` prepend picks up.
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::copy(&claude.executable, bin_dir.join("claude")).expect("claude fixture on PATH");
    let setup = relay(
        root,
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let launch = relay(
        root,
        &[
            "claude",
            "--project-dir",
            &project.to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello there",
        ],
    );
    assert!(
        launch.status.success(),
        "{}",
        String::from_utf8_lossy(&launch.stderr)
    );
    (project, claude)
}

#[test]
fn why_explains_unverifiable_usage_as_source_usage_unknown() {
    let root = tempdir().expect("tempdir");
    let (project, claude) = live_session(root.path());

    let output = relay(
        root.path(),
        &[
            "why",
            "--project",
            &project.to_string_lossy(),
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = json_stdout(&output)["data"].clone();
    // A freshly-launched fake profile has recorded no usage signal at all yet: Relay must fail
    // closed rather than guess, which `relay why` explains as `source_usage_unknown`.
    assert_eq!(data["category"], "source_usage_unknown");
    assert_eq!(data["current_owner"], "alice");
}

#[test]
fn history_shows_the_session_start_event() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let output = relay(
        root.path(),
        &["history", "--project", &project.to_string_lossy()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    let events = payload["data"]["events"].as_array().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event["summary"] == "Session started on alice")
    );
    assert!(payload["data"]["events"][0]["summary"] != Value::Null);
}

#[test]
fn history_limit_bounds_the_number_of_events_returned() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let output = relay(
        root.path(),
        &[
            "history",
            "--project",
            &project.to_string_lossy(),
            "--limit",
            "0",
        ],
    );
    assert!(output.status.success());
    let events = json_stdout(&output)["data"]["events"]
        .as_array()
        .expect("events")
        .clone();
    assert!(events.is_empty());
}

#[test]
fn status_reports_current_owner_and_automatic_handoff_readiness() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let output = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = json_stdout(&output)["data"].clone();
    assert_eq!(data["current_session"]["profile"], "alice");
    assert_eq!(data["automatic_handoff_ready"], true);

    let path_with_bin = std::env::join_paths(
        std::iter::once(root.path().join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("status")
        .arg("--project")
        .arg(&project)
        .env("PATH", path_with_bin)
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in HERDR_ENV_VARS {
        command.env_remove(variable);
    }
    let human_output = command.output().expect("run relay status");
    assert!(human_output.status.success());
    let human = String::from_utf8_lossy(&human_output.stdout);
    assert!(human.contains("Current"));
    assert!(human.contains("alice"));
    assert!(human.contains("Automatic handoff"));
}

#[test]
fn trust_blocks_readiness_per_profile_and_project_and_recovers_after_acceptance() {
    let root = tempdir().unwrap();
    let project = tempdir().unwrap();
    let other_project = tempdir().unwrap();
    let claude = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &claude);
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--usage-integration",
            "true",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(setup.status.success());
    let doctor = |dir: &Path| {
        relay(
            root.path(),
            &[
                "doctor",
                "--project",
                dir.to_str().unwrap(),
                "--claude-executable",
                &claude.path_text(),
            ],
        )
    };
    let output = doctor(project.path());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json_stdout(&output)["data"]["ready"], false);
    let payload = json_stdout(&output);
    let check = payload["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["label"] == "alice (Claude) project trust accepted")
        .unwrap();
    assert_eq!(check["level"], "blocking");
    assert!(check["detail"].as_str().unwrap().contains("trust prompt"));
    assert!(
        check["remedy"]
            .as_str()
            .unwrap()
            .contains("config/profiles/alice/claude")
    );
    accept_project_trust(root.path(), "alice", project.path());
    assert!(doctor(project.path()).status.success());
    assert_eq!(doctor(other_project.path()).status.code(), Some(1));
    // Authentication/integration stay valid; corrupt trust state alone must fail closed.
    std::fs::write(
        root.path()
            .join("config/profiles/alice/claude/.claude.json"),
        "corrupt",
    )
    .unwrap();
    let output = doctor(project.path());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json_stdout(&output)["data"]["ready"], false);
}

#[test]
fn status_uses_its_explicit_project_for_trust_not_the_shell_cwd() {
    let root = tempdir().unwrap();
    let (project, _) = live_session(root.path());
    let path = root
        .path()
        .join("config/profiles/alice/claude/.claude.json");
    std::fs::remove_file(path).unwrap();
    // Only the shell cwd is trusted, not the active session's project.
    accept_project_trust(root.path(), "alice", &std::env::current_dir().unwrap());
    let output = relay(
        root.path(),
        &["status", "--project", project.to_str().unwrap()],
    );
    assert!(output.status.success());
    assert_eq!(
        json_stdout(&output)["data"]["automatic_handoff_ready"],
        false
    );
    accept_project_trust(root.path(), "alice", &project);
    let output = relay(
        root.path(),
        &["status", "--project", project.to_str().unwrap()],
    );
    assert_eq!(
        json_stdout(&output)["data"]["automatic_handoff_ready"],
        true
    );
}

#[test]
fn a_missing_fallback_trust_blocks_doctor_and_status_even_before_a_session_starts() {
    let root = tempdir().unwrap();
    let project = tempdir().unwrap();
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    let bob = FakeClaude::new(root.path(), "bob", "2.1.276", "bbbb2222", "s2");
    adopt_profile(root.path(), "alice", &alice);
    adopt_profile(root.path(), "bob", &bob);
    accept_project_trust(root.path(), "alice", project.path());
    std::fs::create_dir(root.path().join("bin")).unwrap();
    std::fs::copy(&alice.executable, root.path().join("bin/claude")).unwrap();
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            "bob",
            "--usage-integration",
            "true",
            "--claude-executable",
            &alice.path_text(),
        ],
    );
    assert!(setup.status.success());
    let doctor = || {
        relay(
            root.path(),
            &[
                "doctor",
                "--project",
                project.path().to_str().unwrap(),
                "--claude-executable",
                &alice.path_text(),
            ],
        )
    };
    let output = doctor();
    assert_eq!(output.status.code(), Some(1));
    let data = json_stdout(&output);
    let checks = data["data"]["checks"].as_array().unwrap();
    assert!(checks.iter().any(
        |check| check["label"] == "alice (Claude) project trust accepted" && check["level"] == "ok"
    ));
    assert!(checks.iter().any(
        |check| check["label"] == "bob (Claude) project trust accepted"
            && check["level"] == "blocking"
    ));
    let status = relay(
        root.path(),
        &["status", "--project", project.path().to_str().unwrap()],
    );
    assert!(status.status.success());
    assert_eq!(
        json_stdout(&status)["data"]["automatic_handoff_ready"],
        false
    );
    accept_project_trust(root.path(), "bob", project.path());
    assert!(doctor().status.success());
}
