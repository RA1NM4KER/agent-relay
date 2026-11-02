# Workspace access increment from bb0c6ce — 2026-11-02

## Outcome and boundary

Eight real VZ guests can use disposable source snapshots and private writable
drafts while a separate host publisher fences changes to an unexported canonical
project copy. The combined experiment rejects A's new publications while A is
still executing, then verifies exact A kernel stop and further B–H execution and
publication. This is a **workspace primitive**, not actual native Codex workspace
integration, a complete POSIX workspace implementation, or a Relay handoff.
All receipts retain `handoffPermitted:false`. Production locks remain unchanged.

The earlier actual Codex shell-containment milestone remains established by its
retained evidence. It was not repeated or relabeled as workspace integration.
The former `/private/tmp` checkout and native build lab are absent. Their cause of
loss is unproven. Verified the bb0c6ce bundle digest and all 881 mirrored research
files before restoring the research checkout to
`/Users/kefasmanda/repos/agent-relay-concurrency-research`. Evidence and code now
live outside the temporary directory. No cache cleanup or source deletion was
performed. About 17 GiB remains free; the 4 GiB entry and 1.5 GiB runtime guards
remain unchanged. VM drivers additionally stop their exact research objects on
critical macOS memory pressure.

Only missing Alpine boot media was recovered, using the previously retained
SHA-256; no native Codex download, native rebuild, credentials or model calls.
`evidence/media-recovery.json` and `workspace-image.json` identify artifacts.
The ISO's detached signature was not verified. The restored native binaries and
Rust toolchain are **not present**; historical hashes do not describe live files.

## Mechanisms and experiments

| Mechanism | Actual experiment | Decision / limit |
| --- | --- | --- |
| Direct writable virtio-fs project export | Held append FD, shared mmap/msync, detached setsid child, 32 MiB burst; runtime share removal and kernel stop | No further effects observed for one second after detach acknowledgement; no trusted host-I/O drain receipt. Not eligible for handoff. |
| Read-only snapshot + private writable host draft | Read-only source attempts fail; source hashes stay unchanged; detached writes and mappings work in draft; B continues during A detach | Protects the source snapshot in these probes. Draft host I/O still lacks a drain receipt; never reuse/relabel an old live draft. |
| Unexported canonical copy + bounded host publisher | Real guest-produced bytes, durable owner/epoch admission, path reservations, revoke, lost replies, two publisher process crashes, live eight-guest composition | Promising separation of canonical mutation from execution; supports only regular-file puts, not full workspace parity or authenticated native transport. |
| Native static macOS sandbox + private draft | Actual fork/setsid child, held FD, mmap, bulk writes, inherited canonical FD, parent termination; eight 64 MiB workers | Much cheaper microprobe; inherited FD bypass demonstrated. Closing inherited descriptors prevents tested canonical write, but detached children continue after parent death. No whole-tree quiescence guarantee. Deprecated/private sandbox rules are not a selected architecture. |

All shares were generated from `git archive HEAD` of tracked Relay source into
marked fixtures. No `.git`, provider profile, credentials or real user workspace
was exported. A fake outside-export canary tests guest symlink behavior. The
host collector refuses symlinks, hardlinks, traversal, nonregular files and
oversized inputs rather than following guest paths into the host. This is not a
proof against malicious host administrators or kernel/hypervisor compromise.

The new helper also rejects seed/draft export symlinks, control files placed
under an export, scripts outside their control directory, and kernel-commandline
label injection before VM creation: five negative checks. Its positive 128 MiB
boot smoke passes after hardening. The pre-hardening helper source is retained;
all earlier fixtures used actual directories, not export symlinks. Fixture
parents and controls are trusted host-owned objects, not writable by guests;
resisting arbitrary concurrent host path replacement is not claimed.

### Retained run map

Full specs, guest consoles, trusted states, copied drivers, telemetry and cleanup
are under `evidence/runs/`. Host tests and failures are under
`evidence/host-probes/`; `index.json` hashes the evidence. UUIDs identify runs,
not pooled check counts.

| Run prefix | Experiment / result |
| --- | --- |
| ffc32527 | Direct writable export, detach; held/mmap counters stop advancing in observed one-second window |
| 5eb42de1 | Two read-only snapshot/private-draft guests; A detach, B advances; both source hashes unchanged |
| a18e3114 | Two direct-export guests; exact A kernel stop |
| 86867623 | Eight 192 MiB guests, each touches 64 MiB; first stop, resource sample |
| 4c0cfea2 | Two snapshot/draft guests, exact manager self-SIGKILL; A receipt remains CRASHING_UNKNOWN, never quiescent |
| e3479947 | One 128 MiB guest, 64 MiB working-set microprobe |
| 603fca40 | Eight-guest follow-up with actual `footprint` measurement; first-stop cleanup passes |
| ba76efcc | Hardened helper positive 128 MiB smoke; exact stop |
| config-23a13ad4 | Five configuration refusal checks |
| 0a4e97a3 | Eight live guests plus host publisher: six grouped integration assertions; A fence before stop, B–H progress after stop |

Publisher suite: initial 27-check result `publisher-86a1724d`; expanded 33-check
result `publisher-354ad398`; final journal-fullfsync 33-check result
`publisher-abfda541`. Counts overlap. A later collector-selection regression
(`publisher-c526a4d8`, zero checks, KeyError on a configuration-result file) is
retained; selecting only matching run schemas fixes it. No native boundary
bypass test or prior native result was replaced by these checks.

Native sandbox bootstrap failures `3b21925b`, `1a7a7f53`, `f284e513` are retained.
The loader aborted before main (SIGABRT; dyld ignition/CacheFinder stack). Adding
file-map permissions and one guessed Cryptex subdirectory did not fix it. Reading
the installed `dyld-support.sb` showed additional loader bootstrap rules;
importing that system profile made the probe run. The exact first denied resource
is unproven. Sanitized stack/termination evidence and the profile digest are
retained, not the user's entire diagnostic reports. That profile explicitly
identifies itself as a private interface. No system profile was modified.

Successful native probe `20a527ff`: canonical open EPERM, but inherited descriptor
write returns 3 and changes the synthetic canary. `setsid` succeeds; private writes
continue after parent termination. Eight-worker probe `ddf33eb5`, with no canonical
descriptor inherited: canonical files unchanged, all eight detached children
continue private writes after parent termination. This is a counterexample to
using static path policy plus parent death as a quiescence receipt.

## Publication ordering and recovery

`publisher.py` is host-only experimental Python. Supervisor-created random
capabilities remain outside all exports, logs and archived evidence; only their
digests are journaled. Enrollment is a trusted in-process call, not yet connected
to the native ownership store or an authenticated guest protocol. Owner UUID and
epoch are checked at admission and immediately before issuing file effects.
`INTENT` is durable before issue; `ISSUING` precedes filesystem effects; committed
result precedes reply. SQLite WAL uses synchronous FULL, fullfsync and checkpoint
fullfsync. Canonical paths are rooted in pinned nofollow directory descriptors.
Case-folded Unicode-normalized reservations prevent tested APFS name aliases.

Revocation closes admission and waits for in-flight operations. An operation
already issuing may finish after revocation; **no drain receipt precedes its
completion**. A test pauses after real temporary-file bytes are written, at the
userspace `fsync` call boundary: drain is refused while pending, B publishes to a
different path, same-path B is refused, and drain succeeds after A finishes.
This is not a blocked kernel-I/O experiment. Removed unnecessary global I/O
serialization; different reserved paths can progress independently in this test.
Shared disk/SQLite stalls can still affect latency and have not been bounded.

Actual subprocess death after durable intent and after rename-before-result
quarantines the uncertain owner/path. Recovery never guesses or replays that
mutation; a different B file remains usable, including after a case-alias bypass
attempt. A committed lost reply returns its exact retained receipt. No general
filesystem rollback or power-loss durability is claimed. Physical host crash,
blocked host kernel I/O and storage failure remain untested; no user Mac reboot
or power interruption was attempted.

The API accepts immutable bounded bytes, not a guest path to open. Input capture
can reject concurrent changes; it does not supply an atomic multi-file snapshot.
Only regular-file replacement is supported: directories, rename, deletion,
permissions, executable modes, symlinks, hardlinks, watchers, locks, git atomic
updates and multi-file consistency require explicit semantics and adversarial
coverage. New files use mode 0600. Canonical storage assumes an exclusive broker
writer; arbitrary external editors are not coordinated. Therefore this prototype
cannot replace ordinary Relay workspace behavior.

## Eight-agent resource comparison

Host: Apple Silicon, macOS 26.5.2, 8 logical CPUs, **8 GiB RAM**. These are short
single-machine microprobes, not full agents or statistically stable benchmarks.

| Observation | VZ snapshot/draft | Native static sandbox |
| --- | --- | --- |
| Eight workers, touched workload | 192 MiB configured per guest; 64 MiB guest tmpfs each | 64 MiB native allocation each |
| Launch through first real held-FD/mmap effects | 1.150 s (603fca40); 1.707 s earlier run | 0.122 s |
| Measured memory | 1,707,092,504 B total physical footprint (~1.59 GiB), including eight XPC workers and managers | 543,136 KiB aggregate RSS (~530 MiB); **RSS is not physical footprint**, no like-for-like native footprint captured |
| CPU | ~5.4 summed VM-worker CPU seconds at earlier startup sample; short workload, not model/build throughput | Per-process CPU times retained, not workload-equivalent throughput |
| Disk | One shared ~33.1 MiB kernel + ~9.3 MiB initramfs, no per-VM block disk; private copies and burst outputs scale per session | No guest boot images; private copies/output still scale |
| Revocation / lifecycle | Whole-kernel stop established; host shared-I/O drain unproven | Canonical FD hygiene required; no complete descendant quiescence |

Manager RSS alone dramatically understates VZ cost. `footprint.json` includes
compressed accounting and reports ~200 MiB per XPC worker despite smaller RSS in
some samples. Pre-existing pressure varied from warning to normal between runs;
no claim that this experiment alone caused host pressure. Configuring eight
192 MiB Linux microguests is feasible here, but 1.59 GiB is already ~20% of host
RAM **before** full compiler/toolchain/build workloads and Codex frontend/backend
memory. One 128 MiB guest booting is not evidence eight real development workloads
fit. Linux guests also do not preserve arbitrary native macOS command semantics.
VM-per-session remains an evaluated candidate, not the default architecture.

Lighter static isolation plus a canonical publication broker may protect selected
workspace effects, but leaves execution closure and native capability coverage
unproven. Process groups, parent tracking and quiet windows are not substitutes.
Linux delegated cgroup containment remains a platform-specific candidate, not a
macOS solution or an experimentally verified equivalent here. A separate,
appropriately provisioned disposable host could measure full workloads and permit
host-crash tests; the current user Mac must not be used for destructive testing.

## Next implementation gate

Do not advance Relay's transactional handoff: full workspace safety is not proven.
Next implement an authenticated native-authority-to-publisher adapter with an
explicit capability/epoch snapshot, no guest access to credentials/capabilities,
and a closed inventory of supported filesystem operations. Combine native queue
fencing, exact kernel-stop evidence, publication admission closure and zero
unresolved publication operations. Native session authority must be fenced before
publishing a receipt, and a later crash must not restore old write authority.
Unsupported filesystem operations must fail closed without claiming parity.

First recover the native lab reproducibly if actual native integration is needed;
do not treat the new helper's JSON interface as a drop-in for the historical
native `vm_exec.swift` protocol. Restore its exact source/patches, toolchain and
hashes before any native build. Retained boot media can be reused. Do not rerun
completed embedded/shared concurrency feasibility tests without a regression
reason. A full host-crash/pending-I/O proof requires a disposable environment or
an enforceable design that does not depend on sharing canonical host I/O.

## Primary references

- [Apple runtime file-system share property](https://developer.apple.com/documentation/virtualization/vzvirtiofilesystemdevice/share) and local public SDK header permit replacing/removing a share; no complete host-I/O drain contract was identified.
- [Apple shared directories](https://developer.apple.com/documentation/virtualization/shared-directories) describes host directory exposure and read-only configuration.
- [Linux virtio-fs](https://cdn.kernel.org/doc/html/latest/filesystems/virtiofs.html) describes guest/host filesystem sharing; it is not a Relay ownership protocol.
- [SQLite fullfsync](https://www.sqlite.org/pragma.html#pragma_fullfsync) documents macOS full synchronization; this does not replace hardware crash experiments.
- Installed sandbox-exec(1) marks the command deprecated; installed `dyld-support.sb` marks its rules private. Their local identities, not a stable supported production interface, informed this probe.
