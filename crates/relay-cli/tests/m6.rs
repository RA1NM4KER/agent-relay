//! M6 tests: Codex as a second provider. Against the real compiled `relay` binary, with fake
//! `claude` and `codex` shell-script executables standing in for the real CLIs — never a real
//! account, never real network calls. Synthetic `alice`(Claude)/`codex-main`(Codex) profile
//! names only.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::tempdir;

use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES as CLAUDE_AUTH_OVERRIDE_VARIABLES;
use relay_provider_codex::AUTHENTICATION_OVERRIDE_VARIABLES as CODEX_AUTH_OVERRIDE_VARIABLES;

/// GitHub's hosted runners refuse `ps -E` (list a process's environment), and Linux `ps` has no such
/// flag: the handoff safety checks that need it correctly fail closed there, which is what these
/// tests would then observe. They are validated on macOS developer machines (the supported
/// platform); on such a runner they skip instead of reporting Relay's fail-closed behaviour as a
/// failure. (Same policy as `relay_provider_claude`'s own `ps_dash_e_is_available`.)
fn process_env_scan_available() -> bool {
    ["-Eww -o command=", "-Eww -axo pid=,command="]
        .iter()
        .all(|flags| {
            Command::new("ps")
                .args(flags.split(' '))
                .output()
                .is_ok_and(|output| output.status.success())
        })
}

macro_rules! skip_without_process_env_scan {
    () => {
        if !process_env_scan_available() {
            eprintln!("skipping: `ps -E` is unavailable in this environment");
            return;
        }
    };
}

fn relay(root: &Path, arguments: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--json")
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME");
    for variable in CLAUDE_AUTH_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in CODEX_AUTH_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    command.output().expect("run relay")
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

fn json_stderr(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stderr).expect("valid JSON stderr")
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("project dir");
    let run = |args: &[&str]| {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .expect("git")
                .success()
        );
    };
    run(&["-c", "init.defaultBranch=main", "init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("seed file");
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "init"]);
}

/// A fake `claude` covering the subcommands M6's cross-provider paths call. Mirrors
/// `crates/relay-cli/tests/m4.rs`'s `FakeClaude` (kept separate: integration test binaries can't
/// share private items across files).
struct FakeClaude {
    executable: std::path::PathBuf,
}

impl FakeClaude {
    fn new(root: &Path, name: &str, bg_id: &str, session_id: &str, pid: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let executable = root.join(format!("fake-claude-{name}"));
        let auth_json = format!(
            r#"{{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-{name}","email":"{name}@example.com","orgId":"org-1"}}"#
        );
        // `-p` (verification / bootstrap-target turn) replies with a fresh, deterministic
        // session id so a STATE_CONTINUATION Codex -> Claude switch can be asserted against it.
        let script = format!(
            r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' "2.1.276 (Claude Code)" ;;
  --help) printf -- '--output-format stream-json --verbose\n' ;;
  auth)
    case "$2" in
      status) printf '%s\n' '{auth_json}' ;;
      login) exit 0 ;;
      logout) exit 0 ;;
      *) exit 2 ;;
    esac
    ;;
  --bg) printf 'backgrounded \302\267 %s\n' "{bg_id}" ;;
  agents)
    printf '[{{"pid":{pid},"id":"{bg_id}","cwd":"%s","kind":"background","startedAt":1,"sessionId":"{session_id}","name":"x","status":"idle","state":"done"}}]\n' "$PWD"
    ;;
  attach) exit 0 ;;
  -p)
    cat >/dev/null
    printf '{{"session_id":"%s","is_error":false,"subtype":"success"}}\n' "{session_id}-bootstrapped"
    ;;
  *) exit 2 ;;
esac
"#,
        );
        std::fs::write(&executable, script).expect("fake claude script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        Self { executable }
    }

    fn path_text(&self) -> String {
        self.executable.to_string_lossy().into_owned()
    }
}

/// A fake `codex` covering `--version`, `doctor --json`, `login`, `logout`, `exec --json`
/// (reads the bootstrap prompt from stdin, logs it, emits a deterministic `thread.started` +
/// `turn.completed`), and `resume <thread-id>` (for `relay resume`).
struct FakeCodex {
    executable: std::path::PathBuf,
    exec_log_path: std::path::PathBuf,
    resume_log_path: std::path::PathBuf,
}

impl FakeCodex {
    fn new(root: &Path, name: &str, thread_id: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let executable = root.join(format!("fake-codex-{name}"));
        let exec_log_path = root.join(format!("codex-exec-{name}.log"));
        let resume_log_path = root.join(format!("codex-resume-{name}.log"));
        let script = format!(
            r#"#!/bin/sh
case "$1" in
  --version) printf 'codex-cli 0.155.0\n' ;;
  doctor) printf '{{"checks":{{"auth.credentials":{{"status":"ok","summary":"logged in via fake"}}}}}}\n' ;;
  login) exit 0 ;;
  logout) exit 0 ;;
  exec)
    PROMPT="$(cat)"
    printf '{{"codex_home":"%s","project_dir":"%s","prompt":%s}}\n' "$CODEX_HOME" "$PWD" "$(printf '%s' "$PROMPT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' 2>/dev/null || printf '"unavailable"')" >> "{exec_log}"
    printf '{{"type":"thread.started","thread_id":"%s"}}\n' "{thread_id}"
    printf '{{"type":"turn.started"}}\n'
    printf '{{"type":"turn.completed"}}\n'
    ;;
  resume)
    printf '%s %s\n' "$2" "$CODEX_HOME" >> "{resume_log}"
    ;;
  app-server)
    while IFS= read -r line; do
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      [ -z "$id" ] && continue
      case "$line" in
        *'"method":"initialize"'*) printf '{{"id":%s,"result":{{"codexHome":"%s"}}}}\n' "$id" "$CODEX_HOME" ;;
        *'"method":"thread/read"'*)
          tid=$(printf '%s' "$line" | sed -n 's/.*"threadId":"\([^"]*\)".*/\1/p')
          if [ "$tid" = "{thread_id}" ]; then
            printf '{{"id":%s,"result":{{"thread":{{"id":"%s"}}}}}}\n' "$id" "$tid"
          else
            printf '{{"id":%s,"error":{{"code":-32600,"message":"thread not loaded"}}}}\n' "$id"
          fi ;;
        *'"method":"account/read"'*) printf '{{"id":%s,"result":{{"account":{{"type":"chatgpt","email":null,"planType":"plus"}},"requiresOpenaiAuth":true}}}}\n' "$id" ;;
        *'"method":"account/rateLimits/read"'*)
          if [ -f "$CODEX_HOME/limits.json" ]; then
            printf '{{"id":%s,"result":%s}}\n' "$id" "$(cat "$CODEX_HOME/limits.json")"
          else
            printf '{{"id":%s,"result":{{"ordinaryUsageAllowed":true,"accountId":"a","rateLimits":{{"primary":{{"usedPercent":1}}}}}}}}\n' "$id"
          fi ;;
        *) printf '{{"id":%s,"error":{{"code":-32601,"message":"method not found"}}}}\n' "$id" ;;
      esac
    done ;;
  *) exit 2 ;;
esac
"#,
            exec_log = exec_log_path.display(),
            resume_log = resume_log_path.display(),
        );
        std::fs::write(&executable, script).expect("fake codex script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        Self {
            executable,
            exec_log_path,
            resume_log_path,
        }
    }

    fn path_text(&self) -> String {
        self.executable.to_string_lossy().into_owned()
    }

    fn exec_invocations(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.exec_log_path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("logged invocation is JSON"))
            .collect()
    }

    fn resume_invocations(&self) -> Vec<String> {
        std::fs::read_to_string(&self.resume_log_path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// Registers a fresh Claude profile and returns its config dir, exactly as `relay login`'s
/// create-new path does (never manually pre-creates the directory — that is Relay's own job).
fn login_claude(root: &Path, name: &str, claude: &FakeClaude) {
    let output = relay(
        root,
        &[
            "login",
            name,
            "--provider",
            "claude",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "login {name} (claude) failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn login_codex(root: &Path, name: &str, codex: &FakeCodex) {
    let output = relay(
        root,
        &[
            "login",
            name,
            "--provider",
            "codex",
            "--codex-executable",
            &codex.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "login {name} (codex) failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Establishes an active Claude writer lease for `project` via the real `relay claude` path
/// (mirrors `crates/relay-cli/tests/m4.rs`'s equivalent flow).
fn launch_claude_writer(root: &Path, project: &Path, profile: &str, claude: &FakeClaude) {
    let output = relay(
        root,
        &[
            "claude",
            "--project-dir",
            &project.to_string_lossy(),
            "--profile",
            profile,
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "hello there",
        ],
    );
    assert!(
        output.status.success(),
        "relay claude failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_codex_profile_registers_with_the_codex_provider_and_a_distinct_config_dir() {
    let root = tempdir().expect("tempdir");
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-thread-init");
    login_codex(root.path(), "codex-main", &codex);

    let status = relay(
        root.path(),
        &[
            "profile",
            "status",
            "codex-main",
            "--codex-executable",
            &codex.path_text(),
        ],
    );
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let data = json_stdout(&status)["data"].clone();
    assert_eq!(data["profile"]["provider"], "codex");
    assert!(
        data["profile"]["config_dir"]
            .as_str()
            .expect("config dir")
            .ends_with("codex-main/codex")
    );
    assert_eq!(data["authentication"], "authenticated");
}

#[test]
fn switch_claude_to_codex_is_state_continuation_and_moves_the_lease() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    init_git_repo(project.path());

    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-real-thread");
    login_codex(root.path(), "codex-main", &codex);

    launch_claude_writer(root.path(), project.path(), "alice", &claude);

    let output = relay(
        root.path(),
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "--codex-executable",
            &codex.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "switch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let journal = json_stdout(&output)["data"].clone();
    assert_eq!(journal["state"]["state"], "COMPLETE");
    assert_eq!(journal["continuity_type"], "STATE_CONTINUATION");
    assert_eq!(
        journal["verification"]["target_session_id"],
        "01a-real-thread"
    );
    // No provider-native transcript artifacts are ever copied for STATE_CONTINUATION.
    assert!(
        journal["transferred_artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Only a hash/size summary of the bundle is recorded, never its content.
    assert!(journal["bundle_summary"]["sha256"].as_str().unwrap().len() == 64);

    let status = relay(
        root.path(),
        &[
            "lock",
            "status",
            "--project-dir",
            &project.path().to_string_lossy(),
        ],
    );
    let lease = json_stdout(&status)["data"]["lease"].clone();
    assert_eq!(lease["owner_profile"], "codex-main");
    assert_eq!(lease["session_id"], "01a-real-thread");

    // The bootstrap prompt reached Codex over stdin (never argv/env): the fake logged what it
    // read on stdin, and it must contain real project context, never a transcript dump.
    let invocations = codex.exec_invocations();
    assert_eq!(invocations.len(), 1);
    let prompt = invocations[0]["prompt"].as_str().unwrap_or_default();
    assert!(prompt.contains("STATE_CONTINUATION"));
    assert!(prompt.contains("main")); // branch name from the seeded git repo
}

#[test]
fn switch_codex_to_claude_is_also_state_continuation() {
    skip_without_process_env_scan!();
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    init_git_repo(project.path());

    let codex = FakeCodex::new(root.path(), "codex-main", "01a-thread-a");
    login_codex(root.path(), "codex-main", &codex);
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);

    // Seed a Codex-owned writer lease via the already-proven Claude -> Codex switch path, then
    // switch back and assert the reverse direction.
    launch_claude_writer(root.path(), project.path(), "alice", &claude);
    relay(
        root.path(),
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "--codex-executable",
            &codex.path_text(),
        ],
    );

    let output = relay(
        root.path(),
        &[
            "switch",
            "alice",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "--codex-executable",
            &codex.path_text(),
        ],
    );
    assert!(
        output.status.success(),
        "switch back failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let journal = json_stdout(&output)["data"].clone();
    assert_eq!(journal["state"]["state"], "COMPLETE");
    assert_eq!(journal["continuity_type"], "STATE_CONTINUATION");
    assert_eq!(
        journal["verification"]["target_session_id"],
        "11111111-1111-4111-8111-111111111111-bootstrapped"
    );
}

#[test]
fn switching_to_the_current_writer_fails_closed() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    init_git_repo(project.path());
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    launch_claude_writer(root.path(), project.path(), "alice", &claude);

    let output = relay(
        root.path(),
        &[
            "switch",
            "alice",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "already_current_writer"
    );
}

#[test]
fn switching_with_no_active_writer_fails_closed() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    init_git_repo(project.path());
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-thread");
    login_codex(root.path(), "codex-main", &codex);

    let output = relay(
        root.path(),
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--codex-executable",
            &codex.path_text(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "no_active_writer_for_project"
    );
}

#[test]
fn a_malformed_codex_response_fails_the_switch_closed_without_moving_the_lease() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    init_git_repo(project.path());
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    launch_claude_writer(root.path(), project.path(), "alice", &claude);

    // A Codex fixture whose `exec --json` never emits a `thread.started` line at all.
    use std::os::unix::fs::PermissionsExt;
    let executable = root.path().join("fake-codex-broken");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
case "$1" in
  --version) printf 'codex-cli 0.155.0\n' ;;
  doctor) printf '{"checks":{"auth.credentials":{"status":"ok","summary":"ok"}}}\n' ;;
  login) exit 0 ;;
  exec) cat >/dev/null ; printf '{"type":"turn.failed"}\n' ;;
  app-server)
    while IFS= read -r line; do
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      [ -z "$id" ] && continue
      case "$line" in
        *'"method":"initialize"'*) printf '{"id":%s,"result":{"codexHome":"%s"}}\n' "$id" "$CODEX_HOME" ;;
        *'"method":"account/read"'*) printf '{"id":%s,"result":{"account":{"type":"chatgpt"}}}\n' "$id" ;;
        *'"method":"account/rateLimits/read"'*) printf '{"id":%s,"result":{"ordinaryUsageAllowed":true,"accountId":"a","rateLimits":{"primary":{"usedPercent":1}}}}\n' "$id" ;;
      esac
    done ;;
  *) exit 2 ;;
esac
"#,

    )
    .expect("broken codex script");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    let broken_path = executable.to_string_lossy().into_owned();
    login_codex(
        root.path(),
        "codex-broken",
        &FakeCodex {
            executable: executable.clone(),
            exec_log_path: root.path().join("unused.log"),
            resume_log_path: root.path().join("unused2.log"),
        },
    );

    let output = relay(
        root.path(),
        &[
            "switch",
            "codex-broken",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "--codex-executable",
            &broken_path,
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        json_stderr(&output)["error"]["code"],
        "malformed_provider_output"
    );

    let status = relay(
        root.path(),
        &[
            "lock",
            "status",
            "--project-dir",
            &project.path().to_string_lossy(),
        ],
    );
    let lease = json_stdout(&status)["data"]["lease"].clone();
    assert_eq!(
        lease["owner_profile"], "alice",
        "a failed switch must never move ownership"
    );
}

#[test]
fn resume_execs_codex_resume_under_the_profiles_own_codex_home() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project dir");
    init_git_repo(project.path());
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-resume-me");
    login_codex(root.path(), "codex-main", &codex);
    launch_claude_writer(root.path(), project.path(), "alice", &claude);
    relay(
        root.path(),
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "--codex-executable",
            &codex.path_text(),
        ],
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("resume")
        .arg("codex-main")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--codex-executable")
        .arg(codex.path_text())
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME");
    let output = command.output().expect("run relay resume");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let resumed = codex.resume_invocations();
    assert_eq!(resumed.len(), 1);
    assert!(resumed[0].starts_with("01a-resume-me"));
}

/// Runs the real interactive `relay setup` with the given fake provider CLIs (a provider left out
/// is simply not installed: `PATH` is empty, so nothing is discovered by accident).
fn run_setup_wizard(
    root: &Path,
    claude: Option<&FakeClaude>,
    codex: Option<&FakeCodex>,
    answers: &str,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .arg("setup")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(claude) = claude {
        command.arg("--claude-executable").arg(claude.path_text());
    }
    if let Some(codex) = codex {
        command.arg("--codex-executable").arg(codex.path_text());
    }
    // `relay doctor`'s shared readiness model (now also run at the end of an interactive
    // `relay setup`) makes its own fresh `auth status` calls, so this child process needs the
    // same override-free environment `relay()` already gives every other invocation in this file.
    for variable in CLAUDE_AUTH_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in CODEX_AUTH_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let mut child = command.spawn().expect("spawn relay setup");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(answers.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("relay setup output")
}

#[test]
fn setup_works_with_only_codex_installed_and_offers_only_relay_codex() {
    let root = tempdir().expect("tempdir");
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-setup-thread");
    // No Claude Code at all. Answers: profile name (the provider is implied), then "add another?" no.
    let output = run_setup_wizard(root.path(), None, Some(&codex), "codex-main\nn\n");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Claude Code        (not installed)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("relay codex") && stdout.contains("relay resume"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("relay claude"),
        "no unusable command is shown: {stdout}"
    );
    assert!(
        !stdout.contains("hook"),
        "the Claude integration is irrelevant here: {stdout}"
    );
    let rows = json_stdout(&relay(root.path(), &["profiles"]))["data"]["profiles"].clone();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "codex-main")
        .expect("codex-main registered");
    assert_eq!(row["provider"], "codex");
    assert_eq!(row["role"], "primary");
}

#[test]
fn setup_works_with_only_claude_installed_and_offers_only_relay_claude() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    // "Use these?" yes, "add another?" no, "enable automatic quota detection?" yes — reaching the
    // fully-ready completion screen, which is the only one that lists start commands at all.
    let output = run_setup_wizard(root.path(), Some(&claude), None, "y\nn\ny\n");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Codex CLI          (not installed)"),
        "{stdout}"
    );
    assert!(stdout.contains("Agent Relay is ready."), "{stdout}");
    assert!(
        stdout.contains("Start with:\n  relay claude") && !stdout.contains("relay codex"),
        "{stdout}"
    );
}

#[test]
fn setup_with_both_providers_shows_both_start_commands_as_peers() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-setup-thread");
    login_codex(root.path(), "codex-main", &codex);
    // use existing: yes; add another: no; primary: codex-main (a Codex primary is fine);
    // fallback order: default; automatic quota detection: yes — reaching the fully-ready
    // completion screen, which is the only one that lists start commands at all.
    let output = run_setup_wizard(
        root.path(),
        Some(&claude),
        Some(&codex),
        "y\nn\ncodex-main\n\ny\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Agent Relay is ready."), "{stdout}");
    assert!(
        stdout.contains("Start with:\n  relay claude\n  relay codex\n\nContinue the current conversation with:\n  relay resume"),
        "{stdout}"
    );
    assert!(stdout.contains("Primary\n  codex-main"), "{stdout}");
}

#[test]
fn setup_with_no_supported_cli_says_which_ones_are_supported() {
    let root = tempdir().expect("tempdir");
    let output = run_setup_wizard(root.path(), None, None, "");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("Claude Code") && stdout.contains("Codex CLI"),
        "{stdout}"
    );
    assert!(stdout.contains("at least one supported"), "{stdout}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("provider_executable_missing")
            || String::from_utf8_lossy(&output.stderr).contains("executable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// NATIVE_RESUME is only launched for a thread Codex itself confirms in the profile's own home:
/// when the lease's thread id is unknown to Codex, `relay resume` fails closed and never runs
/// `codex resume` (which could otherwise start a different thread).
#[test]
fn resume_refuses_a_codex_thread_the_profile_cannot_confirm() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project");
    init_git_repo(project.path());
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-real-thread");
    login_codex(root.path(), "codex-main", &codex);
    launch_claude_writer(root.path(), project.path(), "alice", &claude);
    relay(
        root.path(),
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.path_text(),
            "--codex-executable",
            &codex.path_text(),
        ],
    );
    // The same profile, but a Codex whose home does not contain the lease's thread.
    let stale = FakeCodex::new(root.path(), "codex-stale", "01a-some-other-thread");
    let output = Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .arg("resume")
        .arg("codex-main")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--codex-executable")
        .arg(stale.path_text())
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .expect("run relay resume");
    assert!(!output.status.success(), "must fail closed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("could not prove that Codex thread"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stale.resume_invocations().is_empty(), "never resumed");
}
