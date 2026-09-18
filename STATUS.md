# Project status

## Current milestone

M2B complete — crash-safe transactional handoff (durable journal/state machine, project-level
writer lease, genuine OS-release-on-crash orchestration lock) proven live in both directions
(Erika -> Megan -> Erika) on the disposable repo. M2A's SESSION_CONTINUATION guarantee and M1.5's
real Erika/Megan adoption remain complete underneath it. No automatic/quota-triggered handoff and
no Herdr integration exist yet; both are explicitly out of scope until separately approved.

## Completed

- Initialized a standalone Git repository.
- Inspected the current Claude Code and Herdr CLIs without changing authentication or configuration.
- Performed a temporary-directory isolation probe for `CLAUDE_CONFIG_DIR`.
- Inspected source and licensing for all required upstream projects.
- Selected Apache-2.0 for original Agent Relay code.
- Documented the M0 research, architecture, dependency decisions, threat model, and implementation plan.
- Scaffolded the Rust workspace and pinned a repository-local development toolchain.
- Implemented provider-neutral profile, identity, authentication, and availability models.
- Implemented canonical OS config/state paths, private profile directories, permission checks, and atomic state writes.
- Implemented FakeProvider and profile add/list/status/remove/doctor commands with JSON output.
- Defined non-executing Claude create/adopt/status/identity/launch interfaces.
- Added the optional Herdr module boundary without an integration dependency.
- Added corruption, identity, auth, path, symlink, permissions, secret-redaction, adoption, and atomic-failure tests.
- Added macOS/Linux CI configuration.
- Recorded the owner's separate successful Megan profile-isolation validation without inspecting the profile.
- Implemented `profile inspect-existing --provider claude` with executable/version validation, environment conflict detection, bounded command execution, and closed schema parsing.
- Implemented reference-only `profile adopt --dry-run` with an exact Relay-owned write plan.
- Defined a versioned non-secret Claude identity pin and fail-closed compatibility policy.
- Added zero-write, malformed-output, secret-redaction, environment override, executable, version, identity, ownership, and dry-run tests.
- Revalidated Erika and Megan as owned, non-symlinked, mode-0700 profile directories.
- Added fixture-tested support for the Claude Code 2.1.276 auth-status schema and verification of its reported config/project paths.
- Completed read-only inspection and zero-write adoption dry-runs for Erika and Megan.
- Verified recursive filesystem metadata for both Claude profile trees and the default `~/.claude` tree remained unchanged.
- Confirmed distinct Erika and Megan non-secret identity pins after Erika's authentication correction.
- Added default rejection of duplicate provider-scoped identity pins in core registration and Claude adoption dry-run.
- Implemented `ClaudeAdoptionProvider`, a real (non-fake) `Provider` that re-inspects immediately
  before Relay's registry write; gating (authentication, identity presence) stays in
  `ProfileService::add`, matching `FakeProvider`'s contract rather than duplicating checks.
- Enabled `relay profile adopt` to perform real, non-dry-run registration by default (`--dry-run`
  still only previews); `profile status`/`doctor` now select the provider matching the profile's
  own stored provider kind instead of always using `FakeProvider`, and gained a matching
  `--claude-executable` override.
- Added tests for real registration success, duplicate-name rejection, duplicate-identity
  rejection, unauthenticated fail-closed behavior, zero writes to any Claude directory, and no
  stray atomic-write temp files; 52 tests pass (was 45); fmt and Clippy pass.
- Confirmed via `--dry-run` against the real Erika and Megan profile directories that both would
  succeed with distinct identity pins under the new code path (zero writes; matches prior M1.5
  validation), before performing the real adoption below.
- Fixed a pre-existing permission gap found while preparing real adoption: `~/.config/agent-relay`
  and `~/.config/agent-relay/profiles` were mode 0755 (only the leaf `.../erika/claude` and
  `.../megan/claude` were correctly 0700), which would have made `ProfileDirectory::prepare_root`
  fail closed with `unsafe_permissions`. Tightened both to 0700 with the owner's explicit
  confirmation; no Claude directory was touched by this fix.
- Performed real, non-dry-run adoption of both Erika and Megan. `~/.config/agent-relay/profiles.toml`
  (mode 0600) now holds both profiles with origin `adopted`, distinct non-secret identity pins
  (distinct normalized email + organization ID), and no credential material.
- Verified `profile status` and `profile doctor` report `healthy: true` / `identity_matches: true`
  for both Erika and Megan against the real Claude executable.
- Verified recursive filesystem metadata for both Claude profile trees (`.../erika/claude`,
  `.../megan/claude`) and the default `~/.claude` tree: no file or directory mtimes changed as a
  result of adoption; the only write was to Relay's own `profiles.toml`.
- **M2A (approved): proved cross-profile Claude session continuation live**, on a new disposable
  repo `~/repos/agent-relay-session-test` (never Schoolscape or other production work):
  - Added `relay-provider-claude::session_transfer` (`discover_session`, `stage_transfer`,
    `escape_project_path`, `validate_session_id`, `ensure_supported_claude_version`,
    `ProcessLister`/`SystemProcessLister`) and wired `relay session stage-transfer
    --source-profile --target-profile --project-dir --session-id` in the CLI. Staging reuses
    Relay's own `FsAtomicWriter` (temp file, mode 0600, fsync, atomic rename, directory fsync) so
    a staged transcript is never more exposed than the original; every written artifact is
    re-hashed against the source before being reported. The source is only ever read.
  - Live run: started a real session under Erika (`claude -p --session-id <uuid>` in the
    disposable repo), made a harmless commit, recorded session id `8586fe71-395b-4449-
    b973-78011d561fed`, transcript
    `~/.config/agent-relay/profiles/erika/claude/projects/-Users-kefasmanda-repos-agent-relay-
    session-test/8586fe71-....jsonl` (sha256 `b697a049...ace3e7fb`, 261250 bytes, 67 lines).
    Verified no Claude process remained for Erika's config dir afterward.
  - Staged that transcript to Megan via the new CLI command; target hash matched the source
    exactly before Megan ever touched it.
  - Resumed under Megan with `claude -p --resume <same-session-id>` (no `--fork-session`) and
    asked it to recall, from memory only, the file it created and the exact approval sequence
    that had blocked the commit. It answered correctly on both counts — detail only present in
    the transcript's turn history, not recoverable from file or git state alone. This is the
    basis for calling M2A genuine **SESSION_CONTINUATION** for this Claude Code version, not
    merely STATE_CONTINUATION.
  - Continued the same session under Megan to make and commit a second harmless change
    (`cb5d865`); same session id throughout.
  - Live-tested five of the six required failure cases through the real CLI: wrong/unregistered
    target profile (`profile_not_found`), missing session id (`session_not_found`), wrong project
    directory (`session_not_found`), a diverging pre-existing target artifact
    (`target_artifact_diverges`, target left untouched), and a genuinely live source profile via a
    real backgrounded Erika session (`source_profile_active`). The sixth (unsupported Claude
    version/layout) is unit-tested only (`ensure_supported_claude_version`) — not live-tested,
    since that would require installing a second, unsupported Claude Code build against a real
    account.
  - Fixed a real bug found during this work: the first staging pass wrote the target transcript
    at mode 0644 (default umask) instead of matching the source's 0600, before switching to
    `FsAtomicWriter`. Re-verified after the fix.
  - 63 tests pass (was 52 after M1.5); fmt and Clippy clean.
- **M2B (approved): crash-safe transactional handoff**, built and proven live:
  - Added `relay-core::handoff`: a durable per-transaction JSON journal and state machine
    (`PREPARING -> CHECKPOINTED -> SOURCE_STOPPING -> SOURCE_STOPPED -> SESSION_TRANSFERRING ->
    SESSION_TRANSFERRED -> TARGET_STARTING -> TARGET_VERIFIED -> COMPLETE`, plus
    `FAILED{phase, reason}` and `RECOVERY_REQUIRED{reason}` reachable from any in-progress state);
    a project-level `WriterLease` (owner profile, process identity, session id, transaction id);
    and an `OrchestrationLock` — a genuine `flock` (via `fd-lock`) held for one `relay
    handoff`/`relay recover` invocation's entire execution, so the OS itself releases it on a
    crash and a later process can prove the earlier one is gone by successfully re-acquiring it,
    rather than trusting a timestamp. `relay-core` stays provider-neutral: the coordinator drives
    the transaction through injected `SourceLiveness`/`SessionStager`/`TargetLauncher` ports.
  - `relay-provider-claude` supplies the real adapters: `SourceLiveness` reuses M2A's process
    check; `SessionStager` reuses M2A's hash-verified `stage_transfer` completely unchanged;
    `TargetLauncher` actually runs `claude -p --resume <id>` under the target profile with a
    fixed, content-free verification prompt and parses session id / success from its JSON output.
  - Recovery re-acquires the same orchestration lock a running transaction would hold, so success
    is itself proof the prior process is gone. It only auto-resolves the unambiguous ends
    (nothing mutated yet -> `Failed`; target already verified -> `Complete`); anything ambiguous
    (mid-transfer, target starting) becomes `RECOVERY_REQUIRED` and is never auto-retried or
    auto-relaunched — matching "fail closed, require explicit recovery."
  - Found and fixed two real gaps while building this: `ProjectId` derivation did not canonicalize
    its input (a symlink, `..`, or trailing slash could have aliased two projects or split one
    project across two lock domains); and `OrchestrationLock` followed a pre-existing symlink at
    its lock path instead of rejecting it.
  - Wired `relay lock status --project-dir`, `relay handoff run --from --to --project --session`,
    `relay handoff status <transaction-id> --project-dir`, and `relay recover <transaction-id>
    --project-dir`.
  - 109 tests pass (was 63); fmt and Clippy clean. New coordinator-level tests cover: concurrent
    handoff attempts (serialized by the lock, exactly one wins), a live lock blocking recovery, a
    stale/crashed transaction allowing recovery, PID-reuse/process-identity mismatch, wrong
    source profile (lease owned by someone else), wrong project (no state leakage between two
    projects), a diverging target artifact, target startup failure, target identity mismatch,
    recovery from every interrupted stage, a corrupted journal, a symlinked lock path, an
    atomic-write failure preserving prior content, and recovering an already-terminal transaction
    twice (idempotent).
  - **Live validation on the disposable repo**, both directions:
    - Started a fresh session under Erika (`cb0b14b8-6a6a-4e5e-b008-bfba1b4ebf87`), committed
      `m2b.txt` (`524a24d`).
    - `relay handoff run --from erika --to megan`: reached `COMPLETE` in ~5.4s; journal recorded
      the checkpoint, the staged/hash-verified artifact, and target verification; lease moved to
      `megan`; no leftover Claude process for either profile afterward.
    - Continued the session under Megan for real (not just the verification canary): it correctly
      recalled the file it had created and appended a second line, committing `5baebe6`.
    - `relay handoff run --from megan --to erika` (reverse direction): the **first attempt
      correctly failed closed** with `target_artifact_diverges` — Erika's directory still held her
      own stale pre-handoff copy of the transcript, and the M2A divergence guard refused to
      silently overwrite it. This is the expected, correct behavior, not a bug; there is no
      conflict-resolution command yet, so resolving it required an explicit operator decision
      (deleting the known-stale copy, which Erika had not written to since ownership moved away)
      before retrying. The retry then reached `COMPLETE` normally; lease moved back to `erika`.
    - Verified live (not just via unit tests): `relay lock status` reporting the correct current
      owner at each step; no active Claude process for either profile at any checkpoint; a
      same-owner-required rejection (`writer_lease_owned_by_another_profile`) when attempting a
      handoff from the profile that no longer owns the lease.

## In progress

- Nothing in progress. M1.5, M2A, and M2B are all complete. Automatic/quota-triggered handoff and
  Herdr integration have not started and require separate owner authorization to begin.

## Blockers

- No conflict-resolution command exists for a target that already holds a stale/divergent copy of
  a session's transcript (discovered live during M2B's reverse-handoff test, described above);
  today this requires an explicit operator action outside Relay, which is safe but manual.
- M2A/M2B's process-liveness check (`SystemProcessLister`) is a best-effort `ps` scan for a
  `CLAUDE_CONFIG_DIR=<dir>` token, not a lock — it has the same known TOCTOU race as any
  process-table inspection (see docs/security.md's "Two simultaneous writers" section) and must
  not be treated as sufficient for unattended/automatic handoff.
  - Compatibility is validated for exactly Claude Code 2.1.276, a single foreground `-p` session,
    no subagents, and no `--bg`/worktree mode. The `-` project-path escaping convention and the
    subagent-sidecar naming assumption (`<session_id>-*.jsonl`) are both observed, not documented
    by Anthropic, and are not re-validated across versions.
  - `relay handoff run` itself spends real API usage (one verification turn under the target);
    there is still no automatic/quota-triggered handoff, and none is planned without separate
    approval.

## Unresolved architecture questions

- What explicit confirmation and UX should a future alias override require, if aliases are supported at all?
- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

Await explicit owner authorization before starting M2C (automatic, usage/quota-triggered
handoff) or Herdr integration, and before trusting this path across any Claude Code
version/layout other than 2.1.276. Do not begin that work without separate approval.
