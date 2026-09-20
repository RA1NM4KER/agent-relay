//! M6: builds a provider-neutral `ContinuationBundle` from Codex's own local state, for a
//! `STATE_CONTINUATION` transaction where Codex is the SOURCE.
//!
//! Deliberately does NOT attempt to read Codex's session/thread history: it lives in
//! `CODEX_HOME/thread_history_*.sqlite`, an undocumented, version-fragile internal schema
//! (observed directly, not published upstream). Parsing it would mean either bundling a sqlite
//! dependency to reverse-engineer an internal format, or hand-parsing the SQLite file format —
//! exactly the "no silent assumptions about session layout" the M6 spec warns against. Repo
//! facts (deterministic, provider-independent) are still captured; `last_user_request` and
//! `recent_context` are left empty/`None` rather than guessed — the target is explicitly told to
//! inspect the repository itself (see `render_bootstrap_prompt`). This is a known, documented M6
//! limitation for the Codex -> * direction (Claude -> Codex is unaffected: Claude's own
//! transcript extraction is unrelated to this).

use std::{
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use relay_core::{
    Error, ProfileName, ProviderKind, Result,
    handoff::{ContextCapturer, ContinuationBundle, RepoFacts},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexContextCapturer;

impl ContextCapturer for CodexContextCapturer {
    fn capture(
        &self,
        _source_config_dir: &Path,
        project_dir: &Path,
        source_session_id: &str,
        source_profile: &ProfileName,
        source_provider: ProviderKind,
        target_provider: ProviderKind,
    ) -> Result<ContinuationBundle> {
        let repo = capture_repo_facts(project_dir)?;
        Ok(ContinuationBundle {
            version: ContinuationBundle::CURRENT_VERSION,
            source_provider,
            source_profile: source_profile.clone(),
            source_session_id: Some(source_session_id.to_owned()),
            target_provider,
            canonical_project_path: project_dir.to_path_buf(),
            generated_unix_ms: now_unix_ms(),
            last_user_request: None,
            repo,
            recent_context: Vec::new(),
        })
    }
}

fn capture_repo_facts(project_dir: &Path) -> Result<RepoFacts> {
    let branch = run_git(project_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let head = run_git(project_dir, &["rev-parse", "HEAD"])?;
    let status = run_git(project_dir, &["status", "--porcelain=v1"])?;
    let mut staged_files = Vec::new();
    let mut unstaged_files = Vec::new();
    let mut untracked_files = Vec::new();
    for line in status.lines() {
        if line.len() < 4 {
            continue;
        }
        let index_status = line.as_bytes()[0];
        let worktree_status = line.as_bytes()[1];
        let path = line[3..].to_owned();
        if index_status == b'?' && worktree_status == b'?' {
            untracked_files.push(path);
            continue;
        }
        if index_status != b' ' {
            staged_files.push(path.clone());
        }
        if worktree_status != b' ' {
            unstaged_files.push(path);
        }
    }
    Ok(RepoFacts {
        branch,
        head,
        staged_files,
        unstaged_files,
        untracked_files,
    })
}

fn run_git(project_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(args)
        .output()
        .map_err(|_| Error::ProviderCommandFailed)?;
    if !output.status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| Error::MalformedProviderOutput)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::CodexContextCapturer;
    use relay_core::{ProfileName, ProviderKind, handoff::ContextCapturer};
    use std::process::Command;

    fn init_git_repo(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("dir");
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
        std::fs::write(dir.join("a.txt"), "x").expect("seed");
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn captures_deterministic_repo_facts_and_leaves_conversation_fields_empty() {
        let root = tempfile::tempdir().expect("tempdir");
        let project = root.path().join("proj");
        init_git_repo(&project);
        std::fs::write(project.join("b.txt"), "y").expect("untracked file");

        let bundle = CodexContextCapturer
            .capture(
                root.path(),
                &project,
                "01a-thread",
                &ProfileName::new("codex-main").expect("name"),
                ProviderKind::Codex,
                ProviderKind::Claude,
            )
            .expect("capture");

        assert_eq!(bundle.repo.branch, "main".to_string());
        assert!(bundle.repo.untracked_files.iter().any(|f| f == "b.txt"));
        assert_eq!(bundle.last_user_request, None);
        assert!(bundle.recent_context.is_empty());
        assert_eq!(bundle.source_provider, ProviderKind::Codex);
        assert_eq!(bundle.target_provider, ProviderKind::Claude);
    }
}
