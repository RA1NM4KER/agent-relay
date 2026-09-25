# Benchmarks: is Agent Relay actually lightweight?

The README claims Relay is local-first and lightweight. This document exists so that claim rests
on numbers someone else can reproduce, not vibes — every measurement below has a command next to
it that regenerates it. Results are from one run, on one machine, honestly reported including the
one result that turned out worse than expected (see "Codex readiness check", below) — nothing here
was re-run until it looked good.

## Methodology and environment

- **Hardware:** Apple M2, 8 GB RAM.
- **OS:** macOS 26.5.2 (build 25F84).
- **Toolchain:** `rustc 1.98.1 (48a229cea 2026-09-01)`.
- **Build:** `cargo build --release` (`target/release/relay`, 4.4 MB, adhoc-codesigned — the same
  shape Homebrew ships, not a debug build; debug-build dispatch overhead is not representative).
- **Commit measured:** `7f1d8a3` (this document is written as of that commit; re-run the commands
  below after any change that could plausibly move these numbers rather than trusting stale text).
- Every "warm" number below is a real subprocess invocation of the compiled binary — not an
  in-process microbenchmark — because that is what a user actually experiences.
- Nothing here spends real provider quota: the Codex measurements read structured, read-only
  metadata endpoints (rate limits, doctor diagnostics) against an already-authenticated account;
  the Claude-involving measurements use a fake `claude` executable (see
  `crates/relay-cli/tests/benchmark.rs`), never a real account.

## 1. CLI startup latency

```sh
BIN=target/release/relay
/usr/bin/time -l "$BIN" --version   # peak RSS + one invocation's wall time
# warm: see the loop in the "Reproducing these numbers" section below
```

| | Result |
|---|---|
| True cold (binary never executed before, freshly copied) | **~0.2–0.5s real**, but `user`+`sys` both report `0.00s` |
| Warm (binary already executed at least once), n=50 | min **2.53ms**, median **2.82ms**, p90 **4.06ms**, max **4.94ms** |

The "cold" number is **not** Relay's own startup cost: `user`/`sys` time is ~0, meaning the wall
time is spent entirely in the kernel before the process's own code ever runs. This is macOS
Gatekeeper's one-time code-signing scan of a binary path it has never seen executed before —
confirmed by copying the same freshly-built binary to a brand-new path twice in a row and timing
each independently (0.28s, then 0.23s — both "first execution of a new inode", both far above the
warm number, neither improving with re-copies of *identical bytes*, which rules out disk-cache
warming as the explanation). It applies once per unique binary path, not once per process start:
after the very first run of an installed `relay` binary (e.g. the first `relay --version` right
after `brew install`), every subsequent invocation is warm. The warm number — **~2.8ms median** —
is what a user experiences for the rest of that binary's life.

## 2. Idle memory footprint

| | Result |
|---|---|
| One-shot invocation (`relay --version`), peak RSS | **~3.0 MB** (`maximum resident set size` from `/usr/bin/time -l`) |
| `relay claude` steady-state, supervising a live (idle) session, RSS sampled every 500ms over 5s | **2.1–4.8 MB** across two independent runs |

## 3. CPU overhead while supervising an idle session

```sh
cargo test --release -p relay-cli --test benchmark idle_supervision_overhead -- --ignored --nocapture
```

Spawns a real `relay claude` attached to a fake, otherwise-silent `claude` executable, waits for
the supervisor to start ticking (`control/supervisor.json` appears), then samples the `relay`
process's RSS and cumulative CPU time via `ps` every 500ms for 5 seconds.

| Run | RSS range | CPU time over 5s window | Average CPU |
|---|---|---|---|
| 1 | 2176–3952 KB | 0.020s | **0.40%** |
| 2 | 2880–4832 KB | 0.030s | **0.60%** |

This is the cost of the 300ms control-channel poll loop (`terminal_session.rs`) that every
Relay-managed interactive session runs, for **Claude**. A supervised **Codex** session adds one
more periodic cost on top: a structured usage read every `RELAY_CODEX_POLL_SECS` (default 120s) —
see §5 for that read's own real cost (~0.7–1.7s each, so at the default interval it adds well under
0.1% average CPU of its own).

## 4. Handoff latency

```sh
cargo test --release -p relay-cli --test benchmark handoff_latency -- --ignored --nocapture
```

Five consecutive full `relay handoff run` transactions (SESSION_CONTINUATION, alternating between
two fake Claude profiles — stop the source, stage and hash-verify the transcript, launch and verify
the target, move the writer lease), each timed end-to-end as a real subprocess invocation.

| | Result |
|---|---|
| n=5, two independent runs | min **1206–1317ms**, median **1228–1433ms**, max **1276–1835ms** |

This is a **lower bound**: the fake `claude` executable responds instantly, so this number is
Relay's own transaction overhead (checkpoint, quiescence verification, staging, process spawns,
journal writes) — a real Claude CLI's own launch/verification turn adds real time on top that this
number does not capture.

### Per-handoff phase diagnostics

Every new handoff journal now persists a `timings` array with monotonic (`Instant`) elapsed
milliseconds. It is intentionally silent during normal use and contains phase names/durations
only — never bundle content, provider output, or credentials. Inspect it through `relay handoff
status --json` or the session's journal under Relay state. A successful session continuation
records `project_git_checkpoint`, `source_liveness`, `source_preflight`,
`source_stop_and_verification`, `session_staging`, `target_process_launch`,
`target_verification`, and `total_handoff`.

State continuation records the phases that actually apply: `project_git_checkpoint`,
`context_capture`, `continuation_bundle_serialization`, `source_liveness`,
`source_stop_and_verification`, `target_process_launch`, `target_verification`,
`bootstrap_continuation_setup`, and `total_handoff`. `bootstrap_continuation_setup` intentionally
overlaps target launch/verification: it is the user-visible elapsed time spent delivering the
bundle through the target's bounded bootstrap turn, while the other two split at the durable spawn
callback. It is not added to the component phases.

Failures retain `total_handoff` and every completed prior phase. A phase that fails before it
returns may be absent; the journal state/error remains the authoritative failure evidence.

### 4b. Representative STATE_CONTINUATION lower bound

```sh
cargo test --release -p relay-core --test handoff_coordinator \
  state_continuation_latency -- --ignored --nocapture --test-threads=1
```

This runs five complete state-continuation coordinator transactions with deterministic fake ports
and prints end-to-end plus context-capture timings. It is separate from §4 because state
continuation has no session-artifact staging and instead constructs/serializes a bundle and
performs bootstrap setup. It is representative of Relay's own orchestration only, not a claim
about real provider latency; real Codex bootstrap still needs an authenticated live measurement.

## 5. Codex-specific overhead: two very different numbers

Codex exposes two structurally different ways to check a profile, and they were both measured
directly against the real, already-installed `codex-cli 0.155.0` and a real authenticated account.
They are not interchangeable and have very different costs:

### 5a. The periodic supervision poll (fast) — `codex app-server` → `account/rateLimits/read`

```sh
RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<name>/codex \
  cargo test --release -p relay-provider-codex --lib app_server::tests::bench_real_read_rate_limits -- --ignored --nocapture
```

This is the structured, typed, schema-generated call `RELAY_CODEX_POLL_SECS` uses while a Codex
terminal is supervised, and what `relay resume`/`relay codex` use for the pre-launch exhaustion
check.

| | Result |
|---|---|
| n=5, real account | min **686ms**, median **802ms**, max **1699ms** |

### 5b. The readiness/auth check (slow) — `codex doctor --json`

`relay doctor` / `relay setup`'s completion screen / `relay status` all check whether each
registered Codex profile is authenticated via `relay-provider-codex::inspection`, which shells out
to `codex doctor --json` — documented in that module as **the only structured, machine-readable
auth signal Codex exposes** (`codex login status` has no `--json` flag; Relay's own policy is to
never trust unstructured CLI text as a safety-relevant signal). `codex doctor` itself is a
comprehensive diagnostic sweep ("installation, config, auth, and runtime health" per its own
`--help`), not a narrow auth check, and it is slow:

| | Result |
|---|---|
| `codex doctor --json` standalone | **18.7s** (`user 6.69s`, `sys 1.38s` — real CPU work, not just I/O wait) |
| `relay doctor` against **one** real Codex profile (isolated: a temp config with only that profile registered) | **12.9s** |
| `relay doctor` against this repo's real 3-profile setup (2 Claude + 1 Codex) | **12.97–14.02s** (n=8) — confirms the Codex profile alone accounts for essentially the entire cost; the 2 Claude profiles' own auth checks are ~0.2–0.5s each |

**This is a genuine, non-cherry-picked finding, not a Relay bug to casually patch.** The comment in
`relay-provider-codex/src/inspection.rs` explains why `codex doctor --json` was chosen: it is
Codex's *only* redacted, structured signal for "is this profile authenticated" — the alternative
would mean parsing `codex login status`'s human-readable text as a safety-relevant signal, which is
exactly the kind of undocumented-format fragility this codebase's stated policy avoids. `relay
codex`'s own pre-launch preflight already avoids this cost by using the fast §5a call instead
(rate-limit availability implies authentication, for that specific purpose) — `relay
doctor`/`setup`/`status`'s general-purpose readiness check has not been changed to match, and doing
so would need its own validation pass, out of scope for this benchmarking pass.

**Practical consequence:** a Codex profile makes `relay doctor`, `relay setup`'s completion screen,
and `relay status --live` take roughly 13 extra seconds per Codex profile checked. This is exactly
the kind of "appears to hang" latency the tasteful-progress-indicator work (see the `feat(cli):
tasteful TTY progress indicators` commit) now covers with a spinner instead of a silent wait — but
it is still real wall-clock time worth knowing about before advertising `relay doctor` as fast, and
worth mentioning explicitly in onboarding material so a first-time Codex user doesn't think it's
stuck.

**Update (relay status performance pass):** plain `relay status` (no `--live`) no longer pays this
cost at all — it was split into a fast, local-only default (measured **~30-40ms** on this same
3-profile setup, versus the ~14s above) and an explicit `relay status --live` that keeps exactly
the behavior benchmarked in this section. `relay doctor` and `relay setup` are unchanged and still
pay the full cost by design (they exist to actively verify, not to be fast); this was the
previously-noted "out of scope for this benchmarking pass" work, now done. See
`crates/relay-cli/src/readiness.rs`'s `assess_for_project_reporting` doc comment for the mechanism.

## Known gaps in this pass

- **No authenticated end-to-end state-continuation result is recorded yet.** The coordinator
  benchmark and journal phases isolate Relay's work, while Codex-source context capture includes
  a real app-server call (see §5a) and a real target bootstrap includes provider startup plus a
  bounded READY turn. Collect those journal timings from a controlled real handoff before making
  an optimisation decision.
- Everything in §2–§4 uses a fake Claude executable that responds instantly; real Claude Code CLI
  startup/response time is not included in those numbers (§1's warm/cold numbers, which measure
  `relay` itself, are unaffected by this).
- One machine, one run per measurement (two runs for §2–§4, explicitly shown as ranges rather than
  averaged into one falsely-precise number). Re-run before trusting these for a capacity decision.

## Reproducing these numbers

```sh
# 1. CLI startup latency
cargo build --release -p relay-cli
BIN=target/release/relay
/usr/bin/time -l "$BIN" --version        # cold-ish (first run of this exact binary path)
python3 - "$BIN" <<'EOF'
import subprocess, sys, time
bin_path = sys.argv[1]
times = []
for _ in range(50):
    t0 = time.perf_counter()
    subprocess.run([bin_path, "--version"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    times.append(time.perf_counter() - t0)
times.sort()
n = len(times)
print(f"n={n} min={times[0]*1000:.2f}ms median={times[n//2]*1000:.2f}ms p90={times[int(n*0.9)]*1000:.2f}ms max={times[-1]*1000:.2f}ms")
EOF

# 2-4. Idle supervision + SESSION_CONTINUATION latency (fake Claude, safe, no real account touched)
cargo test --release -p relay-cli --test benchmark -- --ignored --nocapture --test-threads=1

# 4b. STATE_CONTINUATION coordinator lower bound (deterministic fake ports)
cargo test --release -p relay-core --test handoff_coordinator state_continuation_latency -- --ignored --nocapture --test-threads=1

# 5a. Real Codex app-server rate-limit read (read-only, spends no quota)
RELAY_BENCH_CODEX_HOME=~/.config/agent-relay/profiles/<your-codex-profile>/codex \
  cargo test --release -p relay-provider-codex --lib app_server::tests::bench_real_read_rate_limits -- --ignored --nocapture

# 5b. codex doctor --json cost, standalone and through relay doctor
time codex doctor --json > /dev/null
time relay doctor --project <a-project-dir>
```

Or run everything in one pass with `scripts/benchmark.sh` (CLI startup + the two `--ignored` Rust
benchmarks; the real-Codex ones need `RELAY_BENCH_CODEX_HOME` set, so they're opt-in — see the
script's own `--help`).
