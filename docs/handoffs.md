# Handoffs & capabilities

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/handoffs.html

Understand automatic handoff, session continuity, and current limits.

Handbook examples are verified against stable v0.5.2.

Relay can move a conversation when you request a switch or when it verifies that the current account is exhausted. Both paths use a journaled, recoverable handoff.

## What happens at a usage limit

1. **Verify the limit.** Relay uses structured provider signals. A temporary rate-limit blip or matching words in terminal output are not enough.
2. **Check the fallback order.** Profiles must be eligible, authenticated, and available. Ambiguous evidence cannot establish availability.
3. **Stop and verify the source.** Relay prevents two owners from serving the same managed conversation.
4. **Continue and verify the target.** The handoff is complete only after the target is positively verified. A supervised terminal follows the new owner.

Claude’s installed usage integration supplies limit events and usage snapshots. Codex usage is checked through its structured app-server interface before entering the terminal. On a live-verified Codex version, local rollout observation wakes a usage check when a structured exhaustion event appears. Periodic provider reads are suppressed while observation is healthy; adaptive polling covers unverified versions or lost observation coverage. Only the authoritative usage verdict establishes exhaustion, never a percentage or the local event alone.

No eligible fallback means no successful transfer. Relay does not turn an unknown profile into an available one or bypass a provider’s limits.

## What carries over

| Handoff | Continuation | What to expect |
| --- | --- | --- |
| Claude → Claude | Session continuation | The same native Claude conversation under the target account. |
| Claude → Codex | State continuation | A new native session seeded from a Relay state bundle. |
| Codex → Claude | State continuation | A new native session seeded from a Relay state bundle. |
| Codex → Codex | State continuation | A new native session, even though the provider stays the same. |

A state bundle captures deterministic repository and git facts, plus bounded recent conversation context at handoff time. If the active writer has recorded [advisory working notes](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#working-state), those can accompany the bundle. It is not the full original provider conversation, and it does not transfer provider prompt caches.

Reopening a dormant session with `relay resume` is different: it resumes that session’s current native Claude conversation or Codex thread on the last profile.

## Which account is next?

You choose one priority order in `relay setup`. Relay rechecks that order when the current owner is blocked and selects the first eligible profile. Exhausted profiles remain unavailable until their recorded reset conditions allow reconsideration.

The current owner is **sticky**: if a higher-priority profile becomes available again, your work stays where it is. Use `relay switch <profile>` when you want to move explicitly.

```
relay doctor
relay why
```

Doctor answers “Is this project ready?” Why explains the current automatic-handoff decision using the evidence Relay actually has.

**Readiness and eligibility are separate questions.** Doctor reports whether configured profiles make automatic handoff ready. A particular transfer still requires verified source exhaustion and an eligible target at handoff time.

## What Relay protects

- **One owner per Relay session.** Ownership, process identity, and target verification must all agree.
- **Provider-owned authentication.** Credentials are never copied between profiles.
- **Recoverable transactions.** Interrupted handoffs have a durable journal for recovery.
- **Fail-closed decisions.** Unknown or contradictory evidence blocks a transfer.

Inspect the [ownership architecture](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/architecture.md) and [security model](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/security.md) for the verification boundaries and recovery rules.

Multiple sessions can still edit the same repository. Relay’s ownership guarantee applies to each conversation, not to every file in your working tree.

## Current limits

- **macOS-first.** Linux builds and passes CI tests but is not yet live-supported or validated. Windows is not supported.
- **No automatic fail-back.** Profiles that reset do not reclaim your session.
- **No quota pooling.** Each account’s provider limits still apply.
- **Verification uses a small real API turn.** A handoff is not completely usage-free.
- **No live unmanaged Codex adoption.** Start through Relay to get supervision.
- **No transfer of provider prompt caches.** State continuation is bounded context in a new native session.
- **Claude usage snapshots need freshness.** A headless session can leave them stale; stale evidence cannot trigger a handoff.

The v0.5.2 release records an end-to-end live automatic handoff from real Codex exhaustion to a healthy Claude fallback on macOS with Codex CLI 0.156.0. Codex → Codex transfer remains covered by fake-provider tests. See the [tagged validation notes](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/README.md) and [detection design](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/automatic-handoff.md) for the precise coverage.
