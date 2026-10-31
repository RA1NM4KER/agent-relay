# Executor authority boundary

Admission and quiescence are separate. The current research insertion can fence
new classified native requests, recheck queued requests, and reject later tool
dispatch from a revoked root. It cannot retract a process, syscall, mapped write,
MCP call or remote job that was already admitted. A successful revoke therefore
always reports `executionQuiescent:false` and `handoffPermitted:false`.

The capability key is local controller authority, not a provider credential.
The journal stores its digest, never the key. The threat model includes raw RPC
peers, stale/reconnecting controllers and crashes. Arbitrary same-uid code that
can read controller memory, replace the profile journal, remove the marker, or
run an older unmodified executable is outside this prototype's security boundary.
Executor isolation must protect the controller and journal from workloads before
host-wide adversarial safety can be claimed. Production ProfileSessionLock stays.

## Maintainable architecture to implement and validate

Keep the native conversation/permission/UI host outside executor realms. Give
each owning root an immutable realm identity and generation, inherited by every
subagent, ephemeral execution helper and process capability. Every execution
submission must name that realm; unscoped execution and unknown external
authorities must refuse quiescence. Native permission decisions remain unchanged;
realm confinement adds an authority boundary, not an automatic permission grant.

The realm manager must durably close admission first, then close outstanding
launch/approval queues, revoke external execution capabilities, destroy the exact
realm, and obtain a kernel-backed empty/liveness observation before producing an
opaque receipt. A receipt binds root, owner epoch, host/realm generations, the
complete capability inventory and the durable revocation operation. A caller
boolean, a list of PIDs, and a successful signal command cannot construct it.
Recovery retains reservations if any launch or cleanup outcome is unknown.

For Linux, a cgroup-v2 subtree can cover descendants independent of process groups.
`cgroup.kill` plus `cgroup.events` populated state are useful observations, but
membership must be inescapable: migration requires controlled authority outside
the workload. A writable delegated parent held by the same unrestricted workload
is insufficient. The kernel also describes cgroup namespace delegation
boundaries. This still requires actual escape, concurrent-fork, manager-death and
PID-reuse experiments in the supported deployment.
[Linux kernel cgroup-v2 documentation](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html)

For macOS, a same-uid native process tree/process group is not sufficient. A
separate execution principal or VM is a candidate authority boundary. Apple's
Virtualization framework exposes VM lifecycle control and explicit shared
directories; it does not by itself prove that queued host filesystem effects or
external jobs are quiescent. A Linux VM also changes native macOS command
semantics, so it cannot silently replace existing Full Access behavior.
[Apple Virtualization](https://developer.apple.com/documentation/virtualization/vzvirtualmachine),
[shared directories](https://developer.apple.com/documentation/virtualization/shared-directories)

Remote MCP tools, SSH, launch services, provider-side jobs, code-mode workers and
other remote execution need explicit realm adapters and revocation guarantees.
Killing a local process does not cancel an already-created remote authority.
Uncovered capabilities make handoff unavailable. No blanket guarantee is possible
while arbitrary execution retains access to the supervising user's authority.

## Current platform experiment

The retained Swift Virtualization capability probe supports a VM on this Mac.
A bounded, ad-hoc signed research manager boots a pinned Alpine 3.22.7 arm64
kernel with an immutable initramfs. No privileged service, system extension,
account, installed binary, security policy or production profile was changed.
The ISO publisher SHA256 was checked; its detached signature was not verified.
The ISO and extracted images stay in the disposable lab, not in Git.

The first actual boot passes. A two-kernel experiment passes 10 narrow checks:
both privileged detached guest children escape a naive cgroup and continue after
its kill; forced VZ stop establishes exact A kernel stopped state while B's
detached execution continues; then B stops independently. Manager state comes
from the host VZ object, not from guest console output. Console silence is only
supporting evidence. Both exact owned managers exit zero and record stopped.

There are no shared directories, disks, network interfaces or VSOCK devices.
Guest writes affect only guest RAM. This proves a narrow kernel-realm lifecycle
candidate, NOT native Codex containment, host filesystem quiescence, external
capability cancellation, Linux production deployment, or native macOS behavior
parity. Native execution is not routed into these realms. Manager crash receipts,
PID reuse, queued host effects and production capability coverage remain unproven.
An absent/killed manager is UNKNOWN, never sufficient to advance a transfer.

A Linux guest changes macOS command semantics. Preserving supported native macOS
Full Access behavior while preventing arbitrary code from sharing supervisor
user authority requires an additional OS-enforced principal/capability design.
No process-tree heuristic can fill that gap. Native transfer stays refused;
integrated eight-TUI enforcement certification waits for both foundations.
