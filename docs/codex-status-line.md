# Codex live status: investigation and current limitation

> **Scope.** Priority 5 of the 2026-09-23 overnight hardening pass: does Codex expose a supported
> way to show *dynamic* Relay ownership/status the way Claude's status-line hook lets Relay render
> `[Relay · <profile>]` (see `crates/relay-cli/src/badge.rs`)? This document records what was
> actually found, against the locally installed `codex-cli 0.155.0`, so the conclusion — "no, keep
> the existing one-shot launch banner" — is not just an assertion but a verified, reproducible
> finding. No real session data, pane titles, or account identifiers were copied into this file.

## Conclusion

**No.** Codex today has no supported extension point that lets a third party (like Relay) inject
arbitrary, live-updating text into its TUI. Relay's Codex integration keeps the existing one-shot
launch banner (printed once, before Codex takes over the terminal — see `commands/codex.rs`), and
this limitation is called out explicitly in the README rather than worked around.

Two things were investigated and both came up closed:

## 1. Codex's own status line is a fixed, closed set of built-in items — not a custom command

`codex-cli` does have a configurable TUI status line (`[tui] status_line` in `config.toml`), e.g.:

```toml
[tui]
status_line = [
    "model-with-reasoning",
    "current-dir",
    "five-hour-limit",
    "weekly-limit",
]
status_line_use_colors = true
```

Inspecting the shipped binary's embedded config-UI strings
(`strings .../bin/codex | grep -i statusline`, run against the real installed binary, not a
decompile of proprietary logic) shows the full picker text: *"Configure Status Line — Select which
items to display in the status line"*, followed by the complete list of selectable items —
project name, hostname, open PR number, uncommitted-changes-vs-default-branch, active permission
profile/sandbox mode, active approval mode, context window size, estimated thread cost (Enterprise
only), raw-scrollback state, and workspace notification headline (Enterprise only) — plus the
model/dir/rate-limit items already seen in the local config.

This is an **enum of built-in identifiers**, not a "run this command and show its stdout" item
type the way Claude Code's `statusLine` hook works (which is exactly what `relay hook claude
statusline` chains into for the live `[Relay · <profile>]` badge). There is no `"custom"` or
`"command"` item anywhere in that list. Unless Codex adds one, Relay has no item to register into.

## 2. Codex's hook system is lifecycle-event-driven, not render-driven

Codex does have a real, stable (`features list` → `hooks  stable  true`) hook system
(`~/.codex/hooks.json`, `[hooks.state]` in `config.toml`) — Relay's own Herdr integration already
uses `SessionStart` for `herdr-agent-state.sh`. But every event Codex's hooks can fire on is a
one-shot lifecycle moment, not a per-render tick:

`PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`,
`SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Interrupt`.

None of these fire on a timer or on every frame the way Claude re-invokes its status-line command;
a `SessionStart` hook could print something once (which is functionally what Relay's existing
one-shot banner already does, without needing to become a Codex hook to do it), but nothing in this
list gives Relay a way to keep a footer honest as ownership changes *during* a long-running Codex
session — which is the entire point of Claude's live badge (it re-renders on every status-line
tick and reflects a handoff the moment it completes).

## What this rules out, on purpose

- **No fragile terminal scraping.** Rendering Relay's own overlay by writing directly into the
  TUI's screen region was not attempted — it would require assumptions about Codex's terminal
  rendering internals that Codex has never published and that would break on any TUI layout change.
- **No patching Codex internals.** Not a route Relay will take regardless of what's technically
  possible with a local binary.
- **`SessionStart` hook as a banner substitute was considered and rejected as strictly worse** than
  what Relay already does: Relay's own banner is profile/session-aware and prints exactly once,
  before Codex starts, with everything Relay already knows; routing it through a Codex hook would
  add an extra process fork and a dependency on Codex's hook-approval UI (hooks are shown to the
  user as "new hook — review required" the first time) for zero behavioural gain.

## Revisit trigger

If a future Codex release adds either (a) a custom/command status-line item type, or (b) a hook
event that fires on a UI-relevant tick (not just tool/session lifecycle), this investigation should
be redone — `codex features list` and the `[tui] status_line` picker are the two places to check
first before assuming the conclusion above still holds.
