## https://github.com/RA1NM4KER/agent-relay/issues/18#issuecomment-5850467649

## Phase 1 implementation report

**Status: shipped.** Both gaps #17 identified are closed. Periodic provider polling (#13) is **not** removed or reduced — that remains gated on real dogfood validation and one real natural exhaustion, per this issue's own acceptance criteria. Commit: `a90ed6a`.

### Observer liveness mechanism

`ObserverEvent` gains a third variant: `Disconnected(DisconnectReason)`, where `DisconnectReason` is `Closed` (a completed WebSocket close) or `ReadError` (anything else — a transport error, a protocol violation, or the channel itself reporting the sender is gone). The background reader thread sends this as its *last* act before returning, on every exit path. `Observer::drain_events` additionally synthesizes `Disconnected(ReadError)` if it ever sees the channel report disconnected without having already drained an explicit sentinel (covers a thread panic that never got to send one), and coalesces repeats via a `Cell<bool>` guard so a caller sees it exactly once, not on every 300ms tick forever. This is deterministic local detection — never inferred from "no events recently," which was the actual gap.

### Reconnect state machine

`EventDrivenRuntime` now owns an explicit `AttachState`:

```
Attaching { outcome_rx, stop_flag }  ──▶ Attached(Observer)  ──▶ (Disconnected) ──▶ Attaching { … same known thread id }
        │                                                                                      ▲
        └────────────────────────── (stop_flag set / app-server confirmed dead) ──▶ Dead ◀──────┘
```

A background worker (`attach_worker_loop`) does the actual retrying: it calls `Observer::attach` with whatever thread id is currently known (`None` only ever on the very first attempt of a fresh launch), and on `ObserverError::ResumeNotYetReady(id)` remembers `id` for the next attempt rather than re-waiting for a `thread/started` broadcast that will never repeat. Backoff is local-only (500ms → 3s cap, reset per worker spawn — a fresh call, fresh local variable, no explicit reset code needed), checked against a shared stop flag every 100ms so `EventDrivenRuntime::stop()` is noticed promptly without a tight loop. The worker gives up (sends `GaveUp`, terminal `Dead` state) only when `stop_flag` is set or the app-server's own `ProcessIdentity` is *confirmed* (not merely unconfirmable) gone — an ambiguous liveness reading never stops the retry, since giving up prematurely on a mere `ps` hiccup isn't a safety decision, just an over-eager one.

`EventDrivenRuntime::start()` is now non-blocking beyond the app-server spawn itself: it kicks off the worker and returns immediately, so a fresh launch's arbitrarily-long wait for the user's first message never blocks the interactive command from starting (`remote_args()` only ever depended on the app-server's own endpoint, never the observer).

### Reconciliation-read behavior

`CodexPollScheduler::request_reconciliation()` is new — semantically distinct from `notify_event`'s `UsageLimitExceeded` handling (a reconnect succeeding isn't itself a usage signal), but sharing the exact same guard: ignored while disabled, ignored while a poll is already in flight. `EventDrivenRuntime::tick()` calls it exactly once, the moment a worker reports a successful attach — whether that's the very first attach or the Nth reconnect after a gap of unknown length. Live-verified (see below) that this correctly picks up the remainder of a turn missed mid-flight, matching #17's own Python research finding that the gap's *length* never matters, only that a fresh authoritative read happens once it ends.

### Single-flight proof

Tested directly: a usage-limit event and a reconciliation request arriving together while an evaluation is already in flight both no-op (`a_usage_limit_event_and_a_reconciliation_request_together_still_queue_only_one_evaluation`), on top of the pre-existing single-poll-in-flight guarantee, unchanged.

### Fresh-thread activation

`Observer::attach(_, _, None)`'s existing broadcast-learning path is now actually reachable from production (`EventDrivenRuntime::start` no longer requires a known thread id up front). Live-verified against `codex-cli 0.155.0`: a real driver creates a fresh thread on an app-server that has never had any other thread, and a real `Observer` attached with no thread id learns it from the broadcast and successfully resumes exactly once the driver's first turn actually starts — receiving that turn's full completion signal, including the sanitized `account/rateLimits/updated` hint.

### Cleanup / lifecycle

`EventDrivenRuntime::stop()` now handles all three states: an attached observer is stopped normally; a still-attaching worker is signalled (not joined — its own bounded per-attempt timeout guarantees it exits on its own; joining here would risk blocking ordinary shutdown); `Dead` is a no-op. Every live test confirmed no leftover `codex app-server` process or socket after the run.

### #13 — confirmed unchanged

Not touched. `poll_interval_secs`, the 120s/15s/5s table, `RELAY_CODEX_POLL_SECS` semantics, the always-running timer tick — identical to before this commit. The full workspace test suite (`cargo test --workspace`) passed with zero changes needed to any existing #13 test.

### Tests / gate

67 tests in `relay-provider-codex` (12 in `observer` alone, 3 of them real-binary live tests), 37 in `relay-cli` touching `codex_poll`/`codex_runtime`. `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`, `git diff --check` all green. One pre-existing, unrelated concurrency flake (`m6_auto::a_switch_request_whose_target_fails_preflight_leaves_the_source_alive`) surfaced once during a full-suite run, passed cleanly in isolation and on a full-suite rerun — not touched, not caused by this change, consistent with AGENTS.md's own note about known process-identity concurrency flakes under load.

### Live validation

Three `#[ignore]`d tests against the real `codex-cli 0.155.0` binary and a real authenticated test profile:
- `live_smoke_test_against_the_real_app_server` (unchanged from #16, still green)
- `live_reconnect_after_disconnect_against_the_real_app_server`: attach → abrupt kill → a second real turn starts → reconnect races it → receives the remainder including the real rate-limit hint
- `live_fresh_thread_activation_against_the_real_app_server`: attach with no known thread id against a brand-new app-server → learns the real broadcast → retries resume until a real driver's first turn starts → receives that turn's completion

No quota was intentionally burned; all turns were trivial single-word replies.

### #18's future zero-poll plan (clarifying scope, per this issue's own text)

The long-term target remains **removing** periodic `account/rateLimits/read` polling entirely — timer-driven `Tick`, the 120/15/5 cadence table, the periodic read itself — not merely slowing it to a ceremonial watchdog. What stays regardless: the one authoritative read, single-flight evaluation, startup/resume/switch reconciliation, and now event- and reconnect-triggered verification. That final removal is **not** part of this phase and requires, per this issue's own acceptance criteria: real dogfood use of both pieces shipped here, and one real natural Codex exhaustion observed end-to-end through this path (event → read → `ordinaryUsageAllowed == false` → handoff) — still outstanding, exactly as #15 and #16 both already left open.

Leaving #18 open. #13, #15, #16 all remain open and untouched.

🤖 Generated with [Claude Code](https://claude.com/claude-code)


## https://github.com/RA1NM4KER/agent-relay/issues/18#issuecomment-5860161114

## Natural exhaustion dogfood result (2026-09-27)

A real Codex exhaustion happened while `RELAY_CODEX_EVENT_DRIVEN=1` was enabled, with two concurrent Relay-managed Codex sessions on the same account.

### What is now proven

For Relay Session `302f387c-b34e-487e-b408-b4c4863c582c`:

- Codex exhaustion was authoritatively corroborated at ~21:42:59 UTC.
- Relay selected `claude-main` at 21:43:01.
- The Codex -> Claude handoff completed at 21:43:05, ~4.2s total after fallback selection.
- Claude received the transferred context and immediately continued substantive repo work autonomously, correctly identifying already-completed work, verifying CI/dogfood status, and stopping only at a live-credential validation boundary.

So natural exhaustion -> authoritative verification -> cross-provider handoff -> autonomous continuation is live-proven.

### Important limitation: event causality is NOT yet proven

The durable trace currently labels the evaluator trigger as `codex_poll` regardless of whether the scheduler became due because of:

1. the periodic adaptive timer, or
2. `ObserverEvent::UsageLimitExceeded` calling `notify_event`.

For the agent-relay project, the final trigger sequence was:

```text
21:42:51.320  codex_poll
21:42:52.045  source usage completed: NearLimit
21:42:59.067  codex_poll
21:42:59.803  source usage completed: Exhausted
21:43:01.202  target selection / handoff proceeds
```

The final trigger arrived 7.747s after the previous trigger. That is consistent with the current critical cadence: roughly 5s after the prior evaluation completes, plus evaluator/runtime overhead.

A second concurrent project shows essentially the same pattern (~7.744s between the last NearLimit trigger and the Exhausted trigger).

Therefore this natural exhaustion **does not prove** that `usageLimitExceeded` caused the authoritative read. The event may have fired, but the current trace cannot distinguish it from the timer and the timing is entirely consistent with the periodic path.

### Required observability patch before the next natural exhaustion

Before removing/reducing periodic polling, make trigger provenance durable without changing routing semantics:

- persist/trace when `ObserverEvent::UsageLimitExceeded` is received;
- distinguish scheduler wake reason:
  - `codex_timer_poll`
  - `codex_usage_limit_event`
  - `codex_reconnect_reconciliation`
- record event timestamp -> authoritative read start -> read result;
- preserve single-flight behavior when an event arrives while a read is already in flight;
- do not add another provider read.

Then the next natural exhaustion can conclusively prove:

```text
usageLimitExceeded
-> immediate authoritative account/rateLimits/read
-> ordinaryUsageAllowed=false
-> handoff
```

Until that proof exists, keep #13 polling unchanged. This run is strong end-to-end handoff/continuation evidence, but not sufficient evidence to remove the periodic timer.


## https://github.com/RA1NM4KER/agent-relay/issues/18#issuecomment-5860479443

## Trigger-provenance observability patch (this task)

**Root cause:** `EventDrivenRuntime::tick` forwarded every decoded `ObserverEvent` to
`CodexPollScheduler::notify_event`/`request_reconciliation`, and both of those only ever set
`next_poll_at = Instant::now()`. The scheduler had no memory of *why* it became due, so
`auto_handoff::plan_poll` always built its `AutoWatchPlan` with the same hardcoded
`trigger: "codex_poll"` — collapsing the periodic timer, a real `usageLimitExceeded` event, and a
post-reconnect reconciliation read into one indistinguishable string in the durable trace. This is
exactly what the 2026-09-27 dogfood comment above found: the final trigger was recorded ~7.7s after
the prior one, consistent with either the timer or the event, and unprovable either way.

**Fix — typed trigger provenance:**

`crates/relay-cli/src/auto_handoff.rs` adds `pub(crate) enum CodexEvaluationTrigger { Timer,
UsageLimitEvent, ReconnectReconciliation }` with `as_str()` mapping to `codex_timer_poll` /
`codex_usage_limit_event` / `codex_reconnect_reconciliation`. `CodexPollScheduler` (`codex_poll.rs`)
now carries a `pending_trigger: CodexEvaluationTrigger` field:

- `notify_event(UsageLimitExceeded)` sets it to `UsageLimitEvent` (unless disabled or an evaluation
  is already in flight).
- `request_reconciliation()` (called on every successful observer attach/reconnect) sets it to
  `ReconnectReconciliation`.
- `RateLimitsHint` never touches it — a scheduling nudge can never masquerade as a real event.
- `tick()` reads and consumes `pending_trigger` (resetting to `Timer`) the instant a cycle becomes
  due, and hands it to the `plan` closure — `terminal_session.rs` forwards it straight into
  `auto_handoff::plan_poll(..., trigger)`, which now sets `AutoWatchPlan.trigger = trigger.as_str()`
  instead of the old hardcoded string. Nothing is ever inferred later from timing.

**Event receipt, recorded at the drain point:** `EventDrivenRuntime::tick`'s `Attached` arm now
calls `auto_handoff::trace_event(project_state_dir, "codex_usage_limit_event_received")` the moment
`UsageLimitExceeded` is drained from the observer — *before* it is ever handed to the scheduler —
via a small pure helper (`event_receipt_trace_label`) that returns `Some(...)` only for that one
variant. A successful (re)attach similarly traces
`codex_reconnect_reconciliation_requested` before calling `request_reconciliation()`.

**Sample durable trace** (`<project_state_dir>/auto-handoff.log`, append-only, one process at a
time per project by the existing single-writer/single-flight invariants):

```
[trace unix_ms=1758999999000] codex_usage_limit_event_received
--- automatic handoff evaluation triggered ---
[trace unix_ms=1758999999010] codex_usage_limit_event
[trace unix_ms=1758999999015] auto_watch_spawned
[trace unix_ms=1758999999820] source_usage_check_started profile=codex-main provider=codex
[trace unix_ms=1758999999900] source_usage_check_completed profile=codex-main provider=codex
[trace unix_ms=1758999999901] exhaustion_corroborated
```
versus an ordinary timer cycle:
```
--- automatic handoff evaluation triggered ---
[trace unix_ms=...] codex_timer_poll
[trace unix_ms=...] auto_watch_spawned
...
```
No account IDs, provider payloads, or conversation content in any of these — only fixed labels and
timestamps.

**Single-flight / coalescing, made honest:** an event arriving while an evaluation is already in
flight (`self.child.is_some()`) still never starts a second read — unchanged — but now traces
`codex_usage_limit_event_coalesced_into_in_flight_evaluation` instead of silently no-op'ing, so the
log never implies an event was missed when it was in fact folded into the read already running.

**Tests added** (35 in `codex_poll.rs`, 9 in `codex_runtime.rs`, 3 in `auto_handoff.rs` — all new or
touched by this patch): timer/event/reconnect each hand their own trigger to the plan closure and
never each other's; a `RateLimitsHint` never sets a pending trigger; an event-triggered cycle resets
back to `Timer` afterward; in-flight coalescing is traced and does not spawn a second read; two
schedulers for different projects never share trigger or trace state (concurrent-session
independence); `trace_event`/`open_log` round-trip correctly for every trigger string; and
`event_receipt_trace_label` proves only `UsageLimitExceeded` is ever receipt-worthy (a timer or
reconnect literally cannot reach that code path, so neither can masquerade as the other).

**Validation:** no natural exhaustion was available to re-trigger, so this was validated with
deterministic scheduler-level unit tests driving the real `CodexPollScheduler`/`EventDrivenRuntime`
code paths (not a rewritten test double) with a real temp-dir-backed `auto-handoff.log`, asserting
the actual file contents. No provider usage was spent and no fake app-server fixture needed
extending for this — the trigger/trace logic sits entirely in Relay's own scheduler, not the
protocol layer #16/#17/#18 already live-verified.

**Polling unchanged:** `relay_provider_codex::polling::poll_interval_secs`, the 120s/15s/5s cadence
table, `RELAY_CODEX_POLL_SECS` semantics (unset/0/N>0), `ordinaryUsageAllowed == false`'s sole
authority, UNKNOWN semantics, fallback selection, and the event-driven feature's off-by-default gate
are all untouched — confirmed by the full existing test suite passing unmodified alongside the new
tests. This is purely additive observability.

**Quality gate:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (all green, 0 failures), `git diff --check` — all clean. Commit
`e52537d` on `main`; CI green
(https://github.com/RA1NM4KER/agent-relay/actions/runs/36355567523); dogfood build green and
published as `0.4.2-dev.142-e52537d`.

**Remaining requirement before periodic polling can be removed:** unchanged from this issue's own
standing acceptance criteria — one more real natural Codex exhaustion, observed end-to-end, whose
trace can now actually be inspected to *prove* (not infer from ~7-8s proximity) whether
`codex_usage_limit_event` or `codex_timer_poll` was the deciding trigger. This patch makes that
proof possible; it does not itself constitute that proof. Leaving #18 open, and not touching
#13/#15/#16.


## https://github.com/RA1NM4KER/agent-relay/issues/18#issuecomment-5860754508

## PROVIDER-LEVEL LIVE PROOF (research-scoped, not a full Relay end-to-end run)

This is a narrower, provider-protocol-only finding — not a claim that this satisfies this
issue's own standing full-Relay-path requirement (see below).

**Question answered:** does a REAL, currently-exhausted Codex account's real app-server turn
actually emit the structured `usageLimitExceeded` notification this crate's `Observer` decodes
as `ObserverEvent::UsageLimitExceeded`? Until now this was only ever proven against a scripted
fake server.

**Method:** a new `#[ignore]`d test,
`relay-provider-codex::observer::tests::live_exhausted_turn_emits_usage_limit_event`, added
beside the existing real-app-server live tests in `crates/relay-provider-codex/src/observer.rs`
(built and run from a disposable worktree at `main@e52537d`, never merged). It:

- spawns a real `codex app-server` under the real, currently-exhausted `codex-main` profile's
  own `CODEX_HOME` (via `RELAY_BENCH_CODEX_HOME`, exactly like the existing live tests);
- creates one brand-new thread and sends exactly one trivial turn (`Reply with exactly: OK`),
  using the same fresh-thread attach/retry sequencing as
  `live_fresh_thread_activation_against_the_real_app_server`;
- attaches the real, unmodified `Observer` to that thread;
- drains its decoded events and reports exactly what arrives — success or failure, never
  substituted.

No Relay CLI, preflight, routing, reconciliation, or handoff code was touched or exercised —
this is the provider crate's `Observer` in isolation, against the real backend.

**Result — real proof:**

```
turn/start was accepted; waiting for the async notification stream.
saw_rate_limits_hint=true
LIVE PROOF: ObserverEvent::UsageLimitExceeded received (4.354120334s after the turn attempt)
test observer::tests::live_exhausted_turn_emits_usage_limit_event ... ok
```

- `turn/start` itself was accepted (an empty JSON-RPC result, not an immediate error) — the
  real rejection arrives asynchronously as the `error` notification, exactly as this crate's
  `read_loop`/`events` module already assumed.
- `ObserverEvent::UsageLimitExceeded` was received **4.35s** after the turn was sent.
- `account/rateLimits/updated` (`RateLimitsHint`) also fired independently, as expected.
- No account IDs, credentials, or conversation content were printed or logged — only the
  decoded event, a boolean, and local elapsed time.

**What this confirms:** the one assumption #16/#17/#18's whole event-driven design rests on —
that a real exhausted turn really does produce the real structured notification Relay's
Observer expects — is now live-verified against `codex-cli 0.155.0`, not just a scripted fake
server.

**What this does NOT confirm:** this did not exercise `CodexPollScheduler`, the
`e52537d` trigger-provenance patch, the durable auto-handoff trace, or an actual automatic
Codex → Claude handoff. Per this issue's own standing acceptance criteria, that still requires
one real natural exhaustion observed end-to-end through the full Relay path (event → scheduler
wake reason → authoritative read → `ordinaryUsageAllowed == false` → handoff), which remains
outstanding. A separate, careful attempt at that full-path live validation using this
*already*-exhausted account was considered and explicitly abandoned when it required a
temporary bypass of Relay's own exhaustion-preflight and reconciliation safety checks — even
scoped as env-flag-gated, worktree-isolated, and never merged, that class of change was judged
too close to a safety-relevant bypass to build, and was not attempted.

**Test disposition:** not committed. It is a clean, well-scoped, ignored-by-default research
fixture consistent with this file's existing live-test conventions (same env-var gate, same
sanitization discipline, same fresh-thread retry pattern) and arguably worth keeping
permanently for future re-verification against new Codex CLI versions — but that is a judgment
call for a maintainer to make deliberately, not something to merge silently as a side effect of
this research. Currently exists only in a disposable local worktree.

🤖 Generated with [Claude Code](https://claude.com/claude-code)


## https://github.com/RA1NM4KER/agent-relay/issues/18#issuecomment-5860876146

## Live-proof fixture retained in-tree

The provider-level live regression test from the previous comment
(#issuecomment-5860754508, `observer::tests::live_exhausted_turn_emits_usage_limit_event`) is now
committed and pushed: `5515688` (`test(codex): add live exhausted event fixture`), CI green.

It remains `#[ignore]`d by default (requires `RELAY_BENCH_CODEX_HOME` pointing at a real,
authenticated, currently-exhausted Codex profile; never runs in the default gate or CI), spends/
attempts real Codex usage only when explicitly invoked, and validates provider-protocol emission
only — it does not exercise `CodexPollScheduler`, trigger provenance, the durable auto-handoff
trace, or an actual Codex → Claude handoff.

No production code changed in this commit.

This is purely retaining the already-proven fixture for future protocol regression checks (e.g.
against a newer Codex CLI version). It does **not** change this issue's standing acceptance
criteria — one real natural exhaustion observed end-to-end through the complete Relay path is
still required before #18 can close, and before #13's periodic polling can be removed. Not
closing #18.


## https://github.com/RA1NM4KER/agent-relay/issues/18#issuecomment-6203282938

Closing: the event-driven zero-poll architecture is now proven against a real natural exhaustion, end to end.

A live Codex 0.156.0 session hit a genuine structured `usage_limit_exceeded` record. The rollout watcher observed it and emitted an event-originated trigger after 18.5 minutes with zero `codex_timer_poll` evaluations while healthy. Relay then performed exactly one authoritative usage read, reached the `ordinaryUsageAllowed=false` exhaustion path, and completed automatic handoff transaction `ho-18dec52cac10e3a8-8995` to `claude-main`.

The completed live run also proved:
- no timer-originated fallback decision;
- no duplicate authoritative usage reads;
- no watcher health regression before exhaustion;
- no thread-transition ambiguity;
- no writer/session conflict;
- unchanged interactive Codex argv (`codex resume <thread-id> --yolo`);
- successful target verification and completed transaction journal.

Two real-world failures found during final validation were fixed before the successful run:
- `deeac2f`: removed the 8 KiB rollout-header parsing ceiling exposed by a real 22,275-byte `session_meta`;
- `3b02ebf`: preserved sparse event-driven lifecycle evidence across `auto-handoff.log` retention.

The final architecture shipped in v0.5.0 and is now the default observation path for verified Codex versions, with adaptive structured polling retained as the safety fallback whenever watcher health is unavailable.

With the final natural exhaustion proving the complete causal chain, #18 is complete.
