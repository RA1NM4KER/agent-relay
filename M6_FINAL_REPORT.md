# M6 final report: Codex as a second provider

M6 generalized Agent Relay from "Claude only" to a provider-neutral core with Codex as a proven
second provider: `relay-core`'s handoff machinery now speaks in provider-neutral ports and a
`ContinuityType` (`SESSION_CONTINUATION` / `STATE_CONTINUATION` / `NATIVE_RESUME`) instead of
Claude-specific types, `relay-provider-claude` was adapted to implement those ports rather than
being the only implementation, and a new `relay-provider-codex` crate implements them for Codex.
`relay-cli` gained provider dispatch throughout (`relay setup`, `relay profile`, `relay status`,
`relay claude`), plus two new commands: `relay switch <target>` (cross-provider or cross-profile
handoff) and `relay resume <profile>` (reattach to the writer lease's own session/thread under its
exact provider — `codex resume <thread-id>` for Codex, an interactive `claude --resume <id>` for
Claude).

## Delivered (commits, `main..m6-codex`)

- `91b6c8b` feat: provider-neutral continuity/capability groundwork in `relay-core`
- `4f0b66e` feat: adapt `relay-provider-claude` to the generalized handoff ports
- `08689ea` feat: new `relay-provider-codex` crate
- `4e53243` fix: recovery must stop an orphan target with the TARGET's own stopper
- `7b78c86` feat: `relay-cli` provider dispatch, `relay switch`, `relay resume`
- `579c7f2` test: end-to-end mixed-provider CLI tests with `FakeClaude`/`FakeCodex`
- `2401056` feat: `relay setup`/`profiles`/`status`/`claude` become provider-aware
- `fa4b98c` test: interactive `relay setup` can register a Codex-only profile

## Dogfood-found bug: `relay claude` couldn't attach to its own background session

Found today, live, using Agent Relay on itself (not by a test): `relay claude` reported

```
No job matching '854a2a57'
```

for a background session that was, provably, still running — `relay resume erika` correctly
detected and described the exact same session (`854a2a57-1c41-47d2-b20b-85fa0118fcdd`, background
job `854a2a57`) as active, and manually running

```
CLAUDE_CONFIG_DIR="$HOME/.config/agent-relay/profiles/erika/claude" claude attach 854a2a57
```

worked immediately. The difference between the two: `relay resume` explicitly sets
`CLAUDE_CONFIG_DIR` before execing Claude; `relay claude`'s attach step did not.

### Root cause

`exec_claude_attach` (`crates/relay-cli/src/main.rs`), the function `relay claude` execs into for
its final "hand the user a live terminal" step, ran

```rust
std::process::Command::new(executable).arg("attach").arg(short_id).exec();
```

with no `CLAUDE_CONFIG_DIR` set on the child at all. `claude attach` then searched whichever config
directory this process happened to inherit from its parent shell — not necessarily anything, and
never guaranteed to be the isolated profile directory Relay itself had launched the background job
under — so it never found the job registry entry that job was actually recorded in. Every other
Claude-launching call site in the codebase (`exec_claude_resume`, `run_claude_auth_subcommand`,
`relay-provider-claude`'s own launch/usage/session-registry code) already sets `CLAUDE_CONFIG_DIR`
explicitly; `exec_claude_attach` was the one exception, introduced back in M4.4 and never caught
because no test exercised the attach step's environment — only its exit status.

### Fix

`exec_claude_attach` now takes the target `config_dir` and sets it on the child via
`Command::env("CLAUDE_CONFIG_DIR", config_dir)` — the child's environment only; this process's own
environment is never mutated, preserving credential isolation between profiles exactly as every
other call site already does.

At the call site (`run_claude`), the `config_dir` passed is resolved from `lease.owner_profile` —
looked up in the already-fetched `registered` profile list — never assumed to be the configured
primary profile. This matters specifically because `lease.owner_profile` is *not* always the
primary: after a `relay switch` (Claude A → Claude B handoff) completes, the writer lease's
`owner_profile` is the handoff target, which may be a fallback profile rather than primary, and
`run_claude`'s existing-lease-reuse branch reads that same `lease.json` regardless of whether the
lease reflects a fresh launch or a completed handoff. The fix reads the lease's actual owner every
time, so a post-handoff `relay claude` attaches under the correct profile automatically.

### Regression coverage

Two new tests in `crates/relay-cli/tests/m4.rs`:

- `claude_attach_execs_under_the_lease_owners_isolated_config_dir` — a fresh
  launch-then-attach (`relay claude`, no `--no-attach`) proves the fake `claude`'s logged `attach`
  invocation received `CLAUDE_CONFIG_DIR` equal to the profile's own registered config dir, not
  nothing.
- `resume_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys` (renamed from
  `claude_attach_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys` by the UX-contract
  change in the next section, which retired `relay claude`'s silent-reattach behavior this test
  originally exercised — its core assertion, resolving the lease owner rather than the primary, now
  runs against `relay resume` instead) — sets primary = `alice`, fallback = `bob`, and makes `bob`
  (not the primary) the project's writer lease owner: the exact on-disk shape a completed Claude A
  → Claude B handoff leaves behind. A bare `relay resume` (no profile argument) is then proven to
  attach under `bob`'s `CLAUDE_CONFIG_DIR`, never `alice`'s.

**LIVE VERIFIED**, both directions: both tests were run against a temporary revert of the fix
(`git stash` on `main.rs` alone) and confirmed to fail with the exact real-world symptom
(`config_dir` logged as empty string) before being confirmed to pass with the fix restored.

### fmt/clippy/tests

`cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`:
clean (one lint fixed along the way — `filter(..).next_back()` → `.rfind(..)` in the new test).
`cargo test --workspace`: **354/354, 0 failed**.

### Scope note

This report documents the dogfood-found bug above in full; it does not re-verify or re-attest the
correctness of the eight M6 feature commits listed above, which were built and tested in earlier
sessions — their own test coverage (`crates/relay-cli/tests/m6.rs`, unit tests in `relay-core` and
`relay-provider-codex`) is unchanged by this fix and remains green as part of the same
`cargo test --workspace` run.

## UX contract revision: `relay claude` no longer silently reattaches

Requested directly (not dogfood-found): the pre-existing `relay claude` contract — "reuse an
already-active writer if one exists for this project, otherwise launch a fresh one" — was flagged
as technically safe (`perform_launch` never created two writers) but unintuitive: running `relay
claude` twice could either start a new conversation or silently resume an old one, and the user had
no way to tell which was about to happen from the command alone.

### The mental model, before and after

**Before (M4's original design):** `relay claude` was the single daily entry point for both
starting and continuing — `cd` into a project and run it, and it did whichever of the two was
correct given the project's current state. This was a deliberate M4 choice (see
`M4_FINAL_REPORT.md` §6 / `run_claude`'s pre-existing doc comment): the goal was a single command
that "just works" regardless of state, so a user never had to think about whether a session already
existed.

**After (this change):** starting and continuing are two different, explicit actions:

```
relay claude          -> starts a NEW Relay-managed Claude conversation
relay resume          -> continues the EXISTING Relay-managed conversation for this project
relay claude --new    -> explicitly replaces the existing one with a fresh one
```

`relay claude` never silently creates a second writer and never silently reattaches — if a
Relay-managed session is already active for the project, it fails closed with a message pointing
at the other two commands. This is the same "never trigger a destructive or ambiguous transition
from inferred state alone" principle the M2B.75 stop-and-verify machinery already applies to
process liveness, now applied to the *command's own meaning*.

### Distinguishing the four related concepts

This change is about how a managed session **starts** and how you **continue** it — it does not
touch handoff mechanics at all:

- **STARTING a new managed conversation** (`relay claude`, `relay claude --new`): a brand-new
  Claude session under the configured primary profile, tracked by Relay from its first message.
  Always what "start" means now — never "maybe resume."
- **RESUMING an existing managed conversation** (`relay resume`): reattaching interactively to the
  session/thread the project's writer lease already records, under that lease's *actual current
  owner* profile — resolved automatically, never assumed to be the primary. No new session is
  created; nothing is stopped.
- **SWITCHING/handoff of the current conversation** (`relay switch <profile>`): moving the
  *current* writer's conversation to a different profile/provider, with continuity (session
  transfer or state-continuation bundle, per `ContinuityType`) — unrelated to, and untouched by,
  this change. `relay switch` still refuses when the target is already the current writer
  (`switching_to_the_current_writer_fails_closed`, unchanged) and still requires the *current*
  writer to actually be the one making the request, exactly as before.
- **Automatic provider/profile fallback after exhaustion** (`relay watch run`, Herdr's automatic
  handoff event): entirely separate code (`WatchCoordinator`, `automation` module) that never calls
  `run_claude`/`perform_launch`/`run_resume` at all — it drives `HandoffCoordinator` directly. This
  change touches none of it; continuity semantics (Claude A → Claude B → Codex → ... per configured
  fallback order) are exactly as they were.

### Why M4 chose auto-reattach, and why the smallest change here is a UX layer, not new machinery

Investigated before writing any code: M4's `perform_launch` (shared by `relay launch` and `relay
claude`) already refuses to launch over a *confirmed-live* existing writer
(`Error::WriterAlreadyActive`) — the safety property this task requires was never missing. The gap
was purely in `run_claude`'s own wrapper around it: on a confirmed-live existing lease, `run_claude`
chose to treat that as success (reuse) rather than surfacing it to the user as a choice. The fix is
therefore a UX-layer change only:

- The existing-lease liveness check (`confirm_not_active`, unchanged) still runs first.
- If live and `--new` was **not** passed: return the new `Error::ManagedSessionAlreadyActive`
  (relay-core, carries the full multi-line guidance message) instead of proceeding — no side
  effects, no lease touched, no call to `perform_launch`.
- If live and `--new` **was** passed: resolve the lease owner's *own* provider ports
  (`providers::ports_for`, the same provider-neutral dispatch `relay switch` already uses — Claude
  or Codex, whichever the owner actually is) and call `SessionStopper::stop_and_verify` on it — the
  exact authoritative stop-and-verify primitive the M2B.75 handoff/recovery machinery already
  proved (issues the provider's real stop, polls for multiple *consecutive* quiescent readings,
  never trusts a single observation). Its `Result` is propagated with `?`: a failed/unverifiable
  stop aborts right there, before any launch is attempted.
- Either way, execution then falls through to the **same, unmodified** `perform_launch` call every
  other path already used — which re-checks liveness itself, under its own orchestration lock, as
  the final authority. This is the second, independent safety net: even if something raced between
  the stop and the launch, `perform_launch` refuses rather than overwriting a live lease. No new
  lifecycle, no parallel state machine, no lease file deleted directly anywhere in this change.
- `relay resume`'s only change is that its profile argument became optional
  (`Option<ProfileName>`): when omitted, the resolved profile is `lease.owner_profile` — read from
  the same `LeaseStore` every other command already uses, so it inherits the M6 dogfood fix
  (commit `142668f`) automatically rather than needing it re-implemented. The explicit
  `relay resume <profile>` form is untouched (still refuses unless that exact profile is the
  current writer).

### Preserving the M6 dogfood invariant (commit `142668f`)

Explicit requirement: any resume/attach path must resolve `CLAUDE_CONFIG_DIR` from the *actual*
lease owner, never blindly from the configured primary, and this must keep working after a
Claude-profile-to-Claude-profile handoff (e.g. `erika` configured primary, `megan` owns the lease
after a handoff). This was not just preserved but exercised directly:
`resume_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys` sets primary = `alice`,
fallback = `bob`, makes `bob` the writer lease owner (the shape a completed handoff leaves behind),
and proves a bare `relay resume` execs `claude --resume` under `bob`'s `CLAUDE_CONFIG_DIR`, never
`alice`'s — the exact regression class the dogfood fix closed, now proven for the resume path too.

### Tests (all in `crates/relay-cli/tests/m4.rs`, 27/27 passing)

Two pre-existing tests were retired because their entire premise was the silent-reattach behavior
this change removes, and rewritten to prove the *new* contract instead:

- `claude_entrypoint_reuses_an_active_lease_without_relaunching` →
  **`claude_refuses_when_a_live_managed_session_already_exists`**: a second `relay claude` against
  a live session now fails closed (`managed_session_active`), names the owning profile, points at
  both `relay resume` and `relay claude --new` in its own message, and launches no second writer.
- `claude_attach_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys` →
  **`resume_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys`** (described above).

New tests added:

- `resume_bare_attaches_to_the_active_managed_session` — `relay resume` with no profile argument
  attaches to the project's active session (proven via the fake's logged `--resume <session-id>`
  invocation).
- `resume_with_no_active_session_says_so_clearly` — `relay resume` with nothing running fails with
  the existing, already-descriptive `no_active_writer_for_project` error rather than a generic one.
- `claude_new_stops_the_existing_writer_and_starts_a_fresh_one` — the core `--new` path: proves,
  from the raw invocation log's exact ordering (not internal state), that exactly one `stop` happens
  strictly between exactly two `--bg` launches — never a window with two writers.
- `claude_new_behaves_like_claude_when_no_existing_session` — `--new` with nothing to replace
  behaves exactly like a plain `relay claude`, and critically never issues a stop it doesn't need.
- `claude_new_fails_closed_when_stop_cannot_be_verified_and_never_launches_a_second_writer` — a
  stop that can never be verified quiescent (`StopNotVerified`) aborts `--new` before any relaunch
  is attempted, and leaves the original lease byte-for-byte as it was.
- `claude_recovers_a_stale_lease_without_the_new_flag` — a lease whose recorded owner process is
  genuinely gone (a crash, not a stop) is recovered automatically by a *plain* `relay claude`, no
  `--new` required, and no stop is issued for a process that was never actually there to stop.

**Test-fixture note, since it was a real source of friction worth recording:** several of these
tests needed to prove a session had gone from "live" to "confirmed gone," which `relay`'s own
liveness code (`ClaudeSourceLiveness::check`) deliberately resolves via a fail-closed pid+start-time
fingerprint comparison whenever a session drops out of `agents --json`'s listing — by design, an
*indeterminate* identity (e.g. a synthetic/fake pid that was never a real process) is treated as
still active, never as evidence of absence. A fake pid can therefore prove "still active" (the
common case, already covered) but can never prove "confirmed gone." The tests that need a genuine
"gone" signal (`claude_new_stops_the_existing_writer_and_starts_a_fresh_one`,
`claude_recovers_a_stale_lease_without_the_new_flag`) spawn a real, short-lived OS process and use
its real pid, killing/reaping it at the right moment (tied to the actual `stop` event via a marker
file for the first, or explicitly before the second call for the crash-recovery case) rather than
guessing at wall-clock timing — this machine's own per-`relay`-invocation subprocess overhead
(~1–1.5s observed) made a fixed `sleep N` unreliable. Also fixed along the way: the shared
`FakeClaude` test fixture's `agents --json` output included a `"cwd"` field derived from `$PWD`,
which is meaningless (`query_active_sessions` never sets the spawned command's working directory,
so it reflected `relay`'s own cwd, never the project under test) and silently defeated every
cwd-scoped filter in the real liveness/stop code; omitting it (Relay's own code already treats an
absent `cwd` as "matches any project") was the correct fix, not a fixture-only workaround.

**LIVE VERIFIED**: full 27-test `crates/relay-cli/tests/m4.rs` suite passing.

### fmt/clippy/tests (this change)

`cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`:
clean. `cargo test --workspace`: **360/360, 0 failed** (354 from the prior fix, +6 net new: this
change adds 6 wholly new test functions and renames/repurposes 2 existing ones — see the list
above — rather than adding 8).

### Scope note (this change)

Out of scope, deliberately: no change to `relay switch`, `relay watch run`, the handoff coordinator,
Herdr integration, or any provider adapter — confirmed both by code inspection (none of those call
`run_claude`/`run_resume`/`perform_launch`) and by the full workspace test suite remaining green,
including `crates/relay-cli/tests/m6.rs`'s switch/resume/mixed-provider tests, unchanged.

## Dogfood-found bug #2: `relay resume` unconditionally ran `claude --resume`, which real Claude rejects for a still-live background job

Found live, immediately after validating the fix above on the real dogfooding session: once
`relay claude` correctly refused (per the UX contract change) and pointed at `relay resume`,
`./target/debug/relay resume` itself then failed:

```
Session 854a2a57-1c41-47d2-b20b-85fa0118fcdd is running as a background session (854a2a57).
Run `claude attach 854a2a57` to open it...
```

### Root cause

`run_resume`'s Claude branch unconditionally called `exec_claude_resume`, i.e. `claude --resume
<session_id>`, regardless of whether the lease's recorded `claude --bg` background job was still
live. Real Claude Code refuses `--resume` outright for a session with a live background job — its
own message says exactly what the fix now does: attach to it instead. `--resume` is only the right
command once that background job is confirmed gone. The previous implementation never checked.

### Fix

`run_resume`'s Claude path now resolves which native command is actually safe via a new
`resolve_claude_resume_action` (`crates/relay-cli/src/main.rs`), computed *before* anything is
printed or exec'd so a failure never claims to be "resuming" something it then can't safely
continue:

1. **No `provider_handle` on the lease** (the shape a cross-profile handoff's target lease has —
   its launch is a foreground verification turn that has already exited by the time anyone resumes
   later, never a persistent `--bg` job; see `WriterLease::provider_handle`'s own doc comment) →
   unconditionally `NativeResume` (`claude --resume <session_id>`). Nothing to check liveness
   against.
2. **`provider_handle` present, and `claude agents --json` (queried under the lease *owner's*
   config dir — the `142668f` invariant, preserved and re-verified below) lists it** → `Attach`
   (`claude attach <provider_handle>`) — the exact fix for the bug above.
3. **`provider_handle` present but not listed** → fall back to the recorded owner process's own
   pid+start-time fingerprint, the same authoritative signal `ClaudeSourceLiveness`/`SessionStopper`
   already use elsewhere (`ProcessIdentity::is_still_the_same_process`):
   - Confirmed gone (`Some(false)`) → `NativeResume`.
   - Anything else — genuinely indeterminate (`None`), or the recorded process improbably still
     matching yet the listing disagreeing with it (`Some(true)`) — is treated as an inconsistent
     state that must never be guessed through: `Error::AmbiguousSessionLiveness`, fails closed,
     execs nothing.

Codex's `exec_codex_resume` is untouched — inspected separately (`relay-provider-codex/src/lib.rs`):
Codex has no `--bg`/background-job/attach concept at all (`codex resume <thread-id>` is its only
resume mechanism, always interactive), so the live/dead distinction this fix adds for Claude simply
doesn't apply there. `crates/relay-cli/tests/m6.rs`'s existing Codex resume test is unchanged and
still green.

### Preserving the `142668f` invariant, again

Every branch above resolves against `profile.config_dir` — the profile the *lease* names as owner
(`resolved_profile = lease.owner_profile`, computed once, before any provider dispatch), never the
configured primary. `resume_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys` was
updated (not weakened) to assert this through the *new* correct command: with `bob` owning a live
writer lease, `relay resume` now execs `claude attach <bob's handle>` under `bob`'s
`CLAUDE_CONFIG_DIR`, never `alice`'s (`alice` remains the configured primary throughout) — directly
proving "after erika → megan handoff, resume must use Megan's handle/config if Megan owns the
lease."

### Tests (all in `crates/relay-cli/tests/m4.rs`, invocation-log-based)

Two pre-existing tests were updated because their fixtures represent a *live* background job
(nothing ever stopped it), which is exactly the case that now correctly chooses `attach` over
`--resume` — their assertions were the bug this fix closes, so they now assert the fixed behavior:

- `resume_bare_attaches_to_the_active_managed_session` →
  **`resume_bare_attaches_to_a_live_background_job_not_resume`**: proves `relay resume` execs
  `claude attach aaaa1111` for a live job, and — explicitly — that `--resume` is never invoked at
  all.
- `resume_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys`: same rewrite (attach,
  not resume), described above.

New tests:

- `resume_uses_native_resume_when_the_background_job_is_confirmed_dead` — a real, previously-alive
  process (spawned in-test, matching the `--new`/stale-lease tests' technique for the same reason:
  a synthetic pid can never produce a genuine "confirmed gone" fingerprint) is killed and reaped,
  and the fake's `agents --json` listing is dropped to match; `relay resume` then execs
  `claude --resume <session_id>` and never `attach`.
- `resume_fails_closed_when_liveness_is_ambiguous` — the listing is dropped but the recorded pid is
  synthetic (never a real, establishable fingerprint — genuinely indeterminate, not confirmed
  gone); `relay resume` fails with `ambiguous_session_liveness`, execs neither `attach` nor
  `--resume`, and launches no second writer (`--bg` invocation count stays at 1, from the original
  launch only).

Requirement 6 ("existing `relay claude`/`--new`/`switch`/automatic handoff/Codex behavior remains
green") is covered by the full workspace suite below, including all 22 previously-passing
`m4.rs` tests untouched by this fix and `m6.rs`'s switch/Codex-resume tests unchanged.

**LIVE VERIFIED** for the attach direction specifically: the real dogfooding session
(`854a2a57-1c41-47d2-b20b-85fa0118fcdd`) that surfaced this bug was left completely untouched
throughout this fix — not stopped, restarted, or replaced — per explicit instruction, so the user
could validate `relay resume` against it manually afterward. All verification here used the
fake-provider test suite only.

### fmt/clippy/tests (this fix)

`cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`:
clean. `cargo test --workspace`: **362/362, 0 failed** (360 before this fix, +2 net new: 2 wholly
new test functions added, plus 2 existing ones rewritten in place — not counted as new — whose
fixtures happened to represent exactly the buggy case).
