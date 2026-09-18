# Implementation plan

This plan begins only after the M0 architecture is approved. Real authentication and provider-state mutation require a separate explicit test plan and authorization.

## M1 — profiles and isolation

### 1. Rust workspace and quality gates

- Create the five proposed crates with dependency direction enforced by manifests.
- Pin a supported minimum Rust version.
- Add formatting, Clippy, unit-test, and cross-platform build workflows.
- Define versioned JSON result/error envelopes before commands ship.

### 2. Core types and storage

- Implement typed IDs, profile state, availability observations, and provider capabilities.
- Implement OS-native config/state resolution and restrictive atomic writes.
- Parse configuration with unknown-field and size policies.
- Add fake clocks, process identities, and fault-injectable storage.

### 3. FakeProvider

- Simulate authentication success/failure/wrong identity.
- Simulate session create/resume, rate limits, crashes, hangs, corrupt sessions, delayed shutdown, native-transfer failure, and target-start failure.
- Ensure every test operates in temporary directories with a poison sentinel that fails if `~/.claude` is referenced.

### 4. Profile CLI

- `relay profile add/list/status/remove/doctor` for fake profiles.
- Stable human and `--json` output.
- Refuse unsafe names, symlinked roots, bad ownership, and overly broad permissions.
- Removal never deletes provider state without a separate, explicit destructive flag and confirmation.

### 5. Claude isolation proof

- Implement environment-policy and read-only `auth status --json` parsing.
- Validate explicit config-directory resolution using temporary unauthenticated profiles.
- Prepare, but do not run, the real two-profile authentication experiment.

### 6. M1 tests

- parallel profile operations;
- corrupt/oversized configuration;
- spaces and Unicode paths;
- symlink escape attempts;
- planted secrets never serialized;
- authentication override variables detected without logging values;
- wrong identity and disabled profile behavior;
- atomic-write crash injection.

### M1 acceptance

- The core has no Claude or Herdr dependency.
- Fake profile workflows pass on macOS and Linux CI.
- No command outside an explicitly approved experiment can alter real Claude auth.
- `STATUS.md` records any compatibility assumptions discovered.

## Later milestone outline

### M2 — project/session tracking

Implement project initialization/binding/status, process ownership, two-lock infrastructure, Relay-owned Claude launch, structured session capture, and stale-lock recovery.

### M3 — manual handoff

Implement the full journaled state machine first with FakeProvider, then the version-gated Claude artifact adapter, packet fallback, target verification, and recovery. This is the first product milestone.

### M4 — usage awareness

Consume structured hard-limit hooks, add freshness-aware local observations, then optionally integrate compatible `herdr-agent-usage` output. Keep `UNKNOWN` as a normal state.

### M5 — Herdr

Ship the minimal manifest and adapter against Herdr 0.9.0, with status, handoff, usage, and recovery actions plus a terminal pane. Test with fake agents before a real Claude pane.

### M6 — hardening

Run the full fault matrix, security review, platform builds, documentation polish, and release dry runs. Publish nothing without explicit approval.

### M7 — optional automation

Consider only after manual and suggested handoffs are routine and recovery metrics demonstrate reliability.

## Real-account experiment requiring approval

The first real experiment will be proposed as a separate checklist showing every command and path before execution. It will:

- use two dedicated test profile directories under Agent Relay's config root;
- invoke only Claude's normal authentication flow;
- never inspect or copy credential values;
- create a disposable repository and a benign short conversation;
- stop profile A, copy only the identified session artifacts, and resume under B;
- verify hook identity/session/path evidence;
- record hashes and non-secret results only;
- leave the existing default Claude config untouched;
- offer a precise cleanup list limited to experiment-created paths.

