# Native executor integration: actual shell slice demonstrated

## Resume from 686d93f (2026-11-01)

Real pinned Codex now executes explicit native `thread/shellCommand` requests
inside enrolled, session-specific Linux VM kernels on this Mac. This is an
actual app-server -> native Core -> native executor -> VM -> guest shell path,
not a standalone VM test or a cooperating external ownership wrapper.

The immediate experimental shell milestone is demonstrated. **Full shared-profile
safety, native macOS command parity, host-workspace quiescence and safe handoff
are not demonstrated. Handoff remains disabled.** The guest workspace is RAM-only;
no host directory, disk, network or VSOCK is exposed. No model inference was used.

## Observed native integration

| Experiment | Observed result |
| --- | --- |
| Initial native revocation (`relay-native-executor-fxnwborl`) | 11 checks pass: two exact native roots launch separate kernels, detached guest output is delivered through actual native output deltas, A's native revoke stops A's exact kernel, B continues, both close independently. |
| Native interruption counterexample (`relay-native-executor-wq6jbn59`) | 13 fail-closed checks pass, but **A's kernel cleanup is UNPROVEN**. Native interruption killed the manager before its stop record; no manager process remains, which is not quiescence proof. Native handoff refuses. This remains a retained negative result. |
| Delayed native cancellation after repair (`relay-native-executor-hdfqbif2`) | 15 checks pass. An injected 300 ms manager stop delay exceeds ordinary Core's 100 ms teardown grace. Native interruption waits 0.4144 s, observes the exact stopped kernel, records native `interrupted` turn and failed command exit -1, while B's detached output advances. Both exact kernels stop. |
| Foreground completion (`relay-native-executor-f4qbpbaa`) | 14 checks pass: foreground shell exits while a setsid child is producing effects; native turn completion is accompanied by a trusted `GUEST_STOPPED` kernel record. B continues. No assumption that turn completion alone killed descendants. |
| Ordinary protected native regression | Final server passes all 47 existing native checks; actual host crash, durable replay and independent-owner behavior remain covered to their original scope. |

These counts overlap; they are not 40 distinct safety guarantees. Output progress
uses only `item/commandExecution/outputDelta` for the exact native thread, never
the command text present in started-item events. Kernel stop comes from trusted
host VZ lifecycle state with the native execution's exact identity, not guest text
or process disappearance. Host-only research canaries remain unchanged.

Final tested native server SHA256:
`324607d14e973ff2e4cdf33b6ec41ca820c185cba7bf5d42c54c5d53183aca06`.
Final tested VM manager SHA256:
`67b8b0d35e9453617cc398d2e1178685e9f2304e39cd7f2efc9ff6e89edd05d0`.
The initial successful server/manager are preserved as verified APFS clones at
`lab/native-server-pre-cancel-drain` and `lab/vm-exec-pre-cancel-drain`. All raw
native events, drivers, results, execution intents, manager states and failed
checks are retained in `evidence/executor-resume-686d93f/`. Generated supervisor
capabilities remain in private lab vaults and are excluded from the research packet.

## Cancellation repair

Native Core previously dropped the execution future as soon as cancellation
arrived, allowing process teardown to destroy the manager before VZ acknowledged
stop. A typed native shell route now retains its immutable execution UUID.
Cancellation writes a fsynced marker for that exact execution and retains the
execution future for a bounded five-second drain. The manager watches that
marker as well as its owner-wide revoke fence.

Core's task supervisor normally aborts unfinished work after 100 ms. Only enrolled
experimental shell sessions now receive a six-second grace, permitting the bounded
drain; ordinary profiles keep their original grace. An explicit 300 ms stop-delay
fault exercises this distinction. Stop evidence must match native thread/root,
owner, epoch, host generation and execution identity; the trusted manager record
must match the realm and root and show closed admission and a stopped kernel.
A missing/malformed/mismatched result remains UNKNOWN and cannot permit handoff.

Cancellation does not revoke the entire owner: it closes one execution. Owner
revocation closes that owner's kernels through a separate persistent fence.
No generic caller-supplied boolean or receipt is accepted as a handoff authority.

Latest research Rust: **17 policy tests pass**, strict all-target Clippy and cargo
formatting pass. One parallel test run failed before host creation because two
wall-clock temporary-directory names collided. The fixture now reuses the already
pinned UUID generator; the failure and corrected result are retained. This is a
test-only change after native linking; the serving-source prefix is byte-identical.
No native download or dependency change was needed. Original baseline package,
production Relay, ProfileSessionLock, installed binaries and credentials are unchanged.

## Capacity and resource observations

Brave was verified absent from the process table; both existing approved cache
paths had no open files. Only those HTTP/code caches were removed (856,604,672
allocated bytes, about 817 MiB). The specifically approved updater path was
already absent. Free space reached 6,586,413,056 bytes, and both original disk
guards were preserved. No additional cleanup ran. Final free space is recorded
in the evidence readback; no lab native host or VM manager remains.

Each VM configures 192 MiB; two configure 384 MiB. The initial native run sampled
manager RSS at 11,312 and 11,936 KiB; this **excludes unmeasured guest/helper and
whole-runtime footprint**. Initial host startup plus two native dispatch/guest
output observations took 7.633 s; the delayed-cancellation experiment took 7.929 s
for the same broad setup interval. These are individual observations, not a
latency distribution or a production performance acceptance.

Eight per-session VMs would configure 1.5 GiB before other runtime costs.
VM-per-session is **not selected for production**. A shared Linux VM with a trusted
outer executor and nonprivileged session cgroups/namespaces remains a credible
lighter candidate requiring actual escape/isolation tests. It still changes
macOS command semantics. Deprecated macOS sandbox-exec or process groups alone
provide no demonstrated equivalent dynamic revocation/quiescence guarantee.
No privileged principal, filesystem extension or host security setting was changed.

## Remaining proof obligations and next engineering

- This proves closure of the explicit RAM-only user-shell execution realm. It
  does not close every native execution sink, model/autonomous/subagent/MCP/remote
  capability or host-side persistence/queue operation. Experimental mode refuses
  unsupported mutations and startup overrides; this is not a full native UI mode.
- Execution grants still snapshot controller generation at launch rather than
  carrying immutable ingress operation authority through every queue. A delayed
  old-controller launch across rotation still requires implementation and tests.
- Host/backend/manager crashes beyond the retained interruption counterexample,
  evidence loss, stopped-record replay and complete recovery need integrated tests.
- Host workspace I/O is not shared, brokered or drained. No worktree continuation,
  native macOS tool behavior, arbitrary exit-code/background UX or full session
  quiescence guarantee follows from these kernel-only effects.
- Native frontend capability plumbing, complete permission/quota/cross-provider
  acceptance, eight protected TUIs and Relay's transactional handoff integration
  remain future work. Do not enable any handoff transaction on these results.

Next focus is immutable queue authority and a real workspace/capability boundary,
not repeating the already demonstrated concurrency feasibility or claiming that
these scoped kernel records are complete Relay handoff receipts.

---

## Historical implementation checkpoint before this resume

### Executor integration from 8614951 — incomplete, historical

No native executor containment acceptance result has passed in this increment.
The combined test refused at its unchanged 4 GiB entry guard before creating a
runtime. A subsequent native link stopped at the unchanged 1.5 GiB reserve.
Both failures are retained in `evidence/executor-integration-8614951/`.

## Implemented source

An explicit, digest-bound `shell_realm` enrollment policy adds a native core hook
at actual `execute_user_shell_command`, before host shell snapshots are obtained.
The hook checks actual session identity and current ownership again at execution
time, including when the experimental VM route is absent. In VM mode it checks
the enrolled manager/kernel/initramfs hashes, reserves a fresh realm UUID, and
fsyncs the script and execution intent before returning the launch route. Intent
records native thread/root, owner, epoch, controller generation, host generation,
and realm identity. The native executor launches the trusted VM manager with a
cleared environment; it never evaluates the supplied guest script on the host.

Owner revoke publishes a persistent owner-specific fence before journal mutation.
The manager watches that fence and forcibly stops its exact VZ object. Independent
owners use different fences and realms. Native revoke still reports quiescence
false and handoff false: no full receipt or handoff transaction was implemented.

Experimental VM mode explicitly refuses unsupported model turns/mutations and
startup overrides. It is a small shell-only test envelope, not native TUI or
general executor parity. Normal unprotected native defaults remain unchanged.
The ordinary protected research mode retains its previous feature envelope.

`vm_exec.swift` and `guest_exec_init.sh` carry the native command into a bounded
RAM-only Linux guest. The guest has no shared directory, storage device, network,
or VSOCK. Foreground exit shuts down the whole guest, including detached work;
forced stop comes from VZ lifecycle APIs, not guest output. Scripts over 1024 bytes
refuse in this first slice. No provider/profile environment enters the guest.
The native command's host cwd is not shared with the guest: `/work` is guest RAM.
This is explicit research functionality and cannot replace native macOS commands.

`prepare_executor.py` deterministically reuses the verified retained initramfs;
it downloads nothing and preserves the older VM images. The distinct manager
compiled and was ad-hoc signed with the existing Virtualization entitlement.

## Observed validation and exact build boundary

- Latest standalone policy source: **16 tests pass**, strict all-target Clippy and
  cargo formatting pass. New obligations cover late shell ownership checks,
  durable exact grants, unsupported-route refusal, image tampering, and scoped
  A fencing with B still admissible.
- Initial native shell-hook/VM-route build passed in 2m48s. Later late-check and
  capability-envelope refinements are NOT linked: the incremental build hit the
  disk reserve. The retained executable is from the earlier successful build.
- Current combined driver `check_executor.py` has NOT run a native command or
  started a VM. It refused at the 4 GiB preflight. This is not a VM or Codex test
  failure, and no expected result is promoted to observed evidence.
- No successful eight-TUI/concurrency experiment was repeated. Prior 47 native
  checks, two cross-home tests, and 10 VM checks remain historical evidence for
  their exact recorded binaries. They do not certify these new source changes.

## Remaining correctness gaps

The grant currently snapshots controller generation at execution time; it does
not yet carry an immutable ingress operation through every native queue. A
rotation-vs-queued-launch race therefore still needs explicit provenance and an
adversarial test. Do not call this complete stale-controller enforcement.

Native terminal cancellation may kill the manager before its trusted stop
acknowledgement; that must retain UNKNOWN. The new manager's forced-stop and guest
shell exit-code mapping still need native UX tests. Host death, incomplete grant
publication, manager crashes and recovery need actual integrated fault tests.
No live manager stopped receipt is consumed by Relay or permits handoff.

Autonomous operations, subagents, tools, MCP/remote capabilities, filesystem
effects and protected TUI transport remain unintegrated. The test guest cannot
mutate host workspace files. A real workspace broker and its I/O fence/drain are
required before workspace continuity or protected-work handoff can be claimed.

## Resource and lighter-alternative gate

The experiment configures **192 MiB per VM**. Eight such VMs would configure
1.5 GiB before counting native Codex, TUIs, managers, guest overhead or swap.
Actual integrated startup latency, RSS/footprint, throughput and cancellation
latency remain unmeasured. VM-per-session has NOT been selected for production.

A credible lower-overhead Linux candidate is one shared VM with an outer trusted
executor and nonprivileged, nondelegated session cgroups/namespaces. It needs real
escape, cross-session, PID-reuse, queued-launch and crash experiments. It also
does not preserve native macOS command semantics automatically.

The installed macOS `sandbox-exec` manual explicitly marks it deprecated. A static
sandbox or process group alone is not a demonstrated dynamic revocation and
quiescence authority. A native macOS alternative must protect the supervisor and
mediate filesystem/external effects; none has been proven here. No privileged
principal, filesystem extension or host security configuration was installed.

## Capacity and resume prerequisite

The user approved exactly the two Brave cache paths and the named updater cache,
after Brave closes. Open-file/process checks passed and that cleanup reclaimed
about 1.10 GiB; the updater directory was already empty at deletion time. Free
space reached about 5.54 GiB. After the pause, Brave cache regrowth and increasing
swap coincided with a drop below 4 GiB. Verified lossless artifact compaction saved
another 246,079,488 bytes, preserving decoded bytes and metadata. No other cache,
required artifact, source, evidence, credential or installed binary was deleted.

The last incremental build reached the reserve and stopped. Current free space
is approximately 1.6 GiB, and Brave is running. Closing Brave for the already
approved paths is a pending prerequisite, not a request to authorize more cleanup.
No external physical disk is attached and no remote development environment is
available through the connected tools. A same-disk image cannot add capacity.

Wait for enough safe capacity (at least 4 GiB for the combined test; preferably
6–8 GiB) before a fresh stage. Do not retry the failed link below that headroom.
Then finish the native link, run `check_executor.py` with the retained manager and
`executor-initramfs`, investigate failures, and run existing direct-native
regressions for the changed shell path. Keep every failed runtime and cleanup.
The first meaningful success still awaits actual native execution and verified
independent cancellation. Production ProfileSessionLock remains untouched.
