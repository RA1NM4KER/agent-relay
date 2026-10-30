# Managed app-server runs hooks with the first client's TMUX_PANE/TMUX, misattributing hook events to the wrong terminal pane (0.157 regression)

https://github.com/openai/codex/issues/48500

State at retrieval: open; updated: 2026-10-30T01:21:57Z.

User report / issue discussion, not independently verified behavior.

### Summary
Since 0.157 the TUI talks to a shared `codex app-server --managed-daemon` (one per `CODEX_HOME`), and lifecycle hooks (`UserPromptSubmit`, `Stop`, `PostToolUse`) run inside that daemon. The daemon inherits the environment of whichever TUI first spawned it, so every hook for every later TUI sees that first client's `TMUX` / `TMUX_PANE`. Before 0.157, hooks ran in the TUI's own process and saw the correct pane.

### Reproduction (reproduced on 0.157.1, macOS, tmux)
1. tmux pane A: start `codex`, submit a prompt (the managed daemon starts).
2. tmux pane B, same `CODEX_HOME`: start `codex`, submit a prompt.
3. In a `UserPromptSubmit` hook, log `$TMUX_PANE` and the hook payload's `session_id`.

**Expected:** B's hook reports pane B.
**Actual:** B's hook reports **pane A** together with **B's** `session_id`. If pane A has since been closed while the daemon stays alive, B's hooks report a pane that no longer exists.

### Impact
Tmux-aware hook consumers (e.g. workmux status tracking) silently write B's session to A's pane (input/status then route to the wrong session), or drop B's updates entirely.

### What this is / isn't asking for
Not asking for client metadata on turns (#44774); the thread-vs-client concerns raised there are fair.

Asking only that hooks are not handed a **stale, arbitrary** client's terminal identity. Suggested:
- remove `TMUX`, `TMUX_PANE` (and similar terminal-identity variables) from the daemon's environment before running hooks, so consumers fail safely instead of misattributing;
- optionally, a hook-scoped, local-only payload field carrying the pid of the client that submitted the turn.

Related: #37209 (MCP subprocess omits `TMUX`/`TMUX_PANE`).

Co-authored by AI assistant.

