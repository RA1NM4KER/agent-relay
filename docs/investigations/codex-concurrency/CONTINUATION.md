# Continuation checkpoint

## Executor integration WIP from 8614951 (2026-11-01)

The next native shell containment slice is implemented in source, **not yet
experimentally demonstrated**. An explicit enrolled VM route checks native
session ownership at actual shell execution, records a durable realm intent,
clears the executor environment, and watches an owner-specific native revoke
fence. Experimental VM mode refuses unsupported mutations/model execution and
unclassified startup overrides. No protected host workspace is shared with the
RAM-only guest. No handoff receipt or production functionality claim was added.

Latest policy source passes **16 tests**, strict Clippy and formatting. An initial
native shell-route build passed; later source refinements did not link because
the unchanged 1.5 GiB reserve stopped the incremental build. The combined native
executor driver refused at its unchanged 4 GiB entry gate before launching a
runtime. The previous 47 native and 10 VM checks are retained historical evidence,
not results for these changes. No successful baseline concurrency test was repeated.

The user explicitly approved the named Brave HTTP/code/updater cache cleanup after
Brave closes. Checked cleanup reclaimed about 1.10 GiB; free space reached 5.54
GiB. After the pause, cache regrowth and swap pressure accompanied a drop below
4 GiB. Lossless permitted artifact compression recovered another 234.7 MiB, but
an incremental link reached the reserve. Current free space is about **1.6 GiB**;
Brave is running and no lab host/VM managers remain. Closing Brave for the already
approved paths, or a separately provisioned development volume, is required before
more native builds/tests. No additional cleanup permission is inferred.

Read [EXECUTOR_INTEGRATION.md](native-authority/EXECUTOR_INTEGRATION.md) for exact
build/evidence limits, controller-ingress propagation gaps, VM resource evaluation,
exit/cancellation UX gaps and the next executable test. The new source and manager
must not be labeled a contained native executor until that test actually passes.
Production Relay, installed binaries, active profiles and credentials are untouched.


## Native authority and kernel-realm increment from 60217b7 (2026-10-31)

Research-only native changes now place durable admission inside the app-server
and core execution boundary for classified operations. Enrollment, stable scoped
supervisor recovery, rotating single-connection controller leases, host generations,
actual native creation IDs, queue rechecks and fsync-before-forward/result-before-
reply ordering are implemented. Revocation fences the entire logical owner,
including its other roots, ephemeral children and fresh `/new` attempts. Transfer
always refuses with `executionQuiescent:false` and `handoffPermitted:false`.

The actual native raw-peer suite passes **47 narrow checks**, including actual
SIGKILL after durable intent before forwarding, lost creation replies, exact
committed replay without native execution, scoped B recovery, native B command
after A fencing, stale/foreign/raw peers and unknown RPC refusal. A strengthened
case proves an untouched empty conversation remains natively resumable after a
lost creation reply and host death. The first attempt exposed that native
ThreadPreparation only flushes; normal protected creation now uses actual native
Persist plus Flush before authority commit. Ephemeral persistence stays unchanged.

Further adversarial testing reproduced a cross-home bypass: a separate broker
could select the fenced original UUID by setting `sqlite_home` to the enrolled
store. A different-home explicit-path probe without shared native storage refused
only because history was absent; that incidental refusal is NOT ownership proof.
Native store/history source checks and refusal of unenrolled relocated stores
now pass direct and symlink-store native rejection tests. The final actual server
also passes the full 47-check native regression, 14 policy tests, Clippy with
warnings denied and formatting. All experiment failures and cleanup remain retained.

A bounded local Apple Virtualization experiment boots a pinned Alpine 3.22.7
arm64 kernel. Two independent VM realms pass **10 lifecycle/isolation checks**:
privileged detached guest work escapes a naive cgroup; exact forced VM A stop
establishes VZ stopped state while B's detached work continues; B then stops
independently. A manager-SIGKILL test retains **UNKNOWN**, never a quiescence
receipt. No disks, shared directories, network or VSOCK devices are exposed.
This is RAM-only kernel-realm evidence, not native execution containment or
macOS command parity. Native execution remains outside those realms.

The previous eight-shared-TUI 26-check concurrency feasibility result is retained
and was not rerun. Protected TUI capability plumbing, all autonomous/queued/core
paths, actual approvals, complete executor sinks, remote execution, host I/O
drain, OS isolation of the supervisor, safe handoff and real-world parity remain
unproven. Unknown native mutation routes and unsupported protected storage layouts
fail closed. Production ProfileSessionLock and production Relay remain unchanged.

See [native-authority/WORK_LOG.md](native-authority/WORK_LOG.md),
[protocol decisions](native-authority/PROTOCOL_DECISIONS.md),
[execution audit](native-authority/EXECUTION_INVENTORY.md),
[containment limits](native-authority/CONTAINMENT.md), and
[native evidence](native-authority/evidence/). Earlier claims that no VM was run
or no native gate was implemented are historical, not the current narrow result.
Full parity remains false. No merge, release, installed binary or credential change.


## Next work after the native-authority increment

Read the native-authority work log, protocol decisions, execution audit, final
manifest and indexed evidence first. Reuse the exact retained lab and source
cache; do not repeat completed concurrency feasibility, toolchain downloads or
baseline builds. The final authority server is a different binary from the
preserved baseline package. apply_native.py is a pin-checked reproduction tool,
not a command to run twice on the already modified checkout.

Remaining engineering is immutable session/epoch/realm propagation, autonomous
and queued admission, real approval races and executor integration. Kernel-only
VM evidence does not permit handoff. No privileged system provisioning is in
scope without explicit approval. Do not weaken production locks or change native
macOS execution semantics silently. The current Mac has about 2.44 GiB free and
no external physical disk; the shared harness still requires at least 4 GiB.
Additional source/evidence/required artifact deletion is not authorized. Obtain
safe disposable capacity before a Linux native build or integrated shared suite.
The present guest image has no persistent disk, network or native Linux Codex
build; it is an intentionally narrow containment experiment.

## Shared-daemon follow-up from 19d3c29 (2026-10-31)

The user approved the proposed Playwright cache cleanup. Three inactive caches
were removed (758 MiB); chromium-1248 was retained because NotificationCenter
still maps a resource from it. Lossless APFS compression reclaimed 3.56 GiB from
2,723 inactive native dependency artifacts with per-file decoded SHA256,
permissions and timestamp verification. Required compiled artifacts remain
available; two stopped lab package copies were retained as verified APFS clones.
Free space reached approximately 7 GiB. The 4 GiB entry gate and 1.5 GiB runtime
reserve remain unchanged. No native rebuild or repeated toolchain download ran.

Actual shared validation exposed two harness mistakes, retained as failures:
`app-server proxy --sock` is a raw byte bridge, while native Unix transport uses
WebSocket framing; Ctrl-C opens the shared task-exit menu rather than directly
interrupting. The observer now uses pinned websockets 15.0.1 over Unix, only for
initialize/loaded-root/turn-status reads. A real empty-daemon smoke test passes,
including mutation refusal and cleanup. Escape supplies the native interrupt.

The first Escape run passes 16 checks but fails to submit its third prompt after
resume; all evidence is retained. Explicit bracketed paste avoids the pinned
composer's Enter-as-newline burst heuristic. The final fresh shared run
`relay-native-tui-lab-zqnciiy7` passes all **26 narrow checks** against the actual
managed daemon: eight simultaneous native TUIs, exact in-memory ownership roots,
concurrent held response turns, interruption/disconnect, `/new`, `/fork`, exact
original-root resume, and A cancellation/frontend death while seven exact native
streams continue. All foreground statuses come from that daemon, not persisted
history or embedded fallback. This uses a local synthetic Responses provider,
not credentialed inference or tool/approval parity.

Cleanup exits zero, all frontend processes exit, the daemon PID is absent and its
socket refuses connections. **The native stop command forces shutdown after its
60-second grace period** while synthetic ephemeral helper requests are still
held. This is successful owned-process cleanup, NOT graceful drain or executor
quiescence proof. Shell snapshot validation and local MCP session-delete errors
in daemon stderr are retained as unresolved diagnostics; no native fix is claimed.

A separate pinned native Unix-host adversary uses two independent WebSocket
connections. The external contract fence refuses cooperating stale writes, but a
raw peer resumes the same root and executes a write anyway. A detached setsid
child continues effects after exact turn completion AND host SIGKILL. Forged
quiescence cannot release reservations. Reopening the contract service recovers
committed identity without replay, while its global generation also rejects B's
old admission: scoped recovery remains missing, even though B's existing command
continues. These are confirmed counterexamples, never parity passes.

The containment adversary records BLOCKED with exit 2 on this macOS host: its
candidate requires Linux plus explicit delegated research cgroups. No process-group
kill, quiet file, caller boolean or dead daemon is substituted for containment.
No handoff/ownership transfer is safe to validate until executor authority exists.

All failed runtimes cleaned up their eight frontends and owned managed daemon;
PID absence, refused socket connections and provider closure are recorded.
Evidence: `engineering/evidence/shared-daemon-20261031/`. All 27 feasibility Python
tests pass. Native host-wide fencing, descendant quiescence, transfer/recovery,
credentialed inference and full zero-regression acceptance remain unproven.
Production Relay, ProfileSessionLock, installed binaries and credentials remain
untouched. Earlier preflight/capacity statements below are historical.


## Resume from this checkpoint

Immediate shared behavioral milestone is complete to the narrow fixture scope.
Next implementation must move durable per-root epochs and operation identity into
the actual native host, covering lifecycle creation/resume/fork, queued mutations,
approval replies and autonomous work. An optional frontend callback or a Python
allowlist cannot enforce this boundary. Preserve independent raw-peer negative
tests and fail closed for unclassified paths.

The native `app-server/src/turn_admission.rs` gate is process-global
(closed/paused/active), without root/peer/epoch identity. The dispatcher rechecks
that global gate after request serialization, but this does not supply per-root
ownership. `ext/extension-api/src/turn_admission.rs` also admits autonomous starts
without root identity. Native integration requires a deliberate protocol/core
change, not treating these drain permits as durable ownership receipts.

Before transfer experiments, supply a real executor containment authority and
run its descendant-escape/queued-work/recovery tests. This Mac cannot run the
explicitly delegated Linux cgroup candidate. No permission to create a VM,
reconfigure host security or weaken native/production locks is inferred. A native
server implementation may be developed further, but full safety acceptance
cannot be established here with the existing executor capabilities.

The existing package/toolchain/cache are retained. Current free space after
lossless cloning of the two newly stopped package copies is about 5.46 GiB;
recheck it before any fresh shared test. Use the isolated dependency environment:

```sh
python3 /private/tmp/relay-native-build-20261030-9fw2zv7w/run_stage.py UNIQUE_STAGE /private/tmp/relay-native-build-20261030-9fw2zv7w/python-observer/bin/python /private/tmp/agent-relay-native-validation/docs/investigations/codex-concurrency/feasibility/native_tui_lab.py --codex /private/tmp/relay-native-build-20261030-9fw2zv7w/target/research-package/bin/codex --mode shared --holding-provider
```

Do not repeat the completed build/download or lower disk reserves. Native helper
turns can outlive foreground cancellation; managed forced cleanup must remain
visible and must never be called session-specific quiescence. The original
Playwright approval is resolved (three inactive directories removed, active
chromium-1248 retained). There is no pending deletion approval.

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
