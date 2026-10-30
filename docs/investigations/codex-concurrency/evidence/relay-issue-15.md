# Explore structured Codex events for event-driven exhaustion detection

https://github.com/RA1NM4KER/agent-relay/issues/15

## Goal

Investigate whether Relay can detect Codex usage-limit exhaustion from **structured events emitted by the same Codex runtime that owns the interactive session**, so Codex handoff can become primarily event-driven rather than relying on aggressive near-limit polling.

This is a research/prototype issue.

Issue #13 remains the production baseline and should not be weakened or removed until this approach is proven end-to-end.

## Why

Relay currently has asymmetric exhaustion detection:

```text
Claude
  StopFailure hook
  -> immediate structured evaluation
  -> handoff

Codex
  structured rate-limit polling
  -> 120s normally
  -> 15s near 90%
  -> 5s near 98%
  -> ordinaryUsageAllowed=false
  -> handoff
```

#13 made Codex detection much faster and remains correct, but near-limit polling still means repeatedly launching structured `account/rateLimits/read` checks.

A cleaner long-term model would be:

```text
Codex structured event
        ↓
immediate authoritative account/rateLimits/read
        ↓
ordinaryUsageAllowed == false
        ↓
existing handoff path

slow polling remains only as a watchdog
```

The trigger does not need to be authoritative. It only needs to wake Relay up cheaply and reliably.

## Signals to investigate

Current Codex surfaces appear to include structured app-server/session events such as:

- `account/rateLimits/updated`
- structured error information such as `usage_limit_exceeded`
- thread/session event subscriptions exposed by the app-server protocol

Determine which of these are actually observable by Relay from the **same runtime** used by the interactive Codex TUI.

Do not assume documentation/schema presence means Relay can subscribe to it in the normal TUI architecture. Prove the actual process/runtime topology.

## Key research questions

### 1. Can Relay observe `account/rateLimits/updated` from the interactive Codex runtime?

Current Relay launches the normal Codex TUI as a child and separately performs one-shot `codex app-server` reads.

Investigate whether the TUI's runtime exposes a supported way for an external observer to receive its live rate-limit notifications.

If not, determine whether running:

```text
Relay
  -> codex app-server --listen ...
       -> Codex TUI via --remote
       -> Relay passive observer/subscriber
```

is a supported and practical architecture.

### 2. Can Relay observe structured `usage_limit_exceeded` events?

Codex internally distinguishes usage-limit errors from ordinary failures.

Investigate whether a supported app-server/thread/session subscription exposes a structured error classification when a real interactive turn hits quota.

Prefer:

```text
codex_error_info = usage_limit_exceeded
```

or another stable enum/event over parsing human-facing text.

### 3. Does passive observation preserve normal TUI behavior?

Any prototype must verify:

- interactive terminal behavior remains normal;
- resize/signals/raw mode remain Codex-owned where possible;
- no PTY proxy is introduced merely for observation;
- existing Codex auth/profile isolation still works;
- native thread/session IDs remain correct;
- resume works;
- Relay ownership/process verification still works;
- source stop and cross-provider handoff still work;
- no extra writer/process is accidentally created.

### 4. Can one event trigger immediate authoritative verification?

The intended safety boundary is:

```text
structured event/hint
        ↓
account/rateLimits/read
        ↓
ordinaryUsageAllowed == false
        ↓
handoff
```

The event itself must NOT become the authority for exhaustion unless Codex documents an equally strong structured contract.

Keep the existing rule:

- `ordinaryUsageAllowed == false` = authoritative exhaustion
- `true` = ordinary use still allowed
- `null` / malformed / contradictory / unavailable = UNKNOWN, inert

### 5. Can #13 polling become only a watchdog?

If event-driven detection is proven reliable, evaluate simplifying the default Codex polling policy to something like:

```text
structured event -> immediate verification
120s poll         -> watchdog only
```

Do not remove or relax #13 during this issue's research phase.

The event path must first survive a real natural exhaustion end-to-end.

## Explicitly avoid

Do not solve this research issue by:

- scraping Codex TUI text;
- parsing ANSI screen output;
- reading rollout/transcript JSONL files as an undocumented event bus;
- matching English error strings;
- launching an extra model turn;
- polling every tool call through hooks;
- weakening `ordinaryUsageAllowed == false` authority;
- weakening Relay's process identity / ownership checks;
- introducing a permanent daemon;
- changing stable release behavior.

Text-triggered verification is a separate fallback idea if structured events prove inaccessible. This issue should first exhaust the supported structured options.

## Prototype plan

Build the smallest disposable experiment necessary to answer the runtime questions before changing production architecture.

Suggested sequence:

1. Start an isolated Codex app-server using a test profile.
2. Connect an interactive Codex TUI using the supported remote/app-server mechanism, if available.
3. Attach a second passive observer/subscriber.
4. Verify the observer receives ordinary live thread/session events without interfering with the TUI.
5. Verify whether `account/rateLimits/updated` arrives during normal turns.
6. Record the exact sanitized event shape.
7. Verify whether a structured usage-limit error classification is emitted when quota naturally exhausts.
8. On the relevant event, immediately call the existing authoritative `account/rateLimits/read`.
9. Measure:
   - event emitted -> Relay sees event
   - Relay sees event -> authoritative read completes
   - authoritative read -> handoff begins
10. Compare against #13's adaptive polling latency and overhead.

Do not intentionally burn quota to manufacture exhaustion. Use a natural future exhaustion for the final live proof.

## Required evidence

The research report should clearly classify each candidate mechanism as:

- **supported and live-verified**
- **supported but not yet naturally exhaustion-verified**
- **schema exists but inaccessible from Relay's runtime**
- **undocumented/unsupported, reject**

Include exact Codex version tested.

## Acceptance criteria

This issue is successful if it produces a defensible answer to:

> Can Relay receive a supported structured Codex exhaustion/rate-limit event from the same interactive runtime cheaply enough to use as the primary wake-up signal?

If YES:

- prototype proves passive observation;
- event delivery does not disturb the TUI;
- immediate authoritative verification works;
- existing ownership/session guarantees remain intact;
- create a separate implementation issue before replacing #13 polling behavior.

If NO:

- document precisely why each structured route is inaccessible or insufficient;
- keep #13 as the production design;
- avoid revisiting the same dead ends later.

## Relationship to #13

#13 is implemented and remains the current production solution.

This issue is not a rewrite of #13. It investigates whether Codex can eventually match Claude's cleaner event-driven model, with #13 retained as a watchdog/fallback if appropriate.

