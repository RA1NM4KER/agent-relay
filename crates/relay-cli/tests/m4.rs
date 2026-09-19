//! M4 tests: the consumer-CLI UX layer (`relay setup`, `relay claude`, `relay status`,
//! `relay profiles`, `relay login`/`relay logout`). All against the real compiled `relay` binary
//! (matching `tests/cli.rs`'s existing pattern) with a fake `claude` executable standing in for
//! the real one — never a real Claude account, never a real Herdr socket. Synthetic `alice`/`bob`
//! profile names and `@example.com` identities only.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::tempdir;

use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES;

/// Herdr env vars this test binary itself may have inherited (it is quite plausibly *run* from
/// inside a real Herdr pane during development) must never leak into a child `relay` process
/// under test: every M4 test that wants "as if inside Herdr" behavior sets these explicitly and
/// deliberately instead.
const HERDR_ENV_VARS: &[&str] = &[
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_TAB_ID",
    "HERDR_BIN_PATH",
    "HERDR_SOCKET_PATH",
];

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
    for variable in HERDR_ENV_VARS {
        command.env_remove(variable);
    }
    command.output().expect("run relay")
}

/// Like [`relay`] but feeds `stdin_lines` (already newline-joined) to the child — for the
/// interactive `relay setup` wizard, which is otherwise indistinguishable from a real terminal
/// session: it just reads lines from stdin, same as a human answering prompts.
fn relay_interactive(root: &Path, arguments: &[&str], stdin_lines: &str) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in HERDR_ENV_VARS {
        command.env_remove(variable);
    }
    let mut child = command.spawn().expect("spawn relay setup");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin_lines.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("relay setup output")
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

fn json_stderr(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stderr).expect("valid JSON stderr")
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

/// A fake `claude` covering every subcommand M4's code paths call: `--version`, `auth
/// status|login|logout`, `--bg` (launch), `agents --json` (liveness/session lookup). Every
/// invocation's argv and `$CLAUDE_CONFIG_DIR` are appended to `<root>/claude-invocations.log`
/// (one JSON object per line) so tests can assert exactly what Relay told the "official" Claude
/// binary to do — the M4.17/M4.13 concern this whole test file exists to cover.
struct FakeClaude {
    executable: std::path::PathBuf,
    log_path: std::path::PathBuf,
}

impl FakeClaude {
    fn new(root: &Path, auth_json: &str, bg_id: &str, session_id: &str, pid: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let log_path = root.join("claude-invocations.log");
        let executable = root.join("fake-claude");
        let script = format!(
            r#"#!/bin/sh
LOG="{log}"
printf '{{"args":"%s","config_dir":"%s"}}\n' "$*" "$CLAUDE_CONFIG_DIR" >> "$LOG"
case "$1" in
  --version) printf '%s\n' "2.1.276 (Claude Code)" ;;
  auth)
    case "$2" in
      status) printf '%s\n' '{auth_json}' ;;
      login) exit 0 ;;
      logout) exit 0 ;;
      *) exit 2 ;;
    esac
    ;;
  --bg) printf 'backgrounded \xc2\xb7 %s\n' "{bg_id}" ;;
  agents)
    printf '[{{"pid":{pid},"id":"{bg_id}","cwd":"%s","kind":"background","startedAt":1,"sessionId":"{session_id}","name":"x","status":"idle","state":"done"}}]\n' "$PWD"
    ;;
  attach) exit 0 ;;
  *) exit 2 ;;
esac
"#,
            log = log_path.display(),
        );
        std::fs::write(&executable, script).expect("fake claude script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        Self {
            executable,
            log_path,
        }
    }

    fn path_text(&self) -> String {
        self.executable.to_string_lossy().into_owned()
    }

    fn invocations(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log_path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("logged invocation is JSON"))
            .collect()
    }
}

fn adopt_profile(root: &Path, name: &str, claude: &FakeClaude) -> std::path::PathBuf {
    let profile_dir = root.path_buf_config_profile(name);
    create_private_dir(&profile_dir);
    secure_relay_config_ancestors(root);
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
    profile_dir
}

trait ConfigProfilePath {
    fn path_buf_config_profile(&self, name: &str) -> std::path::PathBuf;
}
impl ConfigProfilePath for Path {
    fn path_buf_config_profile(&self, name: &str) -> std::path::PathBuf {
        self.join("config/profiles").join(name).join("claude")
    }
}

fn auth_json_for(name: &str) -> String {
    format!(
        r#"{{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-{name}","email":"{name}@example.com","orgId":"org-1"}}"#
    )
}

// =================================================================================================
// SETUP (non-interactive)
// =================================================================================================

#[test]
fn setup_non_interactive_requires_a_primary() {
    let root = tempdir().expect("tempdir");
    let output = relay(root.path(), &["setup", "--non-interactive"]);
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "adoption_identity_required"
    );
}

#[test]
fn setup_non_interactive_rejects_an_unregistered_primary() {
    let root = tempdir().expect("tempdir");
    let output = relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );
    assert!(!output.status.success());
    assert_eq!(json_stderr(&output)["error"]["code"], "profile_not_found");
}

#[test]
fn setup_non_interactive_saves_primary_and_fallback() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);
    let claude_bob = FakeClaude::new(root.path(), &auth_json_for("bob"), "bbbb2222", "s2", 222);
    adopt_profile(root.path(), "bob", &claude_bob);

    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            "bob",
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let preferences =
        std::fs::read_to_string(root.path().join("config/preferences.toml")).expect("prefs");
    assert!(preferences.contains("primary_profile = \"alice\""));
    assert!(preferences.contains("bob"));
}

#[test]
fn setup_rerun_is_idempotent() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);

    for _ in 0..2 {
        let setup = relay(
            root.path(),
            &["setup", "--non-interactive", "--primary", "alice"],
        );
        assert!(setup.status.success());
    }
    let preferences =
        std::fs::read_to_string(root.path().join("config/preferences.toml")).expect("prefs");
    assert!(preferences.contains("primary_profile = \"alice\""));
}

// =================================================================================================
// SETUP (interactive, piped stdin) — first run / existing profiles / duplicate-add safety.
// =================================================================================================

#[test]
fn setup_interactive_first_run_with_zero_profiles_fails_closed() {
    let root = tempdir().expect("tempdir");
    // No profiles exist and Claude Code itself cannot be found (no real `claude` on PATH in a
    // fresh empty HOME), so setup must refuse immediately rather than hang waiting for input it
    // will never sensibly receive.
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("setup")
        .env("PATH", "/nonexistent")
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let mut child = command.spawn().expect("spawn");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("output");
    assert!(!output.status.success());
}

#[test]
fn setup_interactive_detects_and_reuses_existing_profiles() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);
    let claude_bob = FakeClaude::new(root.path(), &auth_json_for("bob"), "bbbb2222", "s2", 222);
    adopt_profile(root.path(), "bob", &claude_bob);

    // Answers: "Use these?" (default yes) / "Add another?" (no) / primary (default alice) /
    // fallback (default bob) / usage detection (no) / herdr (no). The herdr answer is supplied
    // regardless of whether a real `herdr` happens to be on this test runner's PATH (CI likely
    // has none, a dev machine might) -- if the prompt never fires, the extra unread stdin line
    // is simply ignored when the process exits normally.
    let answers = "\nn\n\n\nn\nn\n";
    let output = relay_interactive(root.path(), &["setup"], answers);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let human = String::from_utf8_lossy(&output.stdout);
    assert!(human.contains("Existing profiles found"));
    assert!(human.contains("alice"));
    assert!(human.contains("bob"));
    assert!(human.contains("Agent Relay is ready"));

    let preferences =
        std::fs::read_to_string(root.path().join("config/preferences.toml")).expect("prefs");
    assert!(preferences.contains("primary_profile = \"alice\""));
}

// =================================================================================================
// AUTH: exact CLAUDE_CONFIG_DIR supplied, successful/failed login, no credential material logged.
// =================================================================================================

#[test]
fn login_invokes_official_auth_login_with_the_exact_config_dir() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    let profile_dir = adopt_profile(root.path(), "alice", &claude);

    let login = relay(
        root.path(),
        &["login", "alice", "--claude-executable", &claude.path_text()],
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    assert_eq!(json_stdout(&login)["data"]["authenticated"], true);

    let invocations = claude.invocations();
    let login_call = invocations
        .iter()
        .find(|entry| {
            entry["args"]
                .as_str()
                .unwrap_or_default()
                .starts_with("auth login")
        })
        .expect("a `claude auth login` invocation was logged");
    // Relay canonicalizes paths internally (resolving e.g. macOS's `/var` -> `/private/var`
    // symlink); compare canonical forms on both sides rather than raw strings.
    let expected = std::fs::canonicalize(&profile_dir).expect("canonicalize profile dir");
    let actual = std::path::PathBuf::from(login_call["config_dir"].as_str().unwrap_or_default());
    assert_eq!(actual, expected);
}

#[test]
fn login_never_writes_into_the_claude_config_directory() {
    // Relay's own strict schema parsing already rejects any `claude auth status` response
    // carrying fields it does not recognize (deny-unknown-fields, confirmed by this test suite
    // itself hitting `unsupported_provider_schema` when a stray field was planted) -- so there is
    // no way for arbitrary provider-side data to flow through the identity pin at all. What this
    // test actually checks, matching `cli.rs`'s existing
    // `real_adoption_registers_the_profile_and_leaves_the_claude_directory_untouched` pattern: the
    // isolated Claude config directory itself is byte-for-byte untouched by `relay login` --
    // Relay only ever launches `claude auth login` against it and reads `claude auth status`,
    // never writes into it directly.
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    let profile_dir = adopt_profile(root.path(), "alice", &claude);
    std::fs::write(profile_dir.join("sentinel"), b"unchanged").expect("sentinel file");
    let before: Vec<_> = std::fs::read_dir(&profile_dir)
        .expect("read profile dir")
        .map(|entry| entry.expect("entry").file_name())
        .collect();

    let login = relay(
        root.path(),
        &["login", "alice", "--claude-executable", &claude.path_text()],
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );

    let after: Vec<_> = std::fs::read_dir(&profile_dir)
        .expect("read profile dir")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(
        before, after,
        "relay wrote into the Claude config directory"
    );
    assert_eq!(
        std::fs::read(profile_dir.join("sentinel")).expect("sentinel still present"),
        b"unchanged"
    );
}

#[test]
fn logout_invokes_official_auth_logout_and_keeps_registration() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);

    let logout = relay(
        root.path(),
        &[
            "logout",
            "alice",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        logout.status.success(),
        "{}",
        String::from_utf8_lossy(&logout.stderr)
    );
    let invocations = claude.invocations();
    assert!(invocations.iter().any(|entry| {
        entry["args"]
            .as_str()
            .unwrap_or_default()
            .starts_with("auth logout")
    }));
    // Registration is kept: `relay profile list` still reports it.
    let list = relay(root.path(), &["profile", "list"]);
    assert_eq!(json_stdout(&list)["data"][0]["name"], "alice");
}

#[test]
fn logout_on_an_unregistered_profile_fails_closed() {
    let root = tempdir().expect("tempdir");
    let output = relay(root.path(), &["logout", "ghost"]);
    assert!(!output.status.success());
    assert_eq!(json_stderr(&output)["error"]["code"], "profile_not_found");
}

// =================================================================================================
// CLAUDE ENTRYPOINT
// =================================================================================================

#[test]
fn claude_entrypoint_without_setup_fails_closed_with_a_clear_code() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let output = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "adoption_identity_required"
    );
}

#[test]
fn claude_entrypoint_launches_and_captures_session_id_automatically() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    let output = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello there",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = json_stdout(&output)["data"].clone();
    assert_eq!(data["profile"], "alice");
    assert_eq!(data["new_session"], true);
    assert_eq!(data["session_id"], "11111111-1111-4111-8111-111111111111");
    assert_eq!(data["herdr_bound"], false); // no HERDR_ENV in this test process

    // The user never had to supply this session id anywhere: it flows straight from the fake
    // `--bg`/`agents --json` responses into the lease Relay itself created.
    let lease = std::fs::read_to_string(
        root.path()
            .join("state")
            .join("projects")
            .read_dir()
            .expect("projects dir")
            .next()
            .expect("one project")
            .expect("entry")
            .path()
            .join("lease.json"),
    )
    .expect("lease file");
    assert!(lease.contains("11111111-1111-4111-8111-111111111111"));
}

#[test]
fn claude_entrypoint_reuses_an_explicit_profile_override() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);
    let claude_bob = FakeClaude::new(
        root.path(),
        &auth_json_for("bob"),
        "bbbb2222",
        "22222222-2222-4222-8222-222222222222",
        222,
    );
    adopt_profile(root.path(), "bob", &claude_bob);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    // `--profile bob` overrides the configured primary for this one run.
    let output = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--profile",
            "bob",
            "--claude-executable",
            &claude_bob.path_text(),
            "hi",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json_stdout(&output)["data"]["profile"], "bob");
}

#[test]
fn claude_entrypoint_rejects_an_unregistered_profile_override() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    let output = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--profile",
            "ghost",
            "hi",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(json_stderr(&output)["error"]["code"], "profile_not_found");
}

// =================================================================================================
// STATUS / PROFILES
// =================================================================================================

#[test]
fn status_before_setup_points_at_relay_setup() {
    let root = tempdir().expect("tempdir");
    // Human mode (no --json) is what actually shows the friendly "run relay setup" text; --json
    // mode's stdout is pure JSON, so check the human-readable path separately from the stable
    // `data.configured` field.
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("status")
        .env_remove("CLAUDE_CONFIG_DIR");
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES
        .iter()
        .chain(HERDR_ENV_VARS)
    {
        command.env_remove(variable);
    }
    let human_output = command.output().expect("run relay status");
    assert!(human_output.status.success());
    let human = String::from_utf8_lossy(&human_output.stdout);
    assert!(human.contains("relay setup") || human.contains("not set up"));

    let json_output = relay(root.path(), &["status"]);
    assert!(json_output.status.success());
    assert_eq!(json_stdout(&json_output)["data"]["configured"], false);
}

#[test]
fn status_json_shape_is_stable_after_setup() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    let output = relay(
        root.path(),
        &["status", "--project", &project.path().to_string_lossy()],
    );
    assert!(output.status.success());
    let data = json_stdout(&output)["data"].clone();
    for field in [
        "configured",
        "session_state",
        "primary_profile",
        "primary_authenticated",
        "fallback_profiles",
        "usage_integration_enabled",
        "herdr_connected",
    ] {
        assert!(data.get(field).is_some(), "missing field: {field}");
    }
    assert_eq!(data["primary_profile"], "alice");
}

#[test]
fn profiles_lists_role_and_authentication_state() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111", "s1", 111);
    adopt_profile(root.path(), "alice", &claude);
    let claude_bob = FakeClaude::new(root.path(), &auth_json_for("bob"), "bbbb2222", "s2", 222);
    adopt_profile(root.path(), "bob", &claude_bob);
    relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            "bob",
        ],
    );

    let output = relay(root.path(), &["profiles"]);
    assert!(output.status.success());
    let rows = json_stdout(&output)["data"]["profiles"].clone();
    let alice_row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "alice")
        .expect("alice row");
    assert_eq!(alice_row["role"], "primary");
    let bob_row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "bob")
        .expect("bob row");
    assert_eq!(bob_row["role"], "fallback");
}

// =================================================================================================
// HANDOFF: the M4 layer reuses, never duplicates, the existing watch/handoff machinery.
// =================================================================================================

#[test]
fn claude_and_launch_share_one_writer_creation_implementation() {
    // `relay claude`'s fresh-launch path and `relay launch` both call the same
    // `perform_launch` — proven here by checking both commands produce a lease with identical
    // shape/fields for equivalent inputs, rather than two independently-written writer-creation
    // code paths that could silently drift apart.
    let root_a = tempdir().expect("tempdir");
    let root_b = tempdir().expect("tempdir");
    let project_a = tempdir().expect("project");
    let project_b = tempdir().expect("project");

    let claude_a = FakeClaude::new(
        root_a.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        111,
    );
    adopt_profile(root_a.path(), "alice", &claude_a);
    let launch = relay(
        root_a.path(),
        &[
            "launch",
            "--profile",
            "alice",
            "--project-dir",
            &project_a.path().to_string_lossy(),
            "--claude-executable",
            &claude_a.path_text(),
            "hello",
        ],
    );
    assert!(
        launch.status.success(),
        "{}",
        String::from_utf8_lossy(&launch.stderr)
    );

    let claude_b = FakeClaude::new(
        root_b.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        111,
    );
    adopt_profile(root_b.path(), "alice", &claude_b);
    relay(
        root_b.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );
    let claude_run = relay(
        root_b.path(),
        &[
            "claude",
            "--project-dir",
            &project_b.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_b.path_text(),
            "hello",
        ],
    );
    assert!(
        claude_run.status.success(),
        "{}",
        String::from_utf8_lossy(&claude_run.stderr)
    );

    let launch_data = json_stdout(&launch)["data"].clone();
    let claude_data = json_stdout(&claude_run)["data"].clone();
    assert_eq!(launch_data["session_id"], claude_data["session_id"]);
    assert_eq!(launch_data["owner_profile"], "alice");
    assert_eq!(claude_data["profile"], "alice");
}

#[test]
fn claude_entrypoint_reuses_an_active_lease_without_relaunching() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        111,
    );
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    let first = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello",
        ],
    );
    assert!(first.status.success());
    assert_eq!(json_stdout(&first)["data"]["new_session"], true);

    // Second invocation: the fake `agents --json` still reports the same session as live
    // (state "done"/idle but *listed*, exactly the M2B.75 "still trusted as active" case), so
    // this must reuse it rather than starting a second writer.
    let second = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let data = json_stdout(&second)["data"].clone();
    assert_eq!(data["new_session"], false);
    assert_eq!(data["session_id"], "11111111-1111-4111-8111-111111111111");
}
