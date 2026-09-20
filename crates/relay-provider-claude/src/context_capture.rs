//! M6: builds a provider-neutral [`ContinuationBundle`] from Claude's own local session state,
//! for a `STATE_CONTINUATION` transaction where Claude is the SOURCE. Never dumps the raw
//! transcript — only deterministic repo facts and a bounded, verbatim excerpt of the most recent
//! real conversational text (see docs/security.md).

use std::{
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use relay_core::{
    Error, ProfileName, ProviderKind, Result,
    handoff::{
        ContextCapturer, ContinuationBundle, ConversationExcerpt, ExcerptRole, RepoFacts,
        bound_recent_context,
    },
};
use serde_json::Value;

use crate::session_transfer::discover_session;

/// Excerpts beyond this many most-recent user/assistant turns are dropped before byte-bounding
/// even runs, so a very long session cannot make this scan unbounded.
const MAX_RECENT_TURNS: usize = 12;

#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeContextCapturer;

impl ContextCapturer for ClaudeContextCapturer {
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
            extract_recent_context(source_config_dir, project_dir, source_session_id);
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

/// Deterministic, non-fabricated repo facts. By the time this runs the coordinator has already
/// successfully checkpointed the project (see `checkpoint_project` in relay-core), so a failure
/// here is unexpected rather than routine and is propagated rather than silently degraded.
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

/// Best-effort: a missing or unreadable transcript degrades to `(None, [])` rather than failing
/// the whole capture — the M6 spec explicitly prefers an honestly-empty field over fabricated or
/// forced context, and the target can always inspect the repository itself.
fn extract_recent_context(
    config_dir: &Path,
    project_dir: &Path,
    session_id: &str,
) -> (Option<String>, Vec<ConversationExcerpt>) {
    let Ok(artifacts) = discover_session(config_dir, project_dir, session_id) else {
        return (None, Vec::new());
    };
    // Only the primary transcript — subagent sidecar transcripts are a different conversation,
    // not the primary user-facing one, and are excluded by design.
    let Some(primary) = artifacts.first() else {
        return (None, Vec::new());
    };
    let Ok(bytes) = std::fs::read(primary) else {
        return (None, Vec::new());
    };
    let text = String::from_utf8_lossy(&bytes);

    let mut excerpts = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let role = match value.get("type").and_then(Value::as_str) {
            Some("user") => ExcerptRole::User,
            Some("assistant") => ExcerptRole::Assistant,
            _ => continue,
        };
        let content = value
            .get("message")
            .and_then(|message| message.get("content"));
        let content_text = extract_text_content(content);
        if content_text.trim().is_empty() {
            continue;
        }
        excerpts.push(ConversationExcerpt {
            role,
            text: content_text,
        });
    }

    let last_user_request = excerpts
        .iter()
        .rev()
        .find(|excerpt| excerpt.role == ExcerptRole::User)
        .map(|excerpt| excerpt.text.clone());

    let recent = if excerpts.len() > MAX_RECENT_TURNS {
        excerpts.split_off(excerpts.len() - MAX_RECENT_TURNS)
    } else {
        excerpts
    };
    (last_user_request, bound_recent_context(recent))
}

/// Only plain user text and assistant `text` content blocks are kept — `thinking`, `tool_use`,
/// and `tool_result` blocks are excluded (they are internal reasoning or raw tool output, not
/// conversational text, and can be large or contain command output the M6 spec explicitly says
/// not to carry across providers by default).
fn extract_text_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{extract_recent_context, extract_text_content};
    use crate::session_transfer::escape_project_path;
    use serde_json::{Value, json};
    use std::path::Path;

    #[test]
    fn extracts_plain_string_user_content() {
        let value = json!("how do i run this project?");
        assert_eq!(
            extract_text_content(Some(&value)),
            "how do i run this project?"
        );
    }

    #[test]
    fn extracts_only_text_blocks_from_an_assistant_array_and_skips_the_rest() {
        let value = json!([
            {"type": "thinking", "thinking": "internal reasoning", "signature": "x"},
            {"type": "tool_use", "id": "1", "name": "Bash", "input": {}},
            {"type": "text", "text": "I ran the tests and they pass."},
        ]);
        assert_eq!(
            extract_text_content(Some(&value)),
            "I ran the tests and they pass."
        );
    }

    #[test]
    fn a_pure_tool_result_line_yields_empty_text() {
        let value = json!([
            {"type": "tool_result", "tool_use_id": "1", "content": "output", "is_error": false},
        ]);
        assert_eq!(extract_text_content(Some(&value)), "");
    }

    fn seed_transcript(config_dir: &Path, project_dir: &Path, session_id: &str, lines: &[Value]) {
        use std::fmt::Write as _;
        let key = escape_project_path(project_dir);
        let dir = config_dir.join("projects").join(key);
        std::fs::create_dir_all(&dir).expect("session dir");
        let mut contents = String::new();
        for line in lines {
            writeln!(contents, "{line}").expect("write line");
        }
        std::fs::write(dir.join(format!("{session_id}.jsonl")), contents).expect("transcript");
    }

    #[test]
    fn extracts_the_last_user_request_and_a_bounded_recent_window_excluding_sidechains() {
        let root = tempfile::tempdir().expect("tempdir");
        let config_dir = root.path().join("config");
        let project_dir = root.path().join("proj");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        let session_id = "11111111-2222-3333-4444-555555555555";
        seed_transcript(
            &config_dir,
            &project_dir,
            session_id,
            &[
                json!({"type": "user", "message": {"role": "user", "content": "add a health check endpoint"}}),
                json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Added it in src/health.rs."}]}}),
                json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": "subagent-only text"}}),
                json!({"type": "user", "message": {"role": "user", "content": "now add a test for it"}}),
            ],
        );

        let (last_user_request, recent) =
            extract_recent_context(&config_dir, &project_dir, session_id);

        assert_eq!(last_user_request.as_deref(), Some("now add a test for it"));
        assert_eq!(recent.len(), 3, "the sidechain line must be excluded");
        assert!(
            !recent
                .iter()
                .any(|excerpt| excerpt.text.contains("subagent-only"))
        );
    }

    #[test]
    fn a_missing_session_degrades_to_empty_rather_than_erroring() {
        let root = tempfile::tempdir().expect("tempdir");
        let config_dir = root.path().join("config");
        let project_dir = root.path().join("proj");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        let (last_user_request, recent) = extract_recent_context(
            &config_dir,
            &project_dir,
            "11111111-2222-3333-4444-555555555555",
        );
        assert_eq!(last_user_request, None);
        assert!(recent.is_empty());
    }
}
