# Executor integration from 8614951 — incomplete, 2026-11-01

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
