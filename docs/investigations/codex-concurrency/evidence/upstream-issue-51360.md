# SessionStart (source: resume) fires only with the first prompt after /resume, and the replaced startup session gets SessionEnd without SessionStart

https://github.com/openai/codex/issues/51360

State at retrieval: open; updated: 2026-10-28T03:42:32Z.

User report / issue discussion, not independently verified behavior.

Version: codex-cli 0.160.1, macOS 26, interactive TUI

Steps:

1. Configure SessionStart, UserPromptSubmit, and SessionEnd hooks that log hook_event_name, source, and session_id.
2. Start codex, then run /resume and pick an earlier conversation.
3. Wait, then send a prompt.

Expected: SessionStart with source: "resume" fires when the conversation is resumed, so integrations can show that conversation's state before the user types anything.

Actual
- No hook fires on /resume. SessionStart (source: "resume") arrives in the same second as the first UserPromptSubmit; if no prompt is sent, it never fires.
- The blank startup session that /resume replaced gets no SessionStart, yet its SessionEnd (reason: "other") arrives 60 seconds after /resume, while the resumed conversation is already mid-turn. A consumer that treats SessionEnd as "the agent in this terminal exited" tears down the live conversation's state.

Log (session truncated):
+0s   SessionEnd        session=01a11105-3861 reason=other   ← replaced startup session, 60s after /resume
...
      SessionStart      source=resume session=01a110e4-52e2  ← only with the first prompt
      UserPromptSubmit  session=01a110e4-52e2

Ask: fire SessionStart (source: "resume") when /resume selects a conversation, and either pair the startup session's SessionEnd with a SessionStart or skip it for a session that never started a turn.
