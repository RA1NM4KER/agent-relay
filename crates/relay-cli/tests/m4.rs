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
///
/// `relay setup`'s environment-check step looks for a plain `claude` on `PATH` (by design —
/// `setup_interactive_first_run_with_zero_profiles_fails_closed` below locks in that it refuses
/// to proceed at all otherwise), which a real developer machine always has but a bare CI runner
/// does not. `root.join("bin")` is prepended to the child's `PATH` here — harmless if that
/// directory doesn't exist — so a test can drop a `claude`-named fixture there and get realistic
/// "Claude Code is installed" behavior regardless of what's actually on the host's `PATH`.
fn relay_interactive(root: &Path, arguments: &[&str], stdin_lines: &str) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    let fixture_path = std::env::join_paths(
        std::iter::once(root.join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH");
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env("PATH", fixture_path)
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
    stopped_marker: std::path::PathBuf,
}

impl FakeClaude {
    fn new(root: &Path, auth_json: &str, bg_id: &str, session_id: &str, pid: u32) -> Self {
        Self::build(root, auth_json, bg_id, session_id, pid, true)
    }

    /// Like [`Self::new`], but `stop <id>` "succeeds" (exit 0, as the real `claude stop` would
    /// for a hung/unresponsive session) without ever making `agents --json` stop listing the
    /// session — i.e. the authoritative stop is issued but quiescence never actually arrives.
    /// Exists to prove `--new`'s stop-and-verify step fails closed (`StopNotVerified`) rather
    /// than trusting the stop command's own exit code, and that a failed stop never launches a
    /// second writer.
    fn new_with_ineffective_stop(
        root: &Path,
        auth_json: &str,
        bg_id: &str,
        session_id: &str,
        pid: u32,
    ) -> Self {
        Self::build(root, auth_json, bg_id, session_id, pid, false)
    }

    fn build(
        root: &Path,
        auth_json: &str,
        bg_id: &str,
        session_id: &str,
        pid: u32,
        stop_is_effective: bool,
    ) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let log_path = root.join("claude-invocations.log");
        let executable = root.join(format!("fake-claude-{bg_id}"));
        let stopped_marker = root.join(format!("stopped-{bg_id}"));
        // `stop_is_effective = false`: `stop` still exits 0 (as the real command would) but never
        // touches the marker, so `agents --json` keeps listing the session as live forever —
        // simulating a stop that was issued but never actually took effect.
        let stop_case = if stop_is_effective {
            format!(
                r#"stop) touch "{marker}"; exit 0 ;;"#,
                marker = stopped_marker.display()
            )
        } else {
            "stop) exit 0 ;;".to_owned()
        };
        let script = format!(
            r#"#!/bin/sh
LOG="{log}"
printf '{{"args":"%s","config_dir":"%s"}}\n' "$*" "$CLAUDE_CONFIG_DIR" >> "$LOG"
STOPPED="{marker}"
# A fresh interactive session (`claude [prompt] --session-id <uuid> ...`): the user's terminal
# session; it simply ends.
case " $* " in *" --session-id "*) exit 0 ;; esac
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
  --bg) rm -f "$STOPPED"; printf 'backgrounded \302\267 %s\n' "{bg_id}" ;;
  agents)
    if [ -f "$STOPPED" ]; then
      printf '[]\n'
    else
      # No "cwd" field: query_active_sessions never sets the spawned command's working
      # directory, so a real $PWD here would just be relay's own cwd, never the project
      # directory under test -- omitting it (deserializes as None) makes Relay's own cwd
      # filters treat every record as "in this project", matching purely on session id instead.
      printf '[{{"pid":{pid},"id":"{bg_id}","kind":"background","startedAt":1,"sessionId":"{session_id}","name":"x","status":"idle","state":"done"}}]\n'
    fi
    ;;
  {stop_case}
  attach) exit 0 ;;
  --resume) exit 0 ;;
  *) exit 2 ;;
esac
"#,
            log = log_path.display(),
            marker = stopped_marker.display(),
        );
        std::fs::write(&executable, script).expect("fake claude script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        Self {
            executable,
            log_path,
            stopped_marker,
        }
    }

    fn path_text(&self) -> String {
        self.executable.to_string_lossy().into_owned()
    }

    /// Path this instance's `stop` case touches once a stop is actually issued for its session —
    /// lets a test observe (or synchronize on) the exact moment a stop takes effect, independent
    /// of wall-clock guessing.
    fn stopped_marker_path(&self) -> std::path::PathBuf {
        self.stopped_marker.clone()
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
    // The wizard's environment-check step looks for a plain `claude` on `PATH` regardless of
    // which profiles are already adopted (see `relay_interactive`'s doc comment) — a copy of
    // alice's fixture, named `claude`, standing in for a real machine-wide Claude Code install.
    let bin_dir = root.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::copy(&claude.executable, bin_dir.join("claude")).expect("claude fixture on PATH");

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

/// Product contract (post-M4/M6 UX revision): `relay claude` never silently reattaches to an
/// already-active managed session — it fails closed, pointing at `relay resume`/`relay claude
/// --new` instead. Superseded the old `claude_entrypoint_reuses_an_active_lease_without_relaunching`
/// test, whose whole premise (silent reattach) is exactly the behavior this contract removes.
#[test]
fn claude_refuses_when_a_live_managed_session_already_exists() {
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

    // Second invocation: the fake `agents --json` still reports the same session as live (state
    // "done"/idle but *listed*), so this must refuse rather than reattaching or relaunching.
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
    assert!(!second.status.success());
    let error = json_stderr(&second)["error"].clone();
    assert_eq!(error["code"], "managed_session_active");
    assert!(error["message"].as_str().unwrap().contains("alice"));
    assert!(error["message"].as_str().unwrap().contains("relay resume"));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("relay claude --new")
    );

    // No second writer was ever attempted: exactly the one `--bg` launch from `first`.
    assert_eq!(bg_invocation_count(&claude), 1);
}

/// Dogfood-found bug (M6): `relay claude`'s final `claude attach <id>` step must run under the
/// isolated profile's own `CLAUDE_CONFIG_DIR`. Before this fix `exec_claude_attach` set no
/// `CLAUDE_CONFIG_DIR` at all, so `claude attach` searched whatever config dir this process's
/// *parent shell* happened to have (or the real default account's, if none) instead of the
/// profile Relay itself just launched the background job under — `claude attach` then reported
/// "No job matching '<id>'" even though the session was live under the correct profile.
#[test]
fn claude_attach_execs_under_the_lease_owners_isolated_config_dir() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    let alice_config_dir = adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    // No `--no-attach`: the real interactive fresh launch. Relay assigns the session id
    // (`claude --session-id`) and the user types their first message INSIDE Claude, so none is
    // passed here.
    let output = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let launch = claude
        .invocations()
        .into_iter()
        .find(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.contains("--session-id"))
        })
        .expect("an interactive `claude --session-id` invocation was logged");
    // Canonicalize: on macOS `tempdir()` paths live under a `/var/...` symlink that resolves to
    // `/private/var/...`, and profile registration stores the canonical form.
    let expected_config_dir = std::fs::canonicalize(&alice_config_dir).expect("alice config dir");
    assert_eq!(
        launch["config_dir"],
        expected_config_dir.to_string_lossy().to_string(),
        "the interactive launch must run under alice's own CLAUDE_CONFIG_DIR, not whatever this process inherited"
    );
    // Relay chose the session id, and it is exactly what the writer lease records.
    let session_id = launch["args"]
        .as_str()
        .and_then(|args| args.split("--session-id ").nth(1))
        .map(|rest| {
            rest.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .expect("session id argument");
    let lease_path = std::fs::read_dir(root.path().join("state").join("projects"))
        .expect("projects dir")
        .next()
        .expect("one project")
        .expect("entry")
        .path()
        .join("lease.json");
    let lease: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_path).expect("lease")).expect("json");
    assert_eq!(lease["session_id"], session_id.as_str());
    assert_eq!(lease["owner_profile"], "alice");
    assert_ne!(
        lease["owner_process"]["pid"], 0,
        "the real process was recorded"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("What would you like"),
        "Relay never asks for a first message"
    );
}

/// Requirement from the same dogfood bug (commit `142668f`): "do not assume the configured
/// primary is the owner, because after handoff the owner may be a fallback." `relay switch`
/// (Claude -> Claude is SESSION_CONTINUATION) leaves exactly this on-disk shape behind: a project
/// `lease.json` whose `owner_profile` is the handoff target, while `preferences.toml`'s primary
/// profile is untouched. `relay resume`'s bare (no-profile) form reads that same `lease.json` the
/// same way no matter how it got that shape — a completed `relay switch`, or (as constructed
/// directly here, to avoid needing a full Claude session-transfer fixture) a `relay launch
/// --profile bob` — both leave `owner_profile = bob` with `primary = alice`, so this exercises
/// the exact code path a post-handoff `relay resume` hits. (Under the post-M6-UX-revision
/// contract, a second `relay claude` here would correctly *refuse* rather than reattach — see
/// `claude_refuses_when_a_live_managed_session_already_exists` — so this test uses `relay resume`,
/// the sanctioned way to reattach.)
#[test]
fn resume_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");

    let alice_claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        1111,
    );
    let alice_config_dir = adopt_profile(root.path(), "alice", &alice_claude);

    let bob_claude = FakeClaude::new(
        root.path(),
        &auth_json_for("bob"),
        "bbbb2222",
        "22222222-2222-4222-8222-222222222222",
        2222,
    );
    let bob_config_dir = adopt_profile(root.path(), "bob", &bob_claude);

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

    // bob (a fallback, not the primary) is the project's writer — the state a completed
    // Claude A -> Claude B handoff leaves behind.
    let launch = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--profile",
            "bob",
            "--no-attach",
            "--claude-executable",
            &bob_claude.path_text(),
            "hi",
        ],
    );
    assert!(
        launch.status.success(),
        "{}",
        String::from_utf8_lossy(&launch.stderr)
    );
    assert_eq!(json_stdout(&launch)["data"]["profile"], "bob");

    // `relay resume` — no profile argument, so it resolves to the *lease owner* (bob), not the
    // configured primary (alice), exactly as a user's daily "continue where I left off" would.
    // bob's fake `agents --json` still lists the background job as live (never stopped), so the
    // correct native command is `claude attach <bob's handle>` — never `--resume` (M6 dogfood
    // finding #2: `--resume` against a still-live background job is rejected by real Claude).
    // Note: this exec's the chosen command (replacing the child process image), so unlike
    // `--no-attach` calls its stdout carries no JSON — status and the invocation log are all
    // that's observable here.
    let output = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--claude-executable",
            &bob_claude.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let attach_invocation = bob_claude
        .invocations()
        .into_iter()
        .rfind(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("attach "))
        })
        .expect("an `attach` invocation was logged");
    assert_eq!(attach_invocation["args"], "attach bbbb2222");
    assert!(
        bob_claude.invocations().iter().all(|invocation| {
            !invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("--resume "))
        }),
        "a live background job must never be resumed via `--resume`"
    );
    let resume_config_dir = attach_invocation["config_dir"]
        .as_str()
        .expect("config_dir logged as a string")
        .to_owned();
    // Canonicalize: on macOS `tempdir()` paths live under a `/var/...` symlink that resolves to
    // `/private/var/...`, and profile registration stores the canonical form.
    let expected_bob_config_dir =
        std::fs::canonicalize(&bob_config_dir).expect("bob config dir exists");
    let expected_alice_config_dir =
        std::fs::canonicalize(&alice_config_dir).expect("alice config dir exists");
    assert_eq!(
        resume_config_dir,
        expected_bob_config_dir.to_string_lossy().to_string(),
        "resume must run under the lease owner's (bob's) CLAUDE_CONFIG_DIR, not the primary's"
    );
    assert_ne!(
        resume_config_dir,
        expected_alice_config_dir.to_string_lossy().to_string()
    );
}

/// Counts logged `--bg` (background-launch) invocations for one `FakeClaude` — used to prove a
/// second `relay claude` call reused an existing lease rather than launching a fresh session, and
/// (post-UX-revision) that `--new` launches exactly one replacement, never more.
fn bg_invocation_count(claude: &FakeClaude) -> usize {
    claude
        .invocations()
        .into_iter()
        .filter(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("--bg"))
        })
        .count()
}

/// Counts logged `claude stop <id>` invocations — used to prove `--new` issues exactly one
/// authoritative stop (never zero when a live session exists, never more than one), and that a
/// plain `relay claude`/stale-lease recovery never issues one at all.
fn stop_invocation_count(claude: &FakeClaude) -> usize {
    claude
        .invocations()
        .into_iter()
        .filter(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("stop "))
        })
        .count()
}

/// Dogfood-found bug (M6, second finding): `relay resume` used to unconditionally run
/// `claude --resume <session_id>` — but real Claude rejects `--resume` against a session whose
/// `claude --bg` background job is still live ("running as a background session ... run `claude
/// attach <id>`"). `relay resume` (bare, no profile argument) must use `claude attach
/// <provider_handle>` for a live job, proven here via the fake's own invocation log — not
/// `--resume`, which must never be invoked at all in this scenario.
#[test]
fn resume_bare_attaches_to_a_live_background_job_not_resume() {
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

    let launch = relay(
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
    assert!(launch.status.success());

    // The launched background job still lists as live (fake `agents --json`, never stopped).
    let output = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let invocations = claude.invocations();
    let attach_invocation = invocations
        .iter()
        .rfind(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("attach "))
        })
        .expect("an `attach` invocation was logged");
    assert_eq!(attach_invocation["args"], "attach aaaa1111");
    assert!(
        invocations.iter().all(|invocation| {
            !invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("--resume "))
        }),
        "a live background job must never be resumed via `--resume`"
    );
}

/// The other half of the same M6 dogfood finding: once the recorded background job is
/// confirmed gone (a real, previously-alive pid — see the `--new`/stale-lease tests' doc
/// comments for why a synthetic pid can't prove "confirmed gone" — now killed and reaped, and
/// dropped from the fake's own `agents --json` listing), `relay resume` must fall back to native
/// `claude --resume <session_id>` — attaching would find nothing to attach to.
#[test]
fn resume_uses_native_resume_when_the_background_job_is_confirmed_dead() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let mut long_lived = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn long-lived process");
    let claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        long_lived.id(),
    );
    adopt_profile(root.path(), "alice", &claude);
    relay(
        root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );

    let launch = relay(
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
    assert!(launch.status.success());

    // The background job has genuinely ended: the real process behind the recorded pid is
    // confirmed gone, and the provider's own bookkeeping no longer lists it — but the native
    // session itself is still safely resumable.
    let _ = long_lived.kill();
    long_lived.wait().expect("long-lived process exits");
    std::fs::write(claude.stopped_marker_path(), b"").expect("mark session no longer listed");

    let output = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let invocations = claude.invocations();
    let resume_invocation = invocations
        .iter()
        .rfind(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("--resume "))
        })
        .expect("a `--resume` invocation was logged");
    assert_eq!(
        resume_invocation["args"],
        "--resume 11111111-1111-4111-8111-111111111111"
    );
    assert!(
        invocations.iter().all(|invocation| {
            !invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("attach "))
        }),
        "a confirmed-dead background job must never be attached to"
    );
}

/// Safety property: when the background job isn't listed and the recorded owner process's
/// identity can't be established either way (a synthetic pid never has a real start-time
/// fingerprint to compare against — genuinely indeterminate, not confirmed-gone), `relay resume`
/// must fail closed rather than guess between attaching and resuming — and, critically, must
/// never start a second writer as a side effect of that guess.
#[test]
fn resume_fails_closed_when_liveness_is_ambiguous() {
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

    let launch = relay(
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
    assert!(launch.status.success());

    // Drop the session from the provider's own listing with no way to establish whether the
    // recorded (synthetic) pid is really gone -- an indeterminate, not a confirmed, state.
    std::fs::write(claude.stopped_marker_path(), b"").expect("mark session no longer listed");

    let output = relay(
        root.path(),
        &[
            "resume",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "ambiguous_session_liveness"
    );

    // Fails closed: no attach, no resume, and no second writer launched (still just the one
    // `--bg` from the initial launch above).
    let invocations = claude.invocations();
    assert!(invocations.iter().all(|invocation| {
        let args = invocation["args"].as_str().unwrap_or("");
        !args.starts_with("attach ") && !args.starts_with("--resume ")
    }));
    assert_eq!(bg_invocation_count(&claude), 1);
}

/// Requirement 2: "If there is no resumable managed session, say so clearly."
#[test]
fn resume_with_no_active_session_says_so_clearly() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let output = relay(
        root.path(),
        &["resume", "--project-dir", &project.path().to_string_lossy()],
    );
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "no_active_writer_for_project"
    );
}

/// Core `--new` behavior: an active managed session is safely stopped (via the same
/// stop-and-verify machinery `relay switch`/recovery use) and confirmed gone before a genuinely
/// fresh one is launched — proven by the invocation log's exact ordering, never by internal state
/// alone.
///
/// Uses a real OS process's pid rather than a synthetic number: Relay's own liveness/quiescence
/// checks fall back to an authoritative pid+start-time-fingerprint comparison whenever a session
/// drops out of `claude agents --json`'s listing (see `ClaudeSourceLiveness::check`'s
/// "unestablishable identity fails closed" note), and that fallback can only ever resolve to a
/// genuine "confirmed gone" (`Some(false)`) — as opposed to an indeterminate reading it must
/// conservatively treat as still active — for a pid that was really alive when Relay first
/// recorded it. The process is killed and reaped the moment the fake `stop` case actually fires
/// (watched via its marker file) rather than after a fixed sleep: this machine's own per-`relay`-
/// invocation subprocess overhead varies (observed ~1.5s for a single call), so timing the real
/// process's death off wall-clock elapsed time would be flaky; tying it to the actual stop event
/// is not.
#[test]
fn claude_new_stops_the_existing_writer_and_starts_a_fresh_one() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    // Long-lived on purpose: it only needs to survive until the real `stop` event fires, which
    // the reaper thread below watches for directly.
    let mut long_lived = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn long-lived process");
    let long_lived_pid = long_lived.id();
    let claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        long_lived_pid,
    );
    let marker_path = claude.stopped_marker_path();
    let reaper = std::thread::spawn(move || {
        while !marker_path.exists() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = long_lived.kill();
        let _ = long_lived.wait();
    });

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
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(bg_invocation_count(&claude), 1);
    assert_eq!(stop_invocation_count(&claude), 0);

    // The session still lists as active (fake `agents --json`), so a plain `relay claude` here
    // would refuse (proven separately by `claude_refuses_when_a_live_managed_session_already_exists`).
    // `--new` must instead stop it (which triggers the reaper thread above, killing the real
    // process behind the recorded pid), confirm quiescence, then launch a genuinely fresh one —
    // never two writers coexisting.
    let second = relay(
        root.path(),
        &[
            "claude",
            "--new",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello again",
        ],
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(json_stdout(&second)["data"]["new_session"], true);

    assert_eq!(bg_invocation_count(&claude), 2, "exactly one relaunch");
    assert_eq!(
        stop_invocation_count(&claude),
        1,
        "exactly one stop, for the one prior writer"
    );

    let invocations = claude.invocations();
    let bg_indices: Vec<usize> = invocations
        .iter()
        .enumerate()
        .filter(|(_, invocation)| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("--bg"))
        })
        .map(|(index, _)| index)
        .collect();
    let stop_index = invocations
        .iter()
        .position(|invocation| {
            invocation["args"]
                .as_str()
                .is_some_and(|args| args.starts_with("stop "))
        })
        .expect("a stop invocation was logged");
    assert_eq!(bg_indices.len(), 2);
    assert!(
        bg_indices[0] < stop_index && stop_index < bg_indices[1],
        "stop must happen strictly between the two launches — no window where two writers exist"
    );

    reaper.join().expect("reaper thread");
}

/// Requirement: "If there is no existing session, it [`--new`] should simply behave like `relay
/// claude`" — in particular, never issuing a stop when there is nothing to stop.
#[test]
fn claude_new_behaves_like_claude_when_no_existing_session() {
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

    let output = relay(
        root.path(),
        &[
            "claude",
            "--new",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json_stdout(&output)["data"]["new_session"], true);
    assert_eq!(bg_invocation_count(&claude), 1);
    assert_eq!(
        stop_invocation_count(&claude),
        0,
        "nothing to stop, so stop must never be called"
    );
}

/// Safety property: if the authoritative stop can never be verified quiescent (a hung/unresponsive
/// session), `--new` must fail closed and must never proceed to launch a second writer — the
/// original lease is left exactly as it was.
///
/// Uses this test process's own pid — guaranteed alive for the test's entire duration — as the
/// recorded owner, so the pid+fingerprint fallback (see the previous test's doc comment) reads a
/// genuine "still the same process" the whole time and can never spuriously resolve to quiescent.
#[test]
fn claude_new_fails_closed_when_stop_cannot_be_verified_and_never_launches_a_second_writer() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let claude = FakeClaude::new_with_ineffective_stop(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        std::process::id(),
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

    let second = relay(
        root.path(),
        &[
            "claude",
            "--new",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello again",
        ],
    );
    assert!(!second.status.success());
    assert_eq!(json_stderr(&second)["error"]["code"], "stop_not_verified");

    // The failed stop must never be followed by a launch attempt: still just the one `--bg`.
    assert_eq!(bg_invocation_count(&claude), 1);

    // The original lease is untouched — still alice, still the original session.
    let projects_dir = root.path().join("state").join("projects");
    let project_entry = projects_dir
        .read_dir()
        .expect("projects dir")
        .next()
        .expect("one project")
        .expect("entry");
    let lease =
        std::fs::read_to_string(project_entry.path().join("lease.json")).expect("lease file");
    assert!(lease.contains("11111111-1111-4111-8111-111111111111"));
    assert!(lease.contains("\"alice\""));
}

/// Requirement: "stale/dead lease behavior remains recoverable" — without `--new`. When the
/// recorded owner's process is genuinely gone, a plain `relay claude` must recover automatically
/// (launching fresh) rather than refusing or requiring `--new`. Uses a real process's pid
/// (confirmed alive at first launch, then explicitly killed — a crash, not a `stop`), for the
/// same reason the `--new` tests above do: Relay's liveness check can only resolve a session
/// that has dropped out of `agents --json`'s listing to a genuine "confirmed gone" via a real,
/// previously-established pid+start-time fingerprint (see `ClaudeSourceLiveness::check`'s
/// "unestablishable identity fails closed" note) — a synthetic pid can't prove it. Long-lived
/// (not a fixed short sleep) so it reliably survives this machine's own variable per-call
/// subprocess overhead until this test explicitly kills it.
#[test]
fn claude_recovers_a_stale_lease_without_the_new_flag() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    let mut long_lived = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn long-lived process");
    let live_claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        long_lived.id(),
    );
    adopt_profile(root.path(), "alice", &live_claude);
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
            &live_claude.path_text(),
            "hello",
        ],
    );
    assert!(first.status.success());

    // The recorded process is now genuinely, confirmably gone — a crash, not a `stop`.
    let _ = long_lived.kill();
    long_lived.wait().expect("long-lived process exits");

    // A fresh FakeClaude under a different bg_id/session: its own `agents --json` listing never
    // mentions the original session id at all, exactly as a real crashed-then-restarted `claude
    // agents --json` would report once the crashed process is truly gone — the recorded pid (now
    // confirmed absent) is what settles the liveness check definitively.
    let crashed_claude = FakeClaude::new(
        root.path(),
        &auth_json_for("alice"),
        "cccc3333",
        "33333333-3333-4333-8333-333333333333",
        333,
    );

    let recovered = relay(
        root.path(),
        &[
            "claude",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &crashed_claude.path_text(),
            "picking back up",
        ],
    );
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let data = json_stdout(&recovered)["data"].clone();
    assert_eq!(data["new_session"], true);
    assert_eq!(data["session_id"], "33333333-3333-4333-8333-333333333333");
    // No stop was needed or attempted — a stale lease is simply recoverable without `--new`.
    assert_eq!(stop_invocation_count(&crashed_claude), 0);
}
