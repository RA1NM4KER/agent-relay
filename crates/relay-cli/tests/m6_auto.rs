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
{{ printf 'ARGV'; for a in "$@"; do printf '\037%s' "$a"; done; printf '|%s\n' "$CLAUDE_CONFIG_DIR"; }} >> "{argv_log}"
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
        argv_log = root.join("claude.argv").display(),
        stopped = stopped.display(),
    );
    std::fs::write(&executable, script).expect("fake claude");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    executable
}

/// Every recorded invocation of a fake provider as its exact argv (one element per real argument,
/// never re-split), with the isolated home it ran under.
fn argv_log(root: &Path, provider: &str) -> Vec<(Vec<String>, String)> {
    std::fs::read_to_string(root.join(format!("{provider}.argv")))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (argv, home) = line.rsplit_once('|')?;
            let argv = argv.strip_prefix("ARGV")?;
            Some((
                argv.split('\u{1f}').skip(1).map(str::to_owned).collect(),
                home.to_owned(),
            ))
        })
        .collect()
}

/// Provider invocations whose first argument is `first` (for example `--bg`, `exec`, `resume`).
fn argv_starting(root: &Path, provider: &str, first: &str) -> Vec<Vec<String>> {
    argv_log(root, provider)
        .into_iter()
        .map(|(argv, _)| argv)
        .filter(|argv| argv.first().map(String::as_str) == Some(first))
        .collect()
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
{{ printf 'ARGV'; for a in "$@"; do printf '\037%s' "$a"; done; printf '|%s\n' "$CODEX_HOME"; }} >> "{argv_log}"
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
  resume)
    [ -f "$CODEX_HOME/resume_sleep" ] && sleep 30
    exit 0 ;;
  app-server)
    [ -f "$CODEX_HOME/app_server_fail" ] && exit 1
    while IFS= read -r line; do
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      [ -z "$id" ] && continue
      case "$line" in
        *'"method":"initialize"'*) printf '{{"id":%s,"result":{{"codexHome":"%s"}}}}\n' "$id" "$CODEX_HOME" ;;
        *'"method":"thread/read"'*)
          tid=$(printf '%s' "$line" | sed -n 's/.*"threadId":"\([^"]*\)".*/\1/p')
          if [ "$tid" = "01a-auto-thread" ]; then
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
        exec_log = exec_log.display(),
        argv_log = root.join("codex.argv").display(),
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
    world_with(fallback_is_codex, &[])
}

/// [`world`], but the initial `relay claude` launch also passes `passthrough` after `--`.
fn world_with(fallback_is_codex: bool, passthrough: &[&str]) -> World {
    world_opts(fallback_is_codex, passthrough, true)
}

/// The shared builder; with `launch = false` profiles and preferences exist but no session does.
fn world_opts(fallback_is_codex: bool, passthrough: &[&str], launch: bool) -> World {
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
    if launch {
        let project_text = project.path().to_string_lossy().into_owned();
        let claude_text = claude.to_string_lossy().into_owned();
        let mut launch_args = vec![
            "claude",
            "--project-dir",
            &project_text,
            "--no-attach",
            "--claude-executable",
            &claude_text,
            "hello",
        ];
        if !passthrough.is_empty() {
            launch_args.push("--");
            launch_args.extend_from_slice(passthrough);
        }
        let launch = relay(root.path(), &launch_args);
        assert!(
            launch.status.success(),
            "{}",
            String::from_utf8_lossy(&launch.stderr)
        );
    }
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

// ---------------------------------------------------------------------------------------------
// M7: Codex as an automatic *source* (structured usage from `codex app-server`).
// ---------------------------------------------------------------------------------------------

/// alice (Claude, primary) and codex-main (Codex); the CODEX profile is the current writer.
fn codex_writer_world() -> World {
    let world = world(true);
    let root = world.root.path();
    let switch = relay_command(
        root,
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &root.join("bin").join("claude").to_string_lossy(),
            "--codex-executable",
            &root.join("bin").join("codex").to_string_lossy(),
        ],
    )
    .output()
    .expect("switch");
    assert!(
        switch.status.success(),
        "{}",
        String::from_utf8_lossy(&switch.stderr)
    );
    assert_eq!(lease_owner(root), "codex-main");
    world
}

fn codex_home(world: &World) -> PathBuf {
    profile_dir(world.root.path(), "codex-main", "codex")
}

/// What the scripted `account/rateLimits/read` answers for the Codex profile.
fn set_codex_limits(world: &World, allowed: &str, used: u32, resets_at: u64) {
    std::fs::write(
        codex_home(world).join("limits.json"),
        format!(
            r#"{{"ordinaryUsageAllowed":{allowed},"accountId":"acct","rateLimits":{{"primary":{{"usedPercent":{used},"windowDurationMins":300,"resetsAt":{resets_at}}},"secondary":{{"usedPercent":10,"resetsAt":{resets_at}}}}}}}"#
        ),
    )
    .expect("limits");
}

fn lease_session(root: &Path) -> String {
    let lease =
        std::fs::read_to_string(project_state_dir(root).join("lease.json")).expect("lease file");
    serde_json::from_str::<Value>(&lease).expect("lease json")["session_id"]
        .as_str()
        .expect("session")
        .to_owned()
}

fn watch_run_codex(world: &World) -> Command {
    let root = world.root.path();
    let mut command = relay_command(
        root,
        &[
            "watch",
            "run",
            "--profile",
            "codex-main",
            "--fallback",
            "alice",
            "--project",
            &world.project.path().to_string_lossy(),
            "--session",
            &lease_session(root),
            "--claude-executable",
            &root.join("bin").join("claude").to_string_lossy(),
        ],
    );
    command.env("PATH", path_with_fixtures(root));
    command
}

fn ledger(world: &World) -> Value {
    json_stdout(&relay(
        world.root.path(),
        &[
            "watch",
            "status",
            "--project",
            &world.project.path().to_string_lossy(),
        ],
    ))["data"]["ledger"]
        .clone()
}

/// The headline: a genuinely exhausted Codex writer (structured `ordinaryUsageAllowed=false`)
/// hands off to the first eligible Claude profile with STATE_CONTINUATION, the Codex thread is
/// never resumed on Claude, and Codex is recorded exhausted with its reset time.
#[test]
fn an_exhausted_codex_writer_hands_off_to_claude_with_state_continuation() {
    let world = codex_writer_world();
    set_codex_limits(&world, "false", 100, 4_000_000_000);
    let output = watch_run_codex(&world).output().expect("watch run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    assert_eq!(json["data"]["outcome"], "handoff", "{json}");
    assert_eq!(json["data"]["target"], "alice");
    assert_eq!(lease_owner(world.root.path()), "alice");
    let ledger = ledger(&world);
    assert_eq!(
        ledger["recent_handoffs"].as_array().expect("array").len(),
        1
    );
    assert_eq!(ledger["known_exhausted"][0]["profile"], "codex-main");
    assert_eq!(
        ledger["known_exhausted"][0]["reset_unix_ms"],
        4_000_000_000_000_u64
    );
    // The provider-neutral bundle path was used (Codex -> Claude is STATE_CONTINUATION), not a
    // resume of the Codex thread on Claude.
    assert_eq!(
        json["data"]["journal"]["continuity_type"], "STATE_CONTINUATION",
        "{json}"
    );
}

/// Sticky writer: Codex is healthy, Claude Primary is ready — nothing moves.
#[test]
fn a_healthy_codex_writer_stays_put_even_though_claude_is_ready() {
    let world = codex_writer_world();
    set_codex_limits(&world, "true", 12, 4_000_000_000);
    let output = watch_run_codex(&world).output().expect("watch run");
    assert!(output.status.success());
    assert_eq!(json_stdout(&output)["data"]["outcome"], "no_action_needed");
    assert_eq!(lease_owner(world.root.path()), "codex-main");
}

/// A broken app-server yields UNKNOWN usage, which never starts a handoff.
#[test]
fn a_failing_codex_app_server_never_triggers_a_handoff() {
    let world = codex_writer_world();
    set_codex_limits(&world, "false", 100, 4_000_000_000);
    std::fs::write(codex_home(&world).join("app_server_fail"), "").expect("marker");
    let output = watch_run_codex(&world).output().expect("watch run");
    assert!(output.status.success());
    assert_eq!(json_stdout(&output)["data"]["outcome"], "no_action_needed");
    assert_eq!(lease_owner(world.root.path()), "codex-main");
    // and "usage allowed: unknown" (the verdict is missing) is equally inert
    std::fs::remove_file(codex_home(&world).join("app_server_fail")).expect("remove marker");
    std::fs::write(
        codex_home(&world).join("limits.json"),
        r#"{"accountId":"acct","rateLimits":{"primary":{"usedPercent":100,"resetsAt":4000000000}}}"#,
    )
    .expect("limits");
    let output = watch_run_codex(&world).output().expect("watch run");
    assert_eq!(json_stdout(&output)["data"]["outcome"], "no_action_needed");
    assert_eq!(lease_owner(world.root.path()), "codex-main");
}

/// Two simultaneous triggers for the same exhausted Codex writer produce exactly one handoff
/// and exactly one writer.
#[test]
fn duplicate_triggers_for_an_exhausted_codex_writer_create_one_handoff() {
    let world = codex_writer_world();
    set_codex_limits(&world, "false", 100, 4_000_000_000);
    let first = watch_run_codex(&world).spawn().expect("first");
    let second = watch_run_codex(&world).spawn().expect("second");
    let _ = first.wait_with_output().expect("first done");
    let _ = second.wait_with_output().expect("second done");
    assert_eq!(lease_owner(world.root.path()), "alice");
    let ledger = ledger(&world);
    assert_eq!(
        ledger["recent_handoffs"].as_array().expect("array").len(),
        1
    );
}

/// The whole automatic path with the supervised terminal: `relay resume` on a Codex writer; while
/// the terminal is up, the periodic structured-usage check notices real exhaustion, hands the
/// conversation to Claude, and the terminal continues on the new owner — nobody types anything.
#[test]
fn a_supervised_codex_session_notices_exhaustion_and_follows_the_conversation_to_claude() {
    let world = codex_writer_world();
    let root = world.root.path();
    // Healthy at start; the fake `codex resume` blocks (like the TUI) until Relay closes it.
    set_codex_limits(&world, "true", 5, 4_000_000_000);
    std::fs::write(codex_home(&world).join("resume_sleep"), "").expect("marker");
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(["resume", "--project-dir"])
        .arg(world.project.path())
        .arg("--claude-executable")
        .arg(root.join("bin").join("claude"))
        .arg("--codex-executable")
        .arg(root.join("bin").join("codex"))
        .env("PATH", path_with_fixtures(root))
        .env("RELAY_CODEX_POLL_SECS", "1")
        .env("RELAY_AUTO_WATCH_ATTEMPTS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub(&mut command);
    let child = command.spawn().expect("relay resume");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(lease_owner(root), "codex-main", "healthy: nothing may move");
    // The real limit is hit.
    set_codex_limits(&world, "false", 100, 4_000_000_000);
    wait_for(
        "the periodic check to hand the conversation to alice",
        Duration::from_secs(60),
        || lease_owner(root) == "alice",
    );
    let output = child.wait_with_output().expect("relay resume finishes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = claude_log(root);
    assert!(
        log.iter()
            .any(|(args, _)| args.starts_with("attach ") || args.starts_with("--resume ")),
        "the terminal must continue on alice: {log:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("continuing this conversation on 'alice'")
    );
}

/// Codex -> another Codex profile (fake providers only): the hierarchy is honoured and the
/// continuity is STATE_CONTINUATION, never a native resume of the first profile's thread.
#[test]
fn an_exhausted_codex_writer_can_hand_off_to_a_second_codex_profile_with_state_continuation() {
    let world = codex_writer_world();
    let root = world.root.path();
    login(
        root,
        "codex-backup",
        "codex",
        "--codex-executable",
        &root.join("bin").join("codex"),
    );
    set_codex_limits(&world, "false", 100, 4_000_000_000);
    let mut command = relay_command(
        root,
        &[
            "watch",
            "run",
            "--profile",
            "codex-main",
            "--fallback",
            "codex-backup",
            "--project",
            &world.project.path().to_string_lossy(),
            "--session",
            &lease_session(root),
        ],
    );
    command.env("PATH", path_with_fixtures(root));
    let output = command.output().expect("watch run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    assert_eq!(json["data"]["outcome"], "handoff", "{json}");
    assert_eq!(
        json["data"]["journal"]["continuity_type"],
        "STATE_CONTINUATION"
    );
    assert_eq!(lease_owner(root), "codex-backup");
}

// ---------------------------------------------------------------------------------------------
// Provider passthrough (`relay claude -- …`, `relay codex -- …`) and the symmetric entrypoints.
// ---------------------------------------------------------------------------------------------

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn claude_exe(world: &World) -> String {
    world
        .root
        .path()
        .join("bin")
        .join("claude")
        .to_string_lossy()
        .into_owned()
}

fn codex_exe(world: &World) -> String {
    world
        .root
        .path()
        .join("bin")
        .join("codex")
        .to_string_lossy()
        .into_owned()
}

fn stored_args(world: &World) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(project_state_dir(world.root.path()).join("provider_args.json"))
            .expect("provider_args.json"),
    )
    .expect("json")
}

/// A `relay resume` that runs the (instantly exiting) fake provider and reports success.
fn relay_resume(world: &World, extra: &[&str]) -> std::process::Output {
    let root = world.root.path();
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(["resume", "--project-dir"])
        .arg(world.project.path())
        .arg("--claude-executable")
        .arg(claude_exe(world))
        .arg("--codex-executable")
        .arg(codex_exe(world))
        .args(extra)
        .env("PATH", path_with_fixtures(root))
        .env("RELAY_CODEX_POLL_SECS", "0");
    scrub(&mut command);
    command.output().expect("relay resume")
}

#[test]
fn claude_passthrough_reaches_the_background_launch_verbatim_after_relays_own_arguments() {
    let passthrough = [
        "--model",
        "opus",
        "--append-system-prompt",
        "two words; \"quoted\" & spaced",
        "--add-dir=/a b",
        "--add-dir",
        "/c",
        "--add-dir",
        "-",
        "--dangerously-skip-permissions",
    ];
    let world = world_with(false, &passthrough);
    let launches = argv_starting(world.root.path(), "claude", "--bg");
    assert_eq!(launches.len(), 1);
    let mut expected = s(&["--bg", "--permission-mode", "acceptEdits", "hello"]);
    expected.extend(s(&passthrough));
    assert_eq!(
        launches[0], expected,
        "exact argv, no re-splitting or re-quoting"
    );
    let stored = stored_args(&world);
    assert_eq!(stored["claude"], serde_json::json!(passthrough));
    assert_eq!(stored["codex"], serde_json::json!([]));
}

#[test]
fn no_passthrough_means_the_launch_argv_is_exactly_what_it_always_was() {
    let world = world(false);
    let launches = argv_starting(world.root.path(), "claude", "--bg");
    assert_eq!(
        launches[0],
        s(&["--bg", "--permission-mode", "acceptEdits", "hello"])
    );
    assert_eq!(stored_args(&world)["claude"], serde_json::json!([]));
}

#[test]
fn flags_that_would_replace_what_relay_owns_are_rejected_before_anything_starts() {
    let world = world(false);
    let root = world.root.path();
    let project = world.project.path().to_string_lossy().into_owned();
    let (claude, codex) = (claude_exe(&world), codex_exe(&world));
    for (bad, provider) in [
        ("--bg", "claude"),
        ("--resume=abc", "claude"),
        ("-C", "codex"),
    ] {
        let mut args = vec![provider, "--project-dir", &project, "--no-attach", "--new"];
        args.extend(if provider == "claude" {
            ["--claude-executable", claude.as_str()]
        } else {
            ["--codex-executable", codex.as_str()]
        });
        args.extend(["--", bad, "/elsewhere"]);
        let output = relay(root, &args);
        assert!(!output.status.success(), "{bad} must be rejected");
        let error: Value = serde_json::from_slice(&output.stderr).expect("error json");
        assert_eq!(
            error["error"]["code"], "provider_argument_rejected",
            "{bad}"
        );
    }
    assert_eq!(
        argv_starting(root, "claude", "--bg").len(),
        1,
        "only the initial launch ever ran"
    );
    assert!(argv_starting(root, "codex", "exec").is_empty());
    assert_eq!(lease_owner(root), "alice");
}

#[test]
fn relay_codex_starts_a_new_codex_session_and_keeps_user_arguments_off_the_bootstrap_turn() {
    let world = world_opts(true, &[], false);
    let root = world.root.path();
    let output = relay(
        root,
        &[
            "codex",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--codex-executable",
            &codex_exe(&world),
            "--",
            "--sandbox",
            "workspace-write",
            "-c",
            "model=\"o3\"",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(lease_owner(root), "codex-main");
    // Relay's own bootstrap turn only: never the user's arguments.
    assert_eq!(
        argv_starting(root, "codex", "exec"),
        vec![s(&["exec", "--json", "--skip-git-repo-check"])]
    );
    let stored = stored_args(&world);
    assert_eq!(
        stored["codex"],
        serde_json::json!(["--sandbox", "workspace-write", "-c", "model=\"o3\""])
    );
    assert_eq!(stored["claude"], serde_json::json!([]));

    // `relay resume` continues the Codex thread with those arguments, verified thread first.
    let resumed = relay_resume(&world, &[]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(
        argv_starting(root, "codex", "resume"),
        vec![s(&[
            "resume",
            "01a-auto-thread",
            "--sandbox",
            "workspace-write",
            "-c",
            "model=\"o3\""
        ])]
    );
}

#[test]
fn relay_codex_with_a_first_message_opens_the_supervised_session_with_it_after_double_dash() {
    let world = world_opts(true, &[], false);
    let root = world.root.path();
    let output = relay(
        root,
        &[
            "codex",
            "fix",
            "the",
            "bug",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--codex-executable",
            &codex_exe(&world),
            "--",
            "--oss",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        argv_starting(root, "codex", "resume"),
        vec![s(&[
            "resume",
            "01a-auto-thread",
            "--oss",
            "--",
            "fix the bug"
        ])]
    );
}

#[test]
fn without_new_relay_codex_refuses_to_replace_an_active_session() {
    let world = world(true);
    let output = relay(
        world.root.path(),
        &[
            "codex",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
        ],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("error json");
    assert_eq!(error["error"]["code"], "managed_session_active");
    assert_eq!(lease_owner(world.root.path()), "alice");
    assert!(argv_starting(world.root.path(), "codex", "exec").is_empty());
}

#[test]
fn each_entrypoint_picks_the_highest_priority_profile_of_its_own_provider() {
    let world = world_opts(true, &[], false);
    let root = world.root.path();
    // Priority order: the Codex profile first, then the Claude profile.
    let setup = relay(
        root,
        &[
            "setup",
            "--non-interactive",
            "--primary",
            "codex-main",
            "--fallback",
            "alice",
            "--claude-executable",
            &claude_exe(&world),
        ],
    );
    assert!(setup.status.success());
    let project = world.project.path().to_string_lossy().into_owned();
    let codex = relay(
        root,
        &[
            "codex",
            "--project-dir",
            &project,
            "--no-attach",
            "--codex-executable",
            &codex_exe(&world),
        ],
    );
    assert!(
        codex.status.success(),
        "{}",
        String::from_utf8_lossy(&codex.stderr)
    );
    assert_eq!(json_stdout(&codex)["data"]["profile"], "codex-main");
    assert_eq!(lease_owner(root), "codex-main");
    // `relay claude`, though the primary is a Codex profile: the highest-priority CLAUDE one.
    let claude = relay(
        root,
        &[
            "claude",
            "--project-dir",
            &project,
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "hi",
        ],
    );
    assert!(
        claude.status.success(),
        "{}",
        String::from_utf8_lossy(&claude.stderr)
    );
    assert_eq!(json_stdout(&claude)["data"]["profile"], "alice");
    assert_eq!(lease_owner(root), "alice", "still exactly one writer");
}

#[test]
fn a_profile_of_the_wrong_provider_or_none_of_that_provider_fails_clearly() {
    let world = world(true);
    let root = world.root.path();
    let project = world.project.path().to_string_lossy().into_owned();
    let mismatch_codex = relay(
        root,
        &[
            "codex",
            "--profile",
            "alice",
            "--project-dir",
            &project,
            "--no-attach",
            "--new",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
        ],
    );
    let mismatch_claude = relay(
        root,
        &[
            "claude",
            "--profile",
            "codex-main",
            "--project-dir",
            &project,
            "--no-attach",
            "--new",
            "--claude-executable",
            &claude_exe(&world),
            "hi",
        ],
    );
    for output in [mismatch_codex, mismatch_claude] {
        assert!(!output.status.success());
        let error: Value = serde_json::from_slice(&output.stderr).expect("error json");
        assert_eq!(error["error"]["code"], "profile_provider_mismatch");
    }
    assert_eq!(lease_owner(root), "alice", "nothing was stopped or started");

    // Only Claude profiles configured: `relay codex` has nothing to start.
    let claude_only = self::world(false);
    let none = relay(
        claude_only.root.path(),
        &[
            "codex",
            "--project-dir",
            &claude_only.project.path().to_string_lossy(),
            "--no-attach",
            "--new",
            "--claude-executable",
            &claude_exe(&claude_only),
            "--codex-executable",
            &codex_exe(&claude_only),
        ],
    );
    assert!(!none.status.success());
    let error: Value = serde_json::from_slice(&none.stderr).expect("error json");
    assert_eq!(error["error"]["code"], "no_profile_for_provider");
}

#[test]
fn claude_arguments_never_reach_codex_and_codex_arguments_never_reach_claude() {
    let world = world_with(true, &["--dangerously-skip-permissions", "--model", "opus"]);
    let root = world.root.path();
    // Claude -> Codex: the Codex continuation gets ONLY Codex's own (here: none) arguments.
    let to_codex = relay(
        root,
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
        ],
    );
    assert!(
        to_codex.status.success(),
        "{}",
        String::from_utf8_lossy(&to_codex.stderr)
    );
    let resumed = relay_resume(&world, &[]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(
        argv_starting(root, "codex", "resume"),
        vec![s(&["resume", "01a-auto-thread"])],
        "no Claude flag was translated to Codex"
    );
    assert!(
        argv_log(root, "codex").iter().all(|(argv, _)| !argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions" || a == "--model")),
        "no Codex invocation of any kind saw a Claude argument"
    );

    // Give Codex its own arguments, then hand back to Claude: they must not follow.
    let with_codex_args = relay_resume(&world, &["--", "--sandbox", "workspace-write"]);
    assert!(with_codex_args.status.success());
    assert_eq!(
        argv_starting(root, "codex", "resume")
            .last()
            .expect("resume"),
        &s(&["resume", "01a-auto-thread", "--sandbox", "workspace-write"])
    );
    let to_claude = relay(
        root,
        &[
            "switch",
            "alice",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
        ],
    );
    assert!(
        to_claude.status.success(),
        "{}",
        String::from_utf8_lossy(&to_claude.stderr)
    );
    assert_eq!(lease_owner(root), "alice");
    let _ = relay_resume(&world, &[]);
    // Codex -> Claude: the Claude continuation carries Claude's own stored arguments, and the
    // headless verification turn Relay ran before it carries none of anyone's.
    assert_eq!(
        argv_starting(root, "claude", "--resume")
            .last()
            .expect("continuation"),
        &s(&[
            "--resume",
            SESSION_ID,
            "--dangerously-skip-permissions",
            "--model",
            "opus"
        ])
    );
    assert!(
        argv_starting(root, "claude", "-p").iter().all(|argv| argv
            == &s(&[
                "-p",
                "--permission-mode",
                "acceptEdits",
                "--output-format",
                "json"
            ])),
        "Relay's own headless turn never carries user arguments"
    );
    assert!(
        argv_log(root, "claude").iter().all(|(argv, _)| !argv
            .iter()
            .any(|a| a == "--sandbox" || a == "workspace-write")),
        "no Claude invocation ever saw a Codex argument"
    );
    // Claude's own stored arguments are still Claude's.
    assert_eq!(
        stored_args(&world)["claude"],
        serde_json::json!(["--dangerously-skip-permissions", "--model", "opus"])
    );
}

#[test]
fn claude_to_claude_handoff_reuses_the_claude_arguments_on_the_continuation_only() {
    let world = world_with(false, &["--model", "opus"]);
    write_source_transcript(&world);
    let root = world.root.path();
    let switched = relay(
        root,
        &[
            "switch",
            "bob",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
        ],
    );
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );
    assert_eq!(lease_owner(root), "bob");
    let resumed = relay_resume(&world, &[]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let continuation: Vec<Vec<String>> = argv_starting(root, "claude", "--resume");
    assert_eq!(
        continuation.last().expect("continuation"),
        &s(&["--resume", SESSION_ID, "--model", "opus"])
    );
    // Relay's own verification turn (a headless canary) is Relay's arguments only.
    assert!(
        argv_starting(root, "claude", "-p")
            .iter()
            .all(|argv| !argv.iter().any(|a| a == "--model")),
        "the canary/bootstrap turn never carries user arguments"
    );
    // ...and the continuation ran under the NEW owner's isolated config.
    let (_, home) = argv_log(root, "claude")
        .into_iter()
        .rev()
        .find(|(argv, _)| argv.first().map(String::as_str) == Some("--resume") && argv.len() > 2)
        .expect("continuation");
    assert_eq!(
        std::fs::canonicalize(home).expect("bob"),
        std::fs::canonicalize(profile_dir(root, "bob", "claude")).expect("bob dir")
    );
}

#[test]
fn a_provider_argument_can_never_replace_the_session_relay_resumes() {
    let world = world_with(false, &["--model", "opus"]);
    write_source_transcript(&world);
    let root = world.root.path();
    let switched = relay(
        root,
        &[
            "switch",
            "bob",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
        ],
    );
    assert!(switched.status.success());
    let hijack = relay_resume(&world, &["--", "--resume", "some-other-session"]);
    assert!(!hijack.status.success());
    assert!(String::from_utf8_lossy(&hijack.stderr).contains("provider_argument_rejected"));
    assert!(
        argv_starting(root, "claude", "--resume")
            .iter()
            .all(|argv| !argv.iter().any(|a| a == "some-other-session")),
        "the hijacking session id never reached Claude"
    );
    // The stored arguments were not replaced by the rejected ones.
    assert_eq!(
        stored_args(&world)["claude"],
        serde_json::json!(["--model", "opus"])
    );
}

// ---------------------------------------------------------------------------------------------
// The Relay status-line badge and the Claude flags that would disable Relay's hooks.
// ---------------------------------------------------------------------------------------------

/// Runs the real `relay hook claude statusline` exactly as Claude would (JSON on stdin), returning
/// its raw stdout. `chain` is the user's own status-line command Relay wraps.
fn statusline(world: &World, config_dir: &Path, session: &str, chain: Option<&str>) -> String {
    let root = world.root.path();
    let project = std::fs::canonicalize(world.project.path()).expect("project");
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{0}","workspace":{{"project_dir":"{0}","current_dir":"{0}"}}}}"#,
        project.display()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(["hook", "claude", "statusline", "--config-dir"])
        .arg(config_dir);
    if let Some(chain) = chain {
        command.args(["--chain", chain]);
    }
    command
        .env("PATH", path_with_fixtures(root))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub(&mut command);
    let mut child = command.spawn().expect("spawn statusline");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("statusline output");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("utf8")
}

#[test]
fn a_managed_session_shows_the_badge_with_the_current_owner_and_nothing_else() {
    let world = world(false);
    let out = statusline(&world, &world.alice_dir, SESSION_ID, None);
    assert_eq!(out.trim_end(), "[Relay · alice]");
    // plain text only: no escape or control characters anywhere
    assert!(out.chars().all(|c| c == '\n' || !c.is_control()), "{out:?}");
    assert!(!out.contains('%'), "no quota in the badge");
}

#[test]
fn an_unmanaged_claude_session_never_gets_the_badge_and_its_output_is_untouched() {
    let world = world(false);
    // a different Claude session in the same project directory
    let plain = statusline(
        &world,
        &world.alice_dir,
        "99999999-9999-4999-8999-999999999999",
        None,
    );
    assert!(!plain.contains("Relay"), "{plain:?}");
    let chained = statusline(
        &world,
        &world.alice_dir,
        "99999999-9999-4999-8999-999999999999",
        Some("printf 'MY-LINE'"),
    );
    assert_eq!(
        chained, "MY-LINE",
        "byte for byte what the user's own status line printed"
    );
}

#[test]
fn the_badge_is_added_to_the_users_own_status_line_without_changing_it() {
    let world = world(false);
    let out = statusline(
        &world,
        &world.alice_dir,
        SESSION_ID,
        Some("printf '\\033[32mgreen\\033[0m mine'"),
    );
    assert_eq!(out, "\u{1b}[32mgreen\u{1b}[0m mine [Relay · alice]\n");
}

#[test]
fn the_badge_follows_the_lease_owner_after_a_claude_to_claude_handoff() {
    let world = world(false);
    write_source_transcript(&world);
    let root = world.root.path();
    assert_eq!(
        statusline(&world, &world.alice_dir, SESSION_ID, None).trim_end(),
        "[Relay · alice]"
    );
    let switched = relay(
        root,
        &[
            "switch",
            "bob",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
        ],
    );
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );
    // The same conversation (same session id) now belongs to bob — shown in either terminal.
    let bob_dir = profile_dir(root, "bob", "claude");
    assert_eq!(
        statusline(&world, &bob_dir, SESSION_ID, None).trim_end(),
        "[Relay · bob]"
    );
    assert_eq!(
        statusline(&world, &world.alice_dir, SESSION_ID, None).trim_end(),
        "[Relay · bob]",
        "no stale owner"
    );
}

#[test]
fn a_claude_session_the_conversation_has_left_for_codex_shows_no_stale_ownership() {
    let world = world(true);
    let root = world.root.path();
    assert_eq!(
        statusline(&world, &world.alice_dir, SESSION_ID, None).trim_end(),
        "[Relay · alice]"
    );
    let switched = relay(
        root,
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
        ],
    );
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );
    assert_eq!(lease_owner(root), "codex-main");
    let out = statusline(&world, &world.alice_dir, SESSION_ID, None);
    assert!(
        !out.contains("Relay"),
        "the old Claude session must not claim ownership: {out:?}"
    );
    // and Relay's machine-readable state never contains the badge or any escape sequence
    for name in ["lease.json", "provider_args.json"] {
        let text = std::fs::read_to_string(project_state_dir(root).join(name)).unwrap_or_default();
        assert!(
            !text.contains("[Relay") && !text.contains('\u{1b}'),
            "{name}"
        );
    }
}

/// A settings.json with the user's own status line, before and after install/reinstall/uninstall.
#[test]
fn install_composes_with_an_existing_status_line_is_idempotent_and_uninstall_restores_it() {
    let world = world(false);
    let root = world.root.path();
    let dir = profile_dir(root, "bob", "claude");
    std::fs::create_dir_all(&dir).expect("bob dir");
    let settings = dir.join("settings.json");
    let original = "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"my-status --fancy 'x y'\",\n    \"padding\": 2\n  },\n  \"theme\": \"dark\"\n}\n";
    std::fs::write(&settings, original).expect("settings");
    let run = |verb: &str| {
        let mut args = vec!["integration", "claude", verb, "--config-dir"];
        let dir_text = dir.to_string_lossy().into_owned();
        args.push(&dir_text);
        let claude = claude_exe(&world);
        if verb == "install" {
            args.extend(["--claude-executable", &claude]);
        }
        let output = relay(root, &args);
        assert!(
            output.status.success(),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run("install");
    let installed: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let command = installed["statusLine"]["command"]
        .as_str()
        .expect("command");
    assert!(command.contains("hook claude statusline"), "{command}");
    assert!(
        command.contains("my-status --fancy"),
        "the user's command is chained, not replaced: {command}"
    );
    assert_eq!(installed["statusLine"]["padding"], 2);
    assert_eq!(installed["theme"], "dark");
    let first = std::fs::read(&settings).unwrap();
    run("install");
    assert_eq!(
        std::fs::read(&settings).unwrap(),
        first,
        "reinstall is idempotent"
    );
    run("uninstall");
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        original,
        "uninstall restores the user's original settings byte for byte"
    );
}

#[test]
fn flags_that_disable_relays_hooks_are_rejected_with_a_clear_explanation_and_change_nothing() {
    let world = world_with(false, &["--model", "opus"]);
    let root = world.root.path();
    let before = stored_args(&world);
    let project = world.project.path().to_string_lossy().into_owned();
    let claude = claude_exe(&world);
    for bad in [
        vec!["--bare"],
        vec!["--safe-mode"],
        vec!["--restricted"],
        vec!["--setting-sources", "project"],
    ] {
        let mut args = vec![
            "claude",
            "--project-dir",
            &project,
            "--no-attach",
            "--new",
            "--claude-executable",
            &claude,
            "--",
        ];
        args.extend(bad.iter().copied());
        let output = relay(root, &args);
        assert!(!output.status.success(), "{bad:?}");
        let error: Value = serde_json::from_slice(&output.stderr).expect("error json");
        assert_eq!(
            error["error"]["code"], "provider_argument_rejected",
            "{bad:?}"
        );
        let message = error["error"]["message"].as_str().expect("message");
        assert!(message.contains(bad[0]), "{message}");
        assert!(message.contains("Agent Relay"), "{message}");
    }
    assert_eq!(
        argv_starting(root, "claude", "--bg").len(),
        1,
        "nothing was launched"
    );
    assert_eq!(
        stored_args(&world),
        before,
        "stored arguments were not touched"
    );
    assert_eq!(lease_owner(root), "alice");
}

#[test]
fn hook_disabling_flags_are_also_rejected_through_resume_and_switch_but_not_for_codex() {
    let world = world_with(true, &["--model", "opus"]);
    write_source_transcript(&world);
    let root = world.root.path();
    let before = stored_args(&world);
    let resume = relay_resume(&world, &["--", "--bare"]);
    assert!(!resume.status.success());
    assert!(String::from_utf8_lossy(&resume.stderr).contains("provider_argument_rejected"));
    let switch = relay(
        root,
        &[
            "switch",
            "codex-main",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
            "--",
            "--sandbox",
            "workspace-write",
        ],
    );
    assert!(switch.status.success(), "codex passthrough is unaffected");
    let to_claude = relay(
        root,
        &[
            "switch",
            "alice",
            "--project-dir",
            &world.project.path().to_string_lossy(),
            "--no-attach",
            "--claude-executable",
            &claude_exe(&world),
            "--codex-executable",
            &codex_exe(&world),
            "--",
            "--safe-mode",
        ],
    );
    assert!(
        !to_claude.status.success(),
        "a Claude target with --safe-mode is refused"
    );
    assert_eq!(
        lease_owner(root),
        "codex-main",
        "the refused switch moved nothing"
    );
    assert_eq!(
        stored_args(&world)["claude"],
        before["claude"],
        "Claude's stored arguments untouched"
    );
}

#[test]
fn ordinary_and_future_claude_flags_still_pass_through() {
    let world = world_with(
        false,
        &[
            "--setting-sources",
            "user,project",
            "--brand-new-flag",
            "value",
            "--effort",
            "high",
        ],
    );
    let launches = argv_starting(world.root.path(), "claude", "--bg");
    assert_eq!(
        launches[0][4..],
        s(&[
            "--setting-sources",
            "user,project",
            "--brand-new-flag",
            "value",
            "--effort",
            "high"
        ])
    );
}
