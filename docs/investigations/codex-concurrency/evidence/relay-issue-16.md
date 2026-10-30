# Wire structured Codex exhaustion events (usageLimitExceeded + rateLimits/updated hint) behind #13's watchdog

https://github.com/RA1NM4KER/agent-relay/issues/16

## Background

Follow-up to #15's research pass (see [that issue's findings comment](https://github.com/RA1NM4KER/agent-relay/issues/15#issuecomment-5848432815)). Live-verified against `codex-cli 0.155.0`:

- `account/rateLimits/updated` is real, observable, and correctly withholds `ordinaryUsageAllowed`/`accountId` (hint-only by Codex's own protocol design).
- The turn-level structured error enum `usageLimitExceeded` (on `TurnError.codexErrorInfo`) is schema-verified and is the better primary wake-up trigger than `account/rateLimits/updated`, since the latter is a turn-completion side effect (doesn't fire while idle), not a standalone heartbeat.
- Passive multi-client observation of the same runtime works, but only via an explicit `thread/resume` ("rejoin") call per thread — not automatic broadcast.
- Profile isolation (`CODEX_HOME`) transfers cleanly to this architecture.
- **Process ownership does not transfer cleanly** — this is the load-bearing risk in this issue.

**#13 (adaptive polling) is unaffected and remains the sole production mechanism until this survives a real natural exhaustion end-to-end.** Nothing here reduces or replaces #13's cadence unless/until that happens.

## Scope

1. **Compound process-identity/liveness redesign.** Today `ProcessIdentity` tracks exactly one child pid (the interactive provider process) and every liveness/ownership check assumes that pid *is* the session. An external-app-server + `--remote` TUI topology is two children:
   ```
   relay
     ├─ codex app-server --listen ...   (the actual runtime/session owner)
     └─ codex --remote ...              (the user-facing TUI, a pure protocol client)
   ```
   Design and implement a compound liveness check (both children alive *and* the app-server responsive) before any writer-lease decision is made against this topology. Fail closed on any inconsistency between the two, exactly as the current single-pid model already fails closed on ambiguity.

2. **Explicit, tested app-server lifecycle ownership.** Confirmed live: nothing about client disconnects (including the TUI's) terminates an externally-listening `codex app-server`. Relay must own start (when supervision begins) and guaranteed termination (when supervision ends, *including* on Relay's own abnormal exit) — likely via `process_group` binding, with the **opposite** intent from `auto_handoff::spawn_detached`'s existing use of it (that call deliberately decouples a watcher from its parent; this needs the app-server tied *to* Relay so it can never outlive it). No orphaned daemon, ever — this is a hard AGENTS.md invariant ("Relay remains non-daemon").

3. **Transport**: use a short-path `unix://` socket with restrictive file permissions under a fixed, short base path (not nested under a profile's own state directory — hit real `SUN_LEN` failures in the research pass). Do **not** use unauthenticated loopback `ws://`: confirmed live that any local process can attach to an open loopback port with no token, a real new isolation gap versus today's private-stdio-pipe model. If `ws://` is ever used instead, mandatory `--ws-auth capability-token` (or shared-secret) even on loopback.

4. **Primary trigger wiring**: the turn-level `usageLimitExceeded` structured error (delivered via the `error`/`turn/completed` notification path) as the primary wake-up signal — the Codex analogue of Claude's `StopFailure` hook. `account/rateLimits/updated` feeds a secondary/opportunistic cadence hint only (tighten the watchdog as usage climbs), never a trigger on its own. Both routes converge on the unchanged existing rule: event → immediate `account/rateLimits/read` → `ordinaryUsageAllowed == false` → existing handoff. Neither event may ever become authoritative.

5. **#13 stays the watchdog.** Keep adaptive polling exactly as-is as the fallback/safety net for the case this doesn't fire (e.g., quota exhausts while genuinely idle, no turn in flight — confirmed live that `account/rateLimits/updated` does not fire in that case).

## Explicitly out of scope for this issue too (carried over from #15)

- No terminal-output/ANSI/rollout-JSONL parsing.
- No weakening of `ordinaryUsageAllowed == false` authority or UNKNOWN semantics.
- No use of `codex app-server daemon` / `remote-control`'s shared multi-surface daemon system — not investigated, likely incompatible with per-profile isolation.
- No closing this out, and no reducing #13's role, until a real natural exhaustion is observed end-to-end through the new path.

## Open questions from the research pass that need resolving here

- The `thread/resume`-then-immediate-`turn/start` race noted in #15 (§5): an observer's resume racing a separate connection's turn/start once produced a silent no-op turn (no error, no progress). Needs an isolated repro and root-cause before any production sequencing relies on "subscribe, then drive" being safe.
- Millisecond-precision latency (notification emit → Relay receipt) wasn't instrumented in the research pass — needed before claiming a specific latency win over #13's ~0.7–1.7s read.
- Full interactive-TUI rendering fidelity (resize, raw mode, signals) under `--remote` wasn't conclusively demonstrated in a real terminal — the research pass only proved the protocol layer, via a non-TTY harness.

## Acceptance criteria

- Compound liveness model implemented, tested, fails closed on any child/app-server inconsistency.
- App-server process lifetime provably tied to Relay's own supervision lifetime — no orphan possible, including on crash (test: kill Relay's supervising process abnormally, assert no `codex app-server` survives).
- Transport uses a short-path authenticated/permissioned `unix://` socket; no unauthenticated network-visible surface.
- `usageLimitExceeded` wired as primary trigger, `account/rateLimits/updated` as secondary hint only; #13's adaptive cadence unchanged and still runs as watchdog.
- Normal gate green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **Not closed** until one real, natural exhaustion is observed to flow end-to-end through the new path (event → read → handoff) in production dogfooding — mirroring #13's own still-open final item.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

