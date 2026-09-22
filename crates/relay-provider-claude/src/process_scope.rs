//! Which running Claude processes could be a *conflicting writer* for one project.
//!
//! The question Relay must answer before it moves a conversation is not "is any Claude process
//! running under this profile?" — a profile's directory is shared by every project, background job
//! and provider helper that uses that account — but "is there anything besides the exact source
//! process Relay is orchestrating that could write to THIS project?".
//!
//! Every Claude process running under the profile is classified from the strongest evidence
//! available (exact pid + start-time fingerprint, then Claude's own session registry
//! `<config>/sessions/<pid>.json`, then the process's working directory).
//!
//! Relay's ownership unit is the *conversation* (a Relay Session), not the repository: other Claude
//! sessions working in the same project — under this profile or any other — are legitimate and never
//! block a handoff. What must never happen is a second live process for the SAME conversation:
//!
//! | role | meaning | blocks? |
//! |---|---|---|
//! | `ExpectedSource` | the exact recorded source process (pid *and* start time match) | no |
//! | `SameSessionHelper` | a descendant of the source process | no |
//! | `OtherSession` | a registered Claude session for a *different* conversation (any project) | no |
//! | `ProviderHelper` | an unregistered Claude helper (daemon, pty host, spare worker) that is provably not in this project | no |
//! | `ConflictingWriter` | another process serving the very conversation being moved | **yes** |
//! | `Unclassifiable` | contradictory or missing evidence about a process that might be the same conversation | **yes** |
//!
//! Anything that cannot be placed fails closed. A process that is not Claude at all (a shell, a
//! search tool that merely mentions the profile) is not a Claude writer and is not considered.

use std::path::{Path, PathBuf};

use relay_core::handoff::ProcessIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessRole {
    ExpectedSource,
    SameSessionHelper,
    OtherSession,
    ProviderHelper,
    ConflictingWriter,
    Unclassifiable,
}

impl ProcessRole {
    #[must_use]
    pub const fn blocks(self) -> bool {
        matches!(self, Self::ConflictingWriter | Self::Unclassifiable)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassifiedProcess {
    pub pid: u32,
    pub role: ProcessRole,
    /// The evidence behind the role, in words (never argv or environment contents).
    pub evidence: String,
}

/// One process as observed, before classification.
#[derive(Clone, Debug)]
pub struct RawProcess {
    pub pid: u32,
    pub ppid: u32,
    pub argv: Vec<String>,
    /// `CLAUDE_CONFIG_DIR` from the process environment.
    pub config_dir: Option<String>,
    /// `PWD` from the environment (advisory: stale after a `chdir`).
    pub pwd: Option<PathBuf>,
    /// The real working directory, when it could be read.
    pub cwd: Option<PathBuf>,
}

/// A live-session registry entry (`<config>/sessions/<pid>.json`) whose process is running.
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub pid: u32,
    pub session_id: Option<String>,
    /// Where the session says it works. Informational: ownership is per conversation, so another
    /// session's project no longer matters.
    #[allow(dead_code)]
    pub cwd: Option<PathBuf>,
}

/// What is being moved.
pub struct WriterScope<'a> {
    pub config_dir: &'a Path,
    pub project_dir: &'a Path,
    /// The session being handed off, when known.
    pub session_id: Option<&'a str>,
    /// The recorded source process, when known.
    pub expected: Option<&'a ProcessIdentity>,
    /// A `NativeDefault` process is a Claude process that never carries `CLAUDE_CONFIG_DIR` at
    /// all — matching on "the env var equals `config_dir`" would silently match nothing for it,
    /// which is a detection gap, not a safe default. This field is how `classify` tells the two
    /// apart; see [`relay_core::ClaudeConfigMode`].
    pub mode: relay_core::ClaudeConfigMode,
}

fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

/// A Claude Code process: the `claude` binary (including its versioned install path) or a
/// JavaScript runtime running Claude's own entry point.
#[must_use]
pub fn is_claude_process(argv: &[String]) -> bool {
    let Some(first) = argv.first() else {
        return false;
    };
    if basename(first) == "claude" || first.contains("/claude/versions/") {
        return true;
    }
    matches!(basename(first), "node" | "bun")
        && argv
            .get(1)
            .is_some_and(|entry| entry.contains("claude-code") || basename(entry) == "claude")
}

/// Pure classification of every Claude process under `scope.config_dir`.
///
/// `fingerprint_matches` answers whether `pid` is still the recorded source process (exact start
/// time); `None` means it could not be established, which is never treated as a match.
#[must_use]
pub fn classify(
    processes: &[RawProcess],
    registry: &[RegistryEntry],
    scope: &WriterScope<'_>,
    fingerprint_matches: &dyn Fn(u32) -> Option<bool>,
) -> Vec<ClassifiedProcess> {
    let config = scope.config_dir.to_string_lossy();
    let project = scope.project_dir;
    let source_pid = scope
        .expected
        .map(|expected| expected.pid)
        .filter(|pid| *pid != 0);
    let source_is_confirmed = source_pid.is_some_and(|pid| fingerprint_matches(pid) == Some(true));
    let descends_from_source = |mut pid: u32| {
        // Bounded walk up the parent chain (a chain longer than the table is a cycle).
        for _ in 0..processes.len() {
            let Some(parent) = processes
                .iter()
                .find(|candidate| candidate.pid == pid)
                .map(|candidate| candidate.ppid)
            else {
                return false;
            };
            if source_is_confirmed && Some(parent) == source_pid {
                return true;
            }
            pid = parent;
        }
        false
    };

    let mut result = Vec::new();
    for process in processes {
        let same_profile = match scope.mode {
            relay_core::ClaudeConfigMode::Explicit => {
                process.config_dir.as_deref() == Some(config.as_ref())
            }
            relay_core::ClaudeConfigMode::NativeDefault => process.config_dir.is_none(),
        };
        if !same_profile || !is_claude_process(&process.argv) {
            continue;
        }
        let (role, evidence) = classify_one(
            process,
            registry,
            scope,
            project,
            source_pid,
            source_is_confirmed,
            fingerprint_matches,
            &descends_from_source,
        );
        result.push(ClassifiedProcess {
            pid: process.pid,
            role,
            evidence,
        });
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn classify_one(
    process: &RawProcess,
    registry: &[RegistryEntry],
    scope: &WriterScope<'_>,
    project: &Path,
    source_pid: Option<u32>,
    source_is_confirmed: bool,
    fingerprint_matches: &dyn Fn(u32) -> Option<bool>,
    descends_from_source: &dyn Fn(u32) -> bool,
) -> (ProcessRole, String) {
    if Some(process.pid) == source_pid {
        return match fingerprint_matches(process.pid) {
            Some(true) => (
                ProcessRole::ExpectedSource,
                "the recorded source process (pid and start time match)".to_owned(),
            ),
            // A recycled pid is just another process: classify it on its own evidence below.
            Some(false) => classify_by_location(process, registry, scope, project),
            None => (
                ProcessRole::Unclassifiable,
                "the recorded source pid is running but its identity could not be confirmed"
                    .to_owned(),
            ),
        };
    }
    if source_is_confirmed && descends_from_source(process.pid) {
        return (
            ProcessRole::SameSessionHelper,
            "a descendant of the source process".to_owned(),
        );
    }
    classify_by_location(process, registry, scope, project)
}

fn classify_by_location(
    process: &RawProcess,
    registry: &[RegistryEntry],
    scope: &WriterScope<'_>,
    project: &Path,
) -> (ProcessRole, String) {
    let registered = registry.iter().find(|entry| entry.pid == process.pid);
    // The real working directory beats the advisory `PWD`.
    let observed = process.cwd.as_deref().or(process.pwd.as_deref());
    if let Some(entry) = registered {
        if scope.session_id.is_some() && entry.session_id.as_deref() == scope.session_id {
            return (
                ProcessRole::ConflictingWriter,
                "another process serving the very conversation being moved".to_owned(),
            );
        }
        return (
            ProcessRole::OtherSession,
            "a registered Claude session for a different conversation".to_owned(),
        );
    }
    // Unregistered: infrastructure (daemon, pty host, spare worker) has no session of its own.
    // It is only ruled out when it is provably not working inside this project.
    match observed {
        Some(cwd) if cwd.starts_with(project) => (
            ProcessRole::Unclassifiable,
            "an unregistered Claude process working inside this project".to_owned(),
        ),
        Some(_) => (
            ProcessRole::ProviderHelper,
            "an unregistered Claude helper working outside this project".to_owned(),
        ),
        None => (
            ProcessRole::Unclassifiable,
            "an unregistered Claude process whose working directory could not be read".to_owned(),
        ),
    }
}

/// The subset that must stop a handoff.
#[must_use]
pub fn blockers(classified: Vec<ClassifiedProcess>) -> Vec<ClassifiedProcess> {
    classified
        .into_iter()
        .filter(|process| process.role.blocks())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "/profiles/erika/claude";

    fn proc(pid: u32, ppid: u32, argv: &[&str], cwd: Option<&str>) -> RawProcess {
        RawProcess {
            pid,
            ppid,
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            config_dir: Some(CONFIG.to_owned()),
            pwd: cwd.map(PathBuf::from),
            cwd: cwd.map(PathBuf::from),
        }
    }

    fn entry(pid: u32, session: &str, cwd: &str) -> RegistryEntry {
        RegistryEntry {
            pid,
            session_id: Some(session.to_owned()),
            cwd: Some(PathBuf::from(cwd)),
        }
    }

    fn roles(
        processes: &[RawProcess],
        registry: &[RegistryEntry],
        expected: Option<&ProcessIdentity>,
        matches: Option<bool>,
    ) -> Vec<(u32, ProcessRole)> {
        roles_with_mode(
            processes,
            registry,
            expected,
            matches,
            relay_core::ClaudeConfigMode::Explicit,
        )
    }

    fn roles_with_mode(
        processes: &[RawProcess],
        registry: &[RegistryEntry],
        expected: Option<&ProcessIdentity>,
        matches: Option<bool>,
        mode: relay_core::ClaudeConfigMode,
    ) -> Vec<(u32, ProcessRole)> {
        let scope = WriterScope {
            config_dir: Path::new(CONFIG),
            project_dir: Path::new("/work/repo-b"),
            session_id: Some("S1"),
            expected,
            mode,
        };
        classify(processes, registry, &scope, &|_| matches)
            .into_iter()
            .map(|process| (process.pid, process.role))
            .collect()
    }

    fn source() -> ProcessIdentity {
        ProcessIdentity {
            pid: 100,
            start_time_fingerprint: Some("t".to_owned()),
        }
    }

    #[test]
    fn the_exact_source_and_its_descendants_are_not_conflicts() {
        let table = [
            proc(100, 1, &["claude"], Some("/work/repo-b")),
            proc(101, 100, &["claude", "--subagent"], Some("/work/repo-b")),
        ];
        let registry = [entry(100, "S1", "/work/repo-b")];
        let got = roles(&table, &registry, Some(&source()), Some(true));
        assert_eq!(
            got,
            vec![
                (100, ProcessRole::ExpectedSource),
                (101, ProcessRole::SameSessionHelper)
            ]
        );
    }

    #[test]
    fn other_conversations_never_block_wherever_they_run() {
        // Another project, and the SAME project, on the same profile: different conversations.
        let table = [
            proc(200, 1, &["claude"], Some("/work/repo-a")),
            proc(201, 1, &["claude"], Some("/work/repo-b")),
            proc(202, 1, &["claude"], Some("/work")),
        ];
        let registry = [
            entry(200, "OTHER-A", "/work/repo-a"),
            entry(201, "OTHER-B", "/work/repo-b"),
            entry(202, "OTHER-C", "/work"),
        ];
        assert_eq!(
            roles(&table, &registry, Some(&source()), Some(true)),
            vec![
                (200, ProcessRole::OtherSession),
                (201, ProcessRole::OtherSession),
                (202, ProcessRole::OtherSession)
            ]
        );
    }

    #[test]
    fn provider_infrastructure_outside_the_project_does_not_block_but_inside_it_does() {
        let daemon = proc(300, 1, &["/x/claude", "daemon", "run"], Some("/home/me"));
        let spare_elsewhere = proc(301, 300, &["claude", "bg-spare"], Some("/work/repo-a"));
        let spare_inside = proc(302, 300, &["claude", "bg-spare"], Some("/work/repo-b/sub"));
        let got = roles(&[daemon, spare_elsewhere, spare_inside], &[], None, None);
        assert_eq!(
            got,
            vec![
                (300, ProcessRole::ProviderHelper),
                (301, ProcessRole::ProviderHelper),
                (302, ProcessRole::Unclassifiable)
            ]
        );
    }

    #[test]
    fn a_second_process_for_the_same_conversation_blocks() {
        // pid 100 is the recorded source of S1; pid 400 is ANOTHER process serving S1.
        let table = [
            proc(100, 1, &["claude"], Some("/work/repo-b")),
            proc(400, 1, &["claude"], Some("/work/repo-b")),
        ];
        let registry = [
            entry(100, "S1", "/work/repo-b"),
            entry(400, "S1", "/work/repo-b"),
        ];
        let got = roles(&table, &registry, Some(&source()), Some(true));
        assert_eq!(got[0], (100, ProcessRole::ExpectedSource));
        assert_eq!(got[1], (400, ProcessRole::ConflictingWriter));
    }

    /// M4: a native-default Claude process never carries `CLAUDE_CONFIG_DIR` at all (see
    /// `relay_provider_claude::config_mode`'s doc comment) — `classify()` must match same-profile
    /// under `NativeDefault` by the ABSENCE of `config_dir`, not by a specific value, or every
    /// native-default writer would be silently invisible to conflict detection. A process that
    /// DOES carry an explicit `CLAUDE_CONFIG_DIR` (an isolated profile) must never be conflated
    /// with the native-default source even if it resumes the identical session id.
    #[test]
    fn native_default_mode_matches_processes_with_no_config_dir_and_ignores_explicit_ones() {
        let native_default = RawProcess {
            pid: 100,
            ppid: 1,
            argv: vec!["claude".to_owned()],
            config_dir: None,
            pwd: Some(PathBuf::from("/work/repo-b")),
            cwd: Some(PathBuf::from("/work/repo-b")),
        };
        let another_native_default_writer = RawProcess {
            pid: 401,
            ppid: 1,
            argv: vec!["claude".to_owned()],
            config_dir: None,
            pwd: Some(PathBuf::from("/work/repo-b")),
            cwd: Some(PathBuf::from("/work/repo-b")),
        };
        // An explicit isolated profile, coincidentally resuming the same session id: never the
        // same writer as the native-default one, regardless of matching session.
        let explicit_profile = proc(402, 1, &["claude"], Some("/work/repo-b"));
        let registry = [
            entry(100, "S1", "/work/repo-b"),
            entry(401, "S1", "/work/repo-b"),
            entry(402, "S1", "/work/repo-b"),
        ];
        let got = roles_with_mode(
            &[
                native_default,
                another_native_default_writer,
                explicit_profile,
            ],
            &registry,
            Some(&source()),
            Some(true),
            relay_core::ClaudeConfigMode::NativeDefault,
        );
        assert_eq!(got[0], (100, ProcessRole::ExpectedSource));
        assert_eq!(got[1], (401, ProcessRole::ConflictingWriter));
        // The explicit-profile process is not classified as this scope's writer at all: it is
        // filtered out before role assignment (same-profile check fails), so it never appears in
        // the classified list.
        assert!(got.iter().all(|(pid, _)| *pid != 402));
    }

    #[test]
    fn ambiguous_evidence_about_the_source_or_an_unregistered_process_fails_closed() {
        // Unregistered, working directory unreadable.
        let mut unreadable = proc(500, 1, &["claude"], None);
        unreadable.cwd = None;
        unreadable.pwd = None;
        assert_eq!(
            roles(&[unreadable], &[], None, None),
            vec![(500, ProcessRole::Unclassifiable)]
        );
        // The recorded pid is running but its identity cannot be established.
        let unsure = proc(100, 1, &["claude"], Some("/work/repo-b"));
        assert_eq!(
            roles(
                &[unsure],
                &[entry(100, "S1", "/work/repo-b")],
                Some(&source()),
                None
            ),
            vec![(100, ProcessRole::Unclassifiable)]
        );
    }

    #[test]
    fn a_stale_recorded_pid_never_shields_a_live_process_for_the_same_conversation() {
        // The recorded source is gone; a different live process still serves S1.
        let table = [proc(600, 1, &["claude"], Some("/work/repo-b"))];
        let registry = [entry(600, "S1", "/work/repo-b")];
        let got = roles(&table, &registry, Some(&source()), Some(false));
        assert_eq!(got, vec![(600, ProcessRole::ConflictingWriter)]);
    }

    #[test]
    fn a_recycled_pid_is_not_trusted_as_the_source() {
        // pid 100 now serves S1 but its start time differs from the recorded source's: it is
        // NOT the expected source, so it is a second process for the conversation.
        let table = [proc(100, 1, &["claude"], Some("/work/repo-b"))];
        let registry = [entry(100, "S1", "/work/repo-b")];
        assert_eq!(
            roles(&table, &registry, Some(&source()), Some(false)),
            vec![(100, ProcessRole::ConflictingWriter)]
        );
        // If it serves some OTHER conversation it is merely another session.
        assert_eq!(
            roles(
                &table,
                &[entry(100, "OTHER", "/work/repo-b")],
                Some(&source()),
                Some(false)
            ),
            vec![(100, ProcessRole::OtherSession)]
        );
        // …and its child is nobody's helper (the source was never confirmed).
        let child = proc(101, 100, &["claude"], Some("/work/repo-b"));
        let got = roles(
            &[table[0].clone(), child],
            &[
                entry(100, "S1", "/work/repo-b"),
                entry(101, "S1", "/work/repo-b"),
            ],
            Some(&source()),
            Some(false),
        );
        assert!(got.iter().all(|(_, role)| role.blocks()));
    }

    #[test]
    fn processes_that_are_not_claude_or_use_another_profile_are_ignored() {
        let search = proc(
            700,
            1,
            &["ugrep", "CLAUDE_CONFIG_DIR=/profiles/erika/claude"],
            Some("/work/repo-b"),
        );
        let mut other_profile = proc(701, 1, &["claude"], Some("/work/repo-b"));
        other_profile.config_dir = Some("/profiles/megan/claude".to_owned());
        assert!(roles(&[search, other_profile], &[], None, None).is_empty());
        assert!(is_claude_process(&[
            "/u/.local/share/claude/versions/2.1.278".to_owned(),
            "attach".to_owned()
        ]));
        assert!(is_claude_process(&[
            "node".to_owned(),
            "/x/claude-code/cli.js".to_owned()
        ]));
        assert!(!is_claude_process(&["bash".to_owned()]));
    }
}
