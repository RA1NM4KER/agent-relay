# Everyday workflows

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/workflows.html

Start, resume, switch, and adopt coding-agent conversations.

Handbook examples are verified against stable v0.5.2.

Run shell commands from your project directory. If you are typing inside Claude or Codex, use the [in-agent commands](https://ra1nm4ker.github.io/agent-relay/docs/integrations.html) instead.

- [Start fresh](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#new)
- [Resume](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#resume)
- [Switch profiles](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#switch)
- [Adopt Claude](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#existing)

## Start a new conversation

```
relay claude
# Or choose Codex:
relay codex
```

Both commands create a new Relay session. They do not reopen yesterday’s work or replace another running session. An optional first message can go on the command line:

```
relay claude "Find the cause of the failing tests"
```

## Continue yesterday’s work

```
relay resume
```

This reopens a closed, dormant Relay session on its last profile. One available session opens directly; with several, you choose. Codex usage is checked before attach: verified exhaustion can trigger state continuation to an eligible fallback; unknown usage is refused. A session that is already active is never started a second time.

```
relay status
relay resume --session <relay-session-id>
```

Replace the placeholder with the Relay session ID or an unambiguous prefix from status. You can also filter the picker by last profile:

```
relay resume claude-work
```

## Switch accounts or providers

From another terminal in the same project:

```
relay switch
```

Choose an eligible target from the list, or name it directly:

```
relay switch claude-backup
# Or move the work to Codex:
relay switch codex-work
```

With multiple active conversations, select the session too:

```
relay switch codex-work --session <relay-session-id>
```

Claude → Claude keeps the same native Claude conversation. Any handoff involving Codex starts a new native session with a state bundle. [See exactly what carries over](https://ra1nm4ker.github.io/agent-relay/docs/handoffs.html#continuity).

An explicit target is your choice. Relay refuses an unavailable or unverifiable target instead of silently selecting a different account.

## Bring an existing Claude conversation

### A previous conversation that Relay has never managed

```
relay claude --resume
```

Claude opens its native picker. Relay verifies the conversation you select and adopts that same session. If you already know its native Claude ID:

```
relay claude --resume <claude-session-id>
```

### A Claude conversation that is running right now

If the Relay integration was installed before Claude started, type this inside Claude:

```
/relay adopt
```

For a process that predates the integration, use a separate terminal:

```
relay adopt --session <claude-session-id>
```

Relay requires positive evidence of the running conversation’s identity. Ambiguity leaves it untouched. Live adoption of an unmanaged Codex session is not offered; start through `relay codex`.

**Two different kinds of resume.**`relay resume` reopens a session Relay already manages. `relay claude --resume` brings an existing native Claude conversation under Relay. A Relay session ID and a native provider session ID are different identifiers.

## Pass options to your provider

Place Relay options before `--` and provider options after it:

```
relay claude --profile claude-work -- --model opus
relay codex --profile codex-work -- --sandbox workspace-write
```

Provider options are remembered and reused for that provider. They are never translated across Claude and Codex. Options that would take over Relay’s session identity, working directory, output mode, or required hooks are refused.

Use Relay’s own `--resume` option when adopting Claude; do not pass `--resume` through after the separator.

## Set execution intent

```
relay claude --autonomous
# Or change an existing session:
relay mode autonomous
# Return to normal conversational confirmation:
relay mode interactive
```

Autonomous mode asks the agent to continue the current task until a real blocker. It does not grant permissions or change provider approval or sandbox settings. The mode stays with the Relay session across handoffs. `relay resume` preserves it unless you pass `--autonomous` or `--interactive`.

## Work with multiple conversations

A project can have several Relay sessions. Claude sessions can share a profile. Relay permits only one live interactive Codex session per isolated Codex profile; use separate Codex profiles for concurrent Codex work. Each live session has one owner. Closing its provider normally makes it dormant; switching one session does not move the others.

Relay does not coordinate edits between agents in the same working tree. Use separate worktrees when your tasks need file isolation.

Use `--session` when a command needs to identify one conversation. For scripts without a terminal picker, provide it explicitly.
