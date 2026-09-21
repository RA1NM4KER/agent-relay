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
  agents) printf '[]\n' ;;
  stop) exit 0 ;;
  -p) cat >/dev/null
      if [ -n "$RELAY_TEST_BAD_VERIFY" ]; then printf '{{"session_id":"99999999-9999-4999-8999-999999999999","is_error":false,"subtype":"success"}}\n'
      else printf '{{"session_id":"{SESSION}","is_error":false,"subtype":"success"}}\n'; fi ;;
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
      cp "$RELAY_TEST_RAW_REQUEST" "$RELAY_TEST_CONTROL_DIR/request-raw.json"
      for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        [ -f "$RELAY_TEST_CONTROL_DIR/response-raw.json" ] && cp "$RELAY_TEST_CONTROL_DIR/response-raw.json" "$RELAY_TEST_ROOT/raw-response.json" && break
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

    fn state_dir(&self) -> PathBuf {
        let id = relay_core::handoff::ProjectId::for_canonical_path(&self.canonical_project())
            .expect("project id");
        self.root.path().join("state/projects").join(id.as_str())
    }

    fn lease(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.state_dir().join("lease.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn agent_env(&self, command: &mut Command) {
        command.env("RELAY_TEST_ROOT", self.root.path()).env(
            "RELAY_TEST_TRANSCRIPT_DIR",
            self.transcript_dir(&self.alice),
        );
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
    assert!(lease["owner_process"]["pid"].as_u64().unwrap_or(0) > 0);
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

#[test]
fn a_live_relay_writer_blocks_adoption_and_is_left_untouched() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    // A lease whose owner is this very (live) test process.
    let state = world.state_dir();
    std::fs::create_dir_all(&state).expect("state");
    let lease = serde_json::json!({
        "version": 1,
        "project_id": state.file_name().unwrap().to_str().unwrap(),
        "owner_profile": "bob",
        "owner_process": relay_core::handoff::ProcessIdentity::current(),
        "session_id": OTHER_SESSION,
        "transaction_id": "ho-test",
        "acquired_unix_ms": 1,
        "provider_handle": null
    });
    std::fs::write(state.join("lease.json"), lease.to_string()).expect("lease");
    let output = world
        .resume_command(&["--profile", "alice", "--resume", SESSION])
        .output()
        .expect("relay");
    assert!(!output.status.success());
    let code = error_code(&output);
    assert!(
        code == "managed_session_active" || code == "adoption_refused",
        "{code}"
    );
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
fn a_competing_live_writer_blocks_adopting_a_second_conversation() {
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    world.write_transcript(&world.alice, OTHER_SESSION);
    let first = LiveAgent::start(&world, &world.alice, SESSION);
    assert!(
        reason(&first.type_into(&world, &world.alice, SESSION, "/relay adopt")).contains("adopted")
    );
    let second = LiveAgent::start(&world, &world.alice, OTHER_SESSION);
    let text = reason(&second.type_into(&world, &world.alice, OTHER_SESSION, "/relay adopt"));
    assert!(text.contains("refused"), "{text}");
    assert_eq!(
        world.lease().expect("lease")["session_id"],
        SESSION,
        "the writer is unchanged"
    );
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
    let control = world.state_dir().join("control");
    std::fs::create_dir_all(&control).expect("control dir");
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
fn a_second_claude_session_in_the_same_project_refuses_the_switch_before_anything_is_stopped() {
    skip_without_process_env_scan!();
    let world = world();
    world.write_transcript(&world.alice, SESSION);
    let _rival = SleepingClaude::start(&world, OTHER_SESSION, &world.canonical_project());
    let output = switch_via_agent(&world, &[("RELAY_TEST_SLEEP", "4")]);
    assert!(output.status.success());
    let answer = std::fs::read_to_string(world.root.path().join("prompt.out")).expect("answer");
    assert!(answer.contains("refused"), "{answer}");
    assert!(answer.contains("left running"), "{answer}");
    assert_eq!(world.lease().expect("lease")["owner_profile"], "alice");
    // No switch ran at all: the supervisor never launched the transaction.
    assert!(!world.state_dir().join("control/last.json").exists());
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
    let before = std::fs::read(world.state_dir().join("lease.json")).expect("lease");
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
    assert_eq!(
        std::fs::read(world.state_dir().join("lease.json")).expect("lease"),
        before
    );
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
    let before = std::fs::read(world.state_dir().join("lease.json")).expect("lease");
    let (_success, screen) = switch_in_terminal(&world, b"\x1b");
    assert!(screen.contains("Switch this conversation"), "{screen}");
    assert!(screen.contains("bob"), "{screen}");
    assert!(
        screen.contains("alice") && screen.contains("current"),
        "{screen}"
    );
    assert!(screen.contains("switch cancelled"), "{screen}");
    assert_eq!(
        std::fs::read(world.state_dir().join("lease.json")).expect("lease"),
        before
    );
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
