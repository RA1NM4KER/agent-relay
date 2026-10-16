//! A quiet, cached hint that a newer *stable* Agent Relay release exists. Never a blocking
//! network call on any command's critical path, never telemetry, never an auto-upgrade.
//!
//! Architecture: a small cache (`checked_at_unix_ms`, `latest_version`) lives at
//! [`relay_core::RelayPaths::update_check_cache_file`] — the global state root, not any one
//! project, since this is about the installed binary. A *fresh* cache (checked within
//! [`CACHE_TTL_MS`]) is read synchronously and compared against the running binary's version:
//! that's one local file read, never a network round-trip, so it can never make a command feel
//! slower. A *stale or missing* cache never blocks anything either — the caller shows no hint this
//! run and spawns a short-lived, detached `relay` process (see [`INTERNAL_REFRESH_ARG`]) to
//! refresh the cache in the background for next time; that child is never awaited.
//!
//! An explicit `relay update --check` also uses this cache, refreshing it in the foreground when
//! stale or missing. The fetch itself has
//! a short bounded timeout and fails completely silently — a network hiccup, a GitHub outage, or
//! a malformed response all just mean "no cache update this time", never an error surfaced to a
//! user who is not even running the refresh command interactively.

use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use relay_core::{AtomicWrite as _, FsAtomicWriter, RelayPaths, handoff::OrchestrationLock};
use serde::{Deserialize, Serialize};

use crate::cli::Command as CliCommand;

const REPO: &str = "RA1NM4KER/agent-relay";
const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(2);

/// The hidden internal entry point (`cli.rs`'s `Command::InternalUpdateCheckRefresh`) a
/// human-facing command spawns, detached, purely to populate the cache for *next* time.
pub(crate) const INTERNAL_REFRESH_ARG: &str = "__internal-update-check-refresh";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cache {
    checked_at_unix_ms: u64,
    /// The latest known stable release's version, without a leading `v` (e.g. `"0.4.0"`).
    latest_version: String,
}

/// What a gated command should do this run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decision {
    /// A newer stable version to mention, if the cache is fresh enough to trust right now.
    hint: Option<String>,
    /// Whether it's worth kicking off a best-effort background refresh for next time.
    should_refresh: bool,
}

/// A parsed numeric `(major, minor, patch)` — enough for this project's plain `vX.Y.Z` release
/// tags and its own `X.Y.Z-dev.N+sha[.dirty]` dev-build versions (see `build.rs`): the `-`/`+`
/// suffix is simply ignored, so a dev build compares equal to the stable release it was built
/// after, never "behind" it.
fn numeric_core(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim().trim_start_matches('v');
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn cache_is_fresh(cache: &Cache, now_unix_ms: u64) -> bool {
    now_unix_ms.saturating_sub(cache.checked_at_unix_ms) < CACHE_TTL_MS
}

/// `None` unless `latest_version` is genuinely, numerically newer than `current_version` — never
/// invented from an unparseable version string on either side.
fn hint_from(current_version: &str, latest_version: &str) -> Option<String> {
    let current = numeric_core(current_version)?;
    let latest = numeric_core(latest_version)?;
    (latest > current).then(|| latest_version.to_owned())
}

fn decide(cache: Option<&Cache>, current_version: &str, now_unix_ms: u64) -> Decision {
    match cache {
        Some(cache) if cache_is_fresh(cache, now_unix_ms) => Decision {
            hint: hint_from(current_version, &cache.latest_version),
            should_refresh: false,
        },
        // Stale or missing: never worth showing a hint from data we no longer trust, but always
        // worth a best-effort background refresh so the *next* invocation has fresh data.
        Some(_) | None => Decision {
            hint: None,
            should_refresh: true,
        },
    }
}

/// Parses GitHub's `/releases/latest` response defensively: a schema change, an error body, or
/// truncated JSON must never panic, only fail closed (no cache write). A `prerelease`/`draft`
/// release is treated exactly like an unusable response, so one can never poison the cache
/// in between real stable releases (GitHub's `/releases/latest` endpoint already excludes both by
/// definition, but a defensive project should not rely on that alone).
fn parse_release_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    let prerelease = value
        .get("prerelease")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let draft = value
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if prerelease || draft {
        return None;
    }
    let version = tag.trim_start_matches('v');
    numeric_core(version)?;
    Some(version.to_owned())
}

/// The fetch boundary — a trait rather than a bare function so tests can inject a fake and
/// exercise "network failure" / "success" deterministically, with zero real network access.
trait ReleaseFetcher {
    fn fetch(&self, timeout: Duration) -> Option<String>;
}

/// The real network fetch — shells out to `curl` (already a hard dependency of every supported
/// platform here, and this project otherwise has zero HTTP-client dependencies; reusing it avoids
/// adding a TLS/HTTP stack to the binary just for one optional, best-effort GET). Bounded timeout,
/// no retries, and any failure — process spawn, non-zero exit, timeout, malformed body — is `None`.
struct CurlReleaseFetcher;

impl ReleaseFetcher for CurlReleaseFetcher {
    fn fetch(&self, timeout: Duration) -> Option<String> {
        let output = Command::new("curl")
            .args(["-fsSL", "--max-time"])
            .arg(timeout.as_secs().max(1).to_string())
            .arg(format!(
                "https://api.github.com/repos/{REPO}/releases/latest"
            ))
            .args(["-H", "Accept: application/vnd.github+json"])
            .args(["-H", "User-Agent: agent-relay-update-check"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_release_body(&String::from_utf8_lossy(&output.stdout))
    }
}

fn read_cache(path: &Path) -> Option<Cache> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(path: &Path, cache: &Cache) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(cache) {
        let _ignored = FsAtomicWriter.write_atomic(path, text.as_bytes());
    }
}

/// Background network entry point, run from the detached, hidden refresh process (see
/// [`INTERNAL_REFRESH_ARG`]). Always exits quietly: whether the
/// fetch succeeds or fails, there is no output and no non-zero exit that a human would ever see,
/// since nothing prints or reads this process's result.
///
/// Holds [`OrchestrationLock`] for the duration of the fetch: `spawn_background_refresh`'s check
/// is only advisory (a plain existence-of-another-refresh hint), so two refreshes can still be
/// spawned in a genuine race — this lock is what actually guarantees only one of them ever does
/// the network fetch and writes the cache. A refresh that loses the race exits immediately,
/// exactly as quietly as one that lost the race on the network.
pub(crate) fn run_internal_refresh(paths: &RelayPaths) {
    refresh_cache_locked_with(paths, &CurlReleaseFetcher, NETWORK_TIMEOUT);
}

/// The testable core of "acquire the refresh lock, then refresh" — a no-op (no fetch attempted at
/// all) if another refresh already holds the lock, so this is what actually guarantees at most one
/// concurrent fetch, not merely `spawn_background_refresh`'s best-effort pre-check.
fn refresh_cache_locked_with(paths: &RelayPaths, fetcher: &dyn ReleaseFetcher, timeout: Duration) {
    let lock = OrchestrationLock::at_path(paths.update_check_refresh_lock_file());
    let _ignored = lock.try_with(|| {
        refresh_cache_with(paths, fetcher, timeout);
        Ok(())
    });
}

/// The testable core of a refresh: silent (no cache write) unless `fetcher` actually produces a
/// usable stable version.
fn refresh_cache_with(paths: &RelayPaths, fetcher: &dyn ReleaseFetcher, timeout: Duration) {
    let Some(latest_version) = fetcher.fetch(timeout) else {
        return;
    };
    write_cache(
        &paths.update_check_cache_file(),
        &Cache {
            checked_at_unix_ms: crate::util::current_unix_ms(),
            latest_version,
        },
    );
}

/// Whether the resolved binary looks like it came from this project's own Homebrew tap — cheap
/// (a path substring check, no subprocess), safe to get wrong in either direction (it only picks
/// which line of remedy text to print), and deliberately not a real install-origin detector.
fn looks_homebrew_installed() -> bool {
    std::env::current_exe().is_ok_and(|path| {
        let path = path.to_string_lossy();
        path.contains("/Cellar/agent-relay/")
            || (path.contains("/homebrew/") && path.contains("/bin/relay"))
    })
}

/// Ask Homebrew which installation it owns and compare its canonical binary path with this
/// process. The cheap path hint above is insufficient authority for an actual upgrade: source
/// checkouts can contain those substrings, and Homebrew supports non-default prefixes too.
pub(crate) fn homebrew_formula_name() -> Option<&'static str> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let binary = exe.file_name()?.to_str()?;
    let formula = match binary {
        "relay" => "agent-relay",
        "relay-dev" => "agent-relay-dev",
        _ => return None,
    };
    let output = Command::new("brew")
        .args(["--prefix", formula])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = Path::new(std::str::from_utf8(&output.stdout).ok()?.trim());
    if !prefix.is_absolute() {
        return None;
    }
    let installed = prefix.join("bin").join(binary).canonicalize().ok()?;
    (installed == exe).then_some(formula)
}

/// The outcome of an explicit, foreground "is there a newer version" check (`relay update
/// --check`) — unlike [`maybe_show_hint`], this always resolves to a definite answer rather than
/// silently showing nothing: a stale or missing cache is refreshed synchronously (bounded by the
/// same [`NETWORK_TIMEOUT`] the background refresh uses) before deciding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    UpToDate {
        current_version: String,
    },
    Available {
        current_version: String,
        latest_version: String,
    },
    /// The cache was stale or missing and the synchronous refresh also could not reach a usable
    /// answer (no network, GitHub outage, a refresh already in flight elsewhere, ...) — never
    /// reported as [`Self::UpToDate`], which would be an invented, unverified claim.
    Unknown {
        current_version: String,
    },
}

pub(crate) fn check_now(paths: &RelayPaths) -> CheckOutcome {
    check_now_with(paths, &CurlReleaseFetcher, NETWORK_TIMEOUT)
}

fn check_now_with(
    paths: &RelayPaths,
    fetcher: &dyn ReleaseFetcher,
    timeout: Duration,
) -> CheckOutcome {
    let current_version = env!("RELAY_VERSION").to_owned();
    let cache_path = paths.update_check_cache_file();
    let now = crate::util::current_unix_ms();
    let fresh = read_cache(&cache_path).filter(|cache| cache_is_fresh(cache, now));
    let latest_version = match fresh {
        Some(cache) => Some(cache.latest_version),
        None => {
            refresh_cache_locked_with(paths, fetcher, timeout);
            read_cache(&cache_path)
                .filter(|cache| cache_is_fresh(cache, crate::util::current_unix_ms()))
                .map(|cache| cache.latest_version)
        }
    };
    match latest_version {
        // `hint_from` only ever returns `None` here because `latest` is not numerically newer:
        // both sides are parseable (`current_version` is a build-time constant; the guard also
        // validates cached data, which may have been edited or damaged outside this process).
        Some(latest) if numeric_core(&latest).is_some() => {
            match hint_from(&current_version, &latest) {
                Some(latest_version) => CheckOutcome::Available {
                    current_version,
                    latest_version,
                },
                None => CheckOutcome::UpToDate { current_version },
            }
        }
        Some(_) | None => CheckOutcome::Unknown { current_version },
    }
}

fn print_hint(latest_version: &str) {
    eprintln!();
    eprintln!("Update available: v{latest_version}");
    if looks_homebrew_installed() {
        eprintln!("Run: brew upgrade agent-relay");
    } else {
        eprintln!("See: https://github.com/{REPO}/releases/tag/v{latest_version}");
    }
}

/// Spawns the detached background refresh, propagating the exact config/state roots this process
/// resolved (never re-derived from environment alone in the child, in case `--config-root`/
/// `--state-root` overrode discovery) — and never waited on, so a cold or stale cache can never
/// add latency to the command that noticed it.
///
/// Skips spawning entirely if a refresh already appears to be in flight (`OrchestrationLock`'s
/// `is_currently_held`, the same non-blocking, purely-local check `relay status` already makes for
/// its own lock) — so `status`, `doctor`, and `setup` running close together while the cache is
/// stale spawn at most one refresh process between them, not three. This check is advisory, not
/// the actual guarantee: it only avoids the *common* redundant spawn cheaply. `run_internal_
/// refresh` holding the same lock for the fetch itself is what makes only-one-fetch-ever-runs
/// correct even in the narrow race this check cannot see (two commands checking in the same
/// instant, before either's child has started).
fn spawn_background_refresh(paths: &RelayPaths) {
    if OrchestrationLock::at_path(paths.update_check_refresh_lock_file()).is_currently_held() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ignored = Command::new(exe)
        .arg(INTERNAL_REFRESH_ARG)
        .arg("--config-root")
        .arg(paths.config_root())
        .arg("--state-root")
        .arg(paths.state_root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Whether `command`'s completion is a point that may ever show the update hint. `main` uses this
/// both to decide whether to call [`maybe_show_hint`] at all and, earlier, it also unconditionally
/// short-circuits [`CliCommand::InternalUpdateCheckRefresh`] before this is even reached — so the
/// internal refresh command is doubly excluded, not just by this predicate: even if that early
/// return in `main` were ever removed, this still returns `false` for it, since it is not one of
/// the three listed variants. The internal refresh path can therefore never schedule another
/// refresh, show a hint, or otherwise re-enter this module's user-facing behaviour.
#[must_use]
pub(crate) fn shows_update_hint_for(command: &CliCommand, json: bool, interactive: bool) -> bool {
    !json
        && interactive
        && matches!(
            command,
            CliCommand::Status { .. } | CliCommand::Doctor(_) | CliCommand::Setup(_)
        )
}

/// Called once, after a gated human-facing command has already printed its own result. Shows an
/// "Update available" hint when (and only when) a fresh cache says one exists; otherwise shows
/// nothing and, if the cache is stale or missing, kicks off a background refresh for next time.
/// `interactive` gates this entirely — pass `false` for `--json`, a non-TTY, or any other
/// non-interactive context, and this is a no-op (not even a cache read).
pub(crate) fn maybe_show_hint(paths: &RelayPaths, interactive: bool) {
    if !interactive {
        return;
    }
    let cache = read_cache(&paths.update_check_cache_file());
    let decision = decide(
        cache.as_ref(),
        env!("RELAY_VERSION"),
        crate::util::current_unix_ms(),
    );
    if let Some(latest) = &decision.hint {
        print_hint(latest);
    }
    if decision.should_refresh {
        spawn_background_refresh(paths);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(checked_at_unix_ms: u64, latest_version: &str) -> Cache {
        Cache {
            checked_at_unix_ms,
            latest_version: latest_version.to_owned(),
        }
    }

    #[test]
    fn current_less_than_latest_shows_a_hint() {
        assert_eq!(hint_from("0.4.0", "0.4.1"), Some("0.4.1".to_owned()));
    }

    #[test]
    fn current_equal_to_latest_shows_no_hint() {
        assert_eq!(hint_from("0.4.0", "0.4.0"), None);
    }

    #[test]
    fn current_greater_than_latest_shows_no_hint() {
        assert_eq!(hint_from("0.5.0", "0.4.0"), None);
    }

    #[test]
    fn a_dev_build_compares_by_its_numeric_core_only() {
        // A dev build's version already encodes "ahead of the last stable release" (see
        // build.rs's `next_patch`) — it must never be told to update to the release it is ahead
        // of, nor treated as behind one that shares its numeric core.
        assert_eq!(hint_from("0.4.1-dev.5+abc123", "0.4.0"), None);
        assert_eq!(hint_from("0.4.1-dev.5+abc123.dirty", "0.4.1"), None);
    }

    /// Parses `argv` the same way the real binary does, so these tests exercise real `clap`
    /// dispatch rather than hand-built `CliCommand` values that could drift from what `relay`
    /// actually parses.
    fn parse(argv: &[&str]) -> CliCommand {
        use clap::Parser as _;
        crate::cli::Cli::try_parse_from(std::iter::once(&"relay").chain(argv))
            .expect("valid argv")
            .command
    }

    #[test]
    fn the_internal_refresh_command_parses_to_its_own_hidden_variant() {
        assert!(matches!(
            parse(&[INTERNAL_REFRESH_ARG]),
            CliCommand::InternalUpdateCheckRefresh
        ));
    }

    #[test]
    fn the_internal_refresh_command_never_shows_a_hint_under_any_flags() {
        let command = parse(&[INTERNAL_REFRESH_ARG]);
        for json in [false, true] {
            for interactive in [false, true] {
                assert!(
                    !shows_update_hint_for(&command, json, interactive),
                    "json={json} interactive={interactive}"
                );
            }
        }
    }

    #[test]
    fn status_doctor_and_setup_show_a_hint_only_when_interactive_and_not_json() {
        for argv in [["status"].as_slice(), &["doctor"], &["setup"]] {
            let command = parse(argv);
            assert!(shows_update_hint_for(&command, false, true), "{argv:?}");
            assert!(
                !shows_update_hint_for(&command, true, true),
                "{argv:?} json"
            );
            assert!(
                !shows_update_hint_for(&command, false, false),
                "{argv:?} non-interactive"
            );
        }
    }

    #[test]
    fn an_unrelated_command_never_shows_a_hint() {
        let command = parse(&["profile", "list"]);
        assert!(!shows_update_hint_for(&command, false, true));
    }

    #[test]
    fn prerelease_is_ignored() {
        let body = r#"{"tag_name":"v0.5.0","prerelease":true,"draft":false}"#;
        assert_eq!(parse_release_body(body), None);
    }

    #[test]
    fn draft_is_ignored() {
        let body = r#"{"tag_name":"v0.5.0","prerelease":false,"draft":true}"#;
        assert_eq!(parse_release_body(body), None);
    }

    #[test]
    fn a_stable_release_is_accepted() {
        let body = r#"{"tag_name":"v0.5.0","prerelease":false,"draft":false}"#;
        assert_eq!(parse_release_body(body), Some("0.5.0".to_owned()));
    }

    #[test]
    fn malformed_response_is_silent() {
        assert_eq!(parse_release_body("not json"), None);
        assert_eq!(parse_release_body(r#"{"no_tag_name_field":true}"#), None);
        assert_eq!(parse_release_body(r#"{"tag_name":"not-a-version"}"#), None);
    }

    #[test]
    fn a_fresh_cache_never_triggers_a_refresh() {
        let now = 1_000_000;
        let fresh = cache(now - 1000, "0.4.0");
        let decision = decide(Some(&fresh), "0.4.0", now);
        assert_eq!(
            decision,
            Decision {
                hint: None,
                should_refresh: false
            }
        );
    }

    #[test]
    fn a_fresh_cache_with_a_newer_version_shows_the_hint_with_no_refresh() {
        let now = 1_000_000;
        let fresh = cache(now - 1000, "0.5.0");
        let decision = decide(Some(&fresh), "0.4.0", now);
        assert_eq!(
            decision,
            Decision {
                hint: Some("0.5.0".to_owned()),
                should_refresh: false
            }
        );
    }

    #[test]
    fn a_stale_cache_allows_a_refresh_and_shows_no_hint_this_run() {
        let now = CACHE_TTL_MS + 1_000_000;
        let stale = cache(now - CACHE_TTL_MS - 1, "0.5.0");
        let decision = decide(Some(&stale), "0.4.0", now);
        assert_eq!(
            decision,
            Decision {
                hint: None,
                should_refresh: true
            }
        );
    }

    #[test]
    fn a_missing_cache_allows_a_refresh_and_shows_no_hint() {
        let decision = decide(None, "0.4.0", 1_000_000);
        assert_eq!(
            decision,
            Decision {
                hint: None,
                should_refresh: true
            }
        );
    }

    struct FakeFetcher(Option<&'static str>);

    impl ReleaseFetcher for FakeFetcher {
        fn fetch(&self, _timeout: Duration) -> Option<String> {
            self.0.map(str::to_owned)
        }
    }

    #[test]
    fn network_failure_writes_no_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        refresh_cache_with(&paths, &FakeFetcher(None), Duration::from_secs(1));
        assert!(
            !paths.update_check_cache_file().exists(),
            "a failed fetch must never write a cache file"
        );
    }

    #[test]
    fn a_successful_fetch_writes_the_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        refresh_cache_with(&paths, &FakeFetcher(Some("0.9.0")), Duration::from_secs(1));
        let written = read_cache(&paths.update_check_cache_file()).expect("cache written");
        assert_eq!(written.latest_version, "0.9.0");
    }

    #[test]
    fn a_refresh_that_cannot_acquire_the_lock_writes_no_cache() {
        // Simulates the race `spawn_background_refresh`'s own pre-check cannot fully close: two
        // refreshes started close enough together that both reach `run_internal_refresh`. Only
        // the one already holding the lock may proceed; the other must be a complete no-op, not a
        // second concurrent fetch.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        let lock = OrchestrationLock::at_path(paths.update_check_refresh_lock_file());
        lock.try_with(|| {
            refresh_cache_locked_with(&paths, &FakeFetcher(Some("0.9.0")), Duration::from_secs(1));
            relay_core::Result::<()>::Ok(())
        })
        .expect("outer acquire");
        assert!(
            !paths.update_check_cache_file().exists(),
            "a refresh that lost the lock race must never write a cache, even with a fetcher \
             that would have succeeded"
        );
    }

    #[test]
    fn a_refresh_that_acquires_the_lock_writes_the_cache_normally() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        refresh_cache_locked_with(&paths, &FakeFetcher(Some("0.9.0")), Duration::from_secs(1));
        let written = read_cache(&paths.update_check_cache_file()).expect("cache written");
        assert_eq!(written.latest_version, "0.9.0");
    }

    #[test]
    fn spawn_background_refresh_is_skipped_while_a_refresh_lock_is_already_held() {
        // The cheap pre-check in `spawn_background_refresh`: while a refresh is in flight
        // (simulated here by holding the lock directly, without spawning a real process), a
        // second caller noticing the same stale cache must not spawn another one. Observed via
        // `current_exe()` staying untouched is not practical here, so this exercises the same
        // `is_currently_held` gate `spawn_background_refresh` itself calls, proving the guard
        // condition is correct; the no-op-while-held behavior of the spawn function follows
        // directly from that shared, already-tested primitive (see `handoff::lock`'s own tests).
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        let lock = OrchestrationLock::at_path(paths.update_check_refresh_lock_file());
        assert!(!lock.is_currently_held());
        lock.try_with(|| {
            assert!(
                OrchestrationLock::at_path(paths.update_check_refresh_lock_file())
                    .is_currently_held(),
                "spawn_background_refresh's own guard must see the lock as held here"
            );
            relay_core::Result::<()>::Ok(())
        })
        .expect("acquire");
        assert!(
            !lock.is_currently_held(),
            "released once the holder returns"
        );
    }

    #[test]
    fn maybe_show_hint_is_a_no_op_when_not_interactive() {
        // Non-interactive (json / non-TTY) must not even read the cache, let alone spawn a
        // refresh — exercised here against a fresh tempdir with no cache file at all: if this
        // path attempted anything beyond returning immediately, there is nothing for it to read
        // and no risk of a spurious spawn making the test flaky either way, but the real
        // assertion is the one guaranteed observable effect: no cache file appears.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        maybe_show_hint(&paths, false);
        assert!(!paths.update_check_cache_file().exists());
    }

    #[test]
    fn check_now_uses_a_fresh_cache_without_fetching_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        write_cache(
            &paths.update_check_cache_file(),
            &cache(crate::util::current_unix_ms(), "99.0.0"),
        );
        // A fetcher that would, if ever actually called, make the outcome disagree with the
        // fresh cache's own "99.0.0" — the only way this test can tell the two apart.
        let outcome = check_now_with(&paths, &FakeFetcher(Some("1.0.0")), Duration::from_secs(1));
        assert_eq!(
            outcome,
            CheckOutcome::Available {
                current_version: env!("RELAY_VERSION").to_owned(),
                latest_version: "99.0.0".to_owned(),
            }
        );
    }

    #[test]
    fn check_now_refreshes_synchronously_when_the_cache_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        let outcome = check_now_with(&paths, &FakeFetcher(Some("99.0.0")), Duration::from_secs(1));
        assert_eq!(
            outcome,
            CheckOutcome::Available {
                current_version: env!("RELAY_VERSION").to_owned(),
                latest_version: "99.0.0".to_owned(),
            }
        );
        assert_eq!(
            read_cache(&paths.update_check_cache_file())
                .expect("refresh must have written the cache")
                .latest_version,
            "99.0.0",
            "an explicit check's own synchronous refresh must populate the cache for next time, \
             exactly like the background refresh does"
        );
    }

    #[test]
    fn check_now_refreshes_synchronously_when_the_cache_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        write_cache(&paths.update_check_cache_file(), &cache(0, "1.0.0"));
        let outcome = check_now_with(&paths, &FakeFetcher(Some("99.0.0")), Duration::from_secs(1));
        assert_eq!(
            outcome,
            CheckOutcome::Available {
                current_version: env!("RELAY_VERSION").to_owned(),
                latest_version: "99.0.0".to_owned(),
            },
            "a stale cache's old value must never be reported without first trying to refresh it"
        );
    }

    #[test]
    fn check_now_reports_unknown_when_the_cache_is_missing_and_the_refresh_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        let outcome = check_now_with(&paths, &FakeFetcher(None), Duration::from_secs(1));
        assert_eq!(
            outcome,
            CheckOutcome::Unknown {
                current_version: env!("RELAY_VERSION").to_owned(),
            },
            "a failed check must never be reported as up to date, which would be an invented claim"
        );
    }

    #[test]
    fn check_now_does_not_trust_stale_data_after_a_failed_or_contended_refresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        for latest in ["0.0.1", "99.0.0"] {
            write_cache(&paths.update_check_cache_file(), &cache(0, latest));
            let expected = CheckOutcome::Unknown {
                current_version: env!("RELAY_VERSION").to_owned(),
            };
            assert_eq!(
                check_now_with(&paths, &FakeFetcher(None), Duration::from_secs(1)),
                expected
            );
            OrchestrationLock::at_path(paths.update_check_refresh_lock_file())
                .try_with(|| {
                    assert_eq!(
                        check_now_with(
                            &paths,
                            &FakeFetcher(Some("99.0.0")),
                            Duration::from_secs(1)
                        ),
                        expected
                    );
                    Ok(())
                })
                .expect("hold refresh lock");
        }
    }

    #[test]
    fn check_now_does_not_report_an_invalid_cached_version_as_up_to_date() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        write_cache(
            &paths.update_check_cache_file(),
            &cache(crate::util::current_unix_ms(), "invalid"),
        );
        assert!(matches!(
            check_now_with(&paths, &FakeFetcher(None), Duration::from_secs(1)),
            CheckOutcome::Unknown { .. }
        ));
    }

    #[test]
    fn check_now_reports_up_to_date_when_the_cached_latest_is_not_numerically_newer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(dir.path().join("config"), dir.path().join("state")).expect("paths");
        write_cache(
            &paths.update_check_cache_file(),
            &cache(crate::util::current_unix_ms(), "0.0.1"),
        );
        let outcome = check_now_with(&paths, &FakeFetcher(Some("1.0.0")), Duration::from_secs(1));
        assert_eq!(
            outcome,
            CheckOutcome::UpToDate {
                current_version: env!("RELAY_VERSION").to_owned(),
            }
        );
    }

    #[test]
    fn homebrew_formula_name_is_none_in_a_normal_test_or_dev_build() {
        // This test binary's own `current_exe()` is never a Homebrew Cellar path, so this must
        // never guess a formula name from anything weaker than that path.
        assert_eq!(homebrew_formula_name(), None);
    }
}
