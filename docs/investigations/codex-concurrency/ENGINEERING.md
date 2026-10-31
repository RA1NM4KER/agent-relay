# Controlled engineering validation, 2026-10-30

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


## Shared preflight follow-up from 45670f2

Shared validation is still blocked below 4 GiB; additional test-browser cache
removal awaits explicit approval. No new shared suite passes. The pinned package
and native sources/hashes remain unchanged, and all 57 prior native-role evidence
files reverify. A loopback held-response fixture passes actual native stdio
cancellation, and 27 Python checks pass. Native eight-frontend SIGTERM and injected
terminal-write ENOSPC cleanup checks pass. These do not certify shared mode.

The prepared shared harness now verifies in-memory daemon roots and exact native
turn statuses over a restricted read-only proxy, holds eight real response streams
for cancellation/neighbor-progress checks, retains incremental evidence, and
cleans up before terminal evidence writes. Shared execution of these additions is
unverified. See [CONTINUATION.md](CONTINUATION.md) and
`engineering/evidence/native-shared-preflight-20261031/` for failures, limits and
the exact resume command. Full parity and production safety requirements remain.

Latest follow-up: user-authorized cache cleanup resolved disk pressure and the
pinned CLI now links. Eight actual rebuilt embedded TUIs pass initial selection
reporting but fail /new due to missing background-root role handling and blocked
interruption after callback refusal. See NATIVE_BUILD.md and the retained native
runtime evidence. Production and all full-parity requirements remain unchanged.

Pinned build continuation from aa96aea: [NATIVE_BUILD.md](NATIVE_BUILD.md).
Actual native client/TUI cargo check now passes with isolated Rust 1.95.0 and
the original source insertion. Full CLI linking is blocked by the lab's disk
reserve; neither native TUI runtime mode ran. Native enforcement, executor
quiescence and full parity remain unproven. Prior experiments below are retained.

Capability follow-up from c113acf: [LAB_SETUP.md](LAB_SETUP.md) records checkpoint
verification, the blocked remote-backup attempt and exact environmental changes
needed for native integration. The execution environment remains unsuitable;
this follow-up does not introduce another unvalidated architecture or rerun the
already completed native experiments.

## Result and boundary

Progress is preserved on `research/codex-concurrency-native-validation`, based on
`7a6466667ddf1aa3196a9351d5f87c90d9527c77`. The entire prior research packet was
committed as `afabe33dda0e3ff358041ff9336914e4c12cf692` before new experiments.
The original checkout cannot write Git refs in this sandbox. Development therefore
uses `/private/tmp/agent-relay-native-validation`; a verified bundle and original
77-file SHA-256 inventory are retained in `docs/investigations/checkpoints/` in
the original workspace. The pre-existing untracked AGENTS.md is not committed.

**Full native concurrency and zero regressions remain unproven.** None of the
three critical enforcement foundations is complete. The additional working
prototype validates operation recovery and admission composition against actual
native execution. It does not implement the authoritative native host boundary.
Production Relay, ProfileSessionLock, installed binaries, credentials, active
sessions and release artifacts remain unchanged.

## New executable increment

`engineering/host_contract.py` adds an exclusive laboratory service over the
existing real-backend controller. It composes:

- Opaque random peer capabilities; clients cannot choose a public owner name.
  Only capability digests are persisted. This is an in-process API, not a secure
  IPC transport or a security boundary against arbitrary same-uid code.
- Fresh service generations, retained global conversation reservations, per-peer
  serialization and the earlier per-thread admission locks.
- A FULL-synchronous operation journal binding peer + token to a fingerprint of
  method, parameters and selection sequence. Request bodies are not journaled.
- Intent-before-forward and result-before-reply ordering. Concurrent retries of
  a committed operation return the exact original result without forwarding.
- Read-only operation-status recovery after service restart; old-generation
  capabilities cannot silently regain mutation rights. UNKNOWN never retries
  native work or guesses a result from other conversations.
- Exact response-bound creation and fork/resume identity, session-scoped native
  events, and explicit turn-drain-only receipts. Full transfer always refuses
  because no authoritative executor receipt exists.

The service lock stays held while admitted operations finish during close. Two
deterministic tests cover concurrent token retry and close-versus-admitted-call
ordering. This lock protects one supplied laboratory service directory. It is
**not** a host-wide registration mechanism across arbitrary directories/hosts.
Likewise the token currently exists in the service journal, not Codex storage.

## Integrated experiment

Run from the repository root:

```sh
python3 -m unittest discover -s docs/investigations/codex-concurrency/engineering -p test_host_contract.py -v
python3 docs/investigations/codex-concurrency/engineering/run_integrated.py
```

The runner uses a fresh HOME/CODEX_HOME, two disposable projects, eight logical
clients and the installed pinned 0.166.0 backend on its actual stdio interface.
It runs native shell commands; there is no model inference, authenticated quota
or native TUI claim. Commands and their detached children self-expire. Neither
profile scans nor guessed PID termination are used. Results archive in each new
temporary root; retained copies are under `engineering/evidence/`.

The final run passed **22 narrow checks**:

- Eight distinct roots execute simultaneously across two projects.
- Eight concurrent retries of one committed creation return one identical binding
  with no additional native request. Token substitution and unknown peers refuse.
- Eight concurrent fork/new/resume flows return to their exact original roots;
  24 foreground/background roots retain reservations.
- Fencing/interruption stops A's parent command while seven neighbors progress.
- Stale service authority and forged quiescence refuse before native forwarding.
- A deliberately lost pre-commit result becomes durable UNKNOWN; retry refuses
  while B's existing root remains usable. This fault injection occurs after
  native + registry application, **not** an actual native-host SIGKILL.
- Reopening the journal recovers committed results and UNKNOWN; old service
  generations cannot mutate. Scoped events cover all eight original roots and
  no account/usage polling is issued.

Two checks are successful **falsifications**, not successful safety guarantees:
A's detached child still writes after turn drain; a direct request to the real
native dispatcher still writes through the service fence. The test driver retains
the raw backend handle specifically to exercise this bypass. Removing that handle
from ordinary clients does not remove independent native entry points.

`integrated-initial.json` retains the earlier 20-check run. The final
`integrated-transitions.json` adds the eight navigation flows; both explicitly
record `full_parity: false` and `native_host_enforcement: false`. Neither result
should be described as eight native TUIs or a completed automatic handoff.

## Native build and host insertion findings

The pinned full-source fetch was attempted again and failed with exit 128:
`Could not resolve host: github.com`. TCP and Unix binds were independently
rechecked and both fail EPERM. The complete upstream workspace/dependency closure
is absent; only selected source and the independently compilable bridge core are
available. Thus **no complete patched TUI build or execution occurred**. See
`engineering/evidence/environment.json`; do not substitute patch applicability
or the bridge-core binary for this gate. The exact full-source build/PTY commands
remain in FEASIBILITY.md.

New pinned primary source was inspected and retained:
[Codex TurnAdmission](https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/app-server/src/turn_admission.rs),
Git blob `552875473b9929a02317a892f0e7d02d9a7dfe7a`. It already serializes admit
versus close and counts outstanding permits. Its pause is temporary; dropping
the pause reopens admission. The state contains only `closed`, `paused`, `active`:
no thread identity, authenticated controller, durable epoch or executor receipt.

The pinned MessageProcessor's request path (lines 1025–1071 in the cached source)
already admits lifecycle/runtime changes and rechecks queued turn-producing work
before dispatch. Its extension construction (lines 337–374) passes a shared
`TurnStartAdmission` into extension dependencies. These are useful native seams,
but not evidence of complete coverage: the public trait call here supplies no
root/controller identity, and separate native processes have separate gates.
An RPC interceptor alone would miss autonomous work unless the internal admission
and executor paths also propagate authority. The old frontend classification also
omits native review, compact, queue, realtime and settings/revert operations that
appear in this host admission match. Expanding an allowlist without their scoped
authority would be unsafe.

Historical incidents were rechecked: `75ba067` documents the competing observer
resume winning the writer race; `b2320f5` documents remote-mode permission loss.
This increment adds neither an observer writer nor a forced remote TUI. A new
native integration must continue to resolve ordinary local permission parameters
inside Codex and observe via the original controlling relationship.

## Executor containment: additional falsification obligation

The earlier cgroup experiment demonstrates inheritance and killing an owned scope
only against cooperative descendants. It does not establish that a tool cannot
leave that scope. If the executor inherits the manager's authority to write a
parent `cgroup.procs`, a new process can attempt to move itself outside the group.
`cgroup.kill` on the now-empty group would then prove the wrong thing.

`engineering/containment_escape.py` is a new explicit-delegation adversary. It
creates only a fresh UUID scope, admits only its own gated child, then has that
child call setsid and attempt self-migration to the supplied parent. It records
whether writes survive killing the original scope. The child is bounded and
self-expires even after escape. An allowed migration is a falsification, not a
passing containment test. EPERM/EACCES/EROFS demonstrates only this escape path
is denied. No result is promoted to a full executor receipt.

This experiment is **BLOCKED on macOS**, exit 2; Linux escape was not observed
here. Do not report the predicted escape as a measured Linux finding. On a
dedicated Linux lab with explicitly supplied research delegation, run:

```sh
python3 docs/investigations/codex-concurrency/engineering/containment_escape.py --delegated-parent /explicit/research/cgroup --output /tmp/relay-escape-result.json
```

A viable Linux adapter must separate manager authority from tool authority,
close inherited management descriptors, prevent migration/namespace escapes,
and prove the scope empty after an irreversible admission fence. Same-uid writable
delegation alone is not sufficient evidence. macOS still lacks a demonstrated
equivalent adapter; process groups, PID lists and turn completion are rejected.
Native sandbox permissions generally authorize permitted writes; their presence
does not demonstrate revocation of those writes at handoff.

Subagents, external/shared MCP services, remote executors and scheduled work must
also belong to the epoch or offer an authoritative cancellation/fence protocol.
Local containment cannot certify an outstanding request in an external service.
A missing adapter must retain ownership and fail closed, which blocks feature
parity until that supported execution mode has a solution.

## Next production-capable increment

1. Obtain the complete pinned workspace/dependencies on a suitable lab host and
   build the existing thin TUI patch. Run both embedded and shared-daemon PTY
   modes. Fix coverage failures explicitly; preserve native permission behavior.
2. Implement a native controller service at the thread-store/runtime boundary,
   not the laboratory wrapper. Persist managed-root controller epochs and
   creation-token results atomically with native root creation, before execution
   or publication. A lost frontend reply can then use token status; a crash
   between native creation and a separate Relay journal cannot be wished away.
3. Require authenticated controller authority for every managed-root mutation,
   including raw requests, approval replies, queued/goal continuations and
   subagent work. Recheck epochs at dequeue/executor start. Read-only observers
   get no write admission. Compose per-root permits with the existing native
   writer lock and a global registry across processes/projects/state roots.
4. Define irreversible durable fence milestones: no-new-admission, accepted work
   drained, executors fenced/empty, persistence closed, snapshot committed. A
   receipt names root, epoch, host generation, executor domain and operation
   token; its authority comes from the enforcing host/OS, not caller JSON.
   Keep ownership across restart until authoritative reconciliation succeeds.
5. Implement and independently falsify platform/external-executor adapters.
   Only then wire Relay's journal, exact source snapshot, target verification,
   usage-event wakeup and recovery to the receipt. No source reservation release
   or target commit before this point. Retain production profile exclusivity.

The service prototype is a protocol/recovery test substrate. Shipping it as an
external gateway would leave both proven bypasses unresolved. The native-host
direction remains preferred, but cannot be declared feasible at full parity
until actual host/executor enforcement and rebuilt native clients pass.

## Validation scope

New host-contract tests: 2 passed. Existing controller obligations: 8 passed.
Existing production quota-scheduler tests: 42 passed. Retained phase-1 (21 checks)
and phase-2 evidence validators pass. Python compilation passes. No production
Rust files changed, so a new full workspace run was not used to repeat already
documented unrelated/environment failures. Earlier full-suite failures remain
open in EXPERIMENTS.md. New integrated costs, whole-tree memory, eight-TUI/Herdr,
successful inference/subagents, real quota and Claude/Codex handoffs remain
unmeasured/unverified. The sub-second native shell laboratory duration is not a
performance acceptance result.
