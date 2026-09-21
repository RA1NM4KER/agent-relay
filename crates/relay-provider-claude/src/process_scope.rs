//! Which running Claude processes could be a *conflicting writer* for one project.
//!
//! The question Relay must answer before it moves a conversation is not "is any Claude process
//! running under this profile?" — a profile's directory is shared by every project, background job
//! and provider helper that uses that account — but "is there anything besides the exact source
//! process Relay is orchestrating that could write to THIS project?".
//!
//! Every Claude process running under the profile is classified from the strongest evidence
//! available (exact pid + start-time fingerprint, then Claude's own session registry
//! `<config>/sessions/<pid>.json`, then the process's working directory):
//!
//! | role | meaning | blocks? |
//! |---|---|---|
//! | `ExpectedSource` | the exact recorded source process (pid *and* start time match) | no |
//! | `SameSessionHelper` | a descendant of the source, or a process serving the very session being moved | no |
//! | `OtherProject` | a registered Claude session whose project cannot include this one | no |
//! | `ProviderHelper` | an unregistered Claude helper (daemon, pty host, spare worker) that is provably not in this project | no |
//! | `ConflictingWriter` | a registered session in this project (or one that contains it), not the source | **yes** |
//! | `Unclassifiable` | contradictory or missing evidence about a process that might be in this project | **yes** |
//!
//! Anything that cannot be placed fails closed. A process that is not Claude at all (a shell, a
//! search tool that merely mentions the profile) is not a Claude writer and is not considered.

use std::path::{Path, PathBuf};

use relay_core::handoff::ProcessIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessRole {
    ExpectedSource,
    SameSessionHelper,
    OtherProject,
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

fn same_or_nested(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
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
        if process.config_dir.as_deref() != Some(config.as_ref())
            || !is_claude_process(&process.argv)
        {
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
                ProcessRole::SameSessionHelper,
                "a process serving the session being moved".to_owned(),
            );
        }
        let Some(session_cwd) = entry.cwd.as_deref() else {
            return (
                ProcessRole::Unclassifiable,
                "a registered Claude session with no recorded project".to_owned(),
            );
        };
        if same_or_nested(session_cwd, project) {
            return (
                ProcessRole::ConflictingWriter,
                "a registered Claude session in this project".to_owned(),
            );
        }
        // Registry and reality disagree about where it works: do not trust either.
        if process
            .cwd
            .as_deref()
            .is_some_and(|cwd| same_or_nested(cwd, project))
        {
            return (
                ProcessRole::Unclassifiable,
                "its registry entry names another project but it is working in this one".to_owned(),
            );
        }
        return (
            ProcessRole::OtherProject,
            "a registered Claude session in a different project".to_owned(),
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
        let scope = WriterScope {
            config_dir: Path::new(CONFIG),
            project_dir: Path::new("/work/repo-b"),
            session_id: Some("S1"),
            expected,
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
    fn a_session_in_another_project_on_the_same_profile_does_not_block() {
        let table = [proc(200, 1, &["claude"], Some("/work/repo-a"))];
        let registry = [entry(200, "OTHER", "/work/repo-a")];
        assert_eq!(
            roles(&table, &registry, Some(&source()), Some(true)),
            vec![(200, ProcessRole::OtherProject)]
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
    fn a_second_interactive_session_in_the_same_project_blocks() {
        let table = [
            proc(100, 1, &["claude"], Some("/work/repo-b")),
            proc(400, 1, &["claude"], Some("/work/repo-b")),
        ];
        let registry = [
            entry(100, "S1", "/work/repo-b"),
            entry(400, "S2", "/work/repo-b"),
        ];
        let got = roles(&table, &registry, Some(&source()), Some(true));
        assert_eq!(got[1], (400, ProcessRole::ConflictingWriter));
        // A session started above the project could write into it too.
        let above = [proc(401, 1, &["claude"], Some("/work"))];
        assert_eq!(
            roles(
                &above,
                &[entry(401, "S3", "/work")],
                Some(&source()),
                Some(true)
            ),
            vec![(401, ProcessRole::ConflictingWriter)]
        );
    }

    #[test]
    fn ambiguous_or_contradictory_evidence_fails_closed() {
        // Unregistered, working directory unreadable.
        let mut unreadable = proc(500, 1, &["claude"], None);
        unreadable.cwd = None;
        unreadable.pwd = None;
        assert_eq!(
            roles(&[unreadable], &[], None, None),
            vec![(500, ProcessRole::Unclassifiable)]
        );
        // Registry says elsewhere, reality says this project.
        let liar = proc(501, 1, &["claude"], Some("/work/repo-b"));
        assert_eq!(
            roles(&[liar], &[entry(501, "S9", "/work/repo-a")], None, None),
            vec![(501, ProcessRole::Unclassifiable)]
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
    fn a_stale_recorded_pid_never_shields_a_live_conflicting_writer() {
        // The recorded source is gone; a different live session in the project still blocks.
        let table = [proc(600, 1, &["claude"], Some("/work/repo-b"))];
        let registry = [entry(600, "S7", "/work/repo-b")];
        let got = roles(&table, &registry, Some(&source()), Some(false));
        assert_eq!(got, vec![(600, ProcessRole::ConflictingWriter)]);
    }

    #[test]
    fn a_recycled_pid_is_not_trusted_as_the_source() {
        // pid 100 now belongs to an unrelated session in the project: the start time differs.
        let table = [proc(100, 1, &["claude"], Some("/work/repo-b"))];
        let registry = [entry(100, "OTHER", "/work/repo-b")];
        assert_eq!(
            roles(&table, &registry, Some(&source()), Some(false)),
            vec![(100, ProcessRole::ConflictingWriter)]
        );
        // …and its "descendants" are not helpers of anything.
        let child = proc(101, 100, &["claude"], Some("/work/repo-b"));
        let got = roles(
            &[table[0].clone(), child],
            &[
                entry(100, "OTHER", "/work/repo-b"),
                entry(101, "X", "/work/repo-b"),
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
