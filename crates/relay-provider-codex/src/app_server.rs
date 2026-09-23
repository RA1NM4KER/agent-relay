//! A minimal, read-only client for `codex app-server` (JSON-RPC 2.0, one message per line over
//! stdio) — the official structured local interface Codex exposes to first-party clients.
//!
//! Why this and not `codex exec --json`: the exec stream's terminal errors are English prose
//! (arbitrary API-client wording), which Relay never builds automatic behavior on. The app-server
//! protocol has typed methods instead. Relay uses exactly four, all read-only:
//!
//! * `initialize` — capability handshake; its response names the `codexHome` the server actually
//!   resolved, which Relay compares to the profile directory it asked for (isolation check);
//! * `account/rateLimits/read` — backend usage windows and the authoritative
//!   `ordinaryUsageAllowed` flag (see [`crate::usage`]);
//! * `thread/read` — metadata-only lookup used to prove a thread exists (and where) before a
//!   same-profile resume is launched;
//! * `thread/items/list` — a bounded, paginated page of a thread's own user/agent message text,
//!   used to build a real (not repo-facts-only) `STATE_CONTINUATION` context bundle (see
//!   [`crate::context_capture`]). Not `thread/read`'s own `includeTurns: true`: the schema
//!   documents that as deprecated for paginated threads in favour of this call.
//!
//! Relay never reads `auth.json` or any token: the server process reads its own `CODEX_HOME`, and
//! only the typed, non-secret response fields below ever reach Relay. Every failure — spawn,
//! timeout, malformed line, JSON-RPC error, wrong `codexHome` — is an [`AppServerError`], and
//! callers map that to "unknown / not verified", never to an action.

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use relay_core::handoff::{ConversationExcerpt, ExcerptRole};
use serde_json::{Value, json};

use crate::AUTHENTICATION_OVERRIDE_VARIABLES;

/// Wall-clock budget for one whole session (spawn, handshake, every request).
const SESSION_TIMEOUT: Duration = Duration::from_secs(20);
/// A misbehaving server can never make Relay buffer unbounded output.
const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_LINES: usize = 512;

#[derive(Debug, Eq, PartialEq)]
pub enum AppServerError {
    /// The executable could not be started (missing, not executable, or too old for `app-server`).
    Spawn,
    /// No matching response within the session budget.
    Timeout,
    /// A response that does not have the documented shape, or the server closed the pipe.
    Protocol(String),
    /// The server answered with a JSON-RPC error (`code`, `message`).
    Rpc { code: i64, message: String },
    /// The server resolved a different `CODEX_HOME` than the profile directory Relay asked for.
    HomeMismatch,
}

impl std::fmt::Display for AppServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn => write!(f, "codex app-server could not be started"),
            Self::Timeout => write!(f, "codex app-server did not answer in time"),
            Self::Protocol(detail) => write!(f, "unexpected app-server response: {detail}"),
            Self::Rpc { code, message } => write!(f, "app-server error {code}: {message}"),
            Self::HomeMismatch => write!(
                f,
                "codex app-server resolved a different CODEX_HOME than the profile's"
            ),
        }
    }
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    deadline: Instant,
    next_id: u64,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

impl Session {
    fn open(executable: &Path, config_dir: &Path) -> Result<Self, AppServerError> {
        let mut command = Command::new(executable);
        command
            .arg("app-server")
            .env("CODEX_HOME", config_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
            command.env_remove(variable);
        }
        let mut child = command.spawn().map_err(|_| AppServerError::Spawn)?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ignored = child.kill();
            let _ignored = child.wait();
            return Err(AppServerError::Spawn);
        };
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            for _ in 0..MAX_LINES {
                let mut line = String::new();
                match reader
                    .by_ref()
                    .take(MAX_LINE_BYTES as u64)
                    .read_line(&mut line)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
            deadline: Instant::now() + SESSION_TIMEOUT,
            next_id: 1,
        })
    }

    fn write(&mut self, message: &Value) -> Result<(), AppServerError> {
        let mut text = message.to_string();
        text.push('\n');
        self.stdin
            .write_all(text.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|_| AppServerError::Protocol("server closed its input".to_owned()))
    }

    fn notify(&mut self, method: &str) -> Result<(), AppServerError> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method }))
    }

    /// Sends one request and returns its `result`. Server-initiated notifications and requests
    /// (anything carrying a `method`) are ignored: this client answers nothing and asks for
    /// nothing beyond the documented read methods.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or(AppServerError::Timeout)?;
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Err(AppServerError::Timeout),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AppServerError::Protocol(
                        "server closed its output".to_owned(),
                    ));
                }
            };
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if message.get("method").is_some() || message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(AppServerError::Rpc {
                    code: error
                        .get("code")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect(),
                });
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| AppServerError::Protocol("response without a result".to_owned()));
        }
    }

    /// The capability handshake. Fails closed unless the server resolved exactly the profile
    /// directory Relay asked for — the guarantee that a reading belongs to *that* profile.
    fn handshake(&mut self, config_dir: &Path) -> Result<(), AppServerError> {
        let result = self.request(
            "initialize",
            json!({
                "clientInfo": { "name": "agent-relay", "title": "Agent Relay", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": false },
            }),
        )?;
        self.notify("initialized")?;
        let reported = result
            .get("codexHome")
            .and_then(Value::as_str)
            .ok_or_else(|| AppServerError::Protocol("initialize named no codexHome".to_owned()))?;
        if same_directory(Path::new(reported), config_dir) {
            Ok(())
        } else {
            Err(AppServerError::HomeMismatch)
        }
    }
}

fn same_directory(a: &Path, b: &Path) -> bool {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(a) == canonical(b)
}

/// One usage window (`primary` = short, `secondary` = long).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimitWindow {
    pub used_percent: u32,
    pub resets_at_unix_s: Option<u64>,
}

/// The typed subset of `account/rateLimits/read` (plus the account kind from `account/read`)
/// that Relay's usage policy consumes. Contains no credentials and no account identifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateLimitsReport {
    /// `ordinaryUsageAllowed`: the backend's own verdict, validated against the active account.
    /// `None` = unavailable.
    pub ordinary_usage_allowed: Option<bool>,
    /// Some backend account id was supplied with the snapshot (its value is not kept).
    pub account_identified: bool,
    /// `chatgpt` for plan-based accounts; anything else has no plan usage windows to read.
    pub account_kind: Option<String>,
    pub windows: Vec<RateLimitWindow>,
    /// `rateLimitReachedType`, when the backend reports one.
    pub reached_type: Option<String>,
}

fn window(value: &Value) -> Option<RateLimitWindow> {
    let object = value.as_object()?;
    Some(RateLimitWindow {
        used_percent: u32::try_from(object.get("usedPercent")?.as_i64()?.clamp(0, 1000)).ok()?,
        resets_at_unix_s: object
            .get("resetsAt")
            .and_then(Value::as_i64)
            .and_then(|seconds| u64::try_from(seconds).ok()),
    })
}

/// Parses the `account/rateLimits/read` result. Prefers the metered `codex` bucket from
/// `rateLimitsByLimitId`, falling back to the backward-compatible single `rateLimits` view.
pub fn parse_rate_limits(
    result: &Value,
    account_kind: Option<String>,
) -> Result<RateLimitsReport, AppServerError> {
    let snapshot = result
        .get("rateLimitsByLimitId")
        .and_then(|buckets| buckets.get("codex"))
        .or_else(|| result.get("rateLimits"))
        .filter(|value| value.is_object())
        .ok_or_else(|| AppServerError::Protocol("no rate-limit snapshot".to_owned()))?;
    let windows = ["primary", "secondary"]
        .iter()
        .filter_map(|key| snapshot.get(key).and_then(window))
        .collect();
    Ok(RateLimitsReport {
        ordinary_usage_allowed: result.get("ordinaryUsageAllowed").and_then(Value::as_bool),
        account_identified: result
            .get("accountId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        account_kind,
        windows,
        reached_type: snapshot
            .get("rateLimitReachedType")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Reads the account kind and rate limits for the profile at `config_dir`, read-only.
pub fn read_rate_limits(
    executable: &Path,
    config_dir: &Path,
) -> Result<RateLimitsReport, AppServerError> {
    let mut session = Session::open(executable, config_dir)?;
    session.handshake(config_dir)?;
    let account = session.request("account/read", json!({ "refreshToken": false }))?;
    let account_kind = account
        .get("account")
        .and_then(|account| account.get("type"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let limits = session.request(
        "account/rateLimits/read",
        json!({ "excludeResetCreditDetails": true }),
    )?;
    parse_rate_limits(&limits, account_kind)
}

/// What `thread/read` proved about a thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadIdentity {
    pub id: String,
    pub cwd: Option<PathBuf>,
}

/// Proves that `thread_id` exists in the profile's own `CODEX_HOME` (metadata only — no turns are
/// read or started) and returns the id and working directory the server reports. A missing or
/// stale id is an error here rather than the silent new thread an interactive `codex resume`
/// might otherwise start.
pub fn read_thread(
    executable: &Path,
    config_dir: &Path,
    thread_id: &str,
) -> Result<ThreadIdentity, AppServerError> {
    let mut session = Session::open(executable, config_dir)?;
    session.handshake(config_dir)?;
    let result = session.request(
        "thread/read",
        json!({ "threadId": thread_id, "includeTurns": false }),
    )?;
    let thread = result
        .get("thread")
        .ok_or_else(|| AppServerError::Protocol("thread/read returned no thread".to_owned()))?;
    let id = thread
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| AppServerError::Protocol("thread without an id".to_owned()))?
        .to_owned();
    Ok(ThreadIdentity {
        id,
        cwd: thread.get("cwd").and_then(Value::as_str).map(PathBuf::from),
    })
}

/// How many raw thread items to request per page before filtering — deliberately larger than the
/// excerpt count actually kept, since most items on a real thread are tool calls and other
/// non-conversational entries this never surfaces (only verbatim user/agent message text ever
/// leaves this function, exactly as `relay_provider_claude`'s transcript extraction excludes tool
/// use, tool results and reasoning).
const ITEM_PAGE_LIMIT: u32 = 60;

/// Real, verbatim recent conversation text for a thread, oldest first — user and agent messages
/// only. Uses `thread/items/list` (paginated, newest-first, a bounded `limit`) rather than
/// `thread/read`'s own `includeTurns: true`: the app-server's own schema documents that flag as
/// deprecated for paginated threads, in favour of this call and `thread/turns/list`. Best effort,
/// like every other app-server read Relay makes: any failure (old server, unreadable thread, a
/// malformed page) degrades to an empty list rather than failing the caller's whole capture — the
/// M6 spec's "no fabricated project state" rule extends to "no fabricated conversation state."
pub fn recent_conversation_excerpts(
    executable: &Path,
    config_dir: &Path,
    thread_id: &str,
) -> Vec<ConversationExcerpt> {
    fetch_recent_conversation_excerpts(executable, config_dir, thread_id).unwrap_or_default()
}

fn fetch_recent_conversation_excerpts(
    executable: &Path,
    config_dir: &Path,
    thread_id: &str,
) -> Result<Vec<ConversationExcerpt>, AppServerError> {
    let mut session = Session::open(executable, config_dir)?;
    session.handshake(config_dir)?;
    let result = session.request(
        "thread/items/list",
        json!({ "threadId": thread_id, "limit": ITEM_PAGE_LIMIT, "sortDirection": "desc" }),
    )?;
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AppServerError::Protocol("thread/items/list returned no data".to_owned()))?;
    let mut excerpts = Vec::new();
    for entry in data {
        let Some(item) = entry.get("item") else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("agentMessage") => {
                if let Some(text) = item.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    excerpts.push(ConversationExcerpt {
                        role: ExcerptRole::Assistant,
                        text: text.to_owned(),
                    });
                }
            }
            Some("userMessage") => {
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    excerpts.push(ConversationExcerpt {
                        role: ExcerptRole::User,
                        text,
                    });
                }
            }
            // Every other item type (tool calls, tool outputs, hook prompts, reasoning, ...) is
            // deliberately not conversation text and is skipped, not stringified.
            _ => {}
        }
    }
    // The page was requested newest-first; `ConversationExcerpt` lists are oldest-first.
    excerpts.reverse();
    Ok(excerpts)
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted `codex app-server` for tests: answers from fixture files inside the profile
    //! directory (its own `CODEX_HOME`), so tests never touch a real Codex install.
    use std::{os::unix::fs::PermissionsExt as _, path::Path};

    /// Fixture files (all optional): `mode` = `hang` | `garbage` | `exit` | `wronghome`;
    /// `account.json`, `limits.json`, `thread.json`, `items.json` = the `result` bodies to
    /// return; `thread_error` / `items_error` = present → that method answers a JSON-RPC error.
    pub fn install(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("fake-codex");
        std::fs::write(
            &path,
            r#"#!/bin/sh
[ "$1" = "app-server" ] || exit 64
mode=$(cat "$CODEX_HOME/mode" 2>/dev/null)
[ "$mode" = "exit" ] && exit 1
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  [ "$mode" = "hang" ] && sleep 60
  [ "$mode" = "garbage" ] && { echo 'not json'; exit 0; }
  # a server-initiated notification first, as the real server sends
  echo '{"method":"remoteControl/status/changed","params":{}}'
  case "$line" in
    *'"method":"initialize"'*)
      home="$CODEX_HOME"; [ "$mode" = "wronghome" ] && home="/somewhere/else"
      printf '{"id":%s,"result":{"codexHome":"%s"}}\n' "$id" "$home";;
    *'"method":"account/read"'*)
      printf '{"id":%s,"result":%s}\n' "$id" "$(cat "$CODEX_HOME/account.json")";;
    *'"method":"account/rateLimits/read"'*)
      printf '{"id":%s,"result":%s}\n' "$id" "$(cat "$CODEX_HOME/limits.json")";;
    *'"method":"thread/read"'*)
      if [ -f "$CODEX_HOME/thread_error" ]; then
        printf '{"id":%s,"error":{"code":-32600,"message":"thread not loaded"}}\n' "$id"
      else
        printf '{"id":%s,"result":%s}\n' "$id" "$(cat "$CODEX_HOME/thread.json")"
      fi;;
    *'"method":"thread/items/list"'*)
      if [ -f "$CODEX_HOME/items_error" ]; then
        printf '{"id":%s,"error":{"code":-32600,"message":"items not loaded"}}\n' "$id"
      else
        printf '{"id":%s,"result":%s}\n' "$id" "$(cat "$CODEX_HOME/items.json")"
      fi;;
    *) printf '{"id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id";;
  esac
done
"#,
        )
        .expect("write fake codex");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake codex");
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("account.json"), r#"{"account":{"type":"chatgpt","email":"x","planType":"plus"},"requiresOpenaiAuth":true}"#).unwrap();
        std::fs::write(dir.path().join("limits.json"), r#"{"ordinaryUsageAllowed":true,"accountId":"acct","rateLimits":{"primary":{"usedPercent":10,"windowDurationMins":300,"resetsAt":1900000000},"secondary":{"usedPercent":40,"resetsAt":1900100000},"rateLimitReachedType":null}}"#).unwrap();
        dir
    }

    #[test]
    fn reads_the_typed_rate_limit_fields_and_no_identity() {
        let dir = home();
        let exe = fake::install(dir.path());
        let report = read_rate_limits(&exe, dir.path()).expect("report");
        assert_eq!(report.ordinary_usage_allowed, Some(true));
        assert!(report.account_identified);
        assert_eq!(report.account_kind.as_deref(), Some("chatgpt"));
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[1].used_percent, 40);
        assert_eq!(report.windows[0].resets_at_unix_s, Some(1_900_000_000));
    }

    #[test]
    fn a_server_that_resolved_another_codex_home_fails_closed() {
        let dir = home();
        std::fs::write(dir.path().join("mode"), "wronghome").unwrap();
        let exe = fake::install(dir.path());
        assert_eq!(
            read_rate_limits(&exe, dir.path()),
            Err(AppServerError::HomeMismatch)
        );
    }

    #[test]
    fn a_missing_executable_a_dead_server_and_garbage_are_all_errors() {
        let dir = home();
        assert_eq!(
            read_rate_limits(Path::new("/definitely/not/codex"), dir.path()),
            Err(AppServerError::Spawn)
        );
        let exe = fake::install(dir.path());
        std::fs::write(dir.path().join("mode"), "exit").unwrap();
        assert!(read_rate_limits(&exe, dir.path()).is_err());
        std::fs::write(dir.path().join("mode"), "garbage").unwrap();
        assert!(read_rate_limits(&exe, dir.path()).is_err());
    }

    #[test]
    fn thread_read_returns_identity_and_a_missing_thread_is_an_error() {
        let dir = home();
        std::fs::write(
            dir.path().join("thread.json"),
            r#"{"thread":{"id":"t-1","cwd":"/work/proj","preview":"never inspected"}}"#,
        )
        .unwrap();
        let exe = fake::install(dir.path());
        let identity = read_thread(&exe, dir.path(), "t-1").expect("identity");
        assert_eq!(identity.id, "t-1");
        assert_eq!(identity.cwd.as_deref(), Some(Path::new("/work/proj")));
        std::fs::write(dir.path().join("thread_error"), "").unwrap();
        assert!(matches!(
            read_thread(&exe, dir.path(), "t-1"),
            Err(AppServerError::Rpc { .. })
        ));
    }

    #[test]
    fn recent_conversation_excerpts_extracts_only_message_text_oldest_first() {
        let dir = home();
        // Newest-first, as the real server would answer a `sortDirection: "desc"` page — and
        // deliberately includes non-message item types, which must be skipped, not stringified.
        // One line: the fixture is `cat`'d straight into a JSON-RPC response, which is
        // one-message-per-line framing (see this module's own doc comment).
        std::fs::write(
            dir.path().join("items.json"),
            r#"{"data":[{"turnId":"t3","item":{"type":"agentMessage","id":"3","text":"third"}},{"turnId":"t3","item":{"type":"functionCallOutput","id":"x","name":"ls","output":{}}},{"turnId":"t2","item":{"type":"userMessage","id":"2","content":[{"type":"text","text":"second"},{"type":"image","url":"x"}]}},{"turnId":"t1","item":{"type":"agentMessage","id":"1","text":"  "}},{"turnId":"t0","item":{"type":"userMessage","id":"0","content":[{"type":"text","text":"first"}]}}]}"#,
        )
        .unwrap();
        let exe = fake::install(dir.path());
        let excerpts = recent_conversation_excerpts(&exe, dir.path(), "t-1");
        assert_eq!(
            excerpts,
            vec![
                ConversationExcerpt {
                    role: ExcerptRole::User,
                    text: "first".to_owned(),
                },
                ConversationExcerpt {
                    role: ExcerptRole::User,
                    text: "second".to_owned(),
                },
                ConversationExcerpt {
                    role: ExcerptRole::Assistant,
                    text: "third".to_owned(),
                },
            ],
            "must skip non-message items and blank-text messages, oldest first"
        );
    }

    #[test]
    fn recent_conversation_excerpts_degrades_to_empty_on_any_failure() {
        let dir = home();
        std::fs::write(dir.path().join("items_error"), "").unwrap();
        let exe = fake::install(dir.path());
        assert_eq!(recent_conversation_excerpts(&exe, dir.path(), "t-1"), []);

        let missing_executable = dir.path().join("does-not-exist");
        assert_eq!(
            recent_conversation_excerpts(&missing_executable, dir.path(), "t-1"),
            []
        );
    }

    #[test]
    fn a_snapshot_without_windows_or_buckets_is_a_protocol_error() {
        assert!(parse_rate_limits(&json!({}), None).is_err());
        let report = parse_rate_limits(&json!({"rateLimits":{}}), None).expect("empty ok");
        assert_eq!(report.ordinary_usage_allowed, None);
        assert!(report.windows.is_empty());
    }

    /// Benchmark, not a correctness test (see `relay-cli/tests/benchmark.rs`'s module doc for the
    /// same convention): real, timed round trips against the actually-installed `codex` CLI and a
    /// real, already-authenticated `CODEX_HOME` — the periodic-supervision-poll cost
    /// `RELAY_CODEX_POLL_SECS` pays every interval, contrasted against `relay-cli/src/auth.rs`'s
    /// `codex doctor --json`-based readiness check (see `docs/benchmarks.md`: the two Codex checks
    /// have very different costs and this is why). Read-only, spends no quota. Set
    /// `RELAY_BENCH_CODEX_HOME` to a real profile's `CODEX_HOME` and `RELAY_BENCH_CODEX_EXE` to the
    /// `codex` executable (defaults to `codex` on `PATH`) to run it:
    /// `RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<name>/codex cargo test --release -p
    /// relay-provider-codex --lib app_server::tests::bench_real_read_rate_limits -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark against a real, already-authenticated Codex profile — see the doc comment"]
    fn bench_real_read_rate_limits() {
        let Ok(home) = std::env::var("RELAY_BENCH_CODEX_HOME") else {
            eprintln!("skipped: set RELAY_BENCH_CODEX_HOME to a real, authenticated CODEX_HOME");
            return;
        };
        let exe = std::env::var("RELAY_BENCH_CODEX_EXE").unwrap_or_else(|_| "codex".to_owned());
        let exe = std::path::PathBuf::from(exe);
        let home = std::path::PathBuf::from(home);

        const ROUNDS: u32 = 5;
        let mut millis = Vec::new();
        for _ in 0..ROUNDS {
            let start = std::time::Instant::now();
            let report = read_rate_limits(&exe, &home).expect("real read_rate_limits");
            millis.push(start.elapsed().as_secs_f64() * 1000.0);
            assert!(report.account_identified || report.ordinary_usage_allowed.is_some());
        }
        millis.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs"));
        let n = millis.len();
        eprintln!(
            "BENCHMARK codex_app_server_read_rate_limits: n={n} min_ms={:.1} median_ms={:.1} max_ms={:.1} all_ms={:?}",
            millis[0],
            millis[n / 2],
            millis[n - 1],
            millis
        );
    }
}
