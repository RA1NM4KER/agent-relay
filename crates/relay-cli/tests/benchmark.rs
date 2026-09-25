//! Repeatable, honest performance measurements for the "lightweight" claim in the README, not
//! vibes. Every test here is `#[ignore]`d — none of them run as part of the normal `cargo test`
//! gate — because they measure wall-clock/CPU/memory rather than correctness, and would make an
//! otherwise-fast suite timing-fragile on a loaded CI runner. Run them explicitly:
//!
//! ```sh
//! cargo test --release -p relay-cli --test benchmark -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--release` matters: a debug build's dispatch overhead is not representative of what Homebrew
//! ships. `--test-threads=1` matters for the idle-supervision/handoff timings, which read process
//! `ps` samples and would be skewed by other benchmark tests competing for the CPU at the same time.
//!
//! Against a fake `claude` executable (never a real account) — the same fixture style already
//! used by `tests/m4.rs`, extended here to actually stay alive so there is something to sample.
//! See `docs/benchmarks.md` for the methodology and the last recorded results.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::Value;

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
    for variable in relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    command.output().expect("run relay")
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

fn auth_json_for(name: &str) -> String {
    format!(
        r#"{{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-{name}","email":"{name}@example.com","orgId":"org-1"}}"#
    )
}

/// A fake `claude` that answers `--version`/`auth status`/`--bg`/`agents`/`stop` the same way
/// `tests/m4.rs`'s `FakeClaude` does (for `relay launch`/`relay handoff run`), but — the one real
/// difference this file needs — a fresh interactive `--session-id` launch does not exit
/// immediately: it writes the stub transcript Relay expects and then blocks until told to stop,
/// so there is a live child process for `relay claude` to actually supervise while idle.
/// Claude's own native session ids are always UUIDs (`session_transfer::validate_session_id`
/// enforces this); a fixed one here keeps every benchmark run byte-identical instead of adding
/// timing noise from UUID generation, which is irrelevant to what's being measured.
const NATIVE_SESSION_ID: &str = "8f14e45f-ceea-467e-adde-3fb5ba334200";

struct FakeClaude {
    executable: PathBuf,
    stop_flag: PathBuf,
}

impl FakeClaude {
    fn new(root: &Path, auth_json: &str, bg_id: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let executable = root.join(format!("fake-claude-{bg_id}"));
        let stopped_marker = root.join(format!("stopped-{bg_id}"));
        let stop_flag = root.join(format!("stop-flag-{bg_id}"));
        let script = format!(
            r#"#!/bin/sh
STOPPED="{marker}"
STOPFLAG="{stopflag}"
case " $* " in *" --session-id "*)
  PREV=""; SID=""; for a in "$@"; do [ "$PREV" = "--session-id" ] && SID="$a"; PREV="$a"; done
  mkdir -p "$CLAUDE_CONFIG_DIR/projects/-fake" && echo '{{}}' > "$CLAUDE_CONFIG_DIR/projects/-fake/$SID.jsonl"
  # Idle stand-in for an interactive session: block until the benchmark tells it to stop, rather
  # than exiting immediately like the correctness fixtures do (nothing would be left to sample).
  while [ ! -f "$STOPFLAG" ]; do sleep 0.05; done
  exit 0 ;;
esac
# `ClaudeTargetLauncher`'s headless verification turn: `claude -p --permission-mode acceptEdits
# --output-format json --resume <session_id> <canary prompt>` — answered with the exact JSON
# shape `parse_verification` requires, never a real model turn.
case " $* " in *" --resume "*)
  PREV=""; SID=""; for a in "$@"; do [ "$PREV" = "--resume" ] && SID="$a"; PREV="$a"; done
  printf '{{"session_id":"%s","is_error":false}}\n' "$SID"
  exit 0 ;;
esac
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
      printf '[{{"pid":111,"id":"{bg_id}","kind":"background","startedAt":1,"sessionId":"{native_session_id}","name":"x","status":"idle","state":"done"}}]\n'
    fi
    ;;
  stop) touch "$STOPPED"; exit 0 ;;
  attach) exit 0 ;;
  --resume) exit 0 ;;
  *) exit 2 ;;
esac
"#,
            marker = stopped_marker.display(),
            native_session_id = NATIVE_SESSION_ID,
            stopflag = stop_flag.display(),
        );
        std::fs::write(&executable, script).expect("fake claude script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        Self {
            executable,
            stop_flag,
        }
    }

    fn path_text(&self) -> String {
        self.executable.to_string_lossy().into_owned()
    }

    /// Releases a `--session-id` invocation blocked in its idle loop.
    fn signal_stop(&self) {
        std::fs::write(&self.stop_flag, b"stop").expect("write stop flag");
    }
}

fn adopt_profile(root: &Path, name: &str, claude: &FakeClaude) -> PathBuf {
    let profile_dir = root.join("config/profiles").join(name).join("claude");
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

fn init_git_repo(dir: &Path) {
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
    run(&["config", "user.email", "bench@example.com"]);
    run(&["config", "user.name", "Bench"]);
    std::fs::write(dir.join("a.txt"), "x").expect("seed file");
    run(&["add", "a.txt"]);
    run(&["commit", "-q", "-m", "init"]);
}

/// A minimal transcript Claude's own session-transfer discovery accepts: one JSON line recording
/// `cwd`, which is the one fact `discover_by_scanning_projects` (relay-provider-claude) actually
/// checks — see that module for why (Claude's own ground truth, never a re-derived guess).
fn seed_transcript(config_dir: &Path, session_id: &str, project_dir: &Path) {
    let dir = config_dir.join("projects/-fake");
    std::fs::create_dir_all(&dir).expect("transcript dir");
    let line = serde_json::json!({ "cwd": project_dir.to_string_lossy() });
    std::fs::write(dir.join(format!("{session_id}.jsonl")), format!("{line}\n"))
        .expect("seed transcript");
}

fn project_root_dir(root: &Path) -> PathBuf {
    root.join("state")
        .join("projects")
        .read_dir()
        .expect("projects dir")
        .next()
        .expect("one project")
        .expect("entry")
        .path()
}

fn project_state_dir(root: &Path) -> PathBuf {
    let project = project_root_dir(root);
    let mut sessions = std::fs::read_dir(project.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("session.json").exists());
    match (sessions.next(), sessions.next()) {
        (Some(only), None) => only,
        (None, _) => project,
        (Some(_), Some(_)) => panic!("expected exactly one Relay session"),
    }
}

/// Samples of one process's memory (KB) and cumulative CPU time (seconds) via `ps`, macOS/BSD and
/// Linux `procps` compatible (`rss` and `time` are both standard `ps -o` keywords on either).
fn sample_process(pid: u32) -> Option<(u64, f64)> {
    let output = Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split_whitespace();
    let rss_kb: u64 = fields.next()?.parse().ok()?;
    let cpu_time = fields.next()?;
    // `[[dd-]hh:]mm:ss[.cc]` — parse the colon-separated fields we actually see in practice
    // (macOS/Linux both report at least `mm:ss` for a young, low-CPU process).
    let parts: Vec<&str> = cpu_time.split(':').collect();
    let seconds: f64 = match parts.as_slice() {
        [m, s] => m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?,
        [h, m, s] => {
            h.parse::<f64>().ok()? * 3600.0
                + m.parse::<f64>().ok()? * 60.0
                + s.parse::<f64>().ok()?
        }
        _ => return None,
    };
    Some((rss_kb, seconds))
}

fn wait_for(mut condition: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let started = Instant::now();
    while !condition() {
        if started.elapsed() > timeout {
            panic!("timed out waiting for: {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Idle memory footprint and CPU overhead of the `relay` process while it supervises a live,
/// otherwise-silent Claude session — the steady-state cost the README's "lightweight" claim is
/// actually about, not a one-shot CLI invocation.
#[test]
#[ignore = "benchmark, not a correctness test — see tests/benchmark.rs's module doc"]
fn idle_supervision_overhead() {
    let root = tempfile::tempdir().expect("tempdir");
    let root = root.path();
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let claude = FakeClaude::new(root, &auth_json_for("alice"), "aaaa1111");
    adopt_profile(root, "alice", &claude);

    let mut child = Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(["claude", "--profile", "alice", "--project-dir"])
        .arg(&project)
        .arg("--claude-executable")
        .arg(&claude.executable)
        .arg("benchmark prompt")
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn relay claude");
    let relay_pid = child.id();

    // `project_root_dir`/`project_state_dir` both `.expect()` their way down a directory chain
    // that does not exist at all yet at the very start — fine once something is known to be
    // there, wrong as a polling condition, so the first wait checks for that directory by hand.
    wait_for(
        || root.join("state/projects").exists(),
        Duration::from_secs(10),
        "the project directory to be recorded",
    );
    wait_for(
        || project_root_dir(root).join("sessions").exists(),
        Duration::from_secs(10),
        "the Relay session to be recorded",
    );
    wait_for(
        || {
            project_state_dir(root)
                .join("control/supervisor.json")
                .exists()
        },
        Duration::from_secs(10),
        "the supervisor to start ticking",
    );
    // Let it settle into steady state before the first sample.
    std::thread::sleep(Duration::from_millis(500));

    const SAMPLES: u32 = 10;
    const INTERVAL: Duration = Duration::from_millis(500);
    let mut rss_samples = Vec::new();
    let (_, cpu_start) = sample_process(relay_pid).expect("process alive at first sample");
    for _ in 0..SAMPLES {
        if let Some((rss, _)) = sample_process(relay_pid) {
            rss_samples.push(rss);
        }
        std::thread::sleep(INTERVAL);
    }
    let (_, cpu_end) = sample_process(relay_pid).expect("process alive at last sample");
    let elapsed = INTERVAL * SAMPLES;

    claude.signal_stop();
    let status = child.wait().expect("relay claude exits");
    assert!(status.success(), "relay claude should exit cleanly");

    let max_rss_kb = rss_samples.iter().copied().max().unwrap_or_default();
    let min_rss_kb = rss_samples.iter().copied().min().unwrap_or_default();
    let cpu_seconds = (cpu_end - cpu_start).max(0.0);
    let cpu_percent = 100.0 * cpu_seconds / elapsed.as_secs_f64();
    eprintln!(
        "BENCHMARK idle_supervision_overhead: rss_min_kb={min_rss_kb} rss_max_kb={max_rss_kb} \
         cpu_time_s={cpu_seconds:.3} window_s={:.1} avg_cpu_percent={cpu_percent:.3}",
        elapsed.as_secs_f64()
    );
}

/// End-to-end wall-clock time of one complete transactional handoff (`relay handoff run`) between
/// two fake Claude profiles — the SESSION_CONTINUATION path (same-provider), which is the common,
/// fast case: stop the source, stage the session, launch and verify the target, move the lease.
#[test]
#[ignore = "benchmark, not a correctness test — see tests/benchmark.rs's module doc"]
fn handoff_latency() {
    let root = tempfile::tempdir().expect("tempdir");
    let root = root.path();
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    init_git_repo(&project);
    let project = std::fs::canonicalize(&project).expect("canonical project dir");
    let claude_a = FakeClaude::new(root, &auth_json_for("alice"), "aaaa1111");
    let claude_b = FakeClaude::new(root, &auth_json_for("bob"), "bbbb2222");
    let config_a = adopt_profile(root, "alice", &claude_a);
    let _config_b = adopt_profile(root, "bob", &claude_b);
    // The handoff checkpoint step needs a real git repo (done above); the staging step needs an
    // existing transcript for the SOURCE's native session, whose recorded `cwd` matches the exact
    // project directory Relay canonicalizes internally — Claude's own ground truth for "which
    // session is this", not something Relay can invent. Only the initial source (alice) is seeded
    // — staging itself creates the target's copy hash-verified from that seed, and pre-seeding the
    // target too would make its (differently-formatted) content "diverge" from what staging
    // actually writes, a hard error by design (never a silent overwrite of an existing artifact).
    seed_transcript(&config_a, NATIVE_SESSION_ID, &project);

    let launch = relay(
        root,
        &[
            "launch",
            "--profile",
            "alice",
            "--project-dir",
            project.to_str().expect("utf8 path"),
            "--claude-executable",
            &claude_a.path_text(),
            "benchmark prompt",
        ],
    );
    assert!(
        launch.status.success(),
        "launch failed: {}",
        String::from_utf8_lossy(&launch.stderr)
    );
    // `relay handoff run --session` takes the NATIVE provider session id (what `discover_session`
    // looks a transcript up by), never Relay's own session UUID — a distinct id space `relay
    // switch-request` uses instead; conflating the two here previously produced this exact
    // `session_not_found`, diagnosed by comparing against a manual reproduction using the native
    // id directly.
    let session_id = NATIVE_SESSION_ID;

    let mut durations = Vec::new();
    let mut phase_timings: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    const ROUNDS: u32 = 5;
    let mut from = "alice";
    let mut to = "bob";
    for _ in 0..ROUNDS {
        let executable = if to == "bob" {
            claude_b.path_text()
        } else {
            claude_a.path_text()
        };
        let start = Instant::now();
        let result = relay(
            root,
            &[
                "handoff",
                "run",
                "--from",
                from,
                "--to",
                to,
                "--project",
                project.to_str().expect("utf8 path"),
                "--session",
                session_id,
                "--claude-executable",
                &executable,
            ],
        );
        let elapsed = start.elapsed();
        assert!(
            result.status.success(),
            "handoff {from} -> {to} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let journal: Value = serde_json::from_slice(&result.stdout).expect("handoff JSON");
        for timing in journal["data"]["timings"]
            .as_array()
            .expect("journal timings")
        {
            let phase = timing["phase"].as_str().expect("timing phase").to_owned();
            let elapsed_ms = timing["elapsed_ms"].as_u64().expect("timing elapsed");
            phase_timings.entry(phase).or_default().push(elapsed_ms);
        }
        durations.push(elapsed);
        std::mem::swap(&mut from, &mut to);
    }

    let mut millis: Vec<f64> = durations
        .iter()
        .map(Duration::as_secs_f64)
        .map(|s| s * 1000.0)
        .collect();
    millis.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs"));
    let n = millis.len();
    eprintln!(
        "BENCHMARK handoff_latency: n={n} min_ms={:.1} median_ms={:.1} max_ms={:.1} all_ms={:?}",
        millis[0],
        millis[n / 2],
        millis[n - 1],
        millis
    );
    for (phase, mut samples) in phase_timings {
        samples.sort_unstable();
        eprintln!(
            "BENCHMARK handoff_phase: phase={phase} min_ms={} median_ms={} max_ms={} all_ms={samples:?}",
            samples[0],
            samples[samples.len() / 2],
            samples[samples.len() - 1],
        );
    }
}

/// Sanity check that the fixture itself behaves (idempotent to run alongside the two benchmarks
/// above without `--ignored`, so a plain `cargo test` catches a broken fixture before it ever
/// silently produces bogus numbers).
#[test]
fn fake_claude_fixture_responds_to_version_and_auth() {
    let root = tempfile::tempdir().expect("tempdir");
    let claude = FakeClaude::new(root.path(), &auth_json_for("alice"), "aaaa1111");
    let version = Command::new(&claude.executable)
        .arg("--version")
        .output()
        .expect("run fake claude");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("2.1.276"));

    let auth = Command::new(&claude.executable)
        .args(["auth", "status"])
        .env("CLAUDE_CONFIG_DIR", root.path())
        .output()
        .expect("run fake claude auth status");
    assert!(auth.status.success());
    let parsed: Value = serde_json::from_slice(&auth.stdout).expect("json");
    assert_eq!(parsed["loggedIn"], true);
}
