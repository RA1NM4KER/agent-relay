//! Session adoption and in-agent control: `relay claude --resume`, `/relay status|adopt|switch`
//! (answered by Relay's own `UserPromptSubmit` hook), the control channel to the supervising
//! terminal, the bare `relay switch` picker, and `relay login`'s provider resolution.
//!
//! The "agent" here is a fake `claude` that behaves like Claude Code where it matters: it keeps a
//! live-session registry entry (`<config>/sessions/<pid>.json`), reports `SessionStart` and
//! `UserPromptSubmit` to the configured hook commands with the same JSON and environment Claude
//! uses, and can be told to type `/relay …` at itself.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES as CLAUDE_AUTH_OVERRIDE_VARIABLES;
use relay_provider_codex::AUTHENTICATION_OVERRIDE_VARIABLES as CODEX_AUTH_OVERRIDE_VARIABLES;
use serde_json::Value;
use tempfile::tempdir;

/// GitHub's hosted runners refuse `ps -E`; the handoff safety checks that need it fail closed
/// there, so the tests that run a real transaction skip (same policy as `m6_auto.rs`).
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

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_SESSION: &str = "22222222-2222-4222-8222-222222222222";

fn scrub(command: &mut Command) {
    command
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_PID")
        .env_remove("CLAUDE_PROJECT_DIR");
    for variable in CLAUDE_AUTH_OVERRIDE_VARIABLES
        .iter()
        .chain(CODEX_AUTH_OVERRIDE_VARIABLES)
    {
        command.env_remove(variable);
    }
    for variable in [
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_TAB_ID",
        "HERDR_BIN_PATH",
        "HERDR_SOCKET_PATH",
    ] {
        command.env_remove(variable);
    }
}

fn base_command(root: &Path, json: bool, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    if json {
        command.arg("--json");
    }
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env("PATH", path_with_fixtures(root));
    scrub(&mut command);
    command
}

fn relay(root: &Path, arguments: &[&str]) -> std::process::Output {
    base_command(root, true, arguments).output().expect("relay")
}

fn path_with_fixtures(root: &Path) -> std::ffi::OsString {
    std::env::join_paths(
        std::iter::once(root.join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("PATH")
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

fn error_code(output: &std::process::Output) -> String {
    let value: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "JSON error on stderr, got: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    value["error"]["code"].as_str().unwrap_or("").to_owned()
}

fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

/// A fake Claude Code. Everything that is not the interactive resume flow behaves like the
/// fake in `m6_auto.rs`; `--resume <id> --settings <json>` additionally acts as the running agent.
fn install_fake_claude(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let executable = bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
{{ printf 'ARGV'; for a in "$@"; do printf '\037%s' "$a"; done; printf '|%s\n' "$CLAUDE_CONFIG_DIR"; }} >> "{argv}"
NAME=unknown
case "$CLAUDE_CONFIG_DIR" in */alice/*) NAME=alice ;; */bob/*) NAME=bob ;; esac
case "$1" in
  --version) printf '%s\n' "2.1.277 (Claude Code)" ;;
  --help) printf '%s\n' '--output-format <format> (choices: text, json, stream-json)' '--verbose' ;;
  auth)
    case "$2" in
      status) printf '{{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-%s","email":"%s@example.com","orgId":"org-1"}}\n' "$NAME" "$NAME" ;;
      login|logout) exit 0 ;;
      *) exit 2 ;;
    esac ;;
  --session-id)
    # `relay claude` (fresh interactive launch): a real Claude persists a conversation only once
    # the user has spoken; RELAY_TEST_TRANSCRIPT stands in for that.
    ID="$2"
    if [ -n "$RELAY_TEST_TRANSCRIPT" ]; then
      mkdir -p "$CLAUDE_CONFIG_DIR/projects/-fake" && echo '{{}}' > "$CLAUDE_CONFIG_DIR/projects/-fake/$ID.jsonl"
    fi
    [ -n "$RELAY_TEST_SLEEP" ] && exec sleep "$RELAY_TEST_SLEEP"
    exit 0 ;;
  agents) printf '[]\n' ;;
  stop) exit 0 ;;
  -p) cat >/dev/null
      RID="{SESSION}"; PREV=""; for a in "$@"; do [ "$PREV" = "--resume" ] && RID="$a"; PREV="$a"; done
      if [ -n "$RELAY_TEST_BAD_VERIFY" ]; then printf '{{"session_id":"99999999-9999-4999-8999-999999999999","is_error":false,"subtype":"success"}}\n'
      else printf '{{"session_id":"%s","is_error":false,"subtype":"success"}}\n' "$RID"; fi ;;
  --resume)
    ID="$2"; case "$ID" in --*) ID="" ;; esac
    # Only the launch Relay started for `relay claude --resume` acts as the running agent; a
    # continuation on a new owner just exits.
    [ -z "$RELAY_ADOPT_PROFILE" ] && exit 0
    [ -z "$ID" ] && ID="$RELAY_TEST_PICKED"
    mkdir -p "$CLAUDE_CONFIG_DIR/sessions"
    printf '{{"pid":%s,"sessionId":"%s","cwd":"%s","kind":"interactive"}}' "$$" "$ID" "$PWD" > "$CLAUDE_CONFIG_DIR/sessions/$$.json"
    SETTINGS=""; PREV=""
    for a in "$@"; do [ "$PREV" = "--settings" ] && SETTINGS="$a"; PREV="$a"; done
    CMD=$(printf '%s' "$SETTINGS" | sed 's/.*"command":"\([^"]*\)".*/\1/')
    TRANSCRIPT="$RELAY_TEST_TRANSCRIPT_DIR/$ID.jsonl"
    printf '{{"session_id":"%s","cwd":"%s","transcript_path":"%s","hook_event_name":"SessionStart","source":"resume"}}' "$ID" "$PWD" "$TRANSCRIPT" \
      | CLAUDE_PID=$$ CLAUDE_PROJECT_DIR="$PWD" sh -c "$CMD" > "$RELAY_TEST_ROOT/sessionstart.out" 2>&1
    if [ -n "$RELAY_TEST_PROMPT" ]; then
      sleep 1
      printf '{{"session_id":"%s","cwd":"%s","transcript_path":"%s","hook_event_name":"UserPromptSubmit","prompt":"%s"}}' "$ID" "$PWD" "$TRANSCRIPT" "$RELAY_TEST_PROMPT" \
        | CLAUDE_PID=$$ CLAUDE_PROJECT_DIR="$PWD" CLAUDE_CONFIG_DIR="$CLAUDE_CONFIG_DIR" sh -c "$RELAY_TEST_PROMPT_CMD" > "$RELAY_TEST_ROOT/prompt.out" 2>&1
    fi
    if [ -n "$RELAY_TEST_RAW_REQUEST" ]; then
      sleep 1
      # The control directory is the Relay session's own (a glob: its id is not known upfront).
      for d in $RELAY_TEST_CONTROL_DIR; do cp "$RELAY_TEST_RAW_REQUEST" "$d/request-raw.json"; done
      for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        for d in $RELAY_TEST_CONTROL_DIR; do
          [ -f "$d/response-raw.json" ] && cp "$d/response-raw.json" "$RELAY_TEST_ROOT/raw-response.json"
        done
        [ -f "$RELAY_TEST_ROOT/raw-response.json" ] && break
        sleep 0.5
      done
    fi
    exec sleep "${{RELAY_TEST_SLEEP:-1}}" ;;
  *) exit 2 ;;
esac
"#,
        argv = root.join("claude.argv").display(),
    );
    std::fs::write(&executable, script).expect("fake claude");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    executable
}

fn argv_log(root: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(root.join("claude.argv"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (argv, _config) = line.strip_prefix("ARGV")?.rsplit_once('|')?;
            Some(argv.split('\u{1f}').skip(1).map(str::to_owned).collect())
        })
        .collect()
}

fn login_claude(root: &Path, name: &str, claude: &Path) {
    let output = relay(
        root,
        &[
            "login",
            name,
            "--provider",
            "claude",
            "--claude-executable",
            &claude.to_string_lossy(),
        ],
    );
    assert!(
        output.status.success(),
        "login {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct World {
    root: tempfile::TempDir,
    project: tempfile::TempDir,
    claude: PathBuf,
    alice: PathBuf,
}

fn world() -> World {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project");
    init_git_repo(project.path());
    let claude = install_fake_claude(root.path());
    login_claude(root.path(), "alice", &claude);
    login_claude(root.path(), "bob", &claude);
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            "bob",
            "--claude-executable",
            &claude.to_string_lossy(),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let alice = root.path().join("config/profiles/alice/claude");
    World {
        root,
        project,
        claude,
        alice,
    }
}

impl World {
    fn canonical_project(&self) -> PathBuf {
        std::fs::canonicalize(self.project.path()).expect("canonical project")
    }

    fn transcript_dir(&self, config: &Path) -> PathBuf {
        config
            .join("projects")
            .join(relay_provider_claude::escape_project_path(
                &self.canonical_project(),
            ))
    }

    fn write_transcript(&self, config: &Path, session: &str) -> PathBuf {
        let dir = self.transcript_dir(config);
        std::fs::create_dir_all(&dir).expect("transcript dir");
        let path = dir.join(format!("{session}.jsonl"));
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        )
        .expect("transcript");
        path
    }

    /// The project's state directory (its Relay sessions live under `sessions/`).
    fn project_state(&self) -> PathBuf {
        let id = relay_core::handoff::ProjectId::for_canonical_path(&self.canonical_project())
            .expect("project id");
        self.root.path().join("state/projects").join(id.as_str())
    }

    /// Every Relay session directory of the project.
    fn session_dirs(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.project_state().join("sessions"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("session.json").exists())
            .collect()
    }

    /// The one session's directory (the project directory when it has none).
    fn state_dir(&self) -> PathBuf {
        let mut dirs = self.session_dirs();
        match dirs.len() {
            0 => self.project_state(),
            1 => dirs.remove(0),
            _ => panic!("expected at most one Relay session, found several"),
        }
    }

    /// The project's one session as `{owner_profile, session_id, ...}`: its active lease, or —
    /// once its provider process has exited and the lease was released — the history recorded on
    /// the session (`dormant: true`). `None` when the project has no session.
    fn lease(&self) -> Option<Value> {
        let dir = self.session_dirs().into_iter().next()?;
        if let Ok(text) = std::fs::read_to_string(dir.join("lease.json")) {
            return serde_json::from_str(&text).ok();
        }
        let record = self.record_of(&dir);
        Some(serde_json::json!({
            "owner_profile": record["last_profile"],
            "session_id": record["native_session_id"],
            "dormant": true,
        }))
    }

    /// The bytes that describe the one session's ownership (the lease, else the record).
    fn ownership_bytes(&self) -> Vec<u8> {
        let dir = self.state_dir();
        std::fs::read(dir.join("lease.json"))
            .unwrap_or_else(|_| std::fs::read(dir.join("session.json")).expect("session record"))
    }

    /// A session's record.
    fn record_of(&self, dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("session.json")).expect("record"))
            .expect("json")
    }

    fn agent_env(&self, command: &mut Command) {
        command.env("RELAY_TEST_ROOT", self.root.path()).env(
            "RELAY_TEST_TRANSCRIPT_DIR",
            self.transcript_dir(&self.alice),
        );
    }

    /// `relay claude` (a fresh Relay session, human mode, no terminal): the interactive launch.
    fn new_session_command(&self, sleep: &str, transcript: bool, extra: &[&str]) -> Command {
        let mut arguments = vec![
            "claude",
            "--project-dir",
            self.project.path().to_str().expect("utf8"),
            "--claude-executable",
            self.claude.to_str().expect("utf8"),
            "--profile",
            "alice",
        ];
        arguments.extend_from_slice(extra);
        let mut command = base_command(self.root.path(), false, &arguments);
        self.agent_env(&mut command);
        command.env("RELAY_TEST_SLEEP", sleep).stdin(Stdio::null());
        if transcript {
            command.env("RELAY_TEST_TRANSCRIPT", "1");
        }
        command
    }

    fn resume_command(&self, extra: &[&str]) -> Command {
        let mut arguments = vec![
            "claude",
            "--project-dir",
            self.project.path().to_str().expect("utf8"),
            "--claude-executable",
            self.claude.to_str().expect("utf8"),
        ];
        arguments.extend_from_slice(extra);
        let mut command = base_command(self.root.path(), true, &arguments);
        self.agent_env(&mut command);
        command
    }
}

// ---- relay claude --resume ----------------------------------------------------------------------

#[test]
fn claude_resume_adopts_exactly_the_selected_session_in_place() {
    let world = world();
    let transcript = world.write_transcript(&world.alice, SESSION);
    let before = std::fs::read(&transcript).expect("transcript bytes");

    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay claude --resume");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Claude's own resume flow ran, for exactly that id, with nothing that would fork or restart.
    let log = argv_log(world.root.path());
    let launch = log
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("--resume"))
        .expect("native resume flow was invoked");
    assert_eq!(launch[1], SESSION);
    assert!(launch.iter().any(|arg| arg == "--settings"));
    assert!(
        !log.iter()
            .flatten()
            .any(|arg| arg == "--session-id" || arg == "--fork-session")
    );

    // The exact selected conversation is now the managed one: same id, owner, a real process.
    let lease = world.lease().expect("a lease was created");
    assert_eq!(lease["session_id"], SESSION);
    assert_eq!(lease["owner_profile"], "alice");
    assert_eq!(
        lease["dormant"], true,
        "the provider exited, so the session is dormant"
    );
    // No second conversation, and the transcript is untouched.
    let transcripts = std::fs::read_dir(world.transcript_dir(&world.alice))
        .expect("dir")
        .count();
    assert_eq!(transcripts, 1);
    assert_eq!(
        std::fs::read(&transcript).expect("transcript bytes"),
        before
    );
    let hook_out = std::fs::read_to_string(world.root.path().join("sessionstart.out"))
        .expect("the hook answered");
    assert!(hook_out.contains("now managed"), "{hook_out}");
}

#[test]
fn claude_resume_without_an_id_adopts_whatever_the_picker_selected() {
    let world = world();
    world.write_transcript(&world.alice, OTHER_SESSION);
    let mut command = world.resume_command(&["--profile", "alice", "--resume"]);
    command.env("RELAY_TEST_PICKED", OTHER_SESSION);
    let output = command.output().expect("relay claude --resume");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // No id was passed to Claude: its picker chose.
    let log = argv_log(world.root.path());
    let launch = log
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("--resume"))
        .expect("resume flow");
    assert_eq!(launch[1], "--settings");
    assert_eq!(world.lease().expect("lease")["session_id"], OTHER_SESSION);
}

#[test]
fn claude_resume_refuses_unknown_or_malformed_sessions_and_creates_no_lease() {
    let world = world();
    let unknown = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay");
    assert!(!unknown.status.success());
    assert_eq!(error_code(&unknown), "adoption_refused");
    let malformed = world
        .resume_command(&["--profile", "alice", "--resume", "not-a-session"])
        .output()
        .expect("relay");
    assert_eq!(error_code(&malformed), "adoption_refused");
    assert!(world.lease().is_none(), "nothing was adopted");
    assert!(
        !argv_log(world.root.path())
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("--resume")),
        "Claude was never started"
    );
}

#[test]
fn a_session_belonging_to_another_profile_is_not_adopted() {
    let world = world();
    // The conversation lives in bob's profile; asking for alice must not find or adopt it.
    world.write_transcript(
        &world.root.path().join("config/profiles/bob/claude"),
        SESSION,
    );
    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay");
    assert_eq!(error_code(&output), "adoption_refused");
    assert!(world.lease().is_none());
}

/// Writes a legacy (pre-sessions) project lease whose owner is this very (live) test process: the
/// old one-lease-per-project layout, which the new binary migrates into one Relay session.
fn write_legacy_live_lease(world: &World, profile: &str, native: &str) {
    let state = world.project_state();
    std::fs::create_dir_all(&state).expect("state");
    let lease = serde_json::json!({
        "version": 1,
        "project_id": state.file_name().unwrap().to_str().unwrap(),
        "owner_profile": profile,
        "owner_process": relay_core::handoff::ProcessIdentity::current(),
        "session_id": native,
        "transaction_id": "ho-test",
        "acquired_unix_ms": 1,
        "provider_handle": null
    });
    std::fs::write(state.join("lease.json"), lease.to_string()).expect("lease");
}

#[test]
fn another_active_session_in_the_project_never_blocks_resuming_an_old_conversation() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    // Another conversation is live under bob (an old-format lease: migrated into a Relay session).
    write_legacy_live_lease(&world, "bob", OTHER_SESSION);
    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Two Relay sessions now; the other one is exactly as it was: still active, still bob's.
    let dirs = world.session_dirs();
    assert_eq!(dirs.len(), 2);
    let other = dirs
        .iter()
        .find(|dir| world.record_of(dir)["native_session_id"] == OTHER_SESSION)
        .expect("the migrated session");
    let lease: Value = serde_json::from_str(
        &std::fs::read_to_string(other.join("lease.json")).expect("still active"),
    )
    .expect("json");
    assert_eq!(lease["owner_profile"], "bob");
    let adopted = dirs
        .iter()
        .find(|dir| world.record_of(dir)["native_session_id"] == SESSION)
        .expect("the adopted session");
    assert_eq!(world.record_of(adopted)["last_profile"], "alice");
}

#[test]
fn the_exact_conversation_already_active_under_relay_is_never_given_a_second_owner() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    // SESSION is live under bob already.
    write_legacy_live_lease(&world, "bob", SESSION);
    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay");
    assert!(!output.status.success());
    assert_eq!(error_code(&output), "native_session_already_active");
    assert_eq!(world.session_dirs().len(), 1, "no second session");
    assert_eq!(world.lease().expect("lease")["owner_profile"], "bob");
}

#[test]
fn resume_is_a_real_option_and_the_passthrough_form_explains_the_difference() {
    let world = world();
    let pass = base_command(
        world.root.path(),
        true,
        &["claude", "--profile", "alice", "--", "--resume"],
    )
    .output()
    .expect("relay");
    assert_eq!(error_code(&pass), "provider_argument_rejected");
    let message = String::from_utf8_lossy(&pass.stderr).into_owned();
    assert!(message.contains("relay claude --resume"), "{message}");
    assert!(message.contains("relay resume"), "{message}");
    // …and `--resume` itself is no longer a generic unexpected-argument error.
    let bare = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay");
    assert!(!String::from_utf8_lossy(&bare.stderr).contains("unexpected argument"));
}

// ---- /relay inside a running (unmanaged) Claude ------------------------------------------------

struct LiveAgent {
    child: std::process::Child,
}

impl LiveAgent {
    /// A long-running stand-in for a Claude process, registered the way Claude registers itself.
    fn start(world: &World, config: &Path, session: &str) -> Self {
        let child = Command::new("sleep")
            .arg("120")
            .stdin(Stdio::null())
            .spawn()
            .expect("sleep");
        let sessions = config.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        std::fs::write(
            sessions.join(format!("{}.json", child.id())),
            serde_json::json!({
                "pid": child.id(),
                "sessionId": session,
                "cwd": world.canonical_project(),
                "kind": "interactive"
            })
            .to_string(),
        )
        .expect("registry");
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn type_into(
        &self,
        world: &World,
        config: &Path,
        session: &str,
        prompt: &str,
    ) -> Option<Value> {
        self.type_with(world, config, session, session, prompt)
    }

    /// `hook_session` is what the hook payload claims; `session` is what the registry recorded.
    fn type_with(
        &self,
        world: &World,
        config: &Path,
        _registered: &str,
        hook_session: &str,
        prompt: &str,
    ) -> Option<Value> {
        let transcript = world
            .transcript_dir(config)
            .join(format!("{hook_session}.jsonl"));
        let payload = serde_json::json!({
            "session_id": hook_session,
            "cwd": world.canonical_project(),
            "transcript_path": transcript,
            "hook_event_name": "UserPromptSubmit",
            "prompt": prompt,
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
        command
            .arg("--config-root")
            .arg(world.root.path().join("config"))
            .arg("--state-root")
            .arg(world.root.path().join("state"))
            .args(["hook", "claude", "prompt", "--config-dir"])
            .arg(config)
            .env("PATH", path_with_fixtures(world.root.path()))
            .env("CLAUDE_PID", self.pid().to_string())
            .env("CLAUDE_PROJECT_DIR", world.canonical_project())
            .env("CLAUDE_CONFIG_DIR", config)
            .env("CLAUDE_CODE_MESSAGING_TOKEN", "inherited-from-claude")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for variable in CLAUDE_AUTH_OVERRIDE_VARIABLES {
            command.env_remove(variable);
        }
        let mut child = command.spawn().expect("hook");
        {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.to_string().as_bytes())
                .expect("payload");
        }
        let output = child.wait_with_output().expect("hook output");
        assert!(output.status.success(), "a hook never fails the session");
        if output.stdout.is_empty() {
            return None;
        }
        Some(serde_json::from_slice(&output.stdout).expect("hook JSON"))
    }
}

impl Drop for LiveAgent {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

fn reason(value: &Option<Value>) -> String {
    let value = value.as_ref().expect("the hook answered");
    assert_eq!(value["decision"], "block", "the model must never see it");
    value["reason"].as_str().expect("reason").to_owned()
}

#[test]
fn ordinary_prompts_pass_through_the_hook_untouched() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let agent = LiveAgent::start(&world, &world.alice, SESSION);
    assert!(
        agent
            .type_into(&world, &world.alice, SESSION, "fix the bug")
            .is_none()
    );
    assert!(
        agent
            .type_into(&world, &world.alice, SESSION, "/relayx status")
            .is_none()
    );
    assert!(
        agent
            .type_into(&world, &world.alice, SESSION, "please /relay status")
            .is_none()
    );
}

#[test]
fn status_is_read_only_and_says_when_a_conversation_is_not_managed() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let agent = LiveAgent::start(&world, &world.alice, SESSION);
    let answer = agent.type_into(&world, &world.alice, SESSION, "/relay status");
    let text = reason(&answer);
    assert!(text.contains("not managed"), "{text}");
    assert!(text.contains("/relay adopt"), "{text}");
    assert!(world.lease().is_none(), "status cannot create anything");
}

#[test]
fn adopt_brings_the_live_conversation_under_relay_without_restarting_it() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let agent = LiveAgent::start(&world, &world.alice, SESSION);
    let text = reason(&agent.type_into(&world, &world.alice, SESSION, "/relay adopt"));
    assert!(text.contains("adopted"), "{text}");
    assert!(text.contains("alice"), "{text}");
    assert!(!text.contains(SESSION), "no raw session id is shown");
    // The lease names the very process that was already running: nothing was restarted.
    let lease = world.lease().expect("lease");
    assert_eq!(lease["session_id"], SESSION);
    assert_eq!(lease["owner_profile"], "alice");
    assert_eq!(lease["owner_process"]["pid"], agent.pid());
    // Idempotent, and status now reports it managed.
    let again = reason(&agent.type_into(&world, &world.alice, SESSION, "/relay adopt"));
    assert!(again.contains("already managed"), "{again}");
    let status = reason(&agent.type_into(&world, &world.alice, SESSION, "/relay status"));
    assert!(status.contains("Agent Relay: managed"), "{status}");
    assert!(status.contains("Profile: alice"), "{status}");
    assert!(!status.contains(SESSION), "status shows no raw ids");
    assert!(
        status.contains("not available here") || status.contains("switch"),
        "{status}"
    );
    // The managed conversation is what `relay status` and the badge machinery now see.
    assert!(
        relay(
            world.root.path(),
            &[
                "status",
                "--project",
                world.project.path().to_str().unwrap()
            ]
        )
        .status
        .success()
    );
}

#[test]
fn several_live_conversations_in_one_project_are_each_adopted_as_their_own_relay_session() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    world.write_transcript(&world.alice, OTHER_SESSION);
    let first = LiveAgent::start(&world, &world.alice, SESSION);
    assert!(
        reason(&first.type_into(&world, &world.alice, SESSION, "/relay adopt")).contains("adopted")
    );
    let second = LiveAgent::start(&world, &world.alice, OTHER_SESSION);
    let text = reason(&second.type_into(&world, &world.alice, OTHER_SESSION, "/relay adopt"));
    assert!(text.contains("adopted"), "{text}");
    assert!(text.contains("untouched"), "{text}");
    // Both sessions, both active, both alice's — each on its own live process.
    let dirs = world.session_dirs();
    assert_eq!(dirs.len(), 2);
    let leases: Vec<Value> = dirs
        .iter()
        .map(|dir| {
            serde_json::from_str(&std::fs::read_to_string(dir.join("lease.json")).unwrap()).unwrap()
        })
        .collect();
    assert!(leases.iter().all(|lease| lease["owner_profile"] == "alice"));
    let pids: Vec<u64> = leases
        .iter()
        .map(|lease| lease["owner_process"]["pid"].as_u64().unwrap())
        .collect();
    assert_ne!(pids[0], pids[1]);
    // Each shows ITS OWN session in /relay status (never "the project's" one).
    let status = reason(&second.type_into(&world, &world.alice, OTHER_SESSION, "/relay status"));
    assert!(status.contains("Agent Relay: managed"), "{status}");
    // Adopting a conversation that is already managed by a different process is refused; adopting
    // the same live one again is idempotent.
    let again = reason(&second.type_into(&world, &world.alice, OTHER_SESSION, "/relay adopt"));
    assert!(again.contains("already managed"), "{again}");
}

#[test]
fn adoption_fails_closed_when_the_identity_cannot_be_proven() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    world.write_transcript(&world.alice, OTHER_SESSION);
    let agent = LiveAgent::start(&world, &world.alice, SESSION);
    // The hook claims a session that Claude's registry does not have running.
    let text =
        reason(&agent.type_with(&world, &world.alice, SESSION, OTHER_SESSION, "/relay adopt"));
    assert!(
        text.contains("did nothing") || text.contains("refused"),
        "{text}"
    );
    assert!(world.lease().is_none());
    // A transcript-less (empty) conversation is not adoptable either.
    let empty = LiveAgent::start(&world, &world.alice, "33333333-3333-4333-8333-333333333333");
    let text = reason(&empty.type_into(
        &world,
        &world.alice,
        "33333333-3333-4333-8333-333333333333",
        "/relay adopt",
    ));
    assert!(
        text.contains("did nothing") || text.contains("refused"),
        "{text}"
    );
    assert!(world.lease().is_none());
}

#[test]
fn a_claude_profile_that_is_not_registered_is_named_as_such_and_no_profile_is_invented() {
    let world = world();
    let stranger = world.root.path().join("elsewhere/claude");
    std::fs::create_dir_all(&stranger).expect("dir");
    world.write_transcript(&stranger, SESSION);
    let agent = LiveAgent::start(&world, &stranger, SESSION);
    let status = reason(&agent.type_into(&world, &stranger, SESSION, "/relay status"));
    assert!(status.contains("not registered"), "{status}");
    let adopt = reason(&agent.type_into(&world, &stranger, SESSION, "/relay adopt"));
    assert!(adopt.contains("not registered"), "{adopt}");
    assert!(world.lease().is_none());
    let profiles = relay(world.root.path(), &["profiles"]);
    assert!(!String::from_utf8_lossy(&profiles.stdout).contains("elsewhere"));
}

#[test]
fn switch_from_an_unmanaged_or_unsupervised_conversation_never_moves_anything() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let agent = LiveAgent::start(&world, &world.alice, SESSION);
    let unmanaged = reason(&agent.type_into(&world, &world.alice, SESSION, "/relay switch bob"));
    assert!(unmanaged.contains("not managed"), "{unmanaged}");
    assert!(
        reason(&agent.type_into(&world, &world.alice, SESSION, "/relay adopt")).contains("adopted")
    );
    // Managed, but no Relay terminal supervises it: it must say so instead of switching itself.
    let text = reason(&agent.type_into(&world, &world.alice, SESSION, "/relay switch bob"));
    assert!(text.contains("not supervised"), "{text}");
    assert!(text.contains("relay switch bob"), "{text}");
    assert_eq!(world.lease().expect("lease")["owner_profile"], "alice");
    // Without a target it lists the choices in priority order, marking the current one.
    let listing = reason(&agent.type_into(&world, &world.alice, SESSION, "/relay switch"));
    assert!(
        listing.contains("bob") && listing.contains("alice"),
        "{listing}"
    );
    assert!(listing.contains("current"), "{listing}");
}

// ---- the supervised terminal ------------------------------------------------------------------

fn prompt_command(world: &World) -> String {
    format!(
        "'{}' --config-root '{}' --state-root '{}' hook claude prompt --config-dir '{}'",
        env!("CARGO_BIN_EXE_relay"),
        world.root.path().join("config").display(),
        world.root.path().join("state").display(),
        world.alice.display()
    )
}

#[test]
fn an_in_agent_switch_runs_the_normal_transaction_and_the_terminal_follows_the_new_owner() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let mut command = world.resume_command(&["--profile", "alice", "--resume", SESSION]);
    command
        .env("RELAY_TEST_PROMPT", "/relay switch bob")
        .env("RELAY_TEST_PROMPT_CMD", prompt_command(&world))
        .env("RELAY_TEST_SLEEP", "60");
    let output = command.output().expect("relay claude --resume");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer =
        std::fs::read_to_string(world.root.path().join("prompt.out")).expect("prompt output");
    assert!(answer.contains("switching to"), "{answer}");
    assert!(answer.contains("\"decision\":\"block\""), "{answer}");
    // Ownership moved through the regular transaction, and the terminal reopened the same
    // conversation on the new owner (bob's own isolated profile).
    assert_eq!(world.lease().expect("lease")["owner_profile"], "bob");
    let bob =
        std::fs::canonicalize(world.root.path().join("config/profiles/bob/claude")).expect("bob");
    let log = std::fs::read_to_string(world.root.path().join("claude.argv")).expect("log");
    assert!(
        log.lines()
            .any(|line| line.starts_with("ARGV\u{1f}--resume\u{1f}")
                && line.ends_with(&format!("|{}", bob.display()))),
        "the conversation was continued under bob:\n{log}"
    );
    let last = std::fs::read_to_string(world.state_dir().join("control/last.json")).expect("last");
    assert!(last.contains("\"ok\":true"), "{last}");
}

#[test]
fn a_control_request_that_does_not_match_the_current_session_is_refused() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let lease_probe = base_command(world.root.path(), true, &["--version"]);
    drop(lease_probe);
    let request = serde_json::json!({
        "version": 1,
        "id": "raw",
        // Not the session the lease names, and not the process the terminal runs.
        "request": {"kind": "switch", "target": "bob"},
        "session_id": OTHER_SESSION,
        "owner_profile": "alice",
        "caller_pid": 1,
        "requested_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
    });
    let request_path = world.root.path().join("raw-request.json");
    std::fs::write(&request_path, request.to_string()).expect("request");
    let control = world
        .project_state()
        .join("sessions")
        .join("*")
        .join("control");
    let mut command = world.resume_command(&["--profile", "alice", "--resume", SESSION]);
    command
        .env("RELAY_TEST_RAW_REQUEST", &request_path)
        .env("RELAY_TEST_CONTROL_DIR", &control)
        .env("RELAY_TEST_SLEEP", "2");
    let output = command.output().expect("relay claude --resume");
    assert!(output.status.success());
    wait_for("the supervisor's refusal", Duration::from_secs(5), || {
        world.root.path().join("raw-response.json").exists()
    });
    let response: Value = serde_json::from_slice(
        &std::fs::read(world.root.path().join("raw-response.json")).expect("response"),
    )
    .expect("json");
    assert_eq!(response["ok"], false);
    assert!(
        response["message"].as_str().unwrap_or("").contains("stale"),
        "{response}"
    );
    assert_eq!(
        world.lease().expect("lease")["owner_profile"],
        "alice",
        "nothing moved"
    );
}

/// A stand-in Claude *process* (a binary really named `claude`) that lives under alice's profile,
/// registered in Claude's session registry as working in `cwd`.
struct SleepingClaude {
    child: std::process::Child,
}

impl SleepingClaude {
    fn start(world: &World, session: &str, cwd: &Path) -> Self {
        let dir = world.root.path().join("other");
        std::fs::create_dir_all(&dir).expect("dir");
        let link = dir.join("claude");
        // A tiny program really named `claude` (compiled here: `ps -E` hides the environment of
        // protected system binaries such as /bin/sleep, and a copy of one cannot run).
        let source = dir.join("claude.rs");
        std::fs::write(
            &source,
            "fn main() { std::thread::sleep(std::time::Duration::from_secs(120)); }",
        )
        .expect("source");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        assert!(
            Command::new(rustc)
                .arg(&source)
                .arg("-o")
                .arg(&link)
                .status()
                .expect("rustc")
                .success()
        );
        let config = std::fs::canonicalize(&world.alice).expect("canonical");
        let child = Command::new(&link)
            .current_dir(cwd)
            .env("CLAUDE_CONFIG_DIR", &config)
            .stdin(Stdio::null())
            .spawn()
            .expect("sleeper");
        let sessions = config.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        std::fs::write(
            sessions.join(format!("{}.json", child.id())),
            serde_json::json!({
                "pid": child.id(), "sessionId": session, "cwd": cwd, "kind": "interactive"
            })
            .to_string(),
        )
        .expect("registry");
        Self { child }
    }
}

impl Drop for SleepingClaude {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

fn switch_via_agent(world: &World, extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut command = world.resume_command(&["--profile", "alice", "--resume", SESSION]);
    command
        .env("RELAY_TEST_PROMPT", "/relay switch bob")
        .env("RELAY_TEST_PROMPT_CMD", prompt_command(world))
        .env("RELAY_TEST_SLEEP", "60");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("relay claude --resume")
}

fn record_native(world: &World, dir: &Path) -> String {
    world.record_of(dir)["native_session_id"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

fn session_dir_of(world: &World, native: &str) -> Option<PathBuf> {
    world
        .session_dirs()
        .into_iter()
        .find(|dir| record_native(world, dir) == native)
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The heart of "Relay supervises conversations, not repositories": two live Relay sessions of the
/// same project, BOTH on alice. One of them switches itself to bob from inside its agent; the
/// other keeps its process, its lease and its profile, untouched.
#[test]
fn an_in_agent_switch_moves_only_its_own_session_while_a_sibling_keeps_running() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION); // the bystander
    world.write_transcript(&world.alice, OTHER_SESSION); // the one that switches

    // The bystander: a supervised, idle conversation on alice for the length of the test.
    let mut bystander = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .env("RELAY_TEST_SLEEP", "45")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("bystander");
    wait_for(
        "the bystander to be adopted",
        Duration::from_secs(20),
        || session_dir_of(&world, SESSION).is_some_and(|dir| dir.join("lease.json").exists()),
    );
    let bystander_dir = session_dir_of(&world, SESSION).expect("bystander session");
    let before: Value =
        serde_json::from_str(&std::fs::read_to_string(bystander_dir.join("lease.json")).unwrap())
            .unwrap();
    let bystander_pid = before["owner_process"]["pid"].as_u64().expect("pid");
    assert!(pid_alive(bystander_pid));

    // The other conversation asks (from inside its agent) to move to bob.
    let mut command = world.resume_command(&["--profile", "alice", "--resume", OTHER_SESSION]);
    command
        .env("RELAY_TEST_PROMPT", "/relay switch bob")
        .env("RELAY_TEST_PROMPT_CMD", prompt_command(&world))
        .env("RELAY_TEST_SLEEP", "60");
    let output = command.output().expect("switching session");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The switching session moved (and, its terminal having ended, is dormant on bob)…
    let moved = session_dir_of(&world, OTHER_SESSION).expect("switched session");
    let moved_owner = std::fs::read_to_string(moved.join("lease.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map_or_else(
            || world.record_of(&moved)["last_profile"].clone(),
            |lease| lease["owner_profile"].clone(),
        );
    assert_eq!(
        moved_owner,
        "bob",
        "control result: {:?}; answer: {:?}",
        std::fs::read_to_string(moved.join("control/last.json")).ok(),
        std::fs::read_to_string(world.root.path().join("prompt.out")).ok()
    );
    // …and the bystander is exactly as it was: same lease, same live process, no control traffic.
    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(bystander_dir.join("lease.json")).unwrap())
            .unwrap();
    assert_eq!(after["owner_profile"], "alice");
    assert_eq!(after["owner_process"], before["owner_process"]);
    assert_eq!(after["session_id"], SESSION);
    assert!(
        pid_alive(bystander_pid),
        "the sibling's process was not stopped"
    );
    assert!(!bystander_dir.join("control/last.json").exists());
    let _ignored = bystander.kill();
    let _ignored = bystander.wait();
}

#[test]
fn a_claude_session_in_another_project_on_the_same_profile_does_not_block_a_switch() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let elsewhere = tempdir().expect("elsewhere");
    let _unrelated = SleepingClaude::start(
        &world,
        OTHER_SESSION,
        &std::fs::canonicalize(elsewhere.path()).expect("canonical"),
    );
    let output = switch_via_agent(&world, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(world.lease().expect("lease")["owner_profile"], "bob");
    assert_eq!(
        world.lease().expect("lease")["session_id"],
        SESSION,
        "the same session"
    );
}

#[test]
fn another_claude_conversation_in_the_same_project_does_not_block_a_switch() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    // A different conversation, working in this very project on the same profile.
    let _other = SleepingClaude::start(&world, OTHER_SESSION, &world.canonical_project());
    let output = switch_via_agent(&world, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(world.lease().expect("lease")["owner_profile"], "bob");
    assert_eq!(world.lease().expect("lease")["session_id"], SESSION);
}

#[test]
fn a_second_process_for_the_same_conversation_refuses_the_switch_before_anything_is_stopped() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    // A rival process serving the VERY SAME conversation (its registry entry says so).
    let _rival = SleepingClaude::start(&world, SESSION, &world.canonical_project());
    let output = switch_via_agent(&world, &[("RELAY_TEST_SLEEP", "4")]);
    assert!(output.status.success());
    // Two processes claiming one conversation is contradictory evidence: the in-agent command
    // itself fails closed (it cannot tell which one it is), and nothing is switched or stopped.
    let answer = std::fs::read_to_string(world.root.path().join("prompt.out")).expect("answer");
    assert!(
        answer.contains("did nothing") && answer.contains("unambiguously"),
        "{answer}"
    );
    assert!(
        world.lease().is_none(),
        "the ambiguous conversation was never adopted"
    );
    // No switch ran at all: the supervisor never launched the transaction.
    assert!(
        !argv_log(world.root.path())
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("-p"))
    );
}

#[test]
fn a_switch_that_fails_after_the_stop_reopens_the_same_session_and_moves_nothing() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    // The target answers with a different session, so verification fails after the source stopped.
    let output = switch_via_agent(&world, &[("RELAY_TEST_BAD_VERIFY", "1")]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lease = world.lease().expect("lease");
    assert_eq!(lease["owner_profile"], "alice", "ownership never moved");
    assert_eq!(lease["session_id"], SESSION);
    let alice = std::fs::canonicalize(&world.alice).expect("alice");
    let log = std::fs::read_to_string(world.root.path().join("claude.argv")).expect("log");
    let reopened = log
        .lines()
        .filter(|line| {
            line.starts_with("ARGV\u{1f}--resume\u{1f}")
                && line.ends_with(&format!("|{}", alice.display()))
        })
        .count();
    assert_eq!(
        reopened, 2,
        "the original launch plus one native reopen of the same session:\n{log}"
    );
    let last = std::fs::read_to_string(world.state_dir().join("control/last.json")).expect("last");
    assert!(last.contains("\"ok\":false"), "{last}");
}

// ---- bare `relay switch` ------------------------------------------------------------------------

fn managed_world() -> World {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("adopt");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    world
}

#[test]
fn bare_switch_without_a_terminal_is_deterministic_and_emits_no_control_codes() {
    let world = managed_world();
    let before = world.ownership_bytes();
    for json in [true, false] {
        let mut command = base_command(world.root.path(), json, &["switch", "--project-dir"]);
        command.arg(world.project.path()).stdin(Stdio::null());
        let output = command.output().expect("relay switch");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            !stderr.contains('\u{1b}'),
            "no TTY control codes: {stderr:?}"
        );
        assert!(stderr.contains("relay switch <profile>"), "{stderr}");
        assert!(stderr.contains("bob"), "{stderr}");
        if json {
            assert_eq!(error_code(&output), "switch_needs_terminal");
        }
        assert!(output.stdout.is_empty());
    }
    assert_eq!(world.ownership_bytes(), before);
}

/// The picker tests drive a real pseudo-terminal through BSD `script -q /dev/null <cmd>` (macOS,
/// the supported platform); util-linux `script` takes different arguments, so elsewhere they skip.
fn pty_script_available() -> bool {
    let available = cfg!(target_os = "macos")
        && Command::new("script")
            .arg("-q")
            .arg("/dev/null")
            .arg("true")
            .status()
            .is_ok_and(|status| status.success());
    if !available {
        eprintln!("skipping: a BSD `script` pseudo-terminal is unavailable here");
    }
    available
}

/// Runs `relay switch` under a real pseudo-terminal (`script`), feeding it `keys`.
fn switch_in_terminal(world: &World, keys: &[u8]) -> (bool, String) {
    let mut command = Command::new("script");
    command.args(["-q", "/dev/null"]);
    command
        .arg(env!("CARGO_BIN_EXE_relay"))
        .arg("--config-root")
        .arg(world.root.path().join("config"))
        .arg("--state-root")
        .arg(world.root.path().join("state"))
        .args(["switch", "--no-attach", "--claude-executable"])
        .arg(&world.claude)
        .arg("--project-dir")
        .arg(world.project.path())
        .env("PATH", path_with_fixtures(world.root.path()))
        .env("TERM", "xterm")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub(&mut command);
    let mut child = command.spawn().expect("script");
    let mut stdin = child.stdin.take().expect("stdin");
    let keys = keys.to_vec();
    let feeder = std::thread::spawn(move || {
        use std::io::Write as _;
        std::thread::sleep(Duration::from_millis(2500));
        stdin.write_all(&keys).expect("keys");
        std::thread::sleep(Duration::from_millis(500));
        drop(stdin);
    });
    let output = child.wait_with_output().expect("output");
    feeder.join().expect("feeder");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn bare_switch_in_a_terminal_lists_profiles_and_escape_cancels_without_changing_anything() {
    if !pty_script_available() {
        return;
    }
    let world = managed_world();
    let before = world.ownership_bytes();
    let (_success, screen) = switch_in_terminal(&world, b"\x1b");
    assert!(screen.contains("Switch this conversation"), "{screen}");
    assert!(screen.contains("bob"), "{screen}");
    assert!(
        screen.contains("alice") && screen.contains("current"),
        "{screen}"
    );
    assert!(screen.contains("switch cancelled"), "{screen}");
    assert_eq!(world.ownership_bytes(), before);
}

#[test]
fn bare_switch_in_a_terminal_enter_takes_the_same_path_as_switch_with_a_profile() {
    skip_without_process_env_scan!();
    if !pty_script_available() {
        return;
    }
    let world = managed_world();
    world.write_transcript(
        &world.root.path().join("config/profiles/alice/claude"),
        SESSION,
    );
    let (_success, screen) = switch_in_terminal(&world, b"\r");
    assert!(screen.contains("Switching 'alice' -> 'bob'"), "{screen}");
    assert_eq!(world.lease().expect("lease")["owner_profile"], "bob");
}

// ---- login provider resolution -----------------------------------------------------------------

fn fake_codex_version_only(root: &Path) {
    let path = root.join("bin/codex");
    std::fs::write(
        &path,
        "#!/bin/sh\ncase \"$1\" in --version) echo 'codex-cli 0.155.0' ;; *) exit 2 ;; esac\n",
    )
    .expect("fake codex");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("permissions");
}

/// `PATH` with only the fixtures' `bin` (plus the system tools the fake scripts use).
fn login_command(root: &Path, arguments: &[&str]) -> Command {
    let mut command = base_command(root, true, arguments);
    command
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .stdin(Stdio::null());
    command
}

#[test]
fn login_of_a_new_profile_uses_the_only_installed_provider_and_never_silently_claude_when_ambiguous()
 {
    let root = tempdir().expect("root");
    let claude = install_fake_claude(root.path());
    // Only Claude installed (no codex on this PATH): a new profile is a Claude profile.
    let only = login_command(
        root.path(),
        &[
            "login",
            "solo",
            "--claude-executable",
            claude.to_str().unwrap(),
        ],
    )
    .output()
    .expect("relay");
    assert!(
        only.status.success(),
        "{}",
        String::from_utf8_lossy(&only.stderr)
    );
    let profiles = relay(root.path(), &["profiles"]);
    assert!(String::from_utf8_lossy(&profiles.stdout).contains("claude"));

    // Both installed and no terminal to ask in: a clear failure, not a guess.
    fake_codex_version_only(root.path());
    let both = login_command(
        root.path(),
        &[
            "login",
            "either",
            "--claude-executable",
            claude.to_str().unwrap(),
        ],
    )
    .output()
    .expect("relay");
    assert!(!both.status.success());
    assert_eq!(error_code(&both), "provider_choice_required");

    // A registered profile keeps its registered provider without being asked anything.
    let existing = login_command(
        root.path(),
        &[
            "login",
            "solo",
            "--claude-executable",
            claude.to_str().unwrap(),
        ],
    )
    .output()
    .expect("relay");
    assert!(
        existing.status.success(),
        "{}",
        String::from_utf8_lossy(&existing.stderr)
    );
}

// ---- help --------------------------------------------------------------------------------------

#[test]
fn help_has_no_milestone_labels_and_distinguishes_adopt_from_continue() {
    let world = world();
    let help = |args: &[&str]| -> String {
        let output = base_command(world.root.path(), false, args)
            .arg("--help")
            .output()
            .expect("help");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let milestone = |text: &str| {
        text.split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .any(|word| {
                word.len() >= 2
                    && word.starts_with('M')
                    && word[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
                    && word[1..]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.')
            })
    };
    for args in [
        &[][..],
        &["profile"],
        &["session"],
        &["lock"],
        &["handoff"],
        &["handoff", "run"],
        &["integration"],
        &["integration", "herdr"],
        &["watch"],
        &["watch", "run"],
        &["claude"],
        &["switch"],
        &["resume"],
    ] {
        let text = help(args);
        assert!(
            !milestone(&text),
            "milestone label in `relay {args:?} --help`:\n{text}"
        );
    }
    let claude = help(&["claude"]);
    assert!(claude.contains("--resume"), "{claude}");
    assert!(claude.contains("relay resume"), "{claude}");
    let switch = help(&["switch"]);
    assert!(switch.contains("Omit it"), "{switch}");
}

// ---- the session lifecycle: active → dormant, ghosts, several sessions per project -----------------

fn status_sessions(world: &World) -> Vec<Value> {
    let output = relay(
        world.root.path(),
        &[
            "status",
            "--project",
            world.project.path().to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("json");
    value["data"]["sessions"]
        .as_array()
        .expect("sessions")
        .clone()
}

fn json_relay(world: &World, arguments: &[&str]) -> std::process::Output {
    let mut command = base_command(world.root.path(), true, arguments);
    command.stdin(Stdio::null());
    command.output().expect("relay")
}

#[test]
fn a_provider_that_exits_before_any_conversation_leaves_no_ghost_session() {
    let world = world();
    let output = world
        .new_session_command("0", false, &[])
        .output()
        .expect("relay claude");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        world.session_dirs().is_empty(),
        "the provisional session was removed"
    );
    assert!(status_sessions(&world).is_empty());
    // Nothing lingers to block or to be resumed.
    let resume = json_relay(
        &world,
        &[
            "resume",
            "--project-dir",
            world.project.path().to_str().unwrap(),
        ],
    );
    assert_eq!(error_code(&resume), "no_resumable_session");
    // And `relay claude --resume` works straight away.
    world.write_transcript(&world.alice, SESSION);
    let adopt = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("resume");
    assert!(
        adopt.status.success(),
        "{}",
        String::from_utf8_lossy(&adopt.stderr)
    );
}

#[test]
fn a_normal_exit_makes_the_session_dormant_and_relay_resume_reactivates_the_same_one() {
    let world = world();
    let output = world
        .new_session_command("0", true, &[])
        .output()
        .expect("relay claude");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dirs = world.session_dirs();
    assert_eq!(dirs.len(), 1);
    let record = world.record_of(&dirs[0]);
    let native = record["native_session_id"].as_str().unwrap().to_owned();
    let relay_id = record["relay_session_id"].as_str().unwrap().to_owned();
    assert!(
        !dirs[0].join("lease.json").exists(),
        "no active lease after a normal exit"
    );
    let sessions = status_sessions(&world);
    assert_eq!(sessions[0]["state"], "dormant");
    assert_eq!(
        sessions[0]["owner"],
        Value::Null,
        "the last profile is history, not an owner"
    );
    assert_eq!(sessions[0]["profile"], "alice");

    // The one dormant session is resumed directly, natively, as the very same Relay session.
    let mut resume = base_command(world.root.path(), false, &["resume", "--project-dir"]);
    resume
        .arg(world.project.path())
        .env("RELAY_TEST_SLEEP", "0")
        .stdin(Stdio::null());
    let resumed = resume.output().expect("resume");
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(
        argv_log(world.root.path())
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("--resume")
                && argv.get(1) == Some(&native)),
        "`claude --resume <the same native session>`"
    );
    let dirs = world.session_dirs();
    assert_eq!(dirs.len(), 1, "no second session");
    assert_eq!(
        world.record_of(&dirs[0])["relay_session_id"],
        relay_id.as_str()
    );
    assert!(
        !dirs[0].join("lease.json").exists(),
        "dormant again after that exit"
    );
}

#[test]
fn two_relay_claude_sessions_on_the_same_profile_run_side_by_side() {
    let world = world();
    let mut first = world
        .new_session_command("40", true, &[])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("first");
    wait_for(
        "the first session to be active",
        Duration::from_secs(20),
        || {
            world
                .session_dirs()
                .iter()
                .any(|dir| dir.join("lease.json").exists())
        },
    );
    // A second `relay claude` in the same project, same profile: no refusal, its own session.
    let second = world
        .new_session_command("0", true, &[])
        .output()
        .expect("second");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let sessions = status_sessions(&world);
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions.iter().filter(|s| s["state"] == "active").count(),
        1,
        "the first is untouched"
    );
    assert_eq!(
        sessions.iter().filter(|s| s["state"] == "dormant").count(),
        1
    );
    assert!(sessions.iter().all(|s| s["profile"] == "alice"));
    let natives: Vec<&Value> = sessions.iter().map(|s| &s["native_session_id"]).collect();
    assert_ne!(natives[0], natives[1], "two distinct native conversations");
    let _ignored = first.kill();
    let _ignored = first.wait();
}

#[test]
fn provider_arguments_belong_to_their_own_relay_session() {
    let world = world();
    let a = world
        .new_session_command("0", true, &["--", "--model", "opus"])
        .output()
        .expect("A");
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    let b = world
        .new_session_command("0", true, &[])
        .output()
        .expect("B");
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let mut args: Vec<String> = world
        .session_dirs()
        .iter()
        .map(|dir| std::fs::read_to_string(dir.join("provider_args.json")).unwrap_or_default())
        .collect();
    args.sort();
    assert_eq!(args.len(), 2);
    assert!(
        args.iter().filter(|text| text.contains("opus")).count() == 1,
        "only A has them: {args:?}"
    );
}

#[test]
fn relays_own_launch_flags_are_never_stored_as_the_users_provider_arguments() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("resume");
    assert!(output.status.success());
    // The adoption hook was injected with `--settings` on the launch, but it is Relay's own.
    assert!(
        argv_log(world.root.path())
            .iter()
            .flatten()
            .any(|arg| arg == "--settings")
    );
    let stored =
        std::fs::read_to_string(world.state_dir().join("provider_args.json")).unwrap_or_default();
    assert!(!stored.contains("settings"), "{stored}");
}

#[test]
fn several_dormant_sessions_need_a_choice_and_one_can_be_named() {
    let world = world();
    for _ in 0..2 {
        let output = world
            .new_session_command("0", true, &[])
            .output()
            .expect("relay claude");
        assert!(output.status.success());
    }
    assert_eq!(world.session_dirs().len(), 2);
    let project = world.project.path().to_str().unwrap().to_owned();
    // No terminal: deterministic ambiguity, nothing chosen silently.
    let ambiguous = json_relay(&world, &["resume", "--project-dir", &project]);
    assert!(!ambiguous.status.success());
    assert_eq!(error_code(&ambiguous), "session_ambiguous");
    // `--session` names one exactly.
    let dirs = world.session_dirs();
    let chosen = world.record_of(&dirs[0]);
    let id = chosen["relay_session_id"].as_str().unwrap();
    let native = chosen["native_session_id"].as_str().unwrap().to_owned();
    let mut command = base_command(
        world.root.path(),
        false,
        &["resume", "--project-dir", &project, "--session", &id[..8]],
    );
    command.env("RELAY_TEST_SLEEP", "0").stdin(Stdio::null());
    let output = command.output().expect("resume --session");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        argv_log(world.root.path())
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("--resume")
                && argv.get(1) == Some(&native))
    );
}

#[test]
fn an_active_session_is_never_started_a_second_time_by_resume() {
    let world = world();
    let mut live = world
        .new_session_command("40", true, &[])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("live");
    wait_for("an active session", Duration::from_secs(20), || {
        world
            .session_dirs()
            .iter()
            .any(|dir| dir.join("lease.json").exists())
    });
    let id = world.record_of(&world.session_dirs()[0])["relay_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let project = world.project.path().to_str().unwrap().to_owned();
    let output = json_relay(
        &world,
        &["resume", "--project-dir", &project, "--session", &id[..8]],
    );
    assert!(!output.status.success());
    assert_eq!(error_code(&output), "relay_session_active");
    // And with nothing else to resume, bare `relay resume` says so instead of duplicating it.
    let bare = json_relay(&world, &["resume", "--project-dir", &project]);
    assert_eq!(error_code(&bare), "no_resumable_session");
    let _ignored = live.kill();
    let _ignored = live.wait();
}

#[test]
fn a_recycled_pid_never_keeps_a_dead_session_active_but_a_live_owner_is_never_abandoned() {
    let world = world();
    // A live owner (this test process, with its true start time): the session stays ACTIVE even
    // though no supervising terminal exists — a crashed `relay` must not orphan a live provider.
    write_legacy_live_lease(&world, "alice", SESSION);
    assert_eq!(status_sessions(&world)[0]["state"], "active");
    // The same pid with a start time that does not match: a recycled pid is a different process.
    let dirs = world.session_dirs();
    let path = dirs[0].join("lease.json");
    let mut lease: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    lease["owner_process"]["start_time_fingerprint"] =
        Value::String("Sat Jan  1 00:00:00 2000".into());
    std::fs::write(&path, lease.to_string()).unwrap();
    assert_eq!(status_sessions(&world)[0]["state"], "dormant");
}

#[test]
fn old_single_lease_state_is_migrated_once_into_exactly_one_relay_session() {
    let world = world();
    write_legacy_live_lease(&world, "alice", SESSION);
    std::fs::write(
        world.project_state().join("provider_args.json"),
        "{\"claude\":[\"--model\",\"opus\"]}",
    )
    .unwrap();
    std::fs::create_dir_all(world.project_state().join("handoffs")).unwrap();
    std::fs::write(world.project_state().join("handoffs/ho-1.json"), "{}").unwrap();
    let first = status_sessions(&world);
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0]["state"], "active",
        "a live old lease keeps its active supervision"
    );
    assert_eq!(first[0]["native_session_id"], SESSION);
    let dirs = world.session_dirs();
    assert!(
        std::fs::read_to_string(dirs[0].join("provider_args.json"))
            .unwrap()
            .contains("opus")
    );
    assert!(dirs[0].join("handoffs/ho-1.json").exists());
    assert!(!world.project_state().join("lease.json").exists());
    // Run again: idempotent, same session, nothing duplicated.
    let second = status_sessions(&world);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["relay_session_id"], first[0]["relay_session_id"]);
}

#[test]
fn an_old_lease_whose_process_is_gone_migrates_to_a_dormant_session_that_can_be_resumed() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    write_legacy_live_lease(&world, "alice", SESSION);
    let path = world.project_state().join("lease.json");
    let mut lease: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    lease["owner_process"] =
        serde_json::json!({"pid": 999_999, "start_time_fingerprint": "Sat Jan  1 00:00:00 2000"});
    std::fs::write(&path, lease.to_string()).unwrap();
    let sessions = status_sessions(&world);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["state"], "dormant");
    let mut resume = base_command(world.root.path(), false, &["resume", "--project-dir"]);
    resume
        .arg(world.project.path())
        .env("RELAY_TEST_SLEEP", "0")
        .stdin(Stdio::null());
    let output = resume.output().expect("resume");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        argv_log(world.root.path())
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("--resume")
                && argv.get(1).map(String::as_str) == Some(SESSION))
    );
}

#[test]
fn two_simultaneous_adoptions_of_one_conversation_never_produce_two_sessions_or_owners() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let spawn = || {
        world
            .resume_command(&["--profile", "alice", "--resume", SESSION])
            .env("RELAY_TEST_SLEEP", "6")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("adopter")
    };
    let mut one = spawn();
    let mut two = spawn();
    let _ignored = one.wait();
    let _ignored = two.wait();
    // Two live processes claiming one conversation is contradictory evidence (each adopter sees
    // both registry entries) and a race is settled by the registry lock: at most one session, and
    // never two owners.
    let dirs = world.session_dirs();
    assert!(
        dirs.len() <= 1,
        "one conversation, at most one Relay session"
    );
    let leases = dirs
        .iter()
        .filter(|dir| dir.join("lease.json").exists())
        .count();
    assert!(leases <= 1);
}

#[test]
fn with_several_active_sessions_the_cli_switch_needs_a_session_and_moves_only_that_one() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    world.write_transcript(&world.alice, OTHER_SESSION);
    let mut children = Vec::new();
    for id in [SESSION, OTHER_SESSION] {
        children.push(
            world
                .resume_command(&["--profile", "alice", "--resume", id])
                .env("RELAY_TEST_SLEEP", "45")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("agent"),
        );
    }
    wait_for("both sessions active", Duration::from_secs(25), || {
        world
            .session_dirs()
            .iter()
            .filter(|dir| dir.join("lease.json").exists())
            .count()
            == 2
    });
    let project = world.project.path().to_str().unwrap().to_owned();
    // One active session would be used directly; two are ambiguous without a terminal.
    let ambiguous = json_relay(
        &world,
        &["switch", "bob", "--project-dir", &project, "--no-attach"],
    );
    assert!(!ambiguous.status.success());
    assert_eq!(error_code(&ambiguous), "session_ambiguous");
    // `--session` picks the source exactly; only that conversation moves.
    let id = world.record_of(&session_dir_of(&world, SESSION).unwrap())["relay_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let moved = json_relay(
        &world,
        &[
            "switch",
            "bob",
            "--session",
            &id[..8],
            "--project-dir",
            &project,
            "--no-attach",
            "--claude-executable",
            world.claude.to_str().unwrap(),
        ],
    );
    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    let switched = session_dir_of(&world, SESSION).unwrap();
    let untouched = session_dir_of(&world, OTHER_SESSION).unwrap();
    let owner = |dir: &Path| -> Value {
        serde_json::from_str::<Value>(&std::fs::read_to_string(dir.join("lease.json")).unwrap())
            .unwrap()["owner_profile"]
            .clone()
    };
    assert_eq!(owner(&switched), "bob");
    assert_eq!(owner(&untouched), "alice");
    for child in &mut children {
        let _ignored = child.kill();
        let _ignored = child.wait();
    }
}

#[test]
fn with_one_active_session_the_cli_switch_needs_no_session_argument() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let mut child = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .env("RELAY_TEST_SLEEP", "45")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("agent");
    wait_for("the session to be active", Duration::from_secs(20), || {
        world
            .session_dirs()
            .iter()
            .any(|dir| dir.join("lease.json").exists())
    });
    let project = world.project.path().to_str().unwrap().to_owned();
    let output = json_relay(
        &world,
        &[
            "switch",
            "bob",
            "--project-dir",
            &project,
            "--no-attach",
            "--claude-executable",
            world.claude.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(world.lease().expect("lease")["owner_profile"], "bob");
    let _ignored = child.kill();
    let _ignored = child.wait();
}

#[test]
fn a_usage_limit_on_a_shared_profile_hands_each_session_off_independently() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    world.write_transcript(&world.alice, OTHER_SESSION);
    let mut children = Vec::new();
    for id in [SESSION, OTHER_SESSION] {
        children.push(
            world
                .resume_command(&["--profile", "alice", "--resume", id])
                .env("RELAY_TEST_SLEEP", "60")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("agent"),
        );
    }
    wait_for("both sessions active", Duration::from_secs(25), || {
        world
            .session_dirs()
            .iter()
            .filter(|dir| dir.join("lease.json").exists())
            .count()
            == 2
    });
    // alice becomes exhausted: each session's evaluation runs on its own, at the same time.
    let project = world.project.path().to_str().unwrap().to_owned();
    let evaluations: Vec<_> = [SESSION, OTHER_SESSION]
        .into_iter()
        .map(|native| {
            let mut command = base_command(
                world.root.path(),
                true,
                &[
                    "watch",
                    "run",
                    "--profile",
                    "alice",
                    "--fallback",
                    "bob",
                    "--project",
                    &project,
                    "--session",
                    native,
                    "--simulate-usage",
                    "exhausted",
                    "--claude-executable",
                    world.claude.to_str().unwrap(),
                ],
            );
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.spawn().expect("watch run")
        })
        .collect();
    for evaluation in evaluations {
        let output = evaluation.wait_with_output().expect("watch output");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Both moved to bob, each with its own journal, native id and ledger — neither disturbed the other.
    for native in [SESSION, OTHER_SESSION] {
        let dir = session_dir_of(&world, native).expect("session");
        let lease: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("lease.json")).unwrap())
                .unwrap();
        assert_eq!(lease["owner_profile"], "bob");
        assert_eq!(
            lease["session_id"], native,
            "same native Claude conversation"
        );
        let journals = std::fs::read_dir(dir.join("handoffs")).unwrap().count();
        assert_eq!(journals, 1, "exactly this session's transaction");
        let ledger: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("automation_state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(ledger["recent_handoffs"].as_array().unwrap().len(), 1);
    }
    for child in &mut children {
        let _ignored = child.kill();
        let _ignored = child.wait();
    }
}
