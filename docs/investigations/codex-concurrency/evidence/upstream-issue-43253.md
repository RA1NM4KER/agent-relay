# Show read-only conversations when resume encounters an active writer

https://github.com/openai/codex/pull/43253

State at retrieval: closed; updated: 2026-09-06T17:30:13Z.

User report / issue discussion, not independently verified behavior.

Show read-only conversations when resume encounters an active writer

## Why

Resuming a conversation that is open in another app fails with an active-writer error. Allow users to inspect its transcript and retry after closing it in the other app.

## What changed

- Fall back to a read-only history snapshot on active-writer conflicts during startup and session selection.
- Replace the composer with an explanatory notice, `R` to retry, exit shortcuts, and the configured transcript shortcut.
- Block input and freeze active command and MCP animations while viewing the snapshot. Preserve initial prompts and existing drafts for a successful retry.
- Keep the subscription when retrying the same thread and preserve read-only state across thread switches and reconnects.

## Testing

Add regression tests for conflict detection, history viewing, draft preservation, same-thread retry without unsubscribing, exit shortcuts, and notice and frozen-tool rendering.

<!-- copyberry-projection-id: 1c75b096e5e01970e7e3e5147aa2909cb2931a72903f9dadb4aa9673dca7262b -->
