# Investigate zero-poll Codex supervision via events and reconciliation

https://github.com/RA1NM4KER/agent-relay/issues/17

## Goal

Follow-up to #13/#15/#16. Investigate whether Relay can safely eliminate timer-driven periodic Codex `account/rateLimits/read` polling entirely, replacing it with:

- structured events (`usageLimitExceeded`, `account/rateLimits/updated`) during normal operation
- one-shot authoritative reconciliation reads at deterministic lifecycle points (startup/resume, observer reconnect, event-triggered verification)
- local liveness checks (process/connection alive) for non-quota failure modes

Target end-state under evaluation:

```
NORMAL OPERATION: event stream healthy -> no provider reads, no quota polling
EXHAUSTION:        usageLimitExceeded -> immediate account/rateLimits/read -> ordinaryUsageAllowed==false -> handoff
STARTUP/RESUME:    one account/rateLimits/read -> establish state -> attach observer
OBSERVER RECONNECT: reconnect -> one account/rateLimits/read -> reconcile -> continue event-driven
```

No 120s/15s/5s timer loop, if — and only if — every case periodic polling currently covers has either a structured event, a deterministic reconciliation point, or a local-failure-triggered recovery.

## Explicitly out of scope for this issue

- Do not remove polling during this research pass — #13's adaptive polling stays exactly as-is until this is proven and a separate implementation issue is scoped.
- No text/ANSI/PTY/rollout scraping.
- No release/version/Homebrew changes.

## Decision standard

Recommend zero periodic polling only if every practical case has (1) a structured event, (2) a deterministic lifecycle reconciliation point, or (3) a local failure signal with safe recovery. If a realistic case remains where Codex is exhausted, work is blocked, no event fires, and no lifecycle reconciliation point occurs, periodic polling keeps a defensible role.

Research comment with findings to follow on this issue.
