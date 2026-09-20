//! M6 automatic-exhaustion tests: the trigger and the continuation.
//!
//! Regression coverage for the first genuine exhaustion dogfood finding. A real Claude limit was
//! hit, Relay's `StopFailure` hook correctly recorded the evidence, and then nothing ever asked
//! `WatchCoordinator` to evaluate it (the Herdr event never fired; outside Herdr nothing called
//! it at all), and after a manual `relay watch run` moved the conversation the user still had to
//! discover `relay resume`. These tests drive the *real* hook entry points and the *real*
//! `relay` binary against fake `claude`/`codex` executables — including the ambient
//! `CLAUDE_CONFIG_DIR`/messaging variables a hook process inherits from Claude, which Relay's own
//! authentication checks treat as conflicting overrides.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::tempdir;

use relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES as CLAUDE_AUTH_OVERRIDE_VARIABLES;
use relay_provider_codex::AUTHENTICATION_OVERRIDE_VARIABLES as CODEX_AUTH_OVERRIDE_VARIABLES;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const BG_ID: &str = "aaaa1111";

fn scrub(command: &mut Command) {
    command
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME");
    for variable in CLAUDE_AUTH_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    for variable in CODEX_AUTH_OVERRIDE_VARIABLES {
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

fn relay_command(root: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--json")
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments);
    scrub(&mut command);
    command
}

fn relay(root: &Path, arguments: &[&str]) -> std::process::Output {
    relay_command(root, arguments).output().expect("run relay")
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
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

/// One fake `claude` used for every profile: it tells profiles apart by their isolated
/// `CLAUDE_CONFIG_DIR` (`.../alice/...`, `.../bob/...`), logs every invocation as
/// `<argv>|<CLAUDE_CONFIG_DIR>`, honours a real `stop` (the session drops out of `agents --json`
/// until the next `--bg`), and — only when `RELAY_TEST_SWAP` is set — makes `attach` move the
/// writer lease the way a completed handoff would while the user is attached.
fn install_fake_claude(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let executable = bin.join("claude");
    let log = root.join("claude.log");
    let stopped = root.join("claude.stopped");
    let script = format!(
        r#"#!/bin/sh
printf '%s|%s\n' "$*" "$CLAUDE_CONFIG_DIR" >> "{log}"
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
    esac
    ;;
  --bg) rm -f "{stopped}"; printf 'backgrounded \302\267 %s\n' "{BG_ID}" ;;
  agents)
    if [ -f "{stopped}" ]; then printf '[]\n'; else
      printf '[{{"pid":4242,"id":"{BG_ID}","kind":"background","startedAt":1,"sessionId":"{SESSION_ID}","name":"x","status":"idle","state":"done"}}]\n'
    fi ;;
  stop) touch "{stopped}"; exit 0 ;;
  attach)
    if [ -n "$RELAY_TEST_SWAP" ] && [ -f "$RELAY_TEST_SWAP" ]; then mv "$RELAY_TEST_SWAP" "$RELAY_TEST_LEASE"; fi
    exit "${{RELAY_TEST_ATTACH_EXIT:-0}}" ;;
  --resume) exit 0 ;;
  -p) cat >/dev/null; printf '{{"session_id":"{SESSION_ID}","is_error":false,"subtype":"success"}}\n' ;;
  *) exit 2 ;;
esac
"#,
        log = log.display(),
        stopped = stopped.display(),
    );
    std::fs::write(&executable, script).expect("fake claude");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    executable
}

fn claude_log(root: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(root.join("claude.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('|'))
        .map(|(args, dir)| (args.to_owned(), dir.to_owned()))
        .collect()
}

fn install_fake_codex(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let executable = bin.join("codex");
    let exec_log = root.join("codex-exec.log");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  --version) printf 'codex-cli 0.155.0\n' ;;
  doctor) printf '{{"checks":{{"auth.credentials":{{"status":"ok","summary":"logged in via fake"}}}}}}\n' ;;
  login|logout) exit 0 ;;
  exec)
    cat >/dev/null
    printf '{{"codex_home":"%s"}}\n' "$CODEX_HOME" >> "{exec_log}"
    printf '{{"type":"thread.started","thread_id":"01a-auto-thread"}}\n'
    printf '{{"type":"turn.started"}}\n'
    printf '{{"type":"turn.completed"}}\n'
    ;;
  resume) exit 0 ;;
  *) exit 2 ;;
esac
"#,
        exec_log = exec_log.display(),
    );
    std::fs::write(&executable, script).expect("fake codex");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    executable
}

fn login(root: &Path, name: &str, provider: &str, flag: &str, executable: &Path) {
    let output = relay(
        root,
        &[
            "login",
            name,
            "--provider",
            provider,
            flag,
            &executable.to_string_lossy(),
        ],
    );
    assert!(
        output.status.success(),
        "login {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn profile_dir(root: &Path, name: &str, provider: &str) -> PathBuf {
    root.join("config")
        .join("profiles")
        .join(name)
        .join(provider)
}

/// `PATH` with the fixtures' `bin` directory first, so a detached child that resolves `claude` /
/// `codex` from `PATH` (the hook-spawned run passes no explicit executable) finds the fakes.
fn path_with_fixtures(root: &Path) -> std::ffi::OsString {
    std::env::join_paths(
        std::iter::once(root.join("bin")).chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        ),
    )
    .expect("joinable PATH")
}

fn project_state_dir(root: &Path) -> PathBuf {
    root.join("state")
        .join("projects")
        .read_dir()
        .expect("projects dir")
        .next()
        .expect("one project")
        .expect("entry")
        .path()
}

fn lease_owner(root: &Path) -> String {
    let lease =
        std::fs::read_to_string(project_state_dir(root).join("lease.json")).expect("lease file");
    serde_json::from_str::<Value>(&lease).expect("lease json")["owner_profile"]
        .as_str()
        .expect("owner")
        .to_owned()
}

fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for: {what}");
}

/// Runs a Relay hook exactly as Claude would: JSON on stdin, and the *inherited Claude
/// environment* — the source profile's `CLAUDE_CONFIG_DIR` plus the messaging token current
/// Claude Code sets — which Relay's own authentication checks reject as conflicting overrides.
fn run_hook(root: &Path, config_dir: &Path, hook: &str, stdin: &str, extra_env: &[(&str, String)]) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(["hook", "claude", hook, "--config-dir"])
        .arg(config_dir)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .env("CLAUDE_CODE_MESSAGING_TOKEN", "ambient-token-from-claude")
        .env("PATH", path_with_fixtures(root))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("hook output");
    assert!(output.status.success(), "a hook must never fail Claude");
}

fn statusline_at_100(root: &Path, config_dir: &Path) {
    let payload = format!(
        r#"{{"session_id":"{SESSION_ID}","rate_limits":{{"five_hour":{{"used_percentage":100,"resets_at":4000000000}}}}}}"#
    );
    run_hook(root, config_dir, "statusline", &payload, &[]);
}

fn stop_failure(
    root: &Path,
    config_dir: &Path,
    session: &str,
    project: &Path,
    extra: &[(&str, String)],
) {
    let payload = format!(
        r#"{{"hook_event_name":"StopFailure","session_id":"{session}","cwd":"{}","error":"rate_limit","last_assistant_message":"You've hit your session limit"}}"#,
        project.display()
    );
    run_hook(root, config_dir, "stop-failure", &payload, extra);
}

struct World {
    root: tempfile::TempDir,
    project: tempfile::TempDir,
    alice_dir: PathBuf,
}

/// alice (Claude, primary) holds a real managed writer lease for a project; `fallback` is either
/// a Codex profile or a second Claude profile; alice's usage integration is installed.
fn world(fallback_is_codex: bool) -> World {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project");
    init_git_repo(project.path());
    let claude = install_fake_claude(root.path());
    let codex = install_fake_codex(root.path());
    login(
        root.path(),
        "alice",
        "claude",
        "--claude-executable",
        &claude,
    );
    let fallback = if fallback_is_codex {
        login(
            root.path(),
            "codex-main",
            "codex",
            "--codex-executable",
            &codex,
        );
        "codex-main"
    } else {
        login(root.path(), "bob", "claude", "--claude-executable", &claude);
        "bob"
    };
    let setup = relay(
        root.path(),
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "alice",
            "--fallback",
            fallback,
            "--claude-executable",
            &claude.to_string_lossy(),
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
            &project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude.to_string_lossy(),
            "hello",
        ],
    );
    assert!(
        launch.status.success(),
        "{}",
        String::from_utf8_lossy(&launch.stderr)
    );
    let alice_dir = profile_dir(root.path(), "alice", "claude");
    let install = relay(
        root.path(),
        &[
            "integration",
            "claude",
            "install",
            "--config-dir",
            &alice_dir.to_string_lossy(),
            "--claude-executable",
            &claude.to_string_lossy(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    World {
        root,
        project,
        alice_dir,
    }
}

fn auto_log(root: &Path) -> String {
    std::fs::read_to_string(project_state_dir(root).join("auto-handoff.log")).unwrap_or_default()
}

/// The lease flips inside the coordinator; the triggered run records the handoff in the ledger and
/// prints its summary line just after. Assertions about either must wait for that line.
fn wait_for_handoff_summary(root: &Path, target: &str) -> String {
    let line = format!("Automatic handoff to '{target}'");
    wait_for(
        "the triggered run to finish and report the handoff",
        Duration::from_secs(60),
        || auto_log(root).contains(&line),
    );
    auto_log(root)
}

/// THE regression test for the release-blocking finding. Real hook inputs (a corroborating
/// statusline at 100%, then a rate-limit `StopFailure`) for the session Relay manages must, with
/// nothing else running and nobody typing anything, hand the conversation to the fallback — even
/// though the hook process inherits Claude's own `CLAUDE_CONFIG_DIR` and messaging token.
#[test]
fn a_real_limit_event_for_the_managed_session_hands_off_automatically() {
    let world = world(true);
    statusline_at_100(world.root.path(), &world.alice_dir);
    stop_failure(
        world.root.path(),
        &world.alice_dir,
        SESSION_ID,
        world.project.path(),
        &[],
    );

    wait_for(
        "the automatic handoff to move the lease to codex-main",
        Duration::from_secs(90),
        || lease_owner(world.root.path()) == "codex-main",
    );

    let log = wait_for_handoff_summary(world.root.path(), "codex-main");
    assert!(log.contains("[attempt "), "each attempt is logged:\n{log}");
    let status = json_stdout(&relay(
        world.root.path(),
        &[
            "watch",
            "status",
            "--project",
            &world.project.path().to_string_lossy(),
        ],
    ));
    assert_eq!(
        status["data"]["ledger"]["recent_handoffs"]
            .as_array()
            .expect("handoffs")
            .len(),
        1
    );
    assert_eq!(
        status["data"]["ledger"]["known_exhausted"][0]["profile"],
        "alice"
    );
    // The source session was really stopped through Claude, under alice's own config directory —
    // never a config directory borrowed from the fallback or from the ambient environment.
    let alice_dir = world.alice_dir.to_string_lossy().into_owned();
    let alice_dir = std::fs::canonicalize(&alice_dir).unwrap_or_default();
    assert!(claude_log(world.root.path()).iter().any(|(args, dir)| {
        args.starts_with("stop ")
            && std::fs::canonicalize(dir)
                .map(|dir| dir == alice_dir)
                .unwrap_or(false)
    }));
}

/// Claude -> Claude (`SESSION_CONTINUATION`) needs the source session's transcript on disk under
/// the source profile's own config directory, exactly where Claude Code keeps it.
fn write_source_transcript(world: &World) {
    let project = std::fs::canonicalize(world.project.path()).expect("canonical project");
    let key = relay_provider_claude::escape_project_path(&project);
    let dir = world.alice_dir.join("projects").join(key);
    std::fs::create_dir_all(&dir).expect("transcript dir");
    std::fs::write(
        dir.join(format!("{SESSION_ID}.jsonl")),
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
    )
    .expect("transcript");
}

/// The case the Codex-fallback test above cannot cover: a *Claude* fallback. Relay judges a
/// Claude profile's health with authentication checks that treat any inherited
/// `CLAUDE_CONFIG_DIR` naming a different profile, or Claude's own messaging variables, as a
/// conflicting override — and a hook process inherits exactly those from the Claude session it
/// runs in. Started without scrubbing them, the evaluation would judge every fallback unhealthy
/// and silently never hand off.
#[test]
fn a_real_limit_event_hands_off_to_a_claude_fallback_despite_the_ambient_claude_environment() {
    let world = world(false);
    write_source_transcript(&world);
    statusline_at_100(world.root.path(), &world.alice_dir);
    stop_failure(
        world.root.path(),
        &world.alice_dir,
        SESSION_ID,
        world.project.path(),
        &[],
    );
    wait_for(
        "the automatic handoff to move the lease to bob",
        Duration::from_secs(90),
        || lease_owner(world.root.path()) == "bob",
    );
    let _log = wait_for_handoff_summary(world.root.path(), "bob");
    // The target really was launched under bob's own isolated config directory.
    let bob_dir =
        std::fs::canonicalize(profile_dir(world.root.path(), "bob", "claude")).expect("bob dir");
    assert!(claude_log(world.root.path()).iter().any(|(args, dir)| {
        args.contains("--resume")
            && std::fs::canonicalize(dir)
                .map(|dir| dir == bob_dir)
                .unwrap_or(false)
    }));
}

/// The corroborating statusline snapshot can land just after the failure itself. The triggered
/// run must keep evaluating for a bounded time rather than giving up on the first "no action".
#[test]
fn a_statusline_that_lands_after_the_failure_still_triggers_the_handoff() {
    let world = world(true);
    // No statusline snapshot yet: the first evaluation must find the limit uncorroborated.
    stop_failure(
        world.root.path(),
        &world.alice_dir,
        SESSION_ID,
        world.project.path(),
        &[
            ("RELAY_AUTO_WATCH_INTERVAL_MS", "700".to_owned()),
            ("RELAY_AUTO_WATCH_ATTEMPTS", "60".to_owned()),
        ],
    );
    wait_for(
        "the first, uncorroborated evaluation to be logged",
        Duration::from_secs(30),
        || auto_log(world.root.path()).contains("No action needed"),
    );
    assert_eq!(lease_owner(world.root.path()), "alice");

    statusline_at_100(world.root.path(), &world.alice_dir);
    wait_for(
        "a later attempt to see the corroborated limit and hand off",
        Duration::from_secs(90),
        || lease_owner(world.root.path()) == "codex-main",
    );
}

/// Single-writer safety: only the exact session Relay manages for the project may trigger a
/// handoff. Another Claude session that shares the profile, or whose hook fires with some other
/// working directory, must never move the managed writer.
#[test]
fn a_limit_event_for_an_unmanaged_session_never_starts_an_evaluation() {
    let world = world(true);
    statusline_at_100(world.root.path(), &world.alice_dir);
    stop_failure(
        world.root.path(),
        &world.alice_dir,
        "99999999-9999-4999-8999-999999999999",
        world.project.path(),
        &[],
    );
    let elsewhere = tempdir().expect("elsewhere");
    stop_failure(
        world.root.path(),
        &world.alice_dir,
        SESSION_ID,
        elsewhere.path(),
        &[],
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !project_state_dir(world.root.path())
            .join("auto-handoff.log")
            .exists(),
        "no evaluation may be started for a session Relay does not manage"
    );
    assert_eq!(lease_owner(world.root.path()), "alice");
}

/// With no fallback configured there is nothing to hand off to, so nothing is started.
#[test]
fn a_limit_event_with_no_fallback_configured_starts_nothing() {
    let world = world(true);
    let setup = relay(
        world.root.path(),
        &["setup", "--non-interactive", "--primary", "alice"],
    );
    assert!(setup.status.success());
    statusline_at_100(world.root.path(), &world.alice_dir);
    stop_failure(
        world.root.path(),
        &world.alice_dir,
        SESSION_ID,
        world.project.path(),
        &[],
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !project_state_dir(world.root.path())
            .join("auto-handoff.log")
            .exists()
    );
    assert_eq!(lease_owner(world.root.path()), "alice");
}

/// Prepares the moment a handoff completes while the user is attached: a copy of the current
/// lease with the owner flipped to `bob` and no background-job handle (the shape a completed
/// cross-profile handoff leaves behind), which the fake `attach` swaps into place.
fn prepare_handoff_swap(root: &Path) -> (PathBuf, PathBuf) {
    let lease_path = project_state_dir(root).join("lease.json");
    let mut lease: Value =
        serde_json::from_str(&std::fs::read_to_string(&lease_path).expect("lease")).expect("json");
    lease["owner_profile"] = Value::String("bob".to_owned());
    lease["provider_handle"] = Value::Null;
    let swap = root.join("lease.swap.json");
    std::fs::write(&swap, serde_json::to_vec_pretty(&lease).expect("json")).expect("swap");
    (swap, lease_path)
}

fn attach_env(root: &Path) -> Command {
    let mut command = relay_command(root, &[]);
    command.env("PATH", path_with_fixtures(root));
    command
}

/// The UX half of the finding: when a handoff moves the conversation while the user is attached,
/// `relay resume` carries them onto the new owner by itself — with the *new owner's* handle-less
/// native command and *its* isolated config directory — instead of leaving a dead session.
#[test]
fn resume_follows_the_conversation_onto_the_new_owner_after_a_handoff() {
    let world = world(false);
    let (swap, lease_path) = prepare_handoff_swap(world.root.path());
    let claude = world.root.path().join("bin").join("claude");
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(world.root.path().join("config"))
        .arg("--state-root")
        .arg(world.root.path().join("state"))
        .args(["resume", "--project-dir"])
        .arg(world.project.path())
        .arg("--claude-executable")
        .arg(&claude)
        .env("RELAY_TEST_SWAP", &swap)
        .env("RELAY_TEST_LEASE", &lease_path);
    scrub(&mut command);
    let output = command.output().expect("relay resume");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = claude_log(world.root.path());
    let tail: Vec<&(String, String)> = log
        .iter()
        .filter(|(args, _)| args.starts_with("attach ") || args.starts_with("--resume "))
        .collect();
    assert_eq!(
        tail.len(),
        2,
        "attach on alice, then continue on bob: {tail:?}"
    );
    assert_eq!(tail[0].0, format!("attach {BG_ID}"));
    assert_eq!(tail[1].0, format!("--resume {SESSION_ID}"));
    let canonical = |path: &str| std::fs::canonicalize(path).expect("canonical");
    assert_eq!(
        canonical(&tail[0].1),
        std::fs::canonicalize(&world.alice_dir).expect("alice")
    );
    assert_eq!(
        canonical(&tail[1].1),
        std::fs::canonicalize(profile_dir(world.root.path(), "bob", "claude")).expect("bob"),
        "the continuation must run under the NEW owner's isolated config, never alice's"
    );
    assert_eq!(lease_owner(world.root.path()), "bob");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("continuing this conversation on 'bob'"),
        "{text}"
    );
}

/// `relay claude` itself (a fresh start) gets the same continuation, so the daily entry point
/// carries the user through a fallback without any second command.
#[test]
fn claude_follows_the_conversation_onto_the_new_owner_after_a_handoff() {
    let root = tempdir().expect("tempdir");
    let project = tempdir().expect("project");
    init_git_repo(project.path());
    let claude = install_fake_claude(root.path());
    login(
        root.path(),
        "alice",
        "claude",
        "--claude-executable",
        &claude,
    );
    login(root.path(), "bob", "claude", "--claude-executable", &claude);
    assert!(
        relay(
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
        )
        .status
        .success()
    );
    // The lease does not exist until `relay claude` launches, so the swap is prepared by the fake
    // itself from a template of the launched lease: write the template lazily via a wrapper env
    // pointing at a path filled in by a first `--no-attach` launch in a throwaway project.
    let template_project = tempdir().expect("template project");
    init_git_repo(template_project.path());
    assert!(
        relay(
            root.path(),
            &[
                "claude",
                "--project-dir",
                &template_project.path().to_string_lossy(),
                "--no-attach",
                "--claude-executable",
                &claude.to_string_lossy(),
                "template",
            ],
        )
        .status
        .success()
    );
    let template_dir = project_state_dir(root.path());
    let mut lease: Value = serde_json::from_str(
        &std::fs::read_to_string(template_dir.join("lease.json")).expect("template lease"),
    )
    .expect("json");
    let canonical_project = std::fs::canonicalize(project.path()).expect("canonical");
    let project_id =
        relay_core::handoff::ProjectId::for_canonical_path(&canonical_project).expect("project id");
    lease["project_id"] = Value::String(project_id.as_str().to_owned());
    lease["owner_profile"] = Value::String("bob".to_owned());
    lease["provider_handle"] = Value::Null;
    let swap = root.path().join("lease.swap.json");
    std::fs::write(&swap, serde_json::to_vec_pretty(&lease).expect("json")).expect("swap");
    let real_lease = root
        .path()
        .join("state")
        .join("projects")
        .join(project_id.as_str())
        .join("lease.json");

    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.path().join("config"))
        .arg("--state-root")
        .arg(root.path().join("state"))
        .args(["claude", "--project-dir"])
        .arg(project.path())
        .arg("--claude-executable")
        .arg(&claude)
        .arg("go")
        .env("RELAY_TEST_SWAP", &swap)
        .env("RELAY_TEST_LEASE", &real_lease);
    scrub(&mut command);
    let output = command.output().expect("relay claude");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = claude_log(root.path());
    assert!(
        log.iter()
            .any(|(args, _)| args == &format!("attach {BG_ID}"))
    );
    let last = log
        .iter()
        .rfind(|(args, _)| args.starts_with("--resume "))
        .expect("a continuation on the new owner");
    assert_eq!(
        std::fs::canonicalize(&last.1).expect("canonical"),
        std::fs::canonicalize(profile_dir(root.path(), "bob", "claude")).expect("bob")
    );
}

/// With no handoff, nothing changes: the session's own exit code comes back unchanged and no
/// second command is run.
#[test]
fn an_ordinary_exit_returns_the_sessions_own_status_and_continues_nowhere() {
    let world = world(false);
    let claude = world.root.path().join("bin").join("claude");
    let mut command = attach_env(world.root.path());
    command
        .args(["resume", "--project-dir"])
        .arg(world.project.path())
        .arg("--claude-executable")
        .arg(&claude)
        .env("RELAY_TEST_ATTACH_EXIT", "3");
    let output = command.output().expect("relay resume");
    assert_eq!(output.status.code(), Some(3));
    let continuations = claude_log(world.root.path())
        .iter()
        .filter(|(args, _)| args.starts_with("--resume "))
        .count();
    assert_eq!(continuations, 0);
    assert_eq!(lease_owner(world.root.path()), "alice");
}
