//! M6 tests: Codex as a second provider. Against the real compiled `relay` binary, with fake
//! `claude` and `codex` shell-script executables standing in for the real CLIs — never a real
//! account, never real network calls. Synthetic `alice`(Claude)/`codex-main`(Codex) profile
//! names only.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use sha2::{Digest, Sha256};
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
    python3 -c 'import os,json; print(json.dumps(dict((k,os.environ.get(k)) for k in ["RELAY_EXECUTABLE","RELAY_CONFIG_ROOT","RELAY_STATE_ROOT","RELAY_PROJECT_DIR","RELAY_SESSION_ID"])))' > "{resume_log}.context"
    ;;
  app-server)
    # EventDrivenRuntime owns an externally listening app-server. Its spawn contract deliberately
    # waits only for the private endpoint path to exist; this tiny stand-in therefore needs no
    # protocol implementation for the planning/runtime executable-identity regression below.
    if [ "$2" = "--listen" ]; then
      sockpath=$(printf '%s' "$3" | sed 's#^unix://##')
      : > "$sockpath"
      trap 'exit 0' TERM
      while true; do sleep 1; done
    fi
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
    assert!(prompt.contains("single word READY"));
    assert!(prompt.contains("Do not continue the task yet"));
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
    let context: Value = serde_json::from_slice(
        &std::fs::read(format!("{}.context", codex.resume_log_path.display())).unwrap(),
    )
    .unwrap();
    assert_eq!(context["RELAY_EXECUTABLE"], env!("CARGO_BIN_EXE_relay"));
    assert_eq!(
        context["RELAY_CONFIG_ROOT"],
        std::fs::canonicalize(root.path().join("config"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(
        context["RELAY_STATE_ROOT"],
        std::fs::canonicalize(root.path().join("state"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(
        context["RELAY_PROJECT_DIR"],
        std::fs::canonicalize(project.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(
        context["RELAY_SESSION_ID"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        root.path()
            .join("config/profiles/codex-main/codex/skills/relay/SKILL.md")
            .exists()
    );
    let banner = String::from_utf8_lossy(&output.stderr);
    assert!(banner.contains("[Relay · codex-main]"));
    assert!(banner.contains("$relay doctor"));
}

#[test]
fn event_runtime_reuses_the_resolved_terminal_codex_executable() {
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
    let codex = FakeCodex::new(root.path(), "codex-main", "01a-resolved-thread");
    login_claude(root.path(), "alice", &claude);
    login_codex(root.path(), "codex-main", &codex);
    launch_claude_writer(root.path(), project.path(), "alice", &claude);
    let switched = relay(
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
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );

    // The fake is deliberately available as `codex` only through PATH. No explicit override on
    // the first resume means `plan_codex_resume(None, ...)` must discover and canonicalize this
    // path; EventDrivenRuntime must receive that exact resolved `TerminalCommand.program`, not
    // retry a literal relative `codex` path.
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("bin");
    std::os::unix::fs::symlink(&codex.executable, bin.join("codex")).expect("PATH codex link");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let run_resume = |explicit: bool| {
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
            .env("PATH", &path)
            .env("RELAY_CODEX_EVENT_DRIVEN", "1")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME");
        if explicit {
            command.arg("--codex-executable").arg(&codex.executable);
        }
        command.output().expect("run relay resume")
    };

    let via_path = run_resume(false);
    assert!(
        via_path.status.success(),
        "PATH-resolved resume failed: {}",
        String::from_utf8_lossy(&via_path.stderr)
    );
    let explicit = run_resume(true);
    assert!(
        explicit.status.success(),
        "explicit resume failed: {}",
        String::from_utf8_lossy(&explicit.stderr)
    );

    assert_eq!(
        codex.resume_invocations().len(),
        2,
        "both normal terminal plans must execute the same fake Codex"
    );
    let canonical_project = std::fs::canonicalize(project.path()).expect("canonical project");
    let project_id =
        relay_core::handoff::ProjectId::for_canonical_path(&canonical_project).expect("project id");
    let trace = std::fs::read_to_string(
        root.path()
            .join("state/projects")
            .join(project_id.as_str())
            .join("auto-handoff.log"),
    )
    .expect("event-runtime trace");
    assert_eq!(
        trace
            .matches("codex_event_driven_app_server_spawned")
            .count(),
        2,
        "both PATH discovery and an explicit override must pass the exact verified-version gate: {trace}"
    );
    assert!(
        !trace.contains("codex_event_driven_version_unverified"),
        "the planned executable, never literal `./codex`, must reach version verification: {trace}"
    );
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
        // Keep the optional native-default statusline source fully inside this fixture, never
        // coupled to the developer or CI worker's real ~/.claude settings.
        .env("HOME", root)
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
fn setup_with_only_codex_reports_missing_project_trust_without_claude_remedies() {
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
        stdout.contains("automatic handoff is not ready yet")
            && stdout.contains("env CODEX_HOME=")
            && stdout.contains("trust prompt"),
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
    assert!(
        root.path()
            .join("config/profiles/codex-main/codex/skills/relay/SKILL.md")
            .is_file(),
        "setup must install the current executable's Relay Codex skill"
    );
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
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(root.path().join("config/profiles/alice/claude/.claude.json"),
        serde_json::to_vec(&serde_json::json!({"projects": {cwd.to_str().unwrap(): {"hasTrustDialogAccepted": true}}})).unwrap()).unwrap();
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
fn setup_offers_to_copy_a_default_statusline_into_an_empty_isolated_profile() {
    let root = tempdir().expect("tempdir");
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    login_claude(root.path(), "alice", &claude);
    std::fs::create_dir_all(root.path().join(".claude")).unwrap();
    std::fs::write(
        root.path().join(".claude/settings.json"),
        r#"{"statusLine":{"type":"command","command":"my-default-status","padding":2}}"#,
    )
    .unwrap();
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(root.path().join("config/profiles/alice/claude/.claude.json"),
        serde_json::to_vec(&serde_json::json!({"projects": {cwd.to_str().unwrap(): {"hasTrustDialogAccepted": true}}})).unwrap()).unwrap();

    // Use existing, do not add, enable usage integration, then explicitly accept the copy.
    let output = run_setup_wizard(root.path(), Some(&claude), None, "y\nn\ny\ny\n");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("does not inherit it. Copy it"), "{stdout}");
    let settings: Value = serde_json::from_slice(
        &std::fs::read(
            root.path()
                .join("config/profiles/alice/claude/settings.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(settings["statusLine"]["padding"], 2);
    assert!(
        settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|command| command.contains("--chain 'my-default-status'"))
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
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(root.path().join("config/profiles/alice/claude/.claude.json"),
        serde_json::to_vec(&serde_json::json!({"projects": {cwd.to_str().unwrap(): {"hasTrustDialogAccepted": true}}})).unwrap()).unwrap();
    std::fs::write(
        root.path()
            .join("config/profiles/codex-main/codex/config.toml"),
        toml::to_string(
            &serde_json::json!({"projects": {cwd.to_str().unwrap(): {"trust_level": "trusted"}}}),
        )
        .unwrap(),
    )
    .unwrap();
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

#[test]
fn codex_skill_can_be_installed_inspected_and_removed_without_touching_config() {
    let root = tempdir().unwrap();
    let codex = FakeCodex::new(root.path(), "codex-main", "thread-skill");
    login_codex(root.path(), "codex-main", &codex);
    let home = root.path().join("config/profiles/codex-main/codex");
    let config = home.join("config.toml");
    std::fs::write(&config, "# user config\n").unwrap();
    for action in ["install", "install", "status"] {
        let output = relay(
            root.path(),
            &["integration", "codex", action, "--profile", "codex-main"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(json_stdout(&output)["data"]["installed"], true);
    }
    let output = relay(
        root.path(),
        &[
            "integration",
            "codex",
            "uninstall",
            "--profile",
            "codex-main",
        ],
    );
    assert!(output.status.success());
    assert_eq!(json_stdout(&output)["data"]["installed"], false);
    assert_eq!(std::fs::read_to_string(config).unwrap(), "# user config\n");
}

#[test]
fn refresh_only_reconciles_previously_installed_relay_assets() {
    let root = tempdir().unwrap();
    let codex = FakeCodex::new(root.path(), "codex-main", "thread-refresh");
    let spare = FakeCodex::new(root.path(), "codex-spare", "thread-spare");
    login_codex(root.path(), "codex-main", &codex);
    login_codex(root.path(), "codex-spare", &spare);
    let installed = relay(
        root.path(),
        &["integration", "codex", "install", "--profile", "codex-main"],
    );
    assert!(installed.status.success());
    let profiles_before = std::fs::read(root.path().join("config/profiles.toml")).unwrap();
    let output = relay(root.path(), &["refresh"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results = &json_stdout(&output)["data"]["results"];
    assert_eq!(results.as_array().unwrap().len(), 2);
    assert_eq!(results[0]["status"], "current");
    assert_eq!(results[1]["status"], "skipped");
    assert!(
        !root
            .path()
            .join("config/profiles/codex-spare/codex/skills/relay/SKILL.md")
            .exists()
    );
    assert_eq!(
        std::fs::read(root.path().join("config/profiles.toml")).unwrap(),
        profiles_before
    );
}

/// Issue #14's refresh contract is intentionally broader than the active fallback set: every
/// registered profile with a proven Relay-owned integration is reconciled, while a profile that
/// merely happens to be registered remains untouched.  This also models a binary-channel change
/// by replacing the old executable path in managed Claude settings before invoking the currently
/// running test binary.
#[test]
fn refresh_reconciles_owned_assets_across_profiles_without_changing_user_configuration() {
    let root = tempdir().unwrap();
    let claude = FakeClaude::new(
        root.path(),
        "alice",
        "aaaa1111",
        "11111111-1111-4111-8111-111111111111",
        4242,
    );
    let codex = FakeCodex::new(root.path(), "codex-main", "thread-refresh-owned");
    let spare = FakeCodex::new(root.path(), "codex-spare", "thread-refresh-spare");
    login_claude(root.path(), "alice", &claude);
    login_codex(root.path(), "codex-main", &codex);
    login_codex(root.path(), "codex-spare", &spare);

    let installed = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--profile",
            "alice",
            "--claude-executable",
            &claude.path_text(),
        ],
    );
    assert!(installed.status.success(), "{:?}", installed);
    let installed = relay(
        root.path(),
        &["integration", "codex", "install", "--profile", "codex-main"],
    );
    assert!(installed.status.success(), "{:?}", installed);

    // Model a prior Relay binary's skill.  Its sidecar hash proves it is Relay-owned and
    // unedited, which is the only case refresh may replace.
    let codex_dir = root.path().join("config/profiles/codex-main/codex");
    let codex_skill = codex_dir.join("skills/relay/SKILL.md");
    let old_skill = "# old Relay skill\n";
    std::fs::write(&codex_skill, old_skill).unwrap();
    let old_skill_hash = format!("{:x}", Sha256::digest(old_skill.as_bytes()));
    std::fs::write(
        codex_dir.join("skills/relay/.relay-managed.json"),
        serde_json::json!({"version": 1, "sha256": old_skill_hash}).to_string(),
    )
    .unwrap();

    let profiles = root.path().join("config/profiles.toml");
    let profiles_before = std::fs::read(&profiles).unwrap();
    let claude_dir = root.path().join("config/profiles/alice/claude");
    let settings_path = claude_dir.join("settings.json");
    let mut settings: Value =
        serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    let old = "/usr/local/bin/relay";
    let statusline = settings["statusLine"]["command"]
        .as_str()
        .unwrap()
        .replace(env!("CARGO_BIN_EXE_relay"), old);
    settings["statusLine"]["command"] = Value::String(statusline);
    let hook = settings["hooks"]["StopFailure"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .replace(env!("CARGO_BIN_EXE_relay"), old);
    settings["hooks"]["StopFailure"][0]["hooks"][0]["command"] = Value::String(hook);
    std::fs::write(&settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    // A newly introduced managed command is restored, while an independently-owned command at
    // another Relay command path is never overwritten.
    let restored_command = claude_dir.join("commands/relay/history.md");
    std::fs::remove_file(&restored_command).unwrap();
    let foreign_command = claude_dir.join("commands/relay/status.md");
    std::fs::write(&foreign_command, "my status command\n").unwrap();

    let output = relay(root.path(), &["refresh"]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let results = json_stdout(&output)["data"]["results"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .any(|result| result["profile"] == "alice" && result["status"] == "refreshed")
    );
    assert!(
        results
            .iter()
            .any(|result| result["profile"] == "codex-main" && result["status"] == "refreshed")
    );
    assert!(
        results
            .iter()
            .any(|result| result["profile"] == "codex-spare" && result["status"] == "skipped")
    );
    let refreshed = std::fs::read_to_string(&settings_path).unwrap();
    assert!(
        refreshed.contains(env!("CARGO_BIN_EXE_relay")),
        "{refreshed}"
    );
    assert!(!refreshed.contains(old), "{refreshed}");
    assert!(restored_command.exists());
    assert!(
        std::fs::read_to_string(&codex_skill)
            .unwrap()
            .contains("$relay")
    );
    assert_eq!(
        std::fs::read_to_string(&foreign_command).unwrap(),
        "my status command\n"
    );
    assert_eq!(std::fs::read(&profiles).unwrap(), profiles_before);

    // The foreign-file report is informational rather than a write, so a second refresh is a
    // genuine no-op despite continuing to surface the protected file to the user.
    let after_first = std::fs::read(&settings_path).unwrap();
    let second = relay(root.path(), &["refresh"]);
    assert!(second.status.success());
    let results = &json_stdout(&second)["data"]["results"];
    assert!(
        results
            .as_array()
            .unwrap()
            .iter()
            .any(|result| { result["profile"] == "alice" && result["status"] == "current" })
    );
    assert_eq!(std::fs::read(&settings_path).unwrap(), after_first);
    assert_eq!(std::fs::read(&profiles).unwrap(), profiles_before);
}

#[test]
fn codex_doctor_blocks_untrusted_projects_and_passes_after_explicit_trust() {
    let root = tempdir().unwrap();
    let project = tempdir().unwrap();
    let codex = FakeCodex::new(root.path(), "codex-main", "thread-trust");
    login_codex(root.path(), "codex-main", &codex);
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "codex-main",
            "--usage-integration",
            "true",
            "--codex-executable",
            &codex.path_text(),
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
                "--codex-executable",
                &codex.path_text(),
            ],
        )
    };
    assert_eq!(doctor().status.code(), Some(1));
    let config = root
        .path()
        .join("config/profiles/codex-main/codex/config.toml");
    let canonical = std::fs::canonicalize(project.path()).unwrap();
    for (level, ready) in [("untrusted", false), ("trusted", true)] {
        std::fs::write(
            &config,
            toml::to_string(&serde_json::json!({"projects": {
            canonical.to_str().unwrap(): {"trust_level": level}}}))
            .unwrap(),
        )
        .unwrap();
        let output = doctor();
        assert_eq!(
            output.status.success(),
            ready,
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(json_stdout(&output)["data"]["ready"], ready);
    }
}
