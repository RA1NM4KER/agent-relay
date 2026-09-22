# Architecture

Status: M0 decision record, awaiting approval before M1 implementation.

## Product boundary

Agent Relay coordinates one explicit coding-agent handoff at a time. It is not a credential vault, transparent proxy, quota pool, request router, or multi-agent scheduler.

The core guarantee is stronger than session continuity:

> At most one Relay-managed agent has write authority for a canonical project, and every handoff either reaches a verified terminal state or remains durably recoverable.

Native conversation preservation is a capability layered beneath that guarantee. It may be unavailable without making the transaction unsafe.

## Major decisions

1. **Self-contained correctness.** No Herdr plugin or third-party switcher is required to acquire locks, journal a transition, stop a writer, or recover.
2. **Official profile isolation.** A Claude profile is an absolute, dedicated `CLAUDE_CONFIG_DIR`; Relay never swaps credentials into a shared directory.
3. **No credential custody.** Relay invokes normal provider authentication and stores only profile references and non-secret identity metadata.
4. **Versioned native transfer.** Claude transcript copying lives entirely in `relay-provider-claude`, behind capability detection and a tested version matrix.
5. **Truthful continuity classes.** `SESSION_CONTINUATION` means a verified native resume. `STATE_CONTINUATION` means a fresh session bootstrapped from a concise packet.
6. **Two locks.** A writer lease protects project mutation; an orchestration lock serializes handoff/recovery. The coordinator retains the project reservation through the source-to-target gap.
7. **Durable transition first.** Every externally visible mutation is preceded by a journal state that tells `relay recover` what is safe to do next.
8. **Fake provider first.** The state machine, failures, cancellation, locks, and recovery are proven without real accounts before the Claude adapter is allowed to mutate a session store.
9. **Herdr as an adapter.** Herdr contributes pane/session identity and lifecycle operations through documented CLI/plugin APIs. It does not own the transaction.
10. **Manual first.** M1–M3 implement explicit selection. Automatic handoff remains disabled.

## Proposed workspace

```text
agent-relay/
├── crates/
│   ├── relay-cli/               command parsing and human/JSON presentation
│   ├── relay-core/              domain, state machine, policy, ports
│   ├── relay-provider-claude/   Claude process/session/profile adapter
│   ├── relay-herdr/             optional Herdr CLI adapter
│   └── relay-testkit/           FakeProvider, fault injection, fixtures
├── fixtures/
│   ├── fake-claude/
│   └── sessions/
├── plugins/herdr/
│   └── herdr-plugin.toml
└── docs/
```

Dependencies point inward. `relay-core` does not know Claude paths, Herdr pane IDs, terminal escape sequences, or OS Keychain behavior.

## Components

### relay-core

Owns:

- `Profile`, `Project`, `Session`, `Handoff`, `Availability`, and lock metadata;
- state-transition validation and recovery plans;
- target-selection policy for explicit and ordered fallback profiles;
- event definitions and redaction policy;
- ports for providers, storage, process identity, clocks, locks, and project checkpoints.

It accepts provider facts and returns explicit commands/effects. It never parses provider output directly.

### relay-provider-claude

Owns:

- profile setup and authentication preflight;
- environment sanitization and structural process launch;
- hook/event parsing and session discovery;
- Claude-version capability detection;
- native session artifact discovery, transfer, and verification;
- rate-limit signal parsing and graceful stop behavior.

No Claude-specific file path enters `relay-core` except as an opaque, validated provider reference.

### relay-cli

Owns command routing, project discovery, interactive confirmation, rendering, and stable `--json` envelopes. Machine output has a schema version and sends diagnostics to stderr.

Session adoption and in-agent control live here too: `live` (structural identification of a running Claude
session and the adoption contract), `agent_cmd` (`/relay status|switch|adopt`, answered by the
`UserPromptSubmit` hook), `control` (the per-project control directory between that hook and the
supervising terminal), and `target` (the one switch-target model behind both the `relay switch` picker
and the in-agent listing). None of them can move ownership by themselves: adoption writes the lease
only after every proof holds, and every switch is the ordinary `HandoffCoordinator` transaction.

### relay-herdr

Wraps documented Herdr CLI calls, validates JSON responses, and maps focused pane/session information into core requests. A plugin action invokes Relay; the core transaction remains usable with Herdr absent.

### relay-testkit

Provides deterministic fake profiles, processes, sessions, clocks, locks, and failure injection at every handoff boundary. Tests never read or modify `~/.claude`.

## Domain model

Identifiers are typed rather than free-form strings.

### Profile

- stable local `ProfileId` and display name;
- provider kind;
- absolute validated config-directory reference;
- enabled flag;
- optional non-secret identity pin and last verification time;
- last observed availability with source, confidence, and freshness.

### Project

- canonical path and stable ID derived from it;
- the default and ordered fallback profiles (global preferences);
- **many Relay Sessions** (below) — a project is a working context, not an ownership unit.

Aliases and symlink spellings resolve to the same project ID.

### Relay Session

Relay supervises conversations, not repositories. A Relay Session is one logical conversation/
workstream and the unit of continuity, handoff and ownership:

- a stable Relay session ID (a UUID generated by Relay), independent of any provider-native id;
- the canonical project ID;
- the current provider/profile *while active*, and the last profile/provider as **history** while
  dormant;
- the current provider-native session identity (Claude session UUID or Codex thread id) — which
  changes across a Claude ↔ Codex handoff while the Relay Session stays;
- timestamps, provider arguments (scoped to this session), handoff journals and its control endpoint;
- continuation class: `native_verified`, `state_only`, `unknown`, or `incompatible`.

State is **ACTIVE** when a lease exists (Relay supervises a live provider process for it) and
**DORMANT** otherwise. A provisional session whose conversation was never persisted is removed when
its provider exits. The invariant is *one active owner per Relay Session* (and one native
conversation is never active in two Relay Sessions); a project may have any number of active
sessions, on the same profile or different ones. Concurrent sessions in one working tree may edit
the same files; Relay does not serialize filesystem writes.

On disk: `projects/<id>/sessions/<relay-session-id>/{session.json, lease.json, orchestration.lock,
handoffs/, current_transaction.json, automation_state.json, provider_args.json, control/}` plus a
short-lived `projects/<id>/registry.lock` for creating sessions and checking native-id uniqueness.
The former one-lease-per-project layout is migrated deterministically and idempotently into one
Relay Session.

### Handoff

- unique ID and schema version;
- source/target profiles and sessions;
- explicit reason and initiating actor;
- state and monotonic revision;
- checkpoint and artifact hashes;
- verification evidence;
- failure/recovery metadata.

### Writer lease

- canonical project ID;
- lease ID;
- owner PID and process-start identity;
- profile/session/handoff IDs;
- acquisition time and format version.

Metadata describes the lock; it does not constitute the lock.

## Storage layout

OS-native directories are resolved with a tested platform library:

```text
<config>/agent-relay/config.toml
<config>/agent-relay/profiles/<name>/claude/
<state>/agent-relay/projects/<project-id>/writer.lock
<state>/agent-relay/projects/<project-id>/orchestration.lock
<state>/agent-relay/projects/<project-id>/lease.json
```

Project-local, gitignored state:

```text
<project>/.relay/project.toml
<project>/.relay/current.json
<project>/.relay/handoff.json
<project>/.relay/events.jsonl
<project>/.relay/handoff.md       only when fallback is needed
```

Security-critical locks live in the user state directory, keyed by a digest of the canonical project path. A malicious checkout must not be able to replace a lock file with a symlink. Project-local state is treated as untrusted input and reconciled with the trusted journal.

Writes use a temporary file in the destination directory, restrictive create permissions, file sync, atomic rename, and parent-directory sync where supported. JSON records include format version and revision. Event entries are append-only under the orchestration lock and form a hash chain to make accidental or casual alteration detectable; they are not presented as tamper-proof against the local user.

## Locking and process ownership

The writer lock is an OS advisory lock held for the life of the Relay-launched agent. Its metadata includes PID and process-start identity so PID reuse cannot impersonate the owner.

The orchestration lock is short-lived during ordinary commands and retained throughout a handoff/recovery. A handoff follows this ownership model:

```text
source writer running
        │
        ├─ coordinator obtains orchestration lock
        ├─ source stops and releases its writer process
        ├─ coordinator keeps project reserved
        ├─ target starts as the sole designated writer
        └─ target verifies; journal commits or recovery retains reservation
```

There must never be an unlock/relock gap in which an unrelated Relay process can launch. On Unix, the coordinator may retain the lock file descriptor and associate the target child with the lease. Cross-platform details sit behind a `WriterLeaseBackend`; Windows support is designed but not required for v0.1.

`relay lock recover` never breaks a lock merely because a timestamp is old. It must prove that the recorded process is gone or has a different start identity. When proof is unavailable, it refuses and gives a diagnostic.

## Handoff state machine

The durable states are:

```text
PREPARING
  → CHECKPOINTED
  → SOURCE_STOPPING
  → SOURCE_STOPPED
  → SESSION_TRANSFERRING
  → SESSION_TRANSFERRED
  → TARGET_STARTING
  → TARGET_VERIFIED
  → COMPLETE
```

Failure records preserve the phase and error category rather than replacing evidence with one generic state:

- `FAILED_PREPARE`
- `FAILED_STOP`
- `FAILED_TRANSFER`
- `FAILED_TARGET_START`
- `FAILED_VERIFY`

Each transition is idempotent or has an idempotent reconciliation check. Recovery reads the journal and observed process/filesystem state, then chooses one of: continue forward, safely retry target, fall back to state continuation, restore a source launch, or remain stopped and request operator input. It never infers success from the requested state alone.

Cancellation is a first-class fault. Ctrl-C requests cancellation; the coordinator finishes the smallest safety boundary, persists the resulting state, stops any unverified target, and exits with a recovery command.

## Manual handoff protocol

1. Resolve the project to a canonical path and trusted project record.
2. Acquire the orchestration lock and inspect the healthy writer lease.
3. Resolve the explicit target or ordered fallback; reject source-equals-target.
4. Preflight target authentication in its sanitized environment and verify its identity pin.
5. Persist `PREPARING` before any external mutation.
6. Capture branch, HEAD, dirty state, and changed filenames without reading file contents.
7. Obtain the exact session ID and transcript path from provider events where possible.
8. Persist `CHECKPOINTED` and request graceful source stop.
9. Verify process termination using PID plus start identity; retain the project reservation.
10. If native transfer is supported, discover and hash the complete artifact set, reject target divergence, stage copies, sync, rename, and verify hashes.
11. Launch the target structurally with its profile directory and exact resume ID.
12. Verify its structured start event: canonical project, expected session ID, `resume` source, transcript under the target config directory, and pinned target identity.
13. Commit the target as writer, append the immutable event, and mark `COMPLETE`.
14. If native transfer is incompatible before target launch, create a redacted packet and start a new session explicitly as `STATE_CONTINUATION`.
15. If an unverified target was started, stop it before any fallback or source restoration.

The source transcript is retained. Relay never overwrites a newer target transcript without an explicit conflict-resolution command.

## Native Claude transfer capability

The adapter exposes a capability result, not a Boolean guess:

```text
Supported {
  claude_version,
  layout_version,
  artifacts,
  verification_method
}
Unsupported { reason }
Unknown { reason }
```

Initial copied artifacts are the session JSONL and known required session sidecars such as subagent transcripts. Credentials, settings, plugins, `.claude.json`, and unrelated history are never copied. File-history artifacts are excluded until tests demonstrate that they are both necessary and safely separable; rewind behavior may therefore be limited after transfer.

Compatibility requires:

- an allow-listed Claude Code version/layout pair;
- an exact provider-reported transcript path, or a tested path derivation fallback;
- a stable, fully stopped source;
- no divergent destination artifact;
- successful checksums after rename; and
- structured target verification.

A failure at any gate produces `STATE_CONTINUATION` or a recoverable stop, never an optimistic native claim.

## Fallback packet

`.relay/handoff.md` contains only operational state:

- canonical project path;
- source profile and session ID;
- branch and HEAD;
- clean/dirty and changed filenames;
- user-approved task summary, last action, next action, blockers, and warnings.

It contains no environment dump, diff, transcript, credential metadata, or command to execute. The new agent is told to treat it as untrusted text. A fresh target session has a new ID and is labelled `STATE_CONTINUATION` in both human and JSON output.

## Provider interface

The exact Rust API will be refined with the fake implementation, but the narrow responsibilities are:

```rust
trait Provider {
    fn inspect_profile(&self, profile: &ProfileRef) -> Result<ProfileObservation>;
    fn preflight_launch(&self, request: &LaunchRequest) -> Result<LaunchPlan>;
    fn detect_session(&self, process: &ProcessRef) -> Result<SessionObservation>;
    fn request_stop(&self, process: &ProcessRef) -> Result<StopRequest>;
    fn transfer_capability(&self, session: &SessionRef) -> Result<TransferCapability>;
    fn prepare_transfer(&self, request: &TransferRequest) -> Result<PreparedTransfer>;
    fn launch(&self, plan: &LaunchPlan) -> Result<RunningAgent>;
    fn verify(&self, expected: &ExpectedSession, observed: &RunningAgent)
        -> Result<Verification>;
    fn usage(&self, profile: &ProfileRef) -> Result<UsageObservation>;
}
```

Prepared operations carry hashes and preconditions. Provider methods do not silently mutate core state.

## Usage and availability

Every observation records source, observed time, expiry, and confidence. State reduction is conservative:

- a structured current rate-limit event yields `LIMITED`;
- an approaching-limit provider metric may yield `NEARING_LIMIT`;
- a successful auth check does not imply quota availability;
- stale, absent, or conflicting data yields `UNKNOWN`.

Usage suggestions are informational. Manual handoff still verifies target authentication and availability at transaction time.

## Herdr model

The plugin manifest initially declares actions for status, handoff, recovery, and usage. Because Herdr actions do not accept parameters, `Handoff Agent` launches a Relay selector or uses a single deterministic fallback.

Flow:

1. plugin action invokes `relay herdr handoff` as an argv array;
2. adapter reads focused pane/project/session via documented JSON CLI calls;
3. core runs the normal handoff transaction;
4. source pane is stopped and verified;
5. adapter opens a managed replacement pane whose entrypoint is Relay, not raw Claude;
6. Relay supplies the selected profile environment and exact takeover ID;
7. target hook verification completes the transaction;
8. the old stopped pane is closed only after verification, if desired.

The first plugin will require Herdr 0.9.0 and feature-check the needed commands. `herdr-catchup` and `herdr-agent-usage` remain optional enhancements detected at runtime.

## M1 scope and exit criteria

M1 builds the workspace and FakeProvider, then profile management and a non-destructive Claude isolation proof.

Required exit criteria:

- fake profile add/list/status/doctor with stable JSON;
- config directories created with restrictive permissions;
- fake authentication success, failure, and wrong-identity tests;
- provider trait proven without Claude knowledge in core;
- environment override detection/redaction tests;
- state-machine and storage primitives with atomic-write fault tests;
- no test reads or changes the real Claude directory;
- real Claude commands are read-only until a separately approved authentication experiment.

Native transcript transfer belongs to M3, not M1.

## M1.5 reference-only Claude adoption

Existing profiles are inspected and adopted by reference; credentials never move into Relay.

```text
requested directory
  → canonicalize and enforce managed-root policy
  → verify owner, directory type, mode 0700, and no terminal symlink
  → validate canonical Claude executable and supported 2.1.x version
  → inventory authentication overrides by name only
  → reject conflicts before launch
  → run CLAUDE_CONFIG_DIR=<selected> claude auth status --json
  → parse bounded output using the known schema
  → derive a versioned non-secret identity pin
  → preview the Relay registry mutation
```

`profile inspect-existing` never opens Relay's profile registry and performs no filesystem writes. `profile adopt --dry-run` additionally reads the registry to detect corruption and duplicate names, but writes nothing.

An approved real adoption would write only:

- `~/.config/agent-relay/profiles.toml`, updated atomically with the profile name, provider, canonical directory reference, origin, enabled state, non-secret identity pin, and last availability observation;
- a transient same-directory `.profiles.toml.tmp.<pid>.<timestamp>.<sequence>` file used for atomic rename.

It would not write beneath `<profile>/claude`, modify Claude settings or credential files, access Keychain contents, or change the default Claude configuration. The target directory is provider-owned for its entire lifetime.

M1.5 supports only Claude Code 2.1.x until another version's auth-status schema is fixture-tested. This version gate is independent of the stricter, still-unverified session-transfer compatibility gate.

The observed Claude Code 2.1.276 auth-status schema includes `loggedIn`, `authMethod`, `apiProvider`, `email`, `orgId`, `orgName`, `subscriptionType`, `analyticsDisabled`, `configDirectory`, and `projectsDirectory`. The path fields are verification inputs, not identity fields: Relay requires them to refer to the selected canonical profile tree. Claude 2.1.276 did not return an account UUID in the two real inspections, so the current pin falls back to normalized email plus organization ID. Two profile names producing the same provider-scoped pin are rejected by default. Aliasing is reserved for a future explicit override and can never occur silently.

## Rejected alternatives

- **Shared writable session tree:** simpler discovery, but destroys profile ownership and makes concurrent divergence unsafe.
- **Credential-file swapping:** brittle, secret-bearing, and incompatible with Keychain and Relay's product principles.
- **Plugin-to-plugin correctness chain:** increases version coupling and leaves standalone use incomplete.
- **Terminal parsing as primary identity:** vulnerable to output injection and localized/UI changes.
- **Release source lock before target reservation:** creates a second-writer race.
- **Treat resumed process start as success:** cannot distinguish a failed resume that opened a fresh session.

## Fundamental viability

The project is viable if native transfer is presented as a versioned best-effort capability rather than a permanent provider guarantee. Even if Claude removes cross-config transcript compatibility, Relay still provides valuable explicit profiles, single-writer process ownership, durable handoff/recovery, usage awareness, and honest state continuation.

The scope would be flawed only if it promised invisible, lossless continuation across accounts regardless of provider behavior. This architecture explicitly does not make that promise.
