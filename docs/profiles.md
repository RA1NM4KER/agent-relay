# Profiles & accounts

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/profiles.html

Manage isolated accounts and choose your fallback order.

Handbook examples are verified against stable v0.5.2.

A profile is a named provider account with its own configuration. A Relay session is a conversation. One profile can own several different sessions.

**Profiles and sessions have different roles.** A profile identifies an isolated provider account. A Relay session identifies a managed conversation and records its current owner. Renaming an account does not create more quota.

## Add an account

The simplest route is to rerun the setup wizard:

```
relay setup
```

For an explicit new account, name its provider:

```
relay login claude-backup --provider claude
relay login codex-work --provider codex
```

Each opens the provider’s official authentication flow in an isolated profile. Then rerun `relay setup` to include the profile in your preferred order.

## Choose your fallback order

Setup records one global priority order across providers. For example:

| Priority | Profile name | Provider |
| --- | --- | --- |
| 1 · primary | `claude-work` | Claude |
| 2 · fallback | `claude-backup` | Claude |
| 3 · fallback | `codex-work` | Codex |

A new `relay claude` starts with the highest-priority Claude profile; `relay codex` starts with the highest-priority Codex profile. When the current owner is exhausted, Relay checks the shared order for an eligible fallback.

A recovered primary does not automatically take the conversation back. The current owner stays in place until another handoff or an explicit switch.

## Inspect your profiles

```
relay profiles
relay profile doctor claude-work
```

The profile report separates authentication state, diagnostic details, and recovery commands. Codex checks also verify structured usage; a locally configured login alone does not establish availability. Profile doctor offers more detailed checks for one account. For project-specific handoff readiness, run `relay doctor` in the project.

## Sign in again

```
relay login claude-work
```

An existing profile already knows its provider. Use the same name to reauthenticate, then rerun `relay doctor`.

```
relay logout claude-work
```

Logout uses the provider’s own logout command for that profile. It does not remove the profile from Relay’s registry.

## How isolation works

Claude profiles use separate `CLAUDE_CONFIG_DIR` directories; Codex profiles use separate `CODEX_HOME` directories. Provider authentication remains provider-owned. Relay stores profile references and non-secret identity information, never an authentication token or password.

Different names do not make the same account two distinct fallbacks. Relay checks account identity, and duplicate provider identities are rejected.

Only one Relay-managed interactive Codex session can use a given `CODEX_HOME` at a time. Relay acquires a profile lock before creating or driving a Codex thread. This does not serialize file edits across sessions or prevent a manually launched process outside Relay.

## Reuse an existing Claude login

Setup can reuse an isolated Claude configuration. Advanced users can preview profile registration by reference:

```
relay profile adopt claude-work --provider claude --config-dir /absolute/path/to/claude-config --dry-run
```

For Claude’s native default account, use the dedicated mode:

```
relay profile adopt claude-default --provider claude --native-default --dry-run
```

Review the result, then rerun without `--dry-run` to register it. Native-default authentication differs from explicitly pointing `CLAUDE_CONFIG_DIR` at the same path; use the option that matches how Claude actually runs.

Profile registration alone does not adopt a running conversation. See [bringing an existing conversation under Relay](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#existing).
