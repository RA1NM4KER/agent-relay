//! M2C.1: the structured Claude Code usage signals Relay consumes without spending API usage, and
//! the safe, per-profile files they are recorded in.
//!
//! Three sources, none of which needs an extra API request:
//! - the `StopFailure` hook (`error = rate_limit`), recorded by `relay hook claude stop-failure`;
//! - the statusline JSON's `rate_limits` (`five_hour`/`seven_day` `used_percentage`+`resets_at`),
//!   recorded by `relay hook claude statusline`;
//! - `rate_limit_event` objects from `--output-format stream-json`, recorded wherever Relay itself
//!   controls a headless Claude process.
//!
//! Only closed, structured metadata is ever stored. Assistant text, error bodies and arbitrary
//! provider output are inspected in memory for an allowlisted limit phrase and then discarded.
//! Every reader treats a missing, oversized, symlinked or malformed file as "no signal".

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use relay_core::{AtomicWrite, Error, FsAtomicWriter, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Root of everything Relay adds to a Claude profile directory. Created only by an explicit
/// `relay integration claude install`.
pub const INTEGRATION_DIR: &str = "relay-integration";
const SIGNALS_DIR: &str = "signals";
const STATUSLINE_FILE: &str = "statusline.json";
const STATUSLINE_HISTORY_FILE: &str = "statusline_history.json";
const STOP_FAILURES_FILE: &str = "stop_failures.json";
const RATE_LIMIT_EVENTS_FILE: &str = "rate_limit_events.json";
const MAX_SIGNAL_FILE_BYTES: u64 = 256 * 1024;
const MAX_STOP_FAILURE_RECORDS: usize = 32;
/// Enough pre-modal observations for a normally redrawing interactive TUI without becoming an
/// event log. The policy also applies its much shorter freshness bound before using one.
pub const MAX_STATUSLINE_HISTORY: usize = 20;

#[must_use]
pub fn integration_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(INTEGRATION_DIR)
}

#[must_use]
pub fn signals_dir(config_dir: &Path) -> PathBuf {
    integration_dir(config_dir).join(SIGNALS_DIR)
}

/// Which limit a Claude Code message names. Closed set: anything that is not exactly one of
/// these phrases is not a limit message (a generic 429/capacity error never is).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Session,
    Weekly,
    Usage,
    UsageCredit,
    Monthly,
    MonthlySpend,
    /// The bare "You've hit your limit".
    Unspecified,
    Opus,
    Sonnet,
    Fable,
    /// Fast-mode limit: Claude Code falls back to standard speed, so the profile still works.
    Fast,
}

/// Whose work a limit blocks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitScope {
    /// Blocks the whole profile.
    Account,
    /// Blocks only one model family; the profile still works for other models.
    Model(ModelFamily),
    /// Degrades a feature but never blocks the profile.
    NonBlocking,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelFamily {
    Opus,
    Sonnet,
    Fable,
}

impl ModelFamily {
    /// Whether a model id/alias (`claude-opus-5`, `sonnet`, ...) belongs to this family.
    #[must_use]
    pub fn matches_model(self, model: &str) -> bool {
        let model = model.to_lowercase();
        match self {
            Self::Opus => model.contains("opus"),
            Self::Sonnet => model.contains("sonnet"),
            Self::Fable => model.contains("fable"),
        }
    }
}

impl LimitKind {
    #[must_use]
    pub const fn scope(self) -> LimitScope {
        match self {
            Self::Session
            | Self::Weekly
            | Self::Usage
            | Self::UsageCredit
            | Self::Monthly
            | Self::MonthlySpend
            | Self::Unspecified => LimitScope::Account,
            Self::Opus => LimitScope::Model(ModelFamily::Opus),
            Self::Sonnet => LimitScope::Model(ModelFamily::Sonnet),
            Self::Fable => LimitScope::Model(ModelFamily::Fable),
            Self::Fast => LimitScope::NonBlocking,
        }
    }

    /// Maps a stream-json `rateLimitType`.
    #[must_use]
    pub fn from_rate_limit_type(value: &str) -> Option<Self> {
        Some(match value {
            "five_hour" => Self::Session,
            "seven_day" => Self::Weekly,
            "seven_day_opus" => Self::Opus,
            "seven_day_sonnet" => Self::Sonnet,
            "seven_day_overage_included" => Self::Fable,
            "overage" => Self::UsageCredit,
            _ => return None,
        })
    }
}

/// Exact, allowlisted limit phrases as Claude Code 2.1.277 prints them ("You've hit your session
/// limit · resets 3pm"). Longer names first so "usage credit" is never read as "usage".
const LIMIT_NAMES: &[(&str, LimitKind)] = &[
    ("monthly spend limit", LimitKind::MonthlySpend),
    ("usage credit limit", LimitKind::UsageCredit),
    ("session limit", LimitKind::Session),
    ("weekly limit", LimitKind::Weekly),
    ("monthly limit", LimitKind::Monthly),
    ("usage limit", LimitKind::Usage),
    ("opus limit", LimitKind::Opus),
    ("sonnet limit", LimitKind::Sonnet),
    ("fable limit", LimitKind::Fable),
    ("fast limit", LimitKind::Fast),
    ("limit", LimitKind::Unspecified),
];

/// Recognizes a real Claude Code limit message. Deliberately narrow: it requires the literal
/// "hit your <name> limit" construction, so "429", "rate limit exceeded", "overloaded" and
/// "temporary capacity issue" are never limit messages.
#[must_use]
pub fn classify_limit_message(text: &str) -> Option<LimitKind> {
    let lowercase = text.to_lowercase().replace('\u{2019}', "'");
    let mut search_from = 0;
    while let Some(offset) = lowercase[search_from..].find("hit your ") {
        let start = search_from + offset + "hit your ".len();
        let rest = &lowercase[start..];
        if let Some((_, kind)) = LIMIT_NAMES.iter().find(|(name, _)| {
            rest.strip_prefix(name).is_some_and(|after| {
                after
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric())
            })
        }) {
            return Some(*kind);
        }
        search_from = start;
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowUsage {
    pub used_percentage: f64,
    /// Unix epoch seconds, as Claude Code reports it.
    pub resets_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusLineSnapshot {
    pub captured_unix_ms: u64,
    pub session_id: Option<String>,
    pub model_id: Option<String>,
    pub five_hour: Option<WindowUsage>,
    pub seven_day: Option<WindowUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StopFailureRecord {
    pub recorded_unix_ms: u64,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// The hook `error` value (e.g. `rate_limit`).
    pub error: String,
    /// Set only when the failure text named a real limit; a bare `rate_limit` (which can be a
    /// transient 429 capacity error) leaves this `None`.
    pub limit_kind: Option<LimitKind>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RateLimitEventRecord {
    pub recorded_unix_ms: u64,
    pub session_id: Option<String>,
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
    pub rate_limit_type: Option<String>,
    /// Unix epoch seconds.
    pub resets_at: Option<u64>,
    pub utilization: Option<f64>,
    pub is_using_overage: bool,
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn window(value: Option<&Value>) -> Option<WindowUsage> {
    let object = value?.as_object()?;
    let used_percentage = number(object.get("used_percentage")?)?;
    let resets_at = object.get("resets_at")?.as_f64()?;
    if !(0.0..=1.0e12).contains(&resets_at) || used_percentage < 0.0 {
        return None;
    }
    Some(WindowUsage {
        used_percentage,
        resets_at: resets_at as u64,
    })
}

/// Parses the statusline JSON Claude Code feeds a `statusLine` command. Anything that is not a
/// JSON object yields `None`; a valid object without `rate_limits` (an API-key account, or before
/// the first response) yields a snapshot with no windows, which the policy reads as `UNKNOWN`.
#[must_use]
pub fn parse_statusline_input(bytes: &[u8], now_unix_ms: u64) -> Option<StatusLineSnapshot> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let limits = object.get("rate_limits").and_then(Value::as_object);
    Some(StatusLineSnapshot {
        captured_unix_ms: now_unix_ms,
        session_id: object
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_id: object
            .get("model")
            .and_then(|model| model.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        five_hour: window(limits.and_then(|limits| limits.get("five_hour"))),
        seven_day: window(limits.and_then(|limits| limits.get("seven_day"))),
    })
}

/// Parses a `StopFailure` hook payload into safe metadata. The assistant message and error body
/// are only scanned for a limit phrase and then dropped.
#[must_use]
pub fn parse_stop_failure_input(bytes: &[u8], now_unix_ms: u64) -> Option<StopFailureRecord> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    if object.get("hook_event_name").and_then(Value::as_str) != Some("StopFailure") {
        return None;
    }
    let error = object.get("error").and_then(Value::as_str)?;
    let text_kind = ["last_assistant_message", "error_details"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .find_map(classify_limit_message);
    Some(StopFailureRecord {
        recorded_unix_ms: now_unix_ms,
        session_id: object
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd: object.get("cwd").and_then(Value::as_str).map(str::to_owned),
        error: error.chars().take(64).collect(),
        limit_kind: text_kind,
    })
}

/// Extracts every `rate_limit_event` from stream-json output (one JSON object per line).
#[must_use]
pub fn parse_rate_limit_events(
    stdout: &[u8],
    now_unix_ms: u64,
    session_id: Option<&str>,
) -> Vec<RateLimitEventRecord> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|value| value.get("type").and_then(Value::as_str) == Some("rate_limit_event"))
        .filter_map(|value| {
            let info = value.get("rate_limit_info")?.as_object()?;
            let status = info.get("status")?.as_str()?;
            if !matches!(status, "allowed" | "allowed_warning" | "rejected") {
                return None;
            }
            Some(RateLimitEventRecord {
                recorded_unix_ms: now_unix_ms,
                session_id: value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| session_id.map(str::to_owned)),
                status: status.to_owned(),
                rate_limit_type: info
                    .get("rateLimitType")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                resets_at: info
                    .get("resetsAt")
                    .and_then(Value::as_f64)
                    .filter(|value| (0.0..=1.0e12).contains(value))
                    .map(|value| value as u64),
                utilization: info.get("utilization").and_then(number),
                is_using_overage: info
                    .get("isUsingOverage")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SIGNAL_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_SIGNAL_FILE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn write_signal(config_dir: &Path, file: &str, bytes: &[u8]) -> Result<()> {
    let integration = integration_dir(config_dir);
    // Signals are only ever written into a profile that opted in via `integration install`. The
    // manifest is what marks an active install: a session still running after an uninstall must
    // not re-create anything.
    if !fs::symlink_metadata(integration.join("manifest.json"))
        .is_ok_and(|metadata| metadata.file_type().is_file())
    {
        return Err(Error::ProviderCommandFailed);
    }
    let directory = signals_dir(config_dir);
    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        path: directory.clone(),
        source,
    })?;
    FsAtomicWriter.write_atomic(&directory.join(file), bytes)
}

/// The signals a policy evaluation reads for one profile. Missing or unreadable files are simply
/// absent.
#[derive(Clone, Debug, Default)]
pub struct ProfileSignals {
    pub statusline: Option<StatusLineSnapshot>,
    /// Bounded, per-profile statusline history. It contains only the already-sanitized snapshot
    /// shape and lets the policy look behind a modal's null-window statusline redraw.
    pub statusline_history: Vec<StatusLineSnapshot>,
    pub stop_failures: Vec<StopFailureRecord>,
    pub rate_limit_events: Vec<RateLimitEventRecord>,
}

#[must_use]
pub fn read_profile_signals(config_dir: &Path) -> ProfileSignals {
    let directory = signals_dir(config_dir);
    let load = |file: &str| read_bounded(&directory.join(file));
    ProfileSignals {
        statusline: load(STATUSLINE_FILE).and_then(|bytes| serde_json::from_slice(&bytes).ok()),
        statusline_history: load(STATUSLINE_HISTORY_FILE)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default(),
        stop_failures: load(STOP_FAILURES_FILE)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default(),
        rate_limit_events: load(RATE_LIMIT_EVENTS_FILE)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default(),
    }
}

pub fn record_statusline(config_dir: &Path, snapshot: &StatusLineSnapshot) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(|_| Error::AtomicWriteFailed)?;
    write_signal(config_dir, STATUSLINE_FILE, &bytes)?;

    let mut history = read_profile_signals(config_dir).statusline_history;
    // Coalesce identical consecutive observations while retaining the newest timestamp. This
    // keeps a continuously redrawn statusline fresh without growing the history unnecessarily.
    if let Some(last) = history.last_mut()
        && same_statusline_shape(last, snapshot)
    {
        *last = snapshot.clone();
    } else {
        history.push(snapshot.clone());
    }
    if history.len() > MAX_STATUSLINE_HISTORY {
        history.drain(..history.len() - MAX_STATUSLINE_HISTORY);
    }
    let history_bytes =
        serde_json::to_vec_pretty(&history).map_err(|_| Error::AtomicWriteFailed)?;
    write_signal(config_dir, STATUSLINE_HISTORY_FILE, &history_bytes)
}

fn same_statusline_shape(left: &StatusLineSnapshot, right: &StatusLineSnapshot) -> bool {
    left.session_id == right.session_id
        && left.model_id == right.model_id
        && left.five_hour == right.five_hour
        && left.seven_day == right.seven_day
}

pub fn record_stop_failure(config_dir: &Path, record: StopFailureRecord) -> Result<()> {
    let mut records = read_profile_signals(config_dir).stop_failures;
    records.push(record);
    if records.len() > MAX_STOP_FAILURE_RECORDS {
        let excess = records.len() - MAX_STOP_FAILURE_RECORDS;
        records.drain(..excess);
    }
    let bytes = serde_json::to_vec_pretty(&records).map_err(|_| Error::AtomicWriteFailed)?;
    write_signal(config_dir, STOP_FAILURES_FILE, &bytes)
}

/// Keeps the newest event per `rateLimitType` so the file cannot grow without bound.
pub fn record_rate_limit_events(
    config_dir: &Path,
    events: Vec<RateLimitEventRecord>,
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut records = read_profile_signals(config_dir).rate_limit_events;
    for event in events {
        records.retain(|existing| existing.rate_limit_type != event.rate_limit_type);
        records.push(event);
    }
    let bytes = serde_json::to_vec_pretty(&records).map_err(|_| Error::AtomicWriteFailed)?;
    write_signal(config_dir, RATE_LIMIT_EVENTS_FILE, &bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        INTEGRATION_DIR, LimitKind, LimitScope, MAX_STATUSLINE_HISTORY, ModelFamily,
        classify_limit_message, parse_rate_limit_events, parse_statusline_input,
        parse_stop_failure_input, read_profile_signals, record_statusline,
    };

    // Strings below are the literal messages Claude Code 2.1.277 constructs.
    #[test]
    fn real_claude_limit_messages_are_recognized() {
        for (message, kind) in [
            (
                "You've hit your session limit · resets 3pm",
                LimitKind::Session,
            ),
            (
                "You've hit your weekly limit · resets Mon 9am",
                LimitKind::Weekly,
            ),
            ("You've hit your usage limit", LimitKind::Usage),
            ("You've hit your Opus limit · resets 3pm", LimitKind::Opus),
            ("You've hit your Sonnet limit", LimitKind::Sonnet),
            ("You've hit your monthly limit", LimitKind::Monthly),
            ("You've hit your fast limit", LimitKind::Fast),
            ("You’ve hit your usage credit limit", LimitKind::UsageCredit),
            (
                "You've hit your monthly spend limit",
                LimitKind::MonthlySpend,
            ),
            ("You've hit your limit · resets 3pm", LimitKind::Unspecified),
        ] {
            assert_eq!(classify_limit_message(message), Some(kind), "{message}");
        }
    }

    #[test]
    fn generic_429_and_capacity_errors_are_not_limit_messages() {
        for message in [
            "Request rejected (429) · this may be a temporary capacity issue.",
            "API Error: 429 rate_limit_error",
            "rate limit exceeded",
            "usage limit reached",
            "Server is overloaded, please retry",
            "you hit your stride",
            "hit your limitations",
            "",
        ] {
            assert_eq!(classify_limit_message(message), None, "{message}");
        }
    }

    #[test]
    fn model_scoped_and_fast_limits_never_scope_to_the_account() {
        assert_eq!(
            LimitKind::Opus.scope(),
            LimitScope::Model(ModelFamily::Opus)
        );
        assert_eq!(LimitKind::Fast.scope(), LimitScope::NonBlocking);
        assert_eq!(LimitKind::Session.scope(), LimitScope::Account);
        assert!(ModelFamily::Opus.matches_model("claude-opus-5"));
        assert!(!ModelFamily::Opus.matches_model("claude-sonnet-5"));
    }

    #[test]
    fn statusline_input_is_parsed_from_the_documented_shape() {
        let input = br#"{"session_id":"s1","model":{"id":"claude-opus-5","display_name":"Opus"},
            "rate_limits":{"five_hour":{"used_percentage":100,"resets_at":2000000000},
            "seven_day":{"used_percentage":41.5,"resets_at":2000500000}}}"#;
        let snapshot = parse_statusline_input(input, 7).expect("snapshot");
        assert_eq!(snapshot.captured_unix_ms, 7);
        assert_eq!(snapshot.model_id.as_deref(), Some("claude-opus-5"));
        assert_eq!(snapshot.five_hour.expect("five").used_percentage, 100.0);
        assert_eq!(snapshot.seven_day.expect("seven").resets_at, 2_000_500_000);
    }

    #[test]
    fn statusline_without_rate_limits_has_no_windows() {
        let snapshot = parse_statusline_input(br#"{"session_id":"s1"}"#, 1).expect("snapshot");
        assert!(snapshot.five_hour.is_none() && snapshot.seven_day.is_none());
        assert!(parse_statusline_input(b"not json", 1).is_none());
        assert!(parse_statusline_input(b"[1,2]", 1).is_none());
    }

    #[test]
    fn statusline_history_is_bounded_and_preserves_pre_modal_snapshot() {
        let dir = tempdir().expect("temp");
        let integration = dir.path().join(INTEGRATION_DIR);
        fs::create_dir(&integration).expect("integration");
        fs::write(integration.join("manifest.json"), b"{}").expect("manifest");

        let useful = parse_statusline_input(
            br#"{"session_id":"s1","rate_limits":{"seven_day":{"used_percentage":98,"resets_at":2000000000}}}"#,
            1,
        )
        .expect("useful");
        record_statusline(dir.path(), &useful).expect("record useful");
        let null_windows = parse_statusline_input(br#"{"session_id":"s1"}"#, 2).expect("null");
        record_statusline(dir.path(), &null_windows).expect("record null");
        let after_modal = read_profile_signals(dir.path());
        assert_eq!(after_modal.statusline_history.len(), 2);
        assert_eq!(
            after_modal.statusline_history[0].seven_day,
            useful.seven_day
        );
        assert!(after_modal.statusline_history[1].seven_day.is_none());

        for captured in 3..=MAX_STATUSLINE_HISTORY as u64 + 4 {
            let snapshot = parse_statusline_input(
                format!(r#"{{"session_id":"s{captured}"}}"#).as_bytes(),
                captured,
            )
            .expect("snapshot");
            record_statusline(dir.path(), &snapshot).expect("record");
        }
        let signals = read_profile_signals(dir.path());
        assert_eq!(signals.statusline_history.len(), MAX_STATUSLINE_HISTORY);
        assert_eq!(
            signals.statusline.as_ref().map(|s| s.captured_unix_ms),
            Some(24)
        );
        assert!(
            signals
                .statusline_history
                .iter()
                .all(|snapshot| snapshot.captured_unix_ms >= 5)
        );
    }

    #[test]
    fn stop_failure_keeps_only_safe_metadata() {
        let input = br#"{"hook_event_name":"StopFailure","session_id":"s1","cwd":"/p",
            "transcript_path":"/secret/t.jsonl","error":"rate_limit",
            "error_details":"429 secret-body","last_assistant_message":"You've hit your session limit \u00b7 resets 3pm"}"#;
        let record = parse_stop_failure_input(input, 5).expect("record");
        assert_eq!(record.error, "rate_limit");
        assert_eq!(record.limit_kind, Some(LimitKind::Session));
        let serialized = serde_json::to_string(&record).expect("json");
        assert!(!serialized.contains("secret") && !serialized.contains("resets 3pm"));
    }

    #[test]
    fn transient_429_stop_failure_has_no_limit_kind() {
        let input = br#"{"hook_event_name":"StopFailure","session_id":"s1","error":"rate_limit",
            "last_assistant_message":"Request rejected (429) \u00b7 this may be a temporary capacity issue."}"#;
        let record = parse_stop_failure_input(input, 5).expect("record");
        assert_eq!(record.limit_kind, None);
        assert!(parse_stop_failure_input(br#"{"hook_event_name":"Stop"}"#, 5).is_none());
    }

    #[test]
    fn rate_limit_events_are_extracted_from_stream_json() {
        let stdout = br#"{"type":"system","subtype":"init"}
{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":2000000000,"rateLimitType":"five_hour","utilization":1.0,"isUsingOverage":false},"uuid":"u","session_id":"s1"}
{"type":"rate_limit_event","rate_limit_info":{"status":"weird"}}
{"type":"result","is_error":false}"#;
        let events = parse_rate_limit_events(stdout, 9, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, "rejected");
        assert_eq!(events[0].resets_at, Some(2_000_000_000));
        assert_eq!(events[0].session_id.as_deref(), Some("s1"));
        assert!(!events[0].is_using_overage);
    }
}
