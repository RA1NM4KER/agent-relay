# Troubleshooting

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html

Diagnose readiness, unavailable profiles, and interrupted handoffs.

Handbook examples are verified against stable v0.5.2.

Start in the affected project directory. These commands help separate configuration problems from the decision for a particular conversation.

```
relay status --live
relay doctor
relay why
relay history
```

- [No handoff](https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html#not-switching)
- [Project trust](https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html#trust)
- [Authentication](https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html#auth)
- [Recovery](https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html#recovery)

## “Why didn’t Relay switch?”

Run `relay why`. Relay needs verified exhaustion of the current owner and an eligible target. A transient rate limit, stale snapshot, or unknown usage reading does not qualify.

1. Check that the conversation is managed with `relay status`.
2. Run `relay doctor` and resolve each reported blocker.
3. For Claude, inspect the integration with `relay integration claude status --profile <name>`.
4. Use `relay profiles` to confirm the expected fallback is registered, then review your order with `relay setup`.

For several sessions, scope the explanation explicitly:

```
relay why --session <relay-session-id>
```

## “All my profiles are exhausted”

A different profile name does not create more quota. If every eligible account is exhausted, Relay has nowhere to transfer the work.

Wait for the provider’s reset, or add another legitimately available account using `relay setup`. Recheck status and readiness before resuming or switching. Clearing local automation state does not reset provider limits.

## “Doctor says project trust is missing”

Each configured profile needs verified trust for the exact project directory. Being logged in is not sufficient.

1. Run the command printed next to the blocker to open that exact profile in the project.
2. Review and answer the provider’s own trust prompt.
3. Exit when finished and rerun `relay doctor`.

Relay never writes trust acceptance. Missing or malformed trust records cannot pass the check.

## “My profile is logged out”

```
relay login <profile-name>
relay doctor
```

Sign in through the provider flow. Normal launch and resume do not open login automatically. For a recognized revoked or invalid Codex login, follow the reported `relay logout <profile>` then `relay login <profile>` recovery. Unknown usage remains unavailable; timeouts or unsupported responses do not by themselves mean login is needed. If Relay reports an unexpected account identity, verify which account you selected rather than renaming profiles or copying credentials between directories.

## “This Codex profile is already in use”

Relay permits one live interactive Codex session per isolated profile. Return to the existing terminal or choose another eligible Codex profile. Do not remove its lock file. Separate Relay sessions still need separate worktrees if they require file isolation.

## “Resume opens the wrong thing—or nothing”

Use `relay status` to inspect sessions, then select the exact Relay session:

```
relay resume --session <relay-session-id>
```

If the old conversation was never managed, use `relay claude --resume` instead. If the session is still active in another terminal, return to that terminal; Relay will not start a second owner. An empty Claude session that never received a first message may have no transcript to resume.

## “The command or flag isn’t recognised”

```
relay --version
relay --help
relay status --help
```

This handbook targets v0.5.2, which includes `status --live`, `refresh`, `mode`, `state`, and `task`. If your installation is older, review the release notes and [upgrade instructions](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html#update). Verify the executable on your PATH when stable and development builds coexist.

Inside Codex, use `$relay`; inside Claude, use `/relay`. In a shell, use plain `relay`. If a provider option is rejected, verify whether it belongs after `--` and whether it would override something Relay must manage.

## “The in-agent command is missing”

Check the integration for the exact profile:

```
relay integration claude status --profile <claude-profile>
relay integration codex status --profile <codex-profile>
```

Run the matching install command if needed, then reopen the provider through Relay. Already-running processes may not discover newly installed commands or skills. Relay preserves user-edited integration files; inspect the reported conflict rather than assuming an install overwrote them.

## “A handoff was interrupted”

Relay journals each transfer and can recover incomplete transactions before subsequent work. If it cannot safely reconcile the state, follow the specific recovery command it reports.

```
relay handoff status <transaction-id> --project-dir /path/to/project
relay recover <transaction-id> --project-dir /path/to/project
```

A transaction ID identifies a handoff attempt, not a conversation. Recovery checks locks, processes, and artifacts before choosing a next step. Do not delete lock or transcript files to get past a refusal.

**Use acknowledgement only after verifying the condition.**`--acknowledge` records that you have confirmed no target process is still running for a recovery-required transaction. It is not a general “force” flag. Use the reported recovery guidance and inspect the journal first.

## “The target transcript conflicts”

Relay refuses to overwrite a divergent or active conversation. Start with the read-only conflict inspection described in [Advanced use](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#conflicts). A proven stale copy and a copy containing unique conversation turns need different treatment.

## Report a reproducible issue

Include your Relay and provider versions, operating system, the command you ran, the error category, and whether the session was new, resumed, or adopted. Review any diagnostic output before sharing it; omit credentials and private conversation content.

[Open an issue on GitHub ↗](https://github.com/RA1NM4KER/agent-relay/issues)
