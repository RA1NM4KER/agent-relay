//! M6/M11: builds a provider-neutral `ContinuationBundle` from Codex's own local state, for a
//! `STATE_CONTINUATION` transaction where Codex is the SOURCE.
//!
//! Repo facts (deterministic, provider-independent) are always captured. Conversation content
//! (`last_user_request`, `recent_context`) comes from `codex app-server`'s `thread/items/list` —
//! the same official, typed, schema-generated protocol Relay already uses for rate limits and
//! thread identity (see `crate::app_server`), never Codex's undocumented, version-fragile local
//! storage (`CODEX_HOME/thread_history_*.sqlite`, rollout JSONL files): parsing those would mean
//! reverse-engineering an internal format Codex has never published, exactly the "no silent
//! assumptions about session layout" the M6 spec warns against. Best effort like every other
//! app-server read: a missing thread, an old server, or any protocol error degrades to
//! `(None, [])` rather than failing the whole capture — the target is always still told to
//! inspect the repository itself either way (see `render_bootstrap_prompt`).

use std::{
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use relay_core::{
    Error, ProfileName, ProviderKind, Result,
    handoff::{ContextCapturer, ContinuationBundle, ConversationExcerpt, ExcerptRole, RepoFacts},
};

use crate::{app_server, inspection::CodexInspector};

/// Mirrors `relay_provider_claude`'s own `MAX_RECENT_TURNS` bound — the two providers keep the
/// same shape of recent-context budget, even though they source it differently.
const MAX_RECENT_TURNS: usize = 12;

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexContextCapturer;

impl ContextCapturer for CodexContextCapturer {
    fn capture(
        &self,
        source_config_dir: &Path,
        project_dir: &Path,
        source_session_id: &str,
        source_profile: &ProfileName,
        source_provider: ProviderKind,
        target_provider: ProviderKind,
    ) -> Result<ContinuationBundle> {
        let repo = capture_repo_facts(project_dir)?;
        let (last_user_request, recent_context) =
            extract_recent_context(source_config_dir, source_session_id);
        Ok(ContinuationBundle {
            version: ContinuationBundle::CURRENT_VERSION,
            source_provider,
            source_profile: source_profile.clone(),
            source_session_id: Some(source_session_id.to_owned()),
            target_provider,
            canonical_project_path: project_dir.to_path_buf(),
            generated_unix_ms: now_unix_ms(),
            last_user_request,
            repo,
            recent_context,
        })
    }
}

/// Best-effort, exactly like `relay_provider_claude::extract_recent_context`: no installed/
/// discoverable Codex CLI, no reachable app-server, or no matching thread all degrade to
/// `(None, [])` rather than failing the whole capture.
fn extract_recent_context(
    config_dir: &Path,
    thread_id: &str,
) -> (Option<String>, Vec<ConversationExcerpt>) {
    let Ok(inspector) = CodexInspector::discover(None) else {
        return (None, Vec::new());
    };
    let mut excerpts =
        app_server::recent_conversation_excerpts(inspector.executable(), config_dir, thread_id);
    let last_user_request = excerpts
        .iter()
        .rev()
        .find(|excerpt| excerpt.role == ExcerptRole::User)
        .map(|excerpt| excerpt.text.clone());
    if excerpts.len() > MAX_RECENT_TURNS {
        excerpts = excerpts.split_off(excerpts.len() - MAX_RECENT_TURNS);
    }
    (
        last_user_request,
        relay_core::handoff::bound_recent_context(excerpts),
    )
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
