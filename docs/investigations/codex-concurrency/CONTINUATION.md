# Continuation checkpoint

## Shared validation preflight from 45670f2 (2026-10-31)

Reviewed the requested documents and reverified all 57 indexed native-role evidence
files, the six built source files, CLI/package hashes and normalized dependency
lock. No source/toolchain downloads or native rebuilds were repeated. Production
Relay, ProfileSessionLock, installed binaries and credentials remain untouched.

Removed 212 MiB of allocated inactive Homebrew API/Bootsnap, npm npx and pnpm dlx
cache entries under existing authorization, after process/open-file checks. Their
exact local-only inventory remains in the original workspace checkpoints. Free
space still fluctuates around 3.8 GiB, below the 4 GiB entry requirement. Approval
is pending for approximately 1.07 GiB of inactive Playwright test-browser caches;
those files have NOT been deleted. No external physical volume is attached. A
same-disk sparse image adds no capacity, and a 4 GiB RAM volume is unsuitable on
this 8 GiB machine with roughly 3.3 GiB swap already used. No other cleanup or
volume creation was attempted.

The harness now refuses shared startup below 4 GiB, before launching any native
runtime. That actual precondition rejection is retained. The original 1.5 GiB
reserve and SIGTERM cleanup remain. **No new shared suite was executed or passed.**
The read-only shared proof and new shared cleanup readbacks remain unvalidated
against a running daemon until capacity is available.

Prepared stronger shared oracles without rebuilding Codex:

- A supported `app-server proxy --sock` connection issues only initialize,
  `thread/loaded/list` and `thread/turns/list`. Exact foreground roots must be in
  that daemon's in-memory manager, and accepted turns must report inProgress /
  interrupted. This rejects silent embedded fallback without a second resume
  writer. It is test observation, not host-wide admission or peer authentication.
- An opt-in loopback HoldingProvider supplies valid Responses SSE and keeps
  requests open. Native `thread-id` headers identify requests; bodies and other
  headers are discarded. Native TUIs must have eight simultaneous exact streams,
  interrupt A while B–H progress, kill A's idle frontend while B–H remain active,
  then interrupt those seven. No model tools or credentialed inference is used.
- Progress snapshots, bounded terminal/event records, frontend exit codes and
  native cleanup stdout/stderr are retained. A stale Unix socket inode is not
  treated as a live daemon: cleanup uses PID absence and connection refusal.
  Binary hashes stream through bounded buffers to avoid allocating the entire
  574 MiB debug executable on this memory-constrained host.
- Evidence-write failures cannot skip later process cleanup. Native SIGTERM
  regression passes with eight frontends. A second actual eight-frontend probe
  injects ENOSPC on every terminal evidence write; all eight still exit, and
  every injected failure is recorded. These are cleanup checks, not shared proof.

Validation: 27 Python checks pass, including eight real loopback fixture clients,
expiry/stale-disconnect negatives, read-only RPC refusal and stale-socket cases.
The actual pinned native stdio fixture check reports one exact turn interrupted
and its HTTP stream disconnected; execution_quiescent remains false. Its initial
caller used both an explicit interrupt and the old drain helper, issuing a second
interrupt that timed out. That failed result is retained unchanged; the corrected
single-interrupt check passes. Duplicate-interrupt behavior remains a separate
unresolved research concern, not a claimed provider fix. The initial ENOSPC test
launcher also failed before runtime due to a missing Python module path; both
that failure and the corrected passing run are retained.

Evidence: `engineering/evidence/native-shared-preflight-20261031/`. Full workspace
Rust checks were not rerun: production Rust and native binaries are unchanged,
and prior baseline failures remain outstanding. Full parity remains unproven.

Next authorized runtime step, ONLY after at least 4 GiB is confirmed (prefer
6–8 GiB), using a NEW stage name and a fresh isolated home:

```sh
python3 /private/tmp/relay-native-build-20261030-9fw2zv7w/run_stage.py shared-held-responses-after-capacity python3 /private/tmp/agent-relay-native-validation/docs/investigations/codex-concurrency/feasibility/native_tui_lab.py --codex /private/tmp/relay-native-build-20261030-9fw2zv7w/target/research-package/bin/codex --mode shared --holding-provider
```

Do not rebuild/download the existing package or lower either disk gate. Do not
use the stdio/cleanup checks as eight shared native TUIs. Host-wide native fencing,
execution-tree containment, quiescence, scoped transfer/recovery and real-world
acceptance remain release blockers; earlier bypass/descendant counterexamples
still apply. Production ProfileSessionLock stays in place.

Date: 2026-10-30. This is a research/engineering branch, not a release.

## Native role/cancellation follow-up (2026-10-31)

The previous embedded `/new` blocker is repaired in the disposable prototype.
Native lifecycle responses now bind immutable ephemeral roles separately from
primary/display identity. Ordinary writes require the acknowledged primary;
owned ephemeral helpers keep their own identity. Quarantined/fenced owners may
request interruption/unsubscription only for their existing exact reservations;
normal writes remain refused. Metadata inventories are explicitly classified.
Selection and native view updates commit together. The loaded-selection path is
also instrumented. None of this adds native host epochs or executor containment.

The updated pinned CLI builds with isolated Rust 1.95.0 and --locked. SHA256:
`dfc5fa0a2e9b3522665ad8d8db49dd98898f174f2306f8f6203ab3f2a2ee173e`.
Twenty-two Python obligations, two Rust policy tests, standalone probe Clippy
with denied warnings and research Rust formatting pass. Native source generation
matches all six built files. The first logger compiler error and a harness
schema-initialization race are preserved alongside the successful corrections.

Eight rebuilt embedded TUIs now pass all nine narrow checks: initial exact IDs,
foreground starts and completed interruptions, `/new`, second completed turns,
`/fork`, exact native picker resumes and seven surviving frontends after A dies.
The observer uses the original client event stream; no second resume writer is
created. Forty reservations remain ACTIVE (24 ordinary, 16 native ephemeral).
Native turn completion is not proof that detached executors have stopped.

The complete lab package now builds, including real pinned code-mode/executor
helpers and checksum-verified Codex V8 artifacts. The bare CLI package-layout
failure, default V8 URL 404, and a 90-second install timeout are preserved. A
sample confirms debug package hashing dominates preparation. With a bounded
180-second installation window, the managed daemon starts and eight actual shared
TUIs report exact bindings, but the 1.5-GiB reserve guard terminates the harness
before the suite completes. This is DISK-BLOCKED, not a shared-suite pass.

The guard exposed missing SIGTERM cleanup in the harness. It now preserves partial
evidence and enters cleanup; a real eight-TUI SIGTERM test passes with no surviving
frontends. Cleanup errors also fail a run. The interrupted shared daemon's native
stop timed out at 60 seconds; only that exact lab PID/start-time/executable was
terminated, with no reservation release or handoff claim. No lab processes remain.
Verified duplicate package caches and completed crate-download archives were
removed under the existing cache-cleanup authorization; source, toolchain, built
binaries, validated package and evidence remain. About 2.5 GiB free remains.

Next: provide at least 4 GiB free for a fresh shared run (native debug installation
materializes another roughly 0.9 GiB package plus runtime state). Do not keep
retrying against the reserve, substitute dummy/installed helpers, inject remote
flags, reuse uncertain bindings or accept embedded fallback as shared proof.
Evidence: `engineering/evidence/native-roles-20261031/`.
Production remains untouched. Full parity, zero regressions, host enforcement,
executor quiescence, quota handoff and credentialed/Herdr coverage are unproven;
all earlier counterexamples and baseline failures remain applicable. Production
Rust sources are unchanged. Full workspace Clippy/tests are not rerun in this
increment after cache recovery; the earlier documented baseline failures (including
D3) remain unresolved, and no fresh full-workspace certification is claimed.

## Storage recovery and linked native TUI follow-up

User-authorized cache cleanup reclaimed about 6 GiB while preserving this lab's
cache and all source/evidence/active profiles. The cached pinned CLI build now
passes; the binary reports 0.166.0. See NATIVE_BUILD.md's latest section and
engineering/evidence/native-tui-20261030/. Do not repeat setup or cleanup.

Eight actual rebuilt embedded TUIs pass initial pre-prompt selection reporting,
then fail the /new gate. Native ephemeral structured-request threads are owned
but incorrectly constrained to foreground display selection; refusal quarantines
the owner and the bridge's global failed flag then blocks turn interruption.
Startup read-method coverage is also incomplete. Preserve these failures and
repair native role propagation and narrowly owned cancellation before retrying
embedded/shared gates. Do not disable background features, clear reservations,
weaken method policy or infer binding from terminal output. Production is unchanged;
host enforcement, executor quiescence, successful handoff and full parity remain
unproven. Earlier disk-blocked sections below are historical, not current blockers.

## Pinned native build follow-up from aa96aea

[NATIVE_BUILD.md](NATIVE_BUILD.md) records the now-successful isolated Rust
1.95.0 setup, exact upstream source and actual native client/TUI type-check.
The source insertion compiled without changes. A release-tag lockfile mismatch
required only 170 local package version stamps to match 0.166.0; all external
dependencies/checksums/edges are unchanged and builds retain --locked.

The linked CLI build remains **BLOCKED_DISK_RESERVE**, not compiler-failed:
three bounded attempts ended at the lab's 1.5 GiB reserve, even after reclaiming
only generated lab caches/history. Native core compiled; the final attempt
reached app-server. No CLI binary exists, and no embedded/shared TUI runtime
suite was run. All owned build process groups are gone. Production is untouched.

The complete lab and compiled cache remain at
`/private/tmp/relay-native-build-20261030-9fw2zv7w`. First provide additional
storage, then use NATIVE_BUILD.md's new-stage resume command; do not repeat
already completed research or source/toolchain downloads. Host admission and
executor quiescence obligations remain unchanged. The original research tip
aa96aea was backed up remotely after a fresh workflow audit; final new checkpoint
identity/bundle verification are recorded outside the bundle in checkpoint.json.

The sections below retain earlier historical state and blockers. Their DNS,
listener and ps failures are superseded only by the new capability/build evidence.

## Latest capability follow-up

The continuation from c113acf verified its bundle/hash and all 90 mirrored files.
Current DNS, listener and own-process inspection probes remain blocked; PTYs work.
No native suites were rerun. The remote research ref was not found, and remote
backup via GitHub create_blob was rejected because write approval is required
while session policy is never. No remote backup exists from that attempt.

[LAB_SETUP.md](LAB_SETUP.md) contains precise required capability changes, the
verified Rust 1.95.0 pin, safe branch-only push instructions, and the native build/
integration sequence for an authorized disposable lab. The deployment workflows
were checked against remote blob hashes; research backup matches none of their
deployment triggers. The research checkout's origin is the local original
workspace, so use the explicit GitHub URL when backing up remotely.

This follow-up stops at the environmental boundary as requested. It changes no
prototype/production implementation or guarantee status. A new documentation/
evidence commit follows c113acf; exact tip and bundle hash remain in checkpoint.json.

## Durable state

- Original workspace: `/Users/kefasmanda/repos/agent-relay`, still on main at
  `7a6466667ddf1aa3196a9351d5f87c90d9527c77`; no production source modified.
- Development checkout: `/private/tmp/agent-relay-native-validation`.
- Branch: `research/codex-concurrency-native-validation`.
- Prior research checkpoint: `afabe33dda0e3ff358041ff9336914e4c12cf692`.
- Integrated engineering increment: `53c6a64` (full hash in bundle/commit log).
- A subsequent documentation commit adds this continuation record. The exact
  final tip, bundle SHA-256 and verification are in the original workspace's
  `docs/investigations/checkpoints/checkpoint.json`.
- Bundle: `docs/investigations/checkpoints/codex-concurrency-native-validation.bundle`
  in the original workspace. It requires base `7a6466667ddf1aa3196a9351d5f87c90d9527c77`,
  which is already present in the original repository. Original research checksum
  inventory: `docs/investigations/checkpoints/research-inventory.json`.
- Research documents/code/evidence are mirrored back to the original workspace.
  AGENTS.md remains separate/untracked. No push, merge, installation or deployment.

If the temporary checkout disappears, restore into a new disposable checkout:

```sh
LAB=$(mktemp -d)
git clone --no-local /Users/kefasmanda/repos/agent-relay "$LAB/relay"
git -C "$LAB/relay" fetch /Users/kefasmanda/repos/agent-relay/docs/investigations/checkpoints/codex-concurrency-native-validation.bundle refs/heads/research/codex-concurrency-native-validation:refs/heads/research/codex-concurrency-native-validation
git -C "$LAB/relay" switch research/codex-concurrency-native-validation
```

Do not commit a bundle into itself. Keep bundles/checkpoint metadata outside the
branch's codex-concurrency research directory. Retain earlier raw evidence rather
than overwriting it when running a revised experiment.

## Completed and observed

Read FEASIBILITY, ACCEPTANCE, EXPERIMENTS, DEFECTS; inspected relevant prototypes,
production profile-lock/watch integration and regression tests; rechecked the
observer-writer and remote-permission historical incidents.

New code: engineering/host_contract.py, run_integrated.py, test_host_contract.py,
containment_escape.py. New primary source: pinned native TurnAdmission + blob
manifest. Detailed analysis and production-capable blueprint: ENGINEERING.md.

Two real-backend integrated runs retained; final 22 narrow checks pass. Eight
logical controllers execute native commands and fork/new/resume concurrently.
Operation replay avoids duplicate native creation after a committed lost reply;
uncertain results retain ownership; service reopen rejects stale generations.
Stopping A's turn leaves B–H progressing. Detached A and raw-native bypass remain
positive counterexamples. Full handoff refuses; full_parity is explicitly false.

Tests: 2 new contract interleavings, 8 existing controller obligations, 42 existing
production scheduler regressions pass. Both prior evidence validators and Python
compilation pass. Production Rust has no diff. No new full workspace certification;
earlier documented failing/ignored tests remain outstanding.

## Exact blockers

1. Full Codex checkout fetch failed DNS (exit 128). Selected source is not a
   complete build workspace. Full patched native TUI has NOT been built/run.
2. TCP and Unix listeners both fail EPERM. Shared-daemon multiconnection and
   deterministic inference backend tests cannot run here.
3. Native dispatcher and native root persistence do not contain the laboratory
   token/epoch contract. External wrapping is not host-wide enforcement.
4. No platform/native executor quiescence adapter demonstrated. Linux cgroup
   escape probe exits BLOCKED/2 on macOS. Do not turn that into a passing test.
5. No disposable authenticated provider accounts, real quota exhaustion or live
   Herdr testing used. No credential access is authorized by this checkpoint.

## Next executable work, in order

1. On a disposable development host with source/dependencies available, run the
   full pinned-source patch/check/build and both native_tui_lab modes exactly as
   listed in FEASIBILITY.md. Capture compiler errors and actual structured IDs.
   Do not treat source applicability or bridge-core compilation as this result.
2. Port the operation contract into native thread creation/store and scoped
   admission. Extend existing TurnAdmission rather than adding a second writer.
   Source evidence identifies queue rechecks and extension admission seams; the
   no-thread trait and per-process state require actual changes. Persist token
   and binding atomically in native creation, not in a later Relay transaction.
3. Re-run run_integrated.py against that actual native enforcement, changing its
   bypass oracle only when raw native requests demonstrably refuse. Add multiple
   independent clients/processes, stale approval/queued work and real host kills
   at every persistence prefix. Never remove the current falsification oracle
   merely because a cooperative gateway passes.
4. On explicitly delegated disposable Linux, run containment_escape.py, then the
   earlier linux_containment.py. Address self-migration/management authority,
   descriptor inheritance and namespace escape before accepting an empty-group
   receipt. Develop a separate macOS adapter; retain reservations if unavailable.
5. Integrate native executor accounting (shell/background/subagents/MCP/remote)
   and durable fence milestones before enabling any ownership transfer. Then
   test the actual Relay journal/snapshot/target/recovery chain and quota events.
6. Complete eight native TUI/Herdr, natural quota, cross-provider, performance and
   mixed-version acceptance gates. Preserve production ProfileSessionLock until
   all supported modes pass. D1/D2 remain separate follow-ups in DEFECTS.md.

The strongest architecture remains native host epochs + atomic creation tokens +
global admission + executor authority, with a thin native TUI binding. This
checkpoint advances its test substrate, not proof that its critical boundaries
are solved. No zero-regression or full-parity claim is justified.
