//! M2C.1: what the Relay-installed Claude Code hook/statusline commands do when Claude runs them.
//!
//! These run inside a live Claude session, so they must never break it: every failure is
//! swallowed, nothing is printed for hooks, and the statusline passes a chained command's output
//! through untouched.

use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
};

use crate::usage_signals::{
    parse_statusline_input, parse_stop_failure_input, record_statusline, record_stop_failure,
};

const MAX_STDIN_BYTES: u64 = 1024 * 1024;

pub fn read_stdin_bounded(mut reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ignored = (&mut reader).take(MAX_STDIN_BYTES).read_to_end(&mut bytes);
    bytes
}

/// `relay hook claude stop-failure`: records safe metadata for a `StopFailure` event.
pub fn handle_stop_failure(config_dir: &Path, stdin: &[u8], now_unix_ms: u64) {
    if let Some(record) = parse_stop_failure_input(stdin, now_unix_ms) {
        let _ignored = record_stop_failure(config_dir, record);
    }
}

/// `relay hook claude statusline`: records the `rate_limits` snapshot, then either runs the
/// chained original command (same stdin, its stdout/stderr passed straight through, its exit code
/// returned) or prints a short Relay status line.
///
/// `badge` is `Some` only for a Relay-managed session. It is purely additive: it is appended to the
/// last line of whatever would have been shown anyway (the chained command's own output, byte for
/// byte, or Relay's own summary), and without a badge nothing about the previous behaviour changes.
#[must_use]
pub fn handle_statusline(
    config_dir: &Path,
    stdin: &[u8],
    now_unix_ms: u64,
    chain: Option<&str>,
    badge: Option<&str>,
) -> i32 {
    let snapshot = parse_statusline_input(stdin, now_unix_ms);
    if let Some(snapshot) = &snapshot {
        let _ignored = record_statusline(config_dir, snapshot);
    }
    if let Some(chain) = chain {
        return match badge {
            Some(badge) => run_chain_with_badge(chain, stdin, badge),
            None => run_chain(chain, stdin),
        };
    }
    let summary = snapshot
        .map(|snapshot| {
            let part = |label: &str, window: Option<crate::usage_signals::WindowUsage>| {
                window.map(|window| format!("{label} {:.0}%", window.used_percentage))
            };
            [
                part("5h", snapshot.five_hour),
                part("7d", snapshot.seven_day),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_default();
    println!("{}", append_badge(&summary, badge));
    0
}

/// Appends the badge to the last line of `output` (space-separated), or shows it alone when there
/// is nothing else. Never alters any existing byte of `output`.
#[must_use]
pub fn append_badge(output: &str, badge: Option<&str>) -> String {
    let Some(badge) = badge else {
        return output.to_owned();
    };
    let trimmed = output.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        badge.to_owned()
    } else {
        format!("{trimmed} {badge}")
    }
}

/// Like [`run_chain`], but the chained command's stdout is captured (bounded) so the badge can be
/// appended to its last line; its stderr stays inherited and its exit code is returned.
fn run_chain_with_badge(chain: &str, stdin: &[u8], badge: &str) -> i32 {
    let Ok(mut child) = Command::new("sh")
        .arg("-c")
        .arg(chain)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    else {
        return 0;
    };
    if let Some(mut pipe) = child.stdin.take() {
        let _ignored = pipe.write_all(stdin);
    }
    let mut captured = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ignored = stdout.take(MAX_STDIN_BYTES).read_to_end(&mut captured);
    }
    let code = child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&captured);
    println!("{}", append_badge(&text, Some(badge)));
    code
}

fn run_chain(chain: &str, stdin: &[u8]) -> i32 {
    let Ok(mut child) = Command::new("sh")
        .arg("-c")
        .arg(chain)
        .stdin(Stdio::piped())
        .spawn()
    else {
        return 0;
    };
    if let Some(mut pipe) = child.stdin.take() {
        let _ignored = pipe.write_all(stdin);
    }
    child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::append_badge;

    #[test]
    fn the_badge_is_appended_to_the_last_line_and_never_rewrites_anything() {
        assert_eq!(append_badge("mine", None), "mine");
        assert_eq!(append_badge("mine\n", Some("[R]")), "mine [R]");
        assert_eq!(
            append_badge("line one\nline two\n", Some("[R]")),
            "line one\nline two [R]"
        );
        assert_eq!(append_badge("", Some("[R]")), "[R]");
        assert_eq!(
            append_badge("\x1b[32mgreen\x1b[0m", Some("[R]")),
            "\x1b[32mgreen\x1b[0m [R]"
        );
    }

    use tempfile::tempdir;

    use super::handle_stop_failure;
    use crate::usage_signals::{INTEGRATION_DIR, read_profile_signals};

    #[test]
    fn recording_requires_an_installed_integration_directory() {
        let dir = tempdir().unwrap();
        let payload = br#"{"hook_event_name":"StopFailure","session_id":"s","error":"rate_limit"}"#;
        handle_stop_failure(dir.path(), payload, 1);
        assert!(
            !dir.path().join(INTEGRATION_DIR).exists(),
            "must not create the directory"
        );
        fs::create_dir(dir.path().join(INTEGRATION_DIR)).unwrap();
        handle_stop_failure(dir.path(), payload, 1);
        assert!(
            !dir.path().join(INTEGRATION_DIR).join("signals").exists(),
            "no manifest means no active install"
        );
        fs::write(
            dir.path().join(INTEGRATION_DIR).join("manifest.json"),
            b"{}",
        )
        .unwrap();
        handle_stop_failure(dir.path(), payload, 2);
        handle_stop_failure(dir.path(), b"garbage", 3);
        let signals = read_profile_signals(dir.path());
        assert_eq!(signals.stop_failures.len(), 1);
        assert_eq!(signals.stop_failures[0].recorded_unix_ms, 2);
    }
}
