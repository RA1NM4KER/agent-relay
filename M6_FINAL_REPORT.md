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

**Post-fix live validation, by the user, against the real session:** `./target/debug/relay resume`
run for real against `854a2a57-1c41-47d2-b20b-85fa0118fcdd` — Relay attached back into the exact
same live Claude conversation, no error. The `--resume`-against-a-live-job failure this section
documents is confirmed gone. This is the first real (non-fake-provider) confirmation of the fix,
on top of the 29/29 `m4.rs` fake-provider coverage above.

### fmt/clippy/tests (this fix)

`cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`:
clean. `cargo test --workspace`: **362/362, 0 failed** (360 before this fix, +2 net new: 2 wholly
new test functions added, plus 2 existing ones rewritten in place — not counted as new — whose
fixtures happened to represent exactly the buggy case).

## Dogfood-found bug #3: real Claude `--bg` output is ANSI-colored, breaking every real `--bg` launch

Found live, mid-way through setting up the isolated, disposable-project validation this section
requested for itself: attempting a genuine `relay claude` launch under `erika`, scoped to a brand
new disposable workspace, failed:

```json
{"ok": false, "error": {"code": "malformed_provider_output", "message": "provider output was malformed"}}
```

Manually reproducing the underlying `claude --bg` call showed it succeeded and created a real
background job — Relay had simply lost track of it. This is more severe than the two findings
above: it is not a UX nicety, it is `relay claude`'s fresh-launch path failing outright on this
real Claude Code install, invisibly, because no fake-provider fixture in the test suite has ever
emitted what real Claude actually prints.

### Root cause

`parse_background_job_id` (`crates/relay-provider-claude/src/handoff_adapters.rs`) expects the
line `backgrounded · <id>` with nothing but the bare hex id after the bullet, then validates every
byte is an ASCII hex digit — a deliberate, security-motivated strictness (`rejects_a_suspicious_id_
that_is_not_plain_hex`, pre-existing) so this can never become a path/argument-injection vector.
Real Claude Code 2.1.278 wraps the printed id in an ANSI SGR color escape sequence **even when
stdout is piped to a non-terminal**, observed byte-for-byte:

```
backgrounded · \x1b[36m771cb101\x1b[39m
```

The un-stripped escape bytes (`\x1b`, `[`, `3`, `6`, `m`, …) are not hex digits, so validation
failed on every single real invocation, unconditionally — while the background job it was trying
to describe had already been created successfully underneath. Confirmed via an isolated,
instrumented timing probe (a real `--bg` call plus a tight `agents --json` polling loop) that the
job appears in the listing, pid populated, on the very first poll after `--bg` returns — this was
never a timing race, purely the parser rejecting well-formed real output. Every fake-provider
`FakeClaude` fixture across `m4.rs`/`m6.rs` prints plain, uncolored text for `--bg`, so this path
had zero real-Claude coverage anywhere in the suite until this live session hit it directly.

### Fix

A new `strip_ansi_sgr` helper removes only a well-formed ANSI SGR sequence (`ESC '[' <digits/`;`>*
'm'`) from the extracted id before the existing hex-digit check runs. Anything that isn't a
complete, well-formed sequence — an incomplete escape, an unexpected terminator, a stray `ESC` — is
left byte-for-byte as literal text, so the check's original injection-defense property is fully
preserved: a malicious payload wrapped in real-looking color codes (`rejects_a_suspicious_id_even_
when_wrapped_in_ansi_color_codes`, new) is still rejected, because only the color wrapping is ever
removed, never the payload itself.

Two clean-up notes from reproducing this live: each failed real attempt still creates a genuine,
Relay-untracked background Claude session (the `--bg` call itself doesn't know or care whether its
own output gets parsed successfully afterward) — every one encountered while diagnosing this was
found via `claude agents --json` and stopped with the real `claude stop <id>`, under `erika`'s
isolated config, never left running.

### Tests

`crates/relay-provider-claude/src/handoff_adapters.rs`, `parse_background_job_id`'s unit test
group (110/110 passing for the crate; 4 new here):

- `parses_a_backgrounded_line_with_real_ansi_color_codes` — the exact live byte sequence above.
- `rejects_a_suspicious_id_even_when_wrapped_in_ansi_color_codes` — the injection defense above,
  re-verified with the fix in place.
- `strip_ansi_sgr_removes_only_well_formed_sequences` — direct unit coverage of the helper,
  including that an incomplete/malformed escape is left untouched rather than silently dropped.

Confirmed Codex's own output parsing (`relay-provider-codex/src/handoff_adapters.rs`) is not
similarly exposed: every Codex text path Relay parses is either `--json`/NDJSON structured output
(`codex exec --json`, `codex doctor`) or `ps` output — never colorized human text like Claude's
`--bg` line — so no equivalent fix was needed there.

### Live validation: isolated Codex + Claude → Codex `STATE_CONTINUATION`, end to end, for real

With the ANSI fix in place, the disposable-workspace validation this whole finding grew out of was
completed in full, against a dedicated throwaway workspace (`~/agent-relay-m6-validation`, its own
git repo, nothing SchoolScape-related), using `erika` (explicitly authorized by the user for this
one disposable workspace only) and a freshly-registered, isolated Codex profile
(`codex-m6-validation`, its own `CODEX_HOME` under Relay's managed profile root — never the user's
default `~/.codex`/personal ChatGPT login, which was never touched). The live dogfooding session
(`854a2a57-1c41-47d2-b20b-85fa0118fcdd`, `erika`, project `agent-relay`) was re-verified byte-for-
byte unchanged (`lease_owner: erika`, same `current_transaction`, `session_state: active`, still
`busy`/`working` in a real `claude agents --json` listing) before, during, and after every step
below — it was never the target of any command in this validation.

1. **Isolated Codex live validation.** `relay login codex-m6-validation --provider codex` (real,
   interactive, run by the user) registered a new profile with its own isolated `CODEX_HOME`.
   `relay profile status codex-m6-validation` then confirmed, for real: `authentication:
   "authenticated"`, a real identity (`codex doctor --json: checks.auth.credentials` → `AVAILABLE`),
   and a `config_dir` correctly isolated under Relay's managed profile root, distinct from the
   default `~/.codex`.
2. **`relay claude --profile erika --project-dir ~/agent-relay-m6-validation` (real, live).**
   Succeeded post-fix: `new_session: true`, a real background job, real `agents --json` listing —
   this is the same ANSI-parsing path fixed above, now proven working end to end for real, not just
   in the fake-provider suite.
3. **`relay switch codex-m6-validation --project-dir ~/agent-relay-m6-validation --no-attach`
   (real, live) — the `STATE_CONTINUATION` handoff.** Completed: `state: COMPLETE`,
   `continuity_type: STATE_CONTINUATION`. The journal's own `notes` show every real step in order:
   captured a real git checkpoint (branch `main`, clean, `head` recorded) → detected and
   authoritatively stopped the source's real background job (`claude stop`, quiescence-verified,
   confirmed via `agents --json` no longer listing it afterward) → context bundle captured and
   hashed (`sha256`, 740 bytes) → real Codex target launched (`codex exec`, real pid + start-time
   fingerprint recorded) → target verified (`started_successfully: true`, a real new Codex thread
   id) → ownership moved to `codex-m6-validation` in the lease.
4. **Post-handoff verification.** `relay status --project ~/agent-relay-m6-validation` shows
   `lease_owner: "codex-m6-validation"`. The verified Codex thread id is backed by a real,
   persisted rollout file on disk under the isolated `CODEX_HOME`
   (`sessions/2026/09/20/rollout-…-<thread-id>.jsonl`) — not just a successful API response.

Interactive `relay resume` against the resulting Codex thread (`NATIVE_RESUME`,
`codex resume <thread-id>`) was deliberately not exercised live in this pass — attaching would
have required a real interactive terminal this session doesn't have, and the mechanism itself is
already covered live-shaped by `m6.rs`'s `resume_execs_codex_resume_under_the_profiles_own_codex_
home`. Everything upstream of that exec (thread creation, verification, lease ownership) is now
LIVE VERIFIED per steps 1–4.

The disposable workspace (`~/agent-relay-m6-validation`) and the `codex-m6-validation` profile were
left in place after validation, for the user to inspect or remove at their own discretion — nothing
was deleted or torn down automatically.

### fmt/clippy/tests (this fix, including live validation)

`cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`:
clean. `cargo test --workspace`: **365/365, 0 failed** (362 before this fix, +3 net new: the 3
`handoff_adapters` unit tests in `relay-provider-claude` listed above).

## Dogfood-found bug #4 (release-blocking): a real usage limit never triggered the automatic handoff

### The real (non-simulated) exhaustion test

`erika` was the managed writer for this repository. She hit a genuine Claude session limit.

Observed, in order:

1. Relay's `StopFailure` hook fired and, corroborated by the statusline snapshot, **correctly recorded `erika` as known-exhausted**.
2. **No automatic handoff occurred.** `relay watch status` showed `Current owner: erika`, `Known-exhausted profiles: erika`, `Recent automatic handoffs: 0`. The Herdr plugin log shows no invocation after 23:01, i.e. none across the real limit (~23:31).
3. Running the real `relay watch run` by hand completed `erika -> megan` successfully (this session then continued on `megan`, verified by the `RELAY_HANDOFF_VERIFIED` turn). So detection, policy, coordinator and both providers were correct. The missing piece was purely *who calls the coordinator*.
4. Four background audit subagents launched at the limit returned 429s; that is expected provider behaviour, not a Relay defect.

### Root cause

Nothing invoked `WatchCoordinator::evaluate` after the hook recorded exhaustion:

- `relay watch run` is a one-shot evaluator. Its only in-tree caller was the Herdr `pane.agent_status_changed` plugin event, a best-effort third-party signal that did not fire.
- Outside Herdr there was no caller at all.
- `README.md` / `docs/automatic-handoff.md` claimed `relay claude` would re-evaluate. It never did (docs contradicted behaviour).

Second half of the finding (UX): `relay claude` / `relay resume` `exec`'d into Claude and vanished, so even a successful handoff left the user at a dead session and required discovering `relay resume`.

### Fix (smallest robust change; no daemon, no provider polling)

- **Trigger (`crates/relay-cli/src/auto_handoff.rs`)**: when the `StopFailure` hook records a rate-limit failure, it starts one short-lived, detached `relay watch auto` for that project. It is gated so it only fires for the exact session Relay manages: the hook's config dir must be a registered Claude profile, the lease must name that profile *and* that session id, and at least one registered fallback must exist. It retries a bounded number of times (default 7 x 20s) only while the evaluation reports `no_action_needed`, because the corroborating statusline snapshot can land just after the failure, then exits. Output goes to `auto-handoff.log` (0600, size-capped) next to the project's state.
- **Environment scrub**: the detached child runs with `CLAUDE_CONFIG_DIR` and every `AUTHENTICATION_OVERRIDE_VARIABLES` entry removed. The hook inherits Claude's own environment (source profile's `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_MESSAGING_TOKEN`), which trips Relay's `EnvironmentOverrideConflict` checks and made every fallback look unhealthy. A negative control showed the scrub is load-bearing.
- **Continuity (`crates/relay-cli/src/terminal.rs`)**: `relay claude`, `relay resume` and `relay switch` (Codex target) now run the interactive session as a supervised child instead of `exec`. Relay only watches its own project's `lease.json` (1 local read/s while the user's terminal is in use). When the lease owner moves to another profile, the dead source session is closed, Relay waits (bounded) for the handoff transaction to settle, then continues the same conversation on the new owner (up to 4 hops), printing `Agent Relay: '<old>' reached its limit - continuing this conversation on '<new>'...`. An ordinary exit returns that session's own exit code and continues nowhere.
- **Single-writer safety is unchanged**: every ownership change still goes through the existing `WatchCoordinator` / `HandoffCoordinator` under the orchestration lock, with the same cooldown, per-window loop cap and known-exhausted ledger. A concurrent second trigger (e.g. Herdr) simply sees the lock or the cooldown. The supervisor never starts, stops or hands off anything itself.

### Regression coverage

`crates/relay-cli/tests/m6_auto.rs` (8 tests) drives the real hook inputs (statusline at 100% plus a real `StopFailure` payload) with Claude's ambient environment present: automatic Claude->Codex handoff; automatic Claude->Claude SESSION_CONTINUATION; statusline arriving after the failure; unmanaged session never triggers; no fallback configured triggers nothing; `relay resume` and `relay claude` follow the conversation to the new owner; ordinary exit continues nowhere. `terminal.rs` has 6 unit tests (exit code passthrough, lease-move closes child, missing/corrupt/same-profile lease is not a move, lock-wait bounds).

Gate: `cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo test --workspace` 0 failures (379 passed).

### Known limitations / operator notes

- Installed hook commands record Relay's binary path at install time. After upgrading or rebuilding to a new path, re-run `relay integration claude install --profile <p>`; a hook still pointing at an old binary will not trigger the new behaviour.
- A fallback profile needs the usage integration installed for a *second* hop (checked read-only: `erika` and `megan` both have it installed; `megan` has recorded 0 stop failures so far).
- No automatic fail-back to the primary once its window resets.
- Unverified live: whether a real `claude attach` client exits on its own when its background session is stopped. Mitigated: the supervisor SIGTERMs the child when the lease owner moves.
- The Herdr event remains best-effort and is now a redundant trigger, not the only one.
- Codex usage detection is unsupported, so Codex cannot be an automatic *source*.
- The end-to-end continuation after the new trigger has been exercised with fake providers only; the next real exhaustion is the live confirmation.

## Focused pre-merge check: global priority routing and hook freshness

### 1. Routing after a handoff: a real routing bug (plus the misleading status)

Intended model: one ordered hierarchy `primary > fallbacks`, sticky current writer, no eager fail-back; at each exhaustion everything except the current writer is reconsidered (exhausted/reset-pending, unhealthy/disabled and same-identity candidates are excluded).

- **Already correct:** `relay-core::automation::decide` and `WatchCoordinator` (ordered list, known-exhausted honoured only until its reset, elapsed usage windows ignored, source and same-identity excluded). Herdr just forwards whatever ordered list its `relay_profile_fallback` metadata holds; `decide` skips the source by name, so listing the primary there works.
- **Real bug:** the hook-triggered `watch auto` (`auto_handoff::plan`) built its candidate list from `preferences.fallback_profiles` only, minus the current writer. The primary was never in it. With `primary=erika, fallback=[megan]` and current writer `megan` (after erika -> megan), the list was empty, so a limit on megan started no evaluation at all: erika was never reconsidered, and nothing after megan (e.g. codex) was reachable either. It was not sticky-writer behaviour; it was a missing candidate.
- **Status display bug (same root):** `relay status` printed `Fallback:` as the raw preference list, so with megan as writer it showed `Current profile: megan / Fallback: megan`.

Fix (smallest): one helper, `auto_handoff::hierarchy_without(preferences, current, is_registered)`, returns `primary` then `fallback_profiles`, de-duplicated, minus only the current writer. The trigger uses it, and `relay status` uses the same helper for its `Fallback:` line and `fallback_profiles` JSON field (so the current owner is never its own fallback, and the primary appears once it is not the writer; `none` when empty). No fail-back logic was added.

Regression tests: `relay-core` `after_erika_to_megan_a_megan_limit_skips_still_blocked_erika_for_codex` and `once_erikas_reset_has_passed_she_is_first_eligible_again_ahead_of_codex`; `relay-cli` `auto_handoff` unit tests for the candidate order (primary reconsidered first after a handoff away from it; primary-as-writer falls back in order; only the current writer is skipped and unregistered names dropped).

Configuration note: the current `preferences.toml` is `primary=erika, fallback=[megan]`. `codex` is not in the hierarchy, so `erika > megan > codex` needs it added (`relay setup --non-interactive --primary erika --fallback megan --fallback <codex-profile>`). Codex can only be a target, never an automatic source.

### 2. Hook freshness

Hooks call the path `/Users/kefasmanda/repos/agent-relay/target/debug/relay` (not a copy), so they pick up any rebuild at that path; no reinstall is required as long as the binary is rebuilt from HEAD. See the final verification in the commit for the binary's build state.

## M7: Codex as an automatic exhaustion source (structured usage) and Codex continuity

### Upstream research (installed `codex-cli 0.155.0`; protocol schema generated locally with `codex app-server generate-json-schema`, no auth involved)

- `codex app-server` is the official local client protocol: JSON-RPC 2.0, one message per line over stdio (default), also unix/ws transports. A client sends `initialize` (+ `initialized`), then typed requests.
- `account/rateLimits/read` returns `ordinaryUsageAllowed` (documented as the backend's permission for ordinary usage, *validated against the active account*; `null` = unavailable and "clients must not infer recovery from percentages or reset times"), `rateLimits` (backward-compatible single bucket), `rateLimitsByLimitId` (multi-bucket, e.g. `codex`), per-window `usedPercent` / `windowDurationMins` / `resetsAt`, `rateLimitReachedType`, `spendControlReached`, credits, and `accountId`. `account/read` returns the account kind (`chatgpt` / `apiKey` / `amazonBedrock`). `initialize`'s response names the `codexHome` the server actually used.
- `thread/read` (metadata-only) returns the thread `id` and `cwd`, or a JSON-RPC error for an unknown id.
- Codex has hooks (`sessionStart`, `stop`, `interrupt`, …) but **no equivalent of Claude's `StopFailure`**, and installing hooks means editing the profile's Codex config, which Relay does not do. The server also pushes `account/rateLimits/updated`, but only on a connection Relay would have to hold open against the user's own TUI process, which it does not own. So there is no event to hang a trigger on.

### Live checks performed (real, read-only; isolated profile `codex-m6-validation`; default `~/.codex` untouched; nothing exhausted, no quota spent for the reads)

1. `initialize` returned `codexHome` equal to the isolated profile directory (isolation confirmed); ambient `CODEX_HOME`/`OPENAI_*` were removed from the child.
2. `account/read` = ChatGPT plus account; `account/rateLimits/read` = `ordinaryUsageAllowed: true`, primary 0% (5h window), secondary **95%** (weekly), with reset times. **The real Codex account is at 95% of its weekly window** — Relay classifies that `NEAR_LIMIT` (verified through `relay watch run --dry-run`, `source_usage: NearLimit`).
3. `thread/read` on the real persisted validation thread returned the exact id and `cwd` = the disposable workspace; a bogus id returned a clear `thread not loaded` error (no silent new thread, no model call).
4. Manual **Codex -> Claude `STATE_CONTINUATION`** in the disposable workspace (`relay switch erika`, `codex-m6-validation -> erika`): `state: COMPLETE`, `continuity_type: STATE_CONTINUATION`, target `started_successfully: true` under erika's own config dir. (One small Claude turn on erika; the session had ended by cleanup time, nothing left running.)

Not done live (and not claimed): a real Codex exhaustion (never forced); the periodic trigger against real Codex; Codex profile A -> Codex profile B (only one real Codex account exists).

### What Relay now does

- `relay-provider-codex::app_server`: a minimal read-only JSON-RPC client. Spawns `codex app-server` with only `CODEX_HOME=<profile dir>` (auth-override variables removed), 20 s session budget, bounded line size/count, killed on drop. Fails closed on spawn failure, timeout, malformed data, JSON-RPC error, or a `codexHome` different from the profile's.
- `CodexUsageSignal` (now real): `ordinaryUsageAllowed=false` -> `EXHAUSTED` (reset = latest still-future `resetsAt` among windows at 100%; none known -> no reset time, i.e. blocked until `relay watch clear`); `true` -> `AVAILABLE`, or `NEAR_LIMIT` at >=90% in any window; missing verdict, a "reached" type contradicting `allowed`, non-ChatGPT account, no account id, wrong home, any app-server failure -> `UNKNOWN`. New evidence tier `ProviderRateLimitApi`. `PROVIDER_CAPABILITIES.usage_detection` is now `true`. Never parses English error text.
- Routing needed no change: the ledger, reset handling, the global `primary > fallbacks` hierarchy (which already includes Claude profiles below a Codex writer), coordinator pairs (Codex -> Claude/Codex), cooldown, loop guard and the single-writer lock are the existing code.
- **Trigger** (the honest gap): a supervised Codex terminal (`relay resume`/`relay switch`) now runs the same bounded one-shot `watch auto` the Claude hook starts, every 120 s (`RELAY_CODEX_POLL_SECS`, `0` disables), *only while that foreground terminal is open* (`terminal::Tick`). Relay stays non-daemon; Claude is still never polled. An unsupervised Codex session is evaluated only by `relay watch run` or the Herdr event.
- **Same-profile resume**: `relay resume`/handoff continuation now confirms the thread through `thread/read` (id equal, `cwd` equal to the project) before running `codex resume`; a missing/stale/mismatched thread fails closed with `codex_thread_not_verified`.

### Continuity guarantees (final)

| Path | Type | Status |
|---|---|---|
| Codex -> same Codex profile | `NATIVE_RESUME` | verified thread id + cwd via Codex before resuming; live-checked read side |
| Claude -> Claude | `SESSION_CONTINUATION` | unchanged |
| Claude -> Codex | `STATE_CONTINUATION` | unchanged (live-proven in M6) |
| Codex -> Claude | `STATE_CONTINUATION` | **automatic now possible** when Codex is exhausted (fake-provider e2e); manual path live-proven above |
| Codex A -> Codex B | `STATE_CONTINUATION` (never native) | routed by the same coordinator/`ContinuityType::for_transition` code; covered by a fake-provider e2e (two fake Codex profiles, `STATE_CONTINUATION` asserted) but **not live-validated** (one Codex account). Thread history is local to a `CODEX_HOME`; cross-home native resume is unsupported and not attempted |

### Tests (synthetic unless stated)

`relay-provider-codex`: 12 new tests (scripted app-server: typed parse, wrong `codexHome`, dead/garbage/missing server, thread read/unknown thread; AVAILABLE, NEAR_LIMIT, EXHAUSTED + reset, past/absent reset, missing verdict, contradictory snapshot, API-key/no-account -> UNKNOWN, end-to-end exhausted/available/failure). `relay-core`: Codex exhausted -> first Claude reset-pending -> next eligible; -> primary Claude once reset; healthy Codex is sticky. `relay-cli` e2e (fake providers): exhausted Codex -> Claude STATE_CONTINUATION with ledger/reset; healthy Codex stays; failing app-server or missing verdict never hands off; two simultaneous triggers -> one handoff/one writer; supervised `relay resume` on Codex notices exhaustion via the periodic check and the terminal follows the conversation onto Claude; stale Codex thread -> `resume` fails closed and never runs `codex resume`; `terminal::Tick` unit test.

Gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo test --workspace` 407 passed, 0 failed.

### Remaining limitations

- Codex accounts are not pinned to an identity (profile identity is still the isolated `CODEX_HOME` path); `codexHome` matching plus the account-validated verdict is the isolation guarantee. Two Codex profiles logged into the same account would share one quota; Relay cannot yet tell.
- The Codex trigger only runs inside a supervised terminal; there is no unsupervised background trigger by design.
- Thread `cwd` equality is required when Codex reports one; a thread created from a different directory is refused rather than resumed.
- Website not updated (per instruction). The Live handoff visual can now truthfully show automatic Codex -> Claude once the next real Codex exhaustion is observed; until then the strongest honest wording is "supported and tested with fakes; live-proven manually".

## M8: provider passthrough and `relay codex`

- `relay claude` / `relay codex [relay options] -- [provider options]`: everything after `--` is stored as a structured argv list and forwarded verbatim (no shell joining or re-parsing). Per launch path: fresh `claude --bg` gets the Claude options after Relay's own arguments and the prompt; a fresh `relay codex` creates its thread with a fixed content-free `codex exec` (Relay's arguments only) and the Codex options go to the interactive `codex resume` that follows; same-provider `--resume` / `codex resume` continuations reuse that provider's options; `claude attach` and Relay's own headless verification/bootstrap turns carry none; cross-provider handoffs use only the target provider's own stored options (never translated).
- State: `<project state>/provider_args.json` (atomic, private, `{claude:[…], codex:[…]}`); a new managed conversation keeps only the launching provider's options; `relay resume -- …` / `relay switch <p> -- …` replace the continued provider's options.
- Rejected (would replace something Relay must own): Claude `--bg/--background`, `-p/--print`, `--output-format`, `--input-format`, `-r/--resume`, `-c/--continue`, `--fork-session`, `--from-pr`, `--session-id`, `--teleport`, `--cloud`, `--environment`, `-w/--worktree`, `--tmux`, `--no-session-persistence`; Codex `-C/--cd`, `--worktree`, `--ephemeral`, `--json`, `-o/--output-last-message`, `--output-schema`, `--last`, `--all`, `--include-non-interactive`, `--remote`, `--remote-auth-token-env`. Error code `provider_argument_rejected`.
- `relay codex`: highest-priority configured Codex profile (`--profile` must be Codex, else `profile_provider_mismatch`; none configured -> `no_profile_for_provider`); `relay claude` now likewise picks the highest-priority *Claude* profile. Same single-writer rules, `--new`, `--no-attach`, supervised terminal.
- Safety fixes found on the way: the "is the existing writer still active" check used Claude's liveness even for a Codex-owned lease (now the owner's provider decides); Codex liveness/stop now also cover the interactive `codex resume` process (found through its `CODEX_HOME` environment), not only the short-lived `codex exec` pid recorded in the lease.
- Notes: `codex exec`/`resume` in 0.155.0 have no `--full-auto` (Relay forwards it verbatim, so Codex itself would reject it); not exercised live (a live `relay codex` would spend real Codex quota on an account already at 95% weekly).

## M9: status-line badge and hook-disabling Claude flags

- **Badge** `[Relay · <profile>]` (plain text, no colour/escapes) appended to the last line of the Claude status line via the existing Relay statusLine wrapper (`relay hook claude statusline`), which already chains the user's own status line (byte-for-byte output preserved; uninstall restores the original settings byte for byte; reinstall idempotent). Managed <=> the project's lease names this exact session id, so plain Claude sessions and Claude sessions the conversation has left (e.g. for Codex) show nothing; owner = lease owner (so it follows Claude -> Claude handoffs); `[Relay · switching → <target>]` only while this session's handoff journal is in progress. No quota/reset/fallback content.
- **Codex**: no badge. Codex's `tui.status_line` / `terminal_title` only take a fixed list of built-in item ids (current-dir, git-branch, model, session-id, …); there is no custom-text or external-command item, so nothing supported can show Relay's owner. Left unchanged rather than faking one.
- **Newly rejected Claude flags** (audited against `claude --help`): `--bare` (skips hooks; also OAuth/keychain), `--safe-mode` (disables hooks and all customizations), `--restricted` (ignores user/project/local settings files, where Relay's hooks live), and `--setting-sources` whose value lacks `user`. `--settings` (additive overlay; hooks merge) and other flags were audited and not rejected. Codex has no equivalent flags and is unaffected. Validation is shared by `relay claude`, `relay resume` and `relay switch`, always before anything is stored or started.
- Limitation: a user `--settings` that sets its own `statusLine` would replace Relay's wrapper (no badge/usage snapshot; hooks still fire); the badge needs the usage integration (installed by `relay setup`).

## M10: immediate Codex exhaustion preflight

- **Where it runs:** `relay codex` (before anything is stopped, created or spent), `relay resume` on a Codex-owned lease (before the terminal is planned), `relay switch <codex-profile>` (before the transaction commits). Same structured `account/rateLimits/read` interface; only `ordinaryUsageAllowed=false` is exhaustion; anything unavailable/ambiguous/wrong-home is `UNKNOWN`. Periodic supervised polling is unchanged.
- **Fresh `relay codex`, exhausted:** the `codex exec` bootstrap is never run; the existing hierarchy decision (`decide`, known-exhausted skipped, cooldown/loop guard not applied because no transaction is involved) picks the first eligible other profile and the conversation starts there via the normal `relay claude` / `relay codex` path. Codex's arguments are never passed to Claude. Output says "Codex profile 'X' is exhausted. Starting managed work on 'Y' instead."; JSON carries `prelaunch_fallback` with `handoff: false`. No eligible profile -> `no_eligible_profile`. `UNKNOWN` -> `codex_usage_unverified`, nothing started.
- **`relay resume`, exhausted Codex thread:** runs the same one-shot evaluation as `relay watch run` in-process (hierarchy, ledger, cooldown, real transaction; Codex -> Claude is `STATE_CONTINUATION`), then the terminal continues on the new owner. `UNKNOWN` never hands off; with no eligible target the terminal opens anyway and the periodic check keeps watching.
- **`relay switch` to an exhausted/unverifiable Codex target:** refused (`target_profile_exhausted` / `codex_usage_unverified`) before anything commits; never silently rerouted.
- **Live validation (real Codex account, currently exhausted; disposable workspace only):** `relay watch run --dry-run` reported the real profile exhausted (`dry_run_would_handoff -> erika`). `relay codex --profile codex-m6-validation` there printed the exhausted notice and started a new *Claude* session on `erika`: the real Codex profile's session directory still held exactly one session afterwards (no bootstrap ran) — this is **pre-launch fallback, not a Codex -> Claude handoff**. `relay switch codex-m6-validation` was refused with `target_profile_exhausted` and `relay status` still showed `erika` as the single owner. The real exhausted-thread `relay resume` (true `STATE_CONTINUATION`) could **not** be run live: a Codex-owned lease can no longer be created against an exhausted account through Relay (correctly), and hand-writing lease state was not attempted; that path is covered by the fake-provider E2E only. (Side effect fixed during validation: the live run wrote Herdr pane tokens for this session's pane because `HERDR_ENV` was inherited; they were restored to the real owner/session and later live commands scrub `HERDR_*`.)
