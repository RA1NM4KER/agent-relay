# Native authority increment from 60217b7

Latest follow-up from 8614951: see [executor integration WIP](EXECUTOR_INTEGRATION.md).
It adds a source-only native shell/VM route and passes 16 policy tests. Runtime
validation remains capacity-blocked; earlier results below retain their exact scope.

This is a research checkpoint, not production acceptance or full safety parity.

## Implemented and measured

- Reviewed latest continuation/engineering/acceptance and verified all 132 indexed
  shared-daemon evidence files before changing anything. The successful eight-TUI
  26-check concurrency fixture was not repeated.
- Added native durable enrollment, scoped supervisor recovery, rotating frontend
  leases, one live execution connection per lease, actual root reservations,
  owner-wide revocation and generation/queued request checks. Journal version 3
  binds physical profile identity and enrollment policy digest. Intent persists
  before forwarding, actual typed native result before reply. Unknown outcomes
  remain unresolved and cannot replay native work. No guessed native identity.
- Native core reserves actual thread IDs before startup, binds helper ancestry,
  rejects protected profiles without their broker, and checks common tool dispatch.
  Approval replies check connection/root and use fresh host-namespaced IDs.
- Final actual server `7a17eaad6fd97b7d23e50531906024318edfce5efb4059533ae4a82242461c19`
  passes 47 raw-peer native checks (`authority-native-source-regression`). Actual
  host SIGKILL tests cover committed lost reply and unresolved intent. B executes
  after A fencing and reconnects independently. All three exact owned hosts exit
  (two intentional -9, final normal 0); final socket refuses connections.
- Stronger recovery proves an untouched empty normal conversation survives lost
  reply plus host death and resumes its exact original UUID. Native Persist then
  Flush precedes authority commit; no synthetic user turn, no ephemeral promotion.
- Additional real cross-home shared-store and symlink-store tests refuse at native
  initialization with `RELAY_NATIVE_AUTHORITY`, not an external cooperative gate.
  Protected relocated stores refuse until their physical domain can be enrolled.
- Final research Rust: 14 tests pass, Clippy all-targets with warnings denied
  passes, formatting passes. Existing feasibility 27 tests and host-contract two
  tests pass. Actual native server links successfully with Rust 1.95.0, locked
  upstream dependency graph and retained V8 artifacts. Production Rust is unchanged;
  prior full workspace baseline failures remain, no all-green workspace claim.
- The final patch applies against exact upstream 6ff1e1c and modules match its
  retained insertion. The CLI used for the separate-host lock negative is an
  earlier authority build, not the final server source. Baseline verified native
  package `dfc5fa0a...ee173e` remains unchanged; no installed binary was replaced.
- Two independent 192 MiB Apple VZ Linux guest realms pass 10 isolation/lifecycle
  checks. Privileged detached work escapes a naive cgroup; exact A kernel stop
  leaves B detached execution running. Both managers stop zero with trusted VZ
  stopped state. No external devices, shared files, disk, VSOCK or network.
- A separate exact manager SIGKILL preserves UNKNOWN, no trusted quiescence or
  handoff receipt. The local VM manager is ad-hoc signed only for Virtualization;
  no installed service, system extension, account, credentials or security policy
  was changed. Alpine ISO publisher SHA256 checked, detached signature not verified.

## Failed experiments retained

- Original raw driver left alternate CLI stdin open; it timed out reading stdin.
  A bounded diagnostic with DEVNULL rejects missing native authority; corrected
  driver rejects the second protected host. Failure and cleanup retained.
- First positive native creation failed because thread/start's tracked background
  task did not inherit the request task-local. Explicit scope fixes this; old
  server source snapshots/hash remain in evidence, APFS-cloned binary stays in lab.
- Initial empty native resume failed because untouched history was not materialized.
  ThreadPreparation+Flush fixed same-host startup but still failed cold C resume.
  Standard Persist+Flush corrects both and passes actual lost-reply crash recovery.
- A different unprotected home with only a path selector refused incidentally on
  absent native store history; this is NOT an admission pass. With shared
  sqlite_home, the actual fenced UUID resumed, proving a real alternative-home
  bypass. Startup store-domain and core history/store source guards now refuse
  both direct and symlink-store adversaries at the native boundary.
- Initial cross-home launcher without a new process session hit endpoint timeout;
  corrected owned-session launcher connects. The startup timeout is retained and
  not conclusively attributed to a native defect.
- Full combined fault build hit the 1.5 GiB reserve and was terminated; retained.
  Lossless cache compression plus server-only target selection subsequently linked.
- Final policy Clippy found a nonminimal boolean; corrected without weakening the
  fence, reran successfully. Initial VM extraction expected raw ARM64 magic; Alpine
  supplies EFI zboot. Verified Linux header, decoded its gzip payload, reused ISO.

## Resource and provenance decisions

- Reused 977 retained extracted crate packages as a hash-verified directory source.
  Cargo workspace resolution additionally required 319 previously absent locked
  target/dev crates (100.4 MiB compressed). No repeated retained package/toolchain
  or V8 download. Resolver/provenance and intermediate failures are retained.
- Two additional lossless APFS cache compactions preserve every required artifact,
  decoded SHA256/mode/ownership/timestamp: 850 files save 964,476,928 bytes, then
  56 files save 399,958,016 bytes. Updated helper uses distinct run IDs and verifies
  the preserved baseline package. No source, credential, evidence, verified bundle,
  active runtime or installed executable deletion. No unrelated cache deletion.
- Final disk is approximately 2.44 GiB free, below the unchanged 4 GiB shared entry
  guard, above the unchanged 1.5 GiB runtime reserve. No external physical disk is
  attached. A same-disk image adds no capacity. New full Linux native build and
  final eight-TUI integrated certification cannot safely be staged at this capacity.
- Generated local supervisor capabilities stay in private mode-0600 lab vaults,
  excluded from evidence/Git. Retained artifacts were checked against every generated
  raw capability candidate and contain no matches. Journals retain digests only.

## Remaining work and hard limits

Protected native frontend plumbing, complete autonomous/queued/approval/executor
coverage and immutable root/epoch/realm propagation remain engineering work.
See EXECUTION_INVENTORY.md. The common router check is not every execution sink.
The RAM-only VM is not a native executor or filesystem-effect broker. External
MCP/remote jobs, host I/O, copied stores, supervisor isolation, crash receipts,
PID-reuse adversaries and platform/native behavior parity remain unproven.

Do not advance any handoff transaction. First implement a real realm-bound native
executor with capability inventory and kernel-backed stop/drain receipts, then
integrate both foundations into the existing real eight-TUI harness. A supported
Linux native environment needs sufficient disposable build capacity; preserving
native macOS Full Access requires an OS authority design, not silent Linux-command
substitution or same-uid process-group cleanup. No privileged host provisioning
has been authorized or performed. Production ProfileSessionLock stays unchanged.
