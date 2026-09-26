//! M6: the continuity-type concept and the provider-neutral cross-provider handoff payload.
//!
//! `relay-core` still knows nothing about Claude or Codex specifically — this module only
//! defines *shapes* (an enum naming which kind of continuation occurred, and a bounded,
//! structured bundle a provider adapter can populate) plus the one piece of policy that is
//! genuinely provider-neutral: which continuity type a given (source, target) provider pair
//! requires. Building the bundle's *content* is provider-specific and lives in each
//! `relay-provider-*` crate.

use serde::{Deserialize, Serialize};

use crate::{
    ProfileName, ProviderKind,
    handoff::{ExecutionIntent, WorkingStateSnapshot},
};

/// Mirrors the M6 spec's three continuity types. Recorded on every [`super::HandoffJournal`] so
/// the transcript of what actually happened is never ambiguous, and so Claude and Codex are
/// never described as "the same session" when they are not.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContinuityType {
    /// Same provider, same underlying session/transcript resumed under a different profile
    /// (Claude profile A -> Claude profile B, proven since M2B).
    SessionContinuation,
    /// Different providers. The target starts a genuinely new session/thread, bootstrapped from
    /// a bounded, structured [`ContinuationBundle`] built from the source's local state — never
    /// from a raw transcript dump.
    StateContinuation,
    /// Same provider, same profile, same account: reattaching to a session/thread that was
    /// never handed to a different profile at all (e.g. `codex resume <thread-id>` under the
    /// profile that created it). Does not go through [`super::HandoffCoordinator`] — there is no
    /// writer to hand off between two profiles, just a reattach.
    NativeResume,
}

impl ContinuityType {
    /// The only piece of continuity-type *policy* that is genuinely provider-neutral: same
    /// provider on both sides means a same-provider cross-profile transfer is at least
    /// attemptable (`SESSION_CONTINUATION`); anything else is `STATE_CONTINUATION`. Whether the
    /// specific provider actually proved cross-profile transfer works is a capability question
    /// (see [`crate::ProviderCapabilities::native_session_transfer`]), not decided here — a
    /// caller that knows the target provider lacks that capability should still choose
    /// `StateContinuation` even when the providers match.
    #[must_use]
    pub const fn for_transition(source: ProviderKind, target: ProviderKind) -> Self {
        match (source, target) {
            (ProviderKind::Claude, ProviderKind::Claude) => Self::SessionContinuation,
            _ => Self::StateContinuation,
        }
    }
}

/// Deterministic, non-fabricated repository facts captured at handoff time. The same shape
/// [`super::Checkpoint`] already records for `SESSION_CONTINUATION`; `STATE_CONTINUATION` needs
/// staged/untracked broken out separately so the target can tell "already committed" apart from
/// "still needs review" without inspecting the repo itself first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoFacts {
    pub branch: String,
    pub head: String,
    pub staged_files: Vec<String>,
    pub unstaged_files: Vec<String>,
    pub untracked_files: Vec<String>,
}

/// One bounded excerpt of real conversational text, copied verbatim (never summarized or
/// interpreted) from the source provider's own local transcript. Tool invocations, tool
/// results, and hidden reasoning are excluded by the extractor before this type is ever
/// constructed — see docs/security.md.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationExcerpt {
    pub role: ExcerptRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcerptRole {
    User,
    Assistant,
}

/// Total bytes of excerpt text a bundle may carry. Chosen to comfortably fit a handful of
/// recent turns while staying far below anything that would look like a transcript dump.
pub const RECENT_CONTEXT_BYTE_BUDGET: usize = 8_000;
/// A single excerpt is truncated to this many bytes before it counts against the budget above,
/// so one long turn cannot consume the entire allowance by itself.
pub const EXCERPT_BYTE_CAP: usize = 2_000;

/// The provider-neutral cross-provider continuation payload (`STATE_CONTINUATION`'s
/// `ContinuationBundle`). Every semantic field the M6 spec asks for is present, but every field
/// beyond the deterministic ones (`repo`, the provider/profile identifiers, `generated_unix_ms`)
/// is `Option`/empty by construction: `relay-core` and its provider adapters never fabricate an
/// objective, plan, or decision list they cannot actually observe. When a provider adapter has
/// no reliable way to determine a field, it leaves it `None`/empty rather than guessing — see
/// docs/security.md's "no fabricated project state" rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationBundle {
    pub version: u32,
    pub source_provider: ProviderKind,
    pub source_profile: ProfileName,
    /// The source's own session/thread id, when one exists (kept for traceability only — the
    /// target never attempts to resume it; that would be `SESSION_CONTINUATION`, not this).
    pub source_session_id: Option<String>,
    pub target_provider: ProviderKind,
    pub canonical_project_path: std::path::PathBuf,
    pub generated_unix_ms: u64,
    /// The most recent literal user request/instruction, copied verbatim when the source
    /// transcript makes it unambiguous. Never a summary.
    pub last_user_request: Option<String>,
    pub repo: RepoFacts,
    /// Bounded, verbatim, user/assistant-only excerpts of the most recent conversation, oldest
    /// first. Never exceeds [`RECENT_CONTEXT_BYTE_BUDGET`] bytes in total.
    pub recent_context: Vec<ConversationExcerpt>,
    /// Issue #5: the durable, provider-neutral working-state snapshot for this Relay Session,
    /// when one exists. Populated by [`super::HandoffCoordinator`] itself (not by
    /// [`super::ContextCapturer`] implementations) from the session's own `working_state.json` —
    /// see the coordinator's doc comment at the call site. `None` for a session that predates
    /// this feature or has never recorded any semantic state; the bundle/prompt render exactly
    /// as they did before this field existed in that case. Supplements `recent_context`, does
    /// not replace it.
    #[serde(default)]
    pub working_state: Option<WorkingStateSnapshot>,
}

impl ContinuationBundle {
    pub const CURRENT_VERSION: u32 = 1;

    /// Total bytes of `recent_context` text, for callers that want to assert the budget was
    /// respected without recomputing the truncation logic.
    #[must_use]
    pub fn recent_context_bytes(&self) -> usize {
        self.recent_context
            .iter()
            .map(|excerpt| excerpt.text.len())
            .sum()
    }
}

/// Appends excerpts (oldest-first input) to `out` until [`RECENT_CONTEXT_BYTE_BUDGET`] would be
/// exceeded, truncating each individual excerpt to [`EXCERPT_BYTE_CAP`] bytes first (on a char
/// boundary) and keeping the MOST RECENT excerpts when the budget forces a choice, since the
/// most recent turns are the most likely to matter to the target. Pure and provider-neutral;
/// provider adapters supply the already-extracted, already-role-filtered excerpts.
#[must_use]
pub fn bound_recent_context(mut excerpts: Vec<ConversationExcerpt>) -> Vec<ConversationExcerpt> {
    for excerpt in &mut excerpts {
        truncate_to_char_boundary(&mut excerpt.text, EXCERPT_BYTE_CAP);
    }
    let mut kept: Vec<ConversationExcerpt> = Vec::new();
    let mut used = 0usize;
    for excerpt in excerpts.into_iter().rev() {
        let len = excerpt.text.len();
        if used + len > RECENT_CONTEXT_BYTE_BUDGET {
            break;
        }
        used += len;
        kept.push(excerpt);
    }
    kept.reverse();
    kept
}

fn truncate_to_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
}

/// Renders the truthful bootstrap prompt sent to the target's first turn. Deliberately never
/// says "continuing the same conversation" — the whole point of [`ContinuityType::StateContinuation`]
/// is that it is not. Pure and provider-neutral so both `relay-provider-claude` and
/// `relay-provider-codex` render byte-identical wording.
#[must_use]
pub fn render_bootstrap_prompt(bundle: &ContinuationBundle) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are continuing an existing software task previously handled by another coding \
         agent. This is a new session, not a continuation of that agent's own conversation.\n\n",
    );
    prompt.push_str(&format!(
        "Source provider: {}\nSource profile: {}\nContinuity type: STATE_CONTINUATION\n",
        bundle.source_provider, bundle.source_profile
    ));
    prompt.push_str(&format!(
        "Project: {}\n\n",
        bundle.canonical_project_path.display()
    ));
    prompt.push_str("Repository state (observed, not asserted by the prior agent):\n");
    prompt.push_str(&format!("  branch: {}\n", bundle.repo.branch));
    prompt.push_str(&format!("  head: {}\n", bundle.repo.head));
    if bundle.repo.staged_files.is_empty()
        && bundle.repo.unstaged_files.is_empty()
        && bundle.repo.untracked_files.is_empty()
    {
        prompt.push_str("  working tree: clean\n");
    } else {
        push_file_list(&mut prompt, "staged", &bundle.repo.staged_files);
        push_file_list(&mut prompt, "unstaged", &bundle.repo.unstaged_files);
        push_file_list(&mut prompt, "untracked", &bundle.repo.untracked_files);
    }
    if let Some(request) = &bundle.last_user_request {
        prompt.push_str("\nMost recent user request (verbatim):\n");
        prompt.push_str(request);
        prompt.push('\n');
    }
    if !bundle.recent_context.is_empty() {
        prompt.push_str("\nRecent conversation excerpt (verbatim, most recent last):\n");
        for excerpt in &bundle.recent_context {
            let role = match excerpt.role {
                ExcerptRole::User => "user",
                ExcerptRole::Assistant => "assistant",
            };
            prompt.push_str(&format!("[{role}] {}\n", excerpt.text));
        }
    }
    if let Some(working_state) = &bundle.working_state {
        prompt.push_str(&working_state.render_section());
    }
    prompt.push_str(
        "\nThis is Relay's bounded bootstrap turn. Do not continue the task yet and do not use \
         any tools in this turn. Record the context above by replying with the single word READY. \
         Relay will then reopen this new session interactively and explicitly tell you to continue.",
    );
    prompt
}

/// The concise instruction appended to a target's continuation turn when the Relay Session is
/// [`super::ExecutionIntent::Autonomous`]. Deliberately does not suppress every question — an
/// autonomous agent must still stop for a genuine blocker; see the enum's own doc comment. Kept
/// in one place so Claude and Codex targets receive byte-identical wording, and so it is stated
/// exactly once per continuation rather than repeated throughout a bundle/prompt.
#[must_use]
pub const fn render_autonomous_notice() -> &'static str {
    "This Relay Session is autonomous. Continue the current task immediately. Do not ask the \
     user whether you should continue merely because a handoff occurred. Stop only when: the \
     task is complete, you are genuinely blocked on missing information, an action requires \
     explicit user authorization, or continuing would be unsafe or ambiguous."
}

/// Issue #3: the instruction shown to an ALREADY-RUNNING agent when its Relay Session's execution
/// intent changes live — `relay mode`, the Claude `/relay:mode` hook command, and the Codex
/// `$relay mode` skill all render this same text (see `relay-cli`'s `commands::mode` module, the
/// one place that builds it) so a mode change means the same thing regardless of how it was made.
/// Distinct from [`render_autonomous_notice`], which a *new* target reads once right after a
/// handoff: this one never mentions a handoff, since none occurred, and it also covers the
/// `Interactive` direction, which a handoff-time notice never needs to (a handoff only ever
/// *adds* the autonomous notice; it never has to tell a target to go back to asking questions).
#[must_use]
pub const fn render_live_mode_notice(intent: ExecutionIntent) -> &'static str {
    match intent {
        ExecutionIntent::Autonomous => {
            "Execution mode is now autonomous. From this point forward, continue the current \
             task without asking whether you should proceed. Stop only when: the task is \
             complete, you are genuinely blocked on missing information, an action requires \
             explicit user authorization, or continuing would be unsafe or ambiguous."
        }
        ExecutionIntent::Interactive => {
            "Execution mode is now interactive. Normal conversational confirmation is \
             appropriate again — ask before taking significant next steps, as usual."
        }
    }
}

fn push_file_list(prompt: &mut String, label: &str, files: &[String]) {
    if files.is_empty() {
        return;
    }
    prompt.push_str(&format!("  {label}: {}\n", files.join(", ")));
}

#[cfg(test)]
mod tests {
    use super::{
        ContinuationBundle, ContinuityType, ConversationExcerpt, EXCERPT_BYTE_CAP, ExcerptRole,
        RECENT_CONTEXT_BYTE_BUDGET, RepoFacts, WorkingStateSnapshot, bound_recent_context,
        render_autonomous_notice, render_bootstrap_prompt,
    };
    use crate::{ProfileName, ProviderKind};

    fn sample_bundle() -> ContinuationBundle {
        ContinuationBundle {
            version: ContinuationBundle::CURRENT_VERSION,
            source_provider: ProviderKind::Claude,
            source_profile: ProfileName::new("claude-main").expect("name"),
            source_session_id: Some("8586fe71-395b-4449-b973-78011d561fed".to_owned()),
            target_provider: ProviderKind::Codex,
            canonical_project_path: std::path::PathBuf::from("/tmp/proj"),
            generated_unix_ms: 1_000,
            last_user_request: Some("add a health check endpoint".to_owned()),
            repo: RepoFacts {
                branch: "main".to_owned(),
                head: "abc123".to_owned(),
                staged_files: vec!["src/health.rs".to_owned()],
                unstaged_files: Vec::new(),
                untracked_files: Vec::new(),
            },
            recent_context: Vec::new(),
            working_state: None,
        }
    }

    #[test]
    fn same_provider_claude_to_claude_is_session_continuation() {
        assert_eq!(
            ContinuityType::for_transition(ProviderKind::Claude, ProviderKind::Claude),
            ContinuityType::SessionContinuation
        );
    }

    #[test]
    fn any_pair_involving_codex_is_state_continuation() {
        assert_eq!(
            ContinuityType::for_transition(ProviderKind::Claude, ProviderKind::Codex),
            ContinuityType::StateContinuation
        );
        assert_eq!(
            ContinuityType::for_transition(ProviderKind::Codex, ProviderKind::Claude),
            ContinuityType::StateContinuation
        );
        assert_eq!(
            ContinuityType::for_transition(ProviderKind::Codex, ProviderKind::Codex),
            ContinuityType::StateContinuation
        );
    }

    #[test]
    fn bound_recent_context_truncates_a_single_oversized_excerpt() {
        let huge = ConversationExcerpt {
            role: ExcerptRole::User,
            text: "x".repeat(EXCERPT_BYTE_CAP * 3),
        };
        let bounded = bound_recent_context(vec![huge]);
        assert_eq!(bounded.len(), 1);
        assert!(bounded[0].text.len() <= EXCERPT_BYTE_CAP);
    }

    #[test]
    fn bound_recent_context_keeps_the_most_recent_excerpts_within_budget() {
        let excerpts: Vec<ConversationExcerpt> = (0..20)
            .map(|index| ConversationExcerpt {
                role: ExcerptRole::User,
                text: format!("turn-{index}: {}", "y".repeat(500)),
            })
            .collect();
        let bounded = bound_recent_context(excerpts);
        let total: usize = bounded.iter().map(|excerpt| excerpt.text.len()).sum();
        assert!(total <= RECENT_CONTEXT_BYTE_BUDGET);
        assert!(
            bounded
                .last()
                .expect("at least one kept")
                .text
                .starts_with("turn-19")
        );
    }

    #[test]
    fn bootstrap_prompt_never_claims_a_shared_conversation() {
        let prompt = render_bootstrap_prompt(&sample_bundle());
        assert!(!prompt.to_lowercase().contains("same conversation"));
        assert!(prompt.contains("STATE_CONTINUATION"));
        assert!(prompt.contains("claude-main"));
        assert!(prompt.contains("src/health.rs"));
        assert!(prompt.contains("single word READY"));
        assert!(prompt.contains("Do not continue the task yet"));
    }

    #[test]
    fn bootstrap_prompt_reports_a_clean_tree_explicitly() {
        let mut bundle = sample_bundle();
        bundle.repo.staged_files.clear();
        let prompt = render_bootstrap_prompt(&bundle);
        assert!(prompt.contains("working tree: clean"));
    }

    #[test]
    fn bundle_without_working_state_renders_identically_to_before_the_field_existed() {
        // A session predating Issue #5 (or one that never recorded any semantic state) must
        // produce byte-for-byte the same prompt as before this field was added.
        let bundle = sample_bundle();
        assert!(bundle.working_state.is_none());
        let prompt = render_bootstrap_prompt(&bundle);
        assert!(!prompt.contains("Durable working notes"));
        assert!(!prompt.to_lowercase().contains("advisory"));
    }

    #[test]
    fn bundle_with_working_state_renders_the_advisory_section() {
        let mut bundle = sample_bundle();
        bundle.working_state = Some(WorkingStateSnapshot {
            goal: Some("add a health check endpoint".to_owned()),
            current_subtask: Some("wire the route".to_owned()),
            active_decisions: vec!["return 200 with an empty body".to_owned()],
            failed_attempts: Vec::new(),
            relevant_files: Vec::new(),
            next_actions: vec!["add a test".to_owned()],
        });
        let prompt = render_bootstrap_prompt(&bundle);
        assert!(prompt.contains("Durable working notes"));
        assert!(prompt.contains("wire the route"));
        assert!(prompt.contains("return 200 with an empty body"));
        assert!(prompt.contains("add a test"));
    }

    #[test]
    fn autonomous_notice_says_continue_without_asking_but_still_allows_stopping() {
        let notice = render_autonomous_notice();
        assert!(notice.contains("Continue the current task immediately"));
        assert!(notice.to_lowercase().contains("do not ask"));
        // Genuine-blocker language must survive — this is not "never ask anything".
        assert!(notice.to_lowercase().contains("blocked"));
        assert!(notice.to_lowercase().contains("authorization"));
        assert!(notice.to_lowercase().contains("unsafe"));
    }

    #[test]
    fn autonomous_notice_never_mentions_provider_permissions() {
        // The critical separation: intent text must never talk in terms of provider permission
        // flags/modes, only behavioral continuation.
        let notice = render_autonomous_notice().to_lowercase();
        for forbidden in [
            "permission",
            "sandbox",
            "dangerously-skip",
            "full-access",
            "unrestricted",
        ] {
            assert!(
                !notice.contains(forbidden),
                "notice must not mention '{forbidden}'"
            );
        }
    }
}
