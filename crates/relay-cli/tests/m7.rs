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
    // ...then register and add bob as a fallback without touching the usage-integration flag
    // again, so bob never gets the integration installed. (`setup --usage-integration true` now
    // correctly aligns every profile registered at that moment.)
    let bob = FakeClaude::new(root.path(), "bob", "2.1.276", "bbbb2222", "s2");
    adopt_profile(root.path(), "bob", &bob);
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
    assert!(events.iter().any(|event| {
        event["kind"] == "session_started"
            && event["actor"] == "relay"
            && event["profile"] == "alice"
    }));
    assert!(payload["data"]["events"][0]["timestamp_unix_ms"] != Value::Null);
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

/// GitHub Issue #3: `--autonomous` is set once at session creation and `relay status` (a purely
/// local read, per the perf fix above) surfaces it — proving the flag actually reaches the
/// durable Relay Session record end to end, not just that the CLI parses it.
#[test]
fn autonomous_flag_is_persisted_and_surfaced_by_status() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir").keep();
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "2.1.276",
        "aaaa1111",
        "22222222-2222-4222-8222-222222222222",
    );
    adopt_profile(root.path(), "alice", &claude);
    accept_project_trust(root.path(), "alice", &project);
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
    let launch = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.to_string_lossy(),
            "--no-attach",
            "--autonomous",
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

    let status = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy(), "--json"],
    );
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let data = json_stdout(&status)["data"].clone();
    assert_eq!(data["current_session"]["execution_intent"], "autonomous");

    assert!(human_status_output(root.path(), &project).contains("Execution: autonomous"));
}

/// GitHub Issue #3 extension: `relay mode` reads/writes the same durable field the launch-time
/// flag does, through the real compiled binary. Uses `--no-attach` (a real, briefly-active
/// placeholder lease, per `activate_session`'s own doc comment — well within its 60s staleness
/// grace) since `relay mode` specifically requires an ACTIVE session, unlike `relay resume`.
#[test]
fn mode_show_and_set_round_trip_through_the_real_cli() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir").keep();
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "2.1.276",
        "aaaa1111",
        "33333333-3333-4333-8333-333333333333",
    );
    adopt_profile(root.path(), "alice", &claude);
    accept_project_trust(root.path(), "alice", &project);
    assert!(
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
        )
        .status
        .success()
    );
    assert!(
        relay(
            root.path(),
            &[
                "claude",
                "--project-dir",
                &project.to_string_lossy(),
                "--no-attach",
                "--claude-executable",
                &claude.path_text(),
                "hello there",
            ],
        )
        .status
        .success()
    );

    // Starts Interactive (no `--autonomous` at launch).
    let show = relay(
        root.path(),
        &["mode", "--project", &project.to_string_lossy()],
    );
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    assert_eq!(
        json_stdout(&show)["data"]["execution_intent"],
        "interactive"
    );
    assert_eq!(json_stdout(&show)["data"]["changed"], false);

    // Sets it live.
    let set = relay(
        root.path(),
        &[
            "mode",
            "autonomous",
            "--project",
            &project.to_string_lossy(),
        ],
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert_eq!(json_stdout(&set)["data"]["execution_intent"], "autonomous");
    assert_eq!(json_stdout(&set)["data"]["changed"], true);

    // Persisted: an independent `relay status` sees it too, and a bare `relay mode` shows it.
    let status = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    assert_eq!(
        json_stdout(&status)["data"]["current_session"]["execution_intent"],
        "autonomous"
    );
    let show_again = relay(
        root.path(),
        &["mode", "--project", &project.to_string_lossy()],
    );
    assert_eq!(
        json_stdout(&show_again)["data"]["execution_intent"],
        "autonomous"
    );

    // Idempotent: setting the same value again is not an error.
    let set_again = relay(
        root.path(),
        &[
            "mode",
            "autonomous",
            "--project",
            &project.to_string_lossy(),
        ],
    );
    assert!(set_again.status.success());

    // And back to interactive.
    let back = relay(
        root.path(),
        &[
            "mode",
            "interactive",
            "--project",
            &project.to_string_lossy(),
        ],
    );
    assert_eq!(
        json_stdout(&back)["data"]["execution_intent"],
        "interactive"
    );

    // Human output names the mode and, when set, includes the behavioral notice — never
    // permission-shaped language.
    let set_human = relay_human(
        root.path(),
        &[
            "mode",
            "autonomous",
            "--project",
            &project.to_string_lossy(),
        ],
    );
    let human = String::from_utf8_lossy(&set_human.stdout);
    assert!(human.contains("Execution mode: autonomous"));
    assert!(human.contains("continue"));
    assert!(!human.to_lowercase().contains("permission"));
}

/// GitHub Issue #3 extension: `relay resume --autonomous`/`--interactive` change (and persist) the
/// mode of a genuinely DORMANT session — the fake `claude` fixture exits its attach/resume
/// invocations instantly, so a plain (non `--no-attach`) `relay claude` launch here completes an
/// entire attach lifecycle and leaves the session dormant by the time the process returns
/// (`run_managed_terminal` releases synchronously on exit), which is exactly the state `relay
/// resume` needs to react to.
#[test]
fn resume_autonomous_and_interactive_flags_change_and_persist_the_mode() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir").keep();
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "2.1.276",
        "aaaa1111",
        "44444444-4444-4444-8444-444444444444",
    );
    adopt_profile(root.path(), "alice", &claude);
    accept_project_trust(root.path(), "alice", &project);
    assert!(
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
        )
        .status
        .success()
    );
    // A real (attaching) launch: the fixture exits instantly, so this session is dormant by the
    // time this call returns.
    let launch = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.to_string_lossy(),
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

    let resume = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.to_string_lossy(),
            "--autonomous",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        resume.status.success(),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );

    // The fixture's own attach exits instantly, so by the time any of these `relay` calls return
    // the session is dormant again - dormant sessions have no `current_session` (that is only ever
    // the currently ACTIVE/focused one), so this reads the per-session `execution_intent` that is
    // exposed on every row of `sessions` regardless of state.
    let status = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    assert_eq!(
        json_stdout(&status)["data"]["sessions"][0]["execution_intent"],
        "autonomous",
        "the explicit --autonomous resume must be visible to an independent `relay status` call"
    );

    // Dormant again (the resume attach also exits instantly): a plain `relay resume` with NEITHER
    // flag must PRESERVE the mode, not silently reset it back to Interactive.
    let resume_again = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.to_string_lossy(),
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        resume_again.status.success(),
        "{}",
        String::from_utf8_lossy(&resume_again.stderr)
    );
    let status_after = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    assert_eq!(
        json_stdout(&status_after)["data"]["sessions"][0]["execution_intent"],
        "autonomous",
        "resume with no mode flag must preserve the existing mode"
    );

    // And --interactive flips it back, again surviving on its own (dormant once more).
    let resume_interactive = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.to_string_lossy(),
            "--interactive",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        resume_interactive.status.success(),
        "{}",
        String::from_utf8_lossy(&resume_interactive.stderr)
    );
    let status_final = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    assert_eq!(
        json_stdout(&status_final)["data"]["sessions"][0]["execution_intent"],
        "interactive"
    );
}

/// Like [`relay`], but without the forced `--json`, for asserting on real human-terminal output.
fn relay_human(root: &Path, arguments: &[&str]) -> std::process::Output {
    let path_with_bin = std::env::join_paths(
        std::iter::once(root.join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
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

/// Issue #5: a fresh session (no `relay state update` ever run) reports "no working state yet" —
/// a normal condition, not an error, in both JSON and human output.
#[test]
fn state_show_reports_no_working_state_for_a_fresh_session() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let json_output = relay(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    assert!(
        json_output.status.success(),
        "{}",
        String::from_utf8_lossy(&json_output.stderr)
    );
    let data = json_stdout(&json_output)["data"].clone();
    assert_eq!(data["present"], false);

    let human_output = relay_human(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    assert!(
        human_output.status.success(),
        "{}",
        String::from_utf8_lossy(&human_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&human_output.stdout).contains("No working state recorded yet")
    );
}

/// Issue #5: the real end-to-end path — update through the actual CLI (JSON payload, structured
/// fields), then show, through the real active session created by `relay claude`. Also proves
/// `next_actions` replaces wholesale on a second update rather than accumulating.
#[test]
fn state_update_and_show_round_trip_through_the_real_cli() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let update = relay(
        root.path(),
        &[
            "state",
            "update",
            "--project",
            &project.to_string_lossy(),
            r#"{"goal":"ship issue #5","add_decisions":[{"summary":"use a flat snapshot","rationale":"single writer per session"}],"next_actions":["write tests"]}"#,
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );

    let show = relay(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let data = json_stdout(&show)["data"].clone();
    assert_eq!(data["present"], true);
    assert_eq!(data["working_state"]["goal"], "ship issue #5");
    assert_eq!(
        data["working_state"]["decisions"][0]["summary"],
        "use a flat snapshot"
    );
    assert_eq!(data["working_state"]["next_actions"][0], "write tests");

    // A second update replacing next_actions must not accumulate the first list.
    let second_update = relay(
        root.path(),
        &[
            "state",
            "update",
            "--project",
            &project.to_string_lossy(),
            r#"{"next_actions":["run cargo test"]}"#,
        ],
    );
    assert!(
        second_update.status.success(),
        "{}",
        String::from_utf8_lossy(&second_update.stderr)
    );
    let show_again = relay(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    let data = json_stdout(&show_again)["data"].clone();
    let next_actions = data["working_state"]["next_actions"]
        .as_array()
        .expect("array");
    assert_eq!(next_actions.len(), 1, "must be replaced, not accumulated");
    assert_eq!(next_actions[0], "run cargo test");
    // The goal/decision from the first update must survive an update that never mentions them.
    assert_eq!(data["working_state"]["goal"], "ship issue #5");
    assert_eq!(
        data["working_state"]["decisions"][0]["summary"],
        "use a flat snapshot"
    );

    let human_show = relay_human(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    let human_text = String::from_utf8_lossy(&human_show.stdout);
    assert!(human_text.contains("Goal: ship issue #5"));
    assert!(human_text.contains("use a flat snapshot"));
    assert!(human_text.contains("run cargo test"));
}

/// Issue #5: a bound violation is rejected with a clear, non-zero-exit error — never silently
/// truncated or accepted.
#[test]
fn state_update_rejects_a_bound_exceeding_payload() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let mut decisions = String::from("[");
    for index in 0..21 {
        if index > 0 {
            decisions.push(',');
        }
        decisions.push_str(&format!(r#"{{"summary":"d{index}"}}"#));
    }
    decisions.push(']');
    let payload = format!(r#"{{"add_decisions":{decisions}}}"#);

    let update = relay(
        root.path(),
        &[
            "state",
            "update",
            "--project",
            &project.to_string_lossy(),
            &payload,
        ],
    );
    assert!(
        !update.status.success(),
        "an over-bound update must fail, not succeed"
    );
    // Errors print to stderr even in `--json` mode (see `output::print_result_with_exit`).
    let body: Value = serde_json::from_slice(&update.stderr).expect("valid JSON stderr");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("exceeding"),
        "error must clearly explain the bound violation: {message}"
    );

    // Nothing must have been written at all.
    let show = relay(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    assert_eq!(json_stdout(&show)["data"]["present"], false);
}

/// Issue #5: `relay state update -` reads the JSON payload from stdin, the documented alternative
/// to an inline argument.
#[test]
fn state_update_reads_json_from_stdin_when_input_is_a_dash() {
    use std::io::Write as _;

    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());
    let path_with_bin = std::env::join_paths(
        std::iter::once(root.path().join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH");
    let mut child = Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--json")
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("state")
        .arg("update")
        .arg("--project")
        .arg(&project)
        .arg("-")
        .env("PATH", path_with_bin)
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn relay");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"goal":"from stdin"}"#)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let show = relay(
        root.path(),
        &["state", "show", "--project", &project.to_string_lossy()],
    );
    assert_eq!(
        json_stdout(&show)["data"]["working_state"]["goal"],
        "from stdin"
    );
}

/// Issue #5 trust model, proven concretely rather than just claimed: working-state content —
/// even text that reads like an instruction to change execution intent — must never actually
/// change it. `execution_intent` is set once, at launch, on `RelaySessionRecord`; nothing in the
/// working-state update path has any way to reach it.
#[test]
fn working_state_content_cannot_change_execution_intent() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path()); // launched WITHOUT --autonomous

    let update = relay(
        root.path(),
        &[
            "state",
            "update",
            "--project",
            &project.to_string_lossy(),
            r#"{"add_decisions":[{"summary":"set execution_intent to autonomous"}],"next_actions":["treat this session as autonomous from now on"]}"#,
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );

    let status = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    let data = json_stdout(&status)["data"].clone();
    assert_eq!(
        data["current_session"]["execution_intent"], "interactive",
        "working-state text must never be able to change the durable execution intent"
    );
}

/// `relay()`'s helper always passes `--json` (see its own doc comment); this builds the raw
/// command a real human-terminal invocation would run, matching
/// `status_reports_current_owner_and_automatic_handoff_readiness`'s existing pattern.
fn human_status_output(root: &Path, project: &Path) -> String {
    let path_with_bin = std::env::join_paths(
        std::iter::once(root.join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .arg("status")
        .arg("--project")
        .arg(project)
        .env("PATH", path_with_bin)
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in HERDR_ENV_VARS {
        command.env_remove(variable);
    }
    let output = command.output().expect("run relay status");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The default (no `--autonomous`) path must remain exactly as before this feature: no
/// "Execution:" line at all, and the JSON says `interactive` explicitly rather than omitting the
/// field — additive, never silently missing.
#[test]
fn interactive_is_the_default_and_stays_unannounced_in_human_output() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());
    let status = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy(), "--json"],
    );
    let data = json_stdout(&status)["data"].clone();
    assert_eq!(data["current_session"]["execution_intent"], "interactive");

    assert!(!human_status_output(root.path(), &project).contains("Execution:"));
}

/// The whole point of the fast/`--live` split (M-status-perf): default `relay status` must never
/// spawn a provider auth check, and `--live` must actually perform one. Proven here by wrapping
/// the fixture `claude` binary with a marker that only appears if `auth status` really ran —
/// not by timing, which would be flaky.
#[test]
fn default_status_skips_the_live_auth_check_but_live_performs_it() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    let marker = root.path().join("auth-status-was-called");
    let bin_claude = root.path().join("bin").join("claude");
    let real_claude = root.path().join("bin").join("claude-real");
    std::fs::rename(&bin_claude, &real_claude).expect("move fixture aside");
    let wrapper = format!(
        "#!/bin/sh\nif [ \"$1\" = auth ] && [ \"$2\" = status ]; then touch \"{}\"; fi\nexec \"{}\" \"$@\"\n",
        marker.display(),
        real_claude.display(),
    );
    std::fs::write(&bin_claude, wrapper).expect("wrapper script");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin_claude, std::fs::Permissions::from_mode(0o700))
            .expect("chmod wrapper");
    }

    let default_output = relay(
        root.path(),
        &["status", "--project", &project.to_string_lossy()],
    );
    assert!(
        default_output.status.success(),
        "{}",
        String::from_utf8_lossy(&default_output.stderr)
    );
    assert!(
        !marker.exists(),
        "default `relay status` must not perform a live auth check"
    );
    let default_data = json_stdout(&default_output)["data"].clone();
    assert_eq!(default_data["primary_authenticated_source"], "not_checked");
    assert_eq!(default_data["live"], false);

    let live_output = relay(
        root.path(),
        &["status", "--live", "--project", &project.to_string_lossy()],
    );
    assert!(
        live_output.status.success(),
        "{}",
        String::from_utf8_lossy(&live_output.stderr)
    );
    assert!(
        marker.exists(),
        "`relay status --live` must perform a real auth check"
    );
    let live_data = json_stdout(&live_output)["data"].clone();
    assert_eq!(live_data["primary_authenticated_source"], "live");
    assert_eq!(live_data["primary_authenticated"], "authenticated");
    assert_eq!(live_data["live"], true);
}

/// `--json` must stay machine-readable and must never leak a spinner escape sequence into stdout
/// — true for both the fast default (which should not even start a spinner: the whole operation
/// finishes inside `Progress`'s own start-delay) and `--live` (which is slow enough to normally
/// draw one in a real terminal, but `--json` must suppress it regardless of speed).
#[test]
fn json_mode_never_contains_control_characters_default_or_live() {
    let root = tempdir().expect("tempdir");
    let (project, _claude) = live_session(root.path());

    for args in [
        vec!["status", "--json", "--project"],
        vec!["status", "--live", "--json", "--project"],
    ] {
        let mut full_args = args;
        let project_str = project.to_string_lossy().into_owned();
        full_args.push(&project_str);
        let output = relay(root.path(), &full_args);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains('\u{1b}'),
            "--json output must never contain an escape sequence: {full_args:?}"
        );
        // Must still be valid, parseable JSON — not partially overwritten by a spinner frame.
        let _: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!("--json output was not valid JSON for {full_args:?}: {error}\n{stdout}")
        });
    }
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

// =================================================================================================
// INTEGRATION --ALL (Issue #9: dogfood/dev install path — align every Claude profile in one command)
// =================================================================================================

/// The real pain this closes: today's dev-testing workflow requires one `relay integration
/// claude install --profile X` per registered profile. `--all` must install for every registered
/// Claude profile in a single invocation, using the registered profile service — never scanning
/// directories — and must report every profile's own result, not just the last one.
#[test]
fn integration_install_all_aligns_every_registered_claude_profile_in_one_command() {
    let root = tempdir().expect("tempdir");
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    let bob = FakeClaude::new(root.path(), "bob", "2.1.276", "bbbb2222", "s2");
    adopt_profile(root.path(), "alice", &alice);
    adopt_profile(root.path(), "bob", &bob);

    let install = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--all",
            "--allow-unverified-version",
            "--claude-executable",
            &alice.path_text(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let data = json_stdout(&install)["data"].clone();
    let results = data["results"].as_array().expect("results array for --all");
    assert_eq!(results.len(), 2, "one result per registered Claude profile");
    for result in results {
        assert_eq!(result["already_installed"], false);
    }

    // Confirm it actually landed on disk for both profiles, not just reported success.
    for name in ["alice", "bob"] {
        let status = relay(
            root.path(),
            &["integration", "claude", "status", "--profile", name],
        );
        assert!(status.status.success());
        assert_eq!(
            json_stdout(&status)["data"]["status"]["installed"],
            true,
            "{name} should have the integration installed"
        );
    }
}

/// A single explicit target must behave exactly as it did before `--all` existed: a flat
/// `config_dir`/`status` JSON shape, not wrapped in a `results` array — existing scripts and the
/// tests above that read `data["status"]` directly must not need to change.
#[test]
fn integration_status_single_target_keeps_the_flat_json_shape_not_all() {
    let root = tempdir().expect("tempdir");
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &alice);
    let status = relay(
        root.path(),
        &["integration", "claude", "status", "--profile", "alice"],
    );
    assert!(status.status.success());
    let data = json_stdout(&status)["data"].clone();
    assert!(
        data.get("results").is_none(),
        "a single target must not be wrapped in a results array"
    );
    assert!(
        data.get("status").is_some(),
        "single target keeps its flat shape"
    );
}

/// `--all` with zero registered Claude profiles must fail clearly, not silently succeed having
/// done nothing.
#[test]
fn integration_install_all_with_no_claude_profiles_fails_clearly() {
    let root = tempdir().expect("tempdir");
    // A freshly-initialized state root: no profiles registered at all yet.
    let install = relay(root.path(), &["integration", "claude", "install", "--all"]);
    assert!(!install.status.success());
}

/// `--all` is mutually exclusive with `--profile`/`--config-dir`/`--native-default` — clap's own
/// arg-group enforcement, exercised through the real CLI rather than assumed.
#[test]
fn integration_all_conflicts_with_an_explicit_profile_target() {
    let root = tempdir().expect("tempdir");
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    adopt_profile(root.path(), "alice", &alice);
    let install = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--all",
            "--profile",
            "alice",
        ],
    );
    assert!(!install.status.success());
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflict"),
        "expected a clap conflict message, got: {stderr}"
    );
}

/// `relay-dev setup` is the promised one-command channel alignment, not merely a shortcut for
/// profiles selected as today's primary/fallback order. A registered Claude profile can be held
/// outside that order for later use; it must still receive the hook path of the executable that
/// ran setup.
#[test]
fn setup_usage_integration_aligns_every_registered_claude_profile() {
    let root = tempdir().expect("tempdir");
    let alice = FakeClaude::new(root.path(), "alice", "2.1.276", "aaaa1111", "s1");
    let bob = FakeClaude::new(root.path(), "bob", "2.1.276", "bbbb2222", "s2");
    adopt_profile(root.path(), "alice", &alice);
    adopt_profile(root.path(), "bob", &bob);

    // Deliberately omit bob from --fallback: setup must still align all registered profiles.
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
            &alice.path_text(),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    for name in ["alice", "bob"] {
        let status = relay(
            root.path(),
            &["integration", "claude", "status", "--profile", name],
        );
        assert!(status.status.success());
        assert_eq!(
            json_stdout(&status)["data"]["status"]["installed"],
            true,
            "setup should install the usage integration for {name}"
        );
    }
}
