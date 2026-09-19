# Project status

## Current milestone

M3 (Herdr integration) complete through M3.2: a thin, optional Herdr plugin
(`plugins/herdr/herdr-plugin.toml`) exposes Relay's existing status/doctor/recovery/watch/handoff
behind Herdr actions plus an automatic `pane.agent_status_changed` event, live-validated against a
real Herdr 0.9.0 server including one full controlled bidirectional handoff between the real
adopted profiles on a disposable project. Details: `docs/herdr-integration.md`, `M3_FINAL_REPORT.md`.
No `relay-core` changes were required.

v0.1.0 standalone checkpoint (M2C.1 plus release hygiene: README quickstart, redacted docs/fixtures,
integration writes gated on an active install manifest). Profile B -> Profile A through `watch run`
has since been live-validated on the M2C.1 code (transaction `ho-18d68e5c2cc65a38-46090`, session id
`ad32d31c-9036-4dfa-a27f-0a1038c45eba`): completed automatically, same session id preserved, writer
lease moved to Profile A, orchestration lock released, no duplicate writer remained. Automatic
handoff is therefore live-validated in both directions.
M2C.1 complete — real, structured usage detection (StopFailure hook, statusline `rate_limits`,
stream-json `rate_limit_event`), an opt-in installer for them, automatic startup recovery, and
version/capability gating; details in the M2C.1 entry below and `docs/automatic-handoff.md`.
Prior: M2C complete — explicit, opt-in, usage-triggered automatic handoff (`relay watch run`), built on
the unchanged M2B transaction machinery, plus supervision of the target process so an orchestrator
crash during `TARGET_STARTING` can no longer leave an unaccounted-for writer. Details in the M2C
entry below. Automatic fail-back and quota pooling do not exist and require separate approval;
Herdr integration exists as of M3 (see the M3 entry below). Prior milestone summary — M2B.75: Claude session shutdown is now authoritative: `claude stop <id>` (Claude Code's
own documented command) replaces raw `kill` as the source-stop mechanism, verified quiescent
across multiple consecutive observations before a handoff proceeds. Live validation of this
milestone found and fixed a real polarity bug in M2B.5's liveness check (a killed pid was wrongly
allowed to override a still-listed, dormant/resurrectable session to "not active"). Both directions
of the Profile A/Profile B handoff, including a real competing-launch refusal and the authoritative-stop
flow, are live-validated on the disposable repo. M2B.5's conflict resolution, M2B's transactional
handoff, M2A's SESSION_CONTINUATION guarantee, and M1.5's real Profile A/Profile B adoption remain complete
underneath it. 

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
- Recorded the owner's separate successful Profile B profile-isolation validation without inspecting the profile.
- Implemented `profile inspect-existing --provider claude` with executable/version validation, environment conflict detection, bounded command execution, and closed schema parsing.
- Implemented reference-only `profile adopt --dry-run` with an exact Relay-owned write plan.
- Defined a versioned non-secret Claude identity pin and fail-closed compatibility policy.
- Added zero-write, malformed-output, secret-redaction, environment override, executable, version, identity, ownership, and dry-run tests.
- Revalidated Profile A and Profile B as owned, non-symlinked, mode-0700 profile directories.
- Added fixture-tested support for the Claude Code 2.1.276 auth-status schema and verification of its reported config/project paths.
- Completed read-only inspection and zero-write adoption dry-runs for Profile A and Profile B.
- Verified recursive filesystem metadata for both Claude profile trees and the default `~/.claude` tree remained unchanged.
- Confirmed distinct Profile A and Profile B non-secret identity pins after Profile A's authentication correction.
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
- Confirmed via `--dry-run` against the real Profile A and Profile B profile directories that both would
  succeed with distinct identity pins under the new code path (zero writes; matches prior M1.5
  validation), before performing the real adoption below.
- Fixed a pre-existing permission gap found while preparing real adoption: `~/.config/agent-relay`
  and `~/.config/agent-relay/profiles` were mode 0755 (only the leaf `.../profile-a/claude` and
  `.../profile-b/claude` were correctly 0700), which would have made `ProfileDirectory::prepare_root`
  fail closed with `unsafe_permissions`. Tightened both to 0700 with the owner's explicit
  confirmation; no Claude directory was touched by this fix.
- Performed real, non-dry-run adoption of both Profile A and Profile B. `~/.config/agent-relay/profiles.toml`
  (mode 0600) now holds both profiles with origin `adopted`, distinct non-secret identity pins
  (distinct normalized email + organization ID), and no credential material.
- Verified `profile status` and `profile doctor` report `healthy: true` / `identity_matches: true`
  for both Profile A and Profile B against the real Claude executable.
- Verified recursive filesystem metadata for both Claude profile trees (`.../profile-a/claude`,
  `.../profile-b/claude`) and the default `~/.claude` tree: no file or directory mtimes changed as a
  result of adoption; the only write was to Relay's own `profiles.toml`.
- **M2A (approved): proved cross-profile Claude session continuation live**, on a new disposable
  repo `~/repos/agent-relay-session-test` (never any production work):
  - Added `relay-provider-claude::session_transfer` (`discover_session`, `stage_transfer`,
    `escape_project_path`, `validate_session_id`, `ensure_supported_claude_version`,
    `ProcessLister`/`SystemProcessLister`) and wired `relay session stage-transfer
    --source-profile --target-profile --project-dir --session-id` in the CLI. Staging reuses
    Relay's own `FsAtomicWriter` (temp file, mode 0600, fsync, atomic rename, directory fsync) so
    a staged transcript is never more exposed than the original; every written artifact is
    re-hashed against the source before being reported. The source is only ever read.
  - Live run: started a real session under Profile A (`claude -p --session-id <uuid>` in the
    disposable repo), made a harmless commit, recorded session id `8586fe71-395b-4449-
    b973-78011d561fed`, transcript
    `~/.config/agent-relay/profiles/profile-a/claude/projects/-<escaped-project-path>/8586fe71-....jsonl` (sha256 `b697a049...ace3e7fb`, 261250 bytes, 67 lines).
    Verified no Claude process remained for Profile A's config dir afterward.
  - Staged that transcript to Profile B via the new CLI command; target hash matched the source
    exactly before Profile B ever touched it.
  - Resumed under Profile B with `claude -p --resume <same-session-id>` (no `--fork-session`) and
    asked it to recall, from memory only, the file it created and the exact approval sequence
    that had blocked the commit. It answered correctly on both counts — detail only present in
    the transcript's turn history, not recoverable from file or git state alone. This is the
    basis for calling M2A genuine **SESSION_CONTINUATION** for this Claude Code version, not
    merely STATE_CONTINUATION.
  - Continued the same session under Profile B to make and commit a second harmless change
    (`cb5d865`); same session id throughout.
  - Live-tested five of the six required failure cases through the real CLI: wrong/unregistered
    target profile (`profile_not_found`), missing session id (`session_not_found`), wrong project
    directory (`session_not_found`), a diverging pre-existing target artifact
    (`target_artifact_diverges`, target left untouched), and a genuinely live source profile via a
    real backgrounded Profile A session (`source_profile_active`). The sixth (unsupported Claude
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
    - Started a fresh session under Profile A (`cb0b14b8-6a6a-4e5e-b008-bfba1b4ebf87`), committed
      `m2b.txt` (`524a24d`).
    - `relay handoff run --from profile-a --to profile-b`: reached `COMPLETE` in ~5.4s; journal recorded
      the checkpoint, the staged/hash-verified artifact, and target verification; lease moved to
      `profile-b`; no leftover Claude process for either profile afterward.
    - Continued the session under Profile B for real (not just the verification canary): it correctly
      recalled the file it had created and appended a second line, committing `5baebe6`.
    - `relay handoff run --from profile-b --to profile-a` (reverse direction): the **first attempt
      correctly failed closed** with `target_artifact_diverges` — Profile A's directory still held its
      own stale pre-handoff copy of the transcript, and the M2A divergence guard refused to
      silently overwrite it. This is the expected, correct behavior, not a bug; there is no
      conflict-resolution command yet, so resolving it required an explicit operator decision
      (deleting the known-stale copy, which Profile A had not written to since ownership moved away)
      before retrying. The retry then reached `COMPLETE` normally; lease moved back to `profile-a`.
    - Verified live (not just via unit tests): `relay lock status` reporting the correct current
      owner at each step; no active Claude process for either profile at any checkpoint; a
      same-owner-required rejection (`writer_lease_owned_by_another_profile`) when attempting a
      handoff from the profile that no longer owns the lease.

- **M2B.5 (approved): hardened writer ownership and added transcript conflict resolution**,
  closing both weaknesses M2B's report flagged:
  - **Part A.** `SourceLiveness` no longer trusts `ps` text-matching as its primary signal. It now
    checks the exact pid + start-time fingerprint recorded in the project's `WriterLease`
    (`ProcessIdentity::is_still_the_same_process`), corroborated by Claude Code's own session
    bookkeeping (`claude agents --json`, matched by `sessionId`) to discover the pid and to detect
    sessions Relay never launched (now a blocking `untracked_writer_detected`, scoped to the
    project being handed off via a new `project_dir` parameter on the trait). Live testing proved
    `agents --json` alone is not sufficient: after a real `kill -9` of a background session's
    reported pid, it kept reporting `"state": "working"` — so the pid+fingerprint check is always
    the final arbiter when a pid is available.
  - Fixed a real precision gap in `ProcessIdentity` found while building this: a confirmed-dead
    pid was indistinguishable from "could not determine" (both collapsed to `None`); `ps`'s own
    "no such process" answer now resolves to `Some(false)`.
  - Added `relay launch`: spawns Claude as a real Relay-managed writer via `claude --bg`, polls
    `agents --json` for its real session id and pid (pid populates slightly after the session
    record itself — discovered live, and the poll now waits for it specifically), and records a
    `WriterLease` with that real identity. Refuses if another verified-live writer already holds
    the project, regardless of which profile requests the new launch.
  - **Live-discovered macOS/Claude Code limitation**: a `kill -9` of one `--bg` session's reported
    pid was *not* sufficient to make Relay treat it as stopped — Claude Code's own background
    daemon transparently reassigned a new OS process (new pid, new `startedAt`) to the same
    session id, and `agents --json` correctly reported it alive again under the new pid. Only a
    clean `claude stop <id>` reliably and permanently ends a `--bg` session; a single `kill -9` of
    a reported pid does not, because Relay does not control or own that daemon. This is a genuine,
    documented residual gap — see "any remaining race condition" below.
  - **Part B.** Added `relay session conflict inspect/resolve/rollback`, classifying a target
    transcript as missing, byte-identical, a known-stale ancestor (target's bytes are an exact
    prefix of source's — safe, no data lost), divergent/contains unique turns (source is a prefix
    of target, or truly diverged — never auto-resolved), or currently active (never touched
    regardless of decision level). `resolve` always previews unless `--yes` (stale ancestor) or
    `--force-discard-divergent` (divergent) is passed; every actual replacement backs up the
    displaced file first (atomic write, mode 0600, hash-verified) and writes a JSON resolution
    record next to it; `rollback` restores the most recent backup.
  - 133 tests pass (was 109); fmt and Clippy clean. New tests cover: an untracked session blocking
    a handoff, the tri-state pid-confirmation fix, and (in `conflict.rs`) every classification,
    dry-run writing nothing, stale-ancestor and divergent resolution (with and without the
    stronger flag), an always-refused active target, rollback with and without a backup, and
    corrupted resolution metadata failing closed.
  - **Live validation, both directions, with zero manual file deletion**: `relay handoff run
    --from profile-a --to profile-b` initially failed closed (`target_artifact_diverges`, Profile B's
    directory still held a stale copy from the earlier M2B run); `session conflict inspect`
    correctly classified it `target_stale_ancestor`; `session conflict resolve --yes` backed it up
    and replaced it; the handoff then completed. The same sequence was repeated for the reverse
    direction (`--from profile-b --to profile-a`) — the exact scenario M2B's report had required a manual
    `rm` for.
  - **Live competing-writer test**: with a real `relay launch`-started writer alive under Profile A,
    both a second `relay launch --profile profile-a` and a `relay launch --profile profile-b` for the same
    project were correctly refused (`writer_already_active`).
  - **Live crash/stale-ownership test**: after a clean `claude stop` of the managed writer, a
    subsequent `relay launch` correctly detected the recorded pid was confirmed gone and proceeded
    with a fresh launch.
- **M2B.75 (approved): authoritative Claude session shutdown**, closing M2B.5's remaining
  known gap:
  - Added `relay-core::handoff::SessionStopper`: issues an authoritative stop for one exact
    session, then verifies quiescence across multiple consecutive observations (never a single
    reading) before returning. `HandoffCoordinator`'s `SOURCE_STOPPING` phase now calls this
    instead of only checking liveness — an active source is stopped, not immediately refused; only
    a stop that cannot be verified quiescent within the bounded window blocks the transaction.
  - `relay-provider-claude::ClaudeSessionStopper` implements it with Claude Code's own documented
    `claude stop <id>` command (never a raw `kill`), matched by session id and project directory
    so it can never touch a different session or another profile's. Each quiescence observation
    combines Claude's own session bookkeeping (`agents --json`) with the M2B.5 pid+fingerprint
    check.
  - **Live-discovered and fixed a real bug in M2B.5's liveness check during this milestone's own
    validation**: Claude's background daemon keeps a `--bg` session listed (state
    "working"/"blocked") indefinitely after its worker process is killed — it is dormant and
    resurrectable, not gone — until explicitly `claude stop`ped. The prior check let a confirmed-
    dead recorded pid override a still-listed session to "not active," which let a live competing
    `relay launch` through while the original session could still have been resurrected. Presence
    in the listing is now trusted outright as active; the pid+fingerprint check only corroborates
    the case where a session is not listed at all. `relay launch`'s own pre-check was also
    hardened to require several consecutive not-active readings rather than a single snapshot (a
    separate, real transient-gap race also found live).
  - 139 tests pass (was 133); fmt and Clippy pass. New tests: successful/failed provider stop,
    the quiescence state machine (pure function, unit-tested directly: requires more than one
    quiet reading, resets on reappearance, resets when a corroborating pid is still confirmed
    live, reaches quiescence with no recorded pid at all), and the coordinator now stopping an
    active source rather than refusing it outright.
  - **Live validation, both directions**: launched a real Profile A writer via `relay launch`;
    recorded its session id and pid; killed the reported pid and confirmed a competing launch was
    still correctly refused; ran `relay handoff run --from profile-a --to profile-b`, whose journal notes
    show `"source authoritatively stopped and verified quiescent"` — the authoritative stop
    correctly resolved the dormant/killed session with no manual process killing beyond the single
    intentional demonstration kill. The reverse handoff hit the same known stale-artifact case as
    M2B.5 (Profile A's own pre-handoff copy) and was resolved the same way, via `session conflict
    resolve --yes`, with zero manual transcript deletion. Both directions reached `COMPLETE`.

- **M2B.75 adversarial soak test (approved scope: break the existing lifecycle/handoff
  implementation on the disposable repo; no M2C work)**: exercised authoritative-stop
  verification, writer ownership, daemon PID reassignment, dormant/resurrectable sessions, rapid
  stop/start cycles, chained Profile A<->Profile B handoffs (7+ hops), conflict resolution, and crash
  recovery, all live against the disposable repo and real adopted profiles.
  - Found and fixed a real gap: `relay session conflict rollback` had no active-target guard at
    all, unlike `resolve_conflict`'s explicit `TargetActive` refusal. Live-reproduced: resumed a
    session directly under Profile B (bypassing Relay) so it was genuinely active, then ran
    `relay session conflict rollback` for that exact session — it silently overwrote the live
    transcript with an older backup, discarding real in-flight turns, with no refusal and no
    warning. Fixed by giving `rollback_conflict` the same `target_active: bool` parameter and
    refusal `resolve_conflict` already had, and wiring the CLI's `Rollback` command to compute it
    via the same `target_is_active` liveness check `Resolve` already uses (including matching
    `--claude-executable` support). Re-verified live both ways: rollback now refuses
    (`conflict_requires_resolution`) while the session is still listed, and succeeds once it is
    genuinely stopped. Added a regression test
    (`rollback_refuses_an_active_target_even_with_a_backup_available`); 140 tests pass (was 139);
    fmt and Clippy clean.
  - Every other scenario held: a killed/dormant session's competing launch was correctly refused;
    a chained authoritative-stop handoff resolved a dormant session with zero manual process
    killing; two fully concurrent `relay launch` calls against the same project were serialized by
    the orchestration lock with exactly one winner and no leaked lock/lease state across four rapid
    launch/stop cycles; seven consecutive Profile A<->Profile B handoffs (beyond the two previously
    validated) showed no degradation, each correctly demanding explicit stale/divergent conflict
    resolution before proceeding; `relay recover` correctly reached `Failed{Stop}` for a
    `relay handoff run` process killed during `SOURCE_STOPPING` and `RecoveryRequired` for one
    killed during `TARGET_STARTING`, both idempotent on a second recovery attempt, with the
    `WriterLease` never touched in either case.
  - **Noted, not fixed (inherent to `SIGKILL`ing the orchestrator, not a Relay logic bug)**:
    killing `relay handoff run` during `TARGET_STARTING` can leave its child `claude -p --resume`
    process orphaned and still running — it finished its verification turn and appended to the
    target's transcript with no journal record of it (the journal died mid-phase). The existing
    `RecoveryRequired` + conflict-resolution design caught this correctly (the orphaned turn showed
    up as a genuinely divergent target on the next attempt and required explicit
    `--force-discard-divergent`, with the prior content backed up first) — no data was silently
    lost or overwritten, but it is a real-API-spending untracked process risk worth carrying into
    M2C's design: an unsupervised orchestrator crashing mid-launch can leave a real, running,
    billing Claude process that Relay no longer knows about.
  - **Verdict: M2C is safe to begin.** No correctness bug in the M2B.75 lifecycle/handoff path
    itself was found beyond the rollback gap above, which is now fixed and regression-tested.

- **M2C (approved): automatic usage-triggered handoff and target-start supervision.**
  - **Usage detection** (`relay-core::usage`, `relay-provider-claude::usage`): states `AVAILABLE`,
    `NEAR_LIMIT`, `EXHAUSTED`, `RESET_PENDING`, `UNKNOWN`; only `EXHAUSTED`/`RESET_PENDING` can
    trigger a handoff and `UNKNOWN` fails closed. Tiers in priority order: structured
    `claude agents --json` session state, then (opt-in `--probe`, spends a small real API call) a
    structured `is_error`/`result` probe, then an exact allowlisted phrase match. Vague errors
    classify `UNKNOWN`. No Claude hook or status command exposing usage was found, and no
    exhaustion-shaped `agents --json` state has been observed live, so tier 1 is currently inert
    and real detection relies on the opt-in probe or an operator/test signal. An observation
    records evidence category, a short secret-free description, timestamp and reset time; never raw
    provider output.
  - **Target selection and policy** (`relay-core::automation`): the fallbacks given on the command
    line are tried in order. A target must be enabled, healthy (fresh `doctor`), a different
    identity from the source, not currently exhausted, and not recorded exhausted. A recorded
    exhaustion with no reset time blocks that profile until `relay watch clear`; one with a reset
    time blocks it only until that time passes, after which it can be a failover target again.
    Reset never triggers fail-back on its own. Per-project ledger (`automation_state.json`) holds a
    30 s cooldown, at most 5 automatic handoffs per hour, and known-exhausted profiles. Every
    attempt, completed or failed, counts toward cooldown and the cap. No eligible target reports
    `WAITING_FOR_CAPACITY` and mutates nothing.
  - **Flow**: `relay watch run --profile P --fallback Q... --project D --session S` evaluates once,
    then calls the unchanged `HandoffCoordinator::run` (full PREPARING..COMPLETE sequence, all M2B
    gates). It is not a daemon; nothing monitors anything unless invoked (cron or a shell loop can
    call it). `--dry-run` persists nothing, `--simulate-usage` fault-injects the source signal,
    `--json` is global. `relay watch status` and `relay watch clear` show and reset the ledger.
  - **Orphan-target supervision**: the coordinator now persists `target_launch` (pid + start-time
    fingerprint) from an `on_started` callback the launcher fires immediately after spawn, before
    blocking on the child. `relay recover` on an interrupted `TARGET_STARTING` now (1) stops the
    recorded target, signalling the exact process only after re-confirming pid + fingerprint
    (SIGTERM, then SIGKILL), or, if no identity reached the journal, scans the process table for
    `claude -p --resume <session>` under exactly that profile's `CLAUDE_CONFIG_DIR` and stops it;
    (2) only then re-runs target verification, so a second target never runs beside a live first;
    (3) on success completes the transaction with the orphan's transcript turns untouched.
    If a stop cannot be confirmed the transaction becomes `RECOVERY_REQUIRED`, retryable by
    running `recover` again; `recover --acknowledge` is the explicit operator exit. A fresh
    handoff is refused (`pending_recovery_required`) while one is pending. Interrupted
    `SESSION_TRANSFERRING` now recovers to `FAILED` (staging is atomic and hash-verified) instead
    of a state that blocked the project.
  - **Bugs found and fixed during live validation**: (1) `agents --json` records for interactive
    sessions have no `id`; the required-field parse made any live interactive session on a profile
    fail every Relay liveness/stop/usage check with `malformed_provider_output`. `id` is now
    optional. (2) The first draft of orphan recovery relied on `claude stop`, which cannot address
    a foreground `-p` child, so a live orphan would never have been stopped; replaced by the
    verified-process termination above. (3) That draft also left `RECOVERY_REQUIRED` as a dead end
    once the pending-recovery gate existed; added retry and `--acknowledge`.
  - 191 tests pass (was 140); fmt and Clippy pass with `-D warnings`. New tests cover: usage
    classification incl. false positives, the pure decision function, watch orchestration with
    fake ports (handoff, no-action for every non-exhausted state, dry-run zero mutation, waiting
    for capacity, identity alias, cooldown, failed-attempt cooldown, reset handling, loop guard,
    concurrent triggers, corrupted ledger), orphan recovery (recorded, unrecorded, unstoppable,
    failed re-verification, transient retry, acknowledge, gate), real-process termination
    (verified, fingerprint mismatch untouched, no fingerprint fails closed), process-table scan
    matching, and the optional interactive `id`.
  - **Live validation** (disposable repo only, simulated exhaustion, real handoff machinery):
    a real `relay launch` Profile A writer was handed off by `watch run` to Profile B in 8.8 s (COMPLETE,
    same session id, Profile A listing empty, Profile B owns the lease, Profile B's transcript extends Profile A's
    byte for byte). Relay was SIGKILLed during `TARGET_STARTING` with the `claude -p --resume`
    orphan alive; `recover` stopped/confirmed it, re-verified, completed, preserved every earlier
    transcript byte, left no process. That was repeated with and without the spawn reaching the
    journal. A real orphan left by an earlier interrupted run was also recovered this way. No
    capacity (both profiles exhausted) reported `WAITING_FOR_CAPACITY` with zero journal or lease
    change; cooldown and the pending-recovery gate were exercised live.
  - **Not live-validated in this M2C session**: Profile B -> Profile A through `watch run`. The M2A
    guard refuses staging from any profile that has a running Claude process, and this validation
    ran inside a Profile B session, so it was correctly refused (`source_profile_active`,
    transaction FAILED, no state change). The reverse path is exercised by the same code and by
    tests, and a Profile B -> Profile A handoff completed earlier in M2B/M2B.5/M2B.75; it was later
    live-validated through `watch run` itself under M2C.1 (see the M2C.1 entry below).

- **M2C.1: standalone-readiness pass (structured usage detection, integration installer, startup
  recovery, capability gating).**
  - **Research result** (Claude Code 2.1.277): documented `StopFailure` hook (matcher `rate_limit`,
    fire-and-forget), documented statusline `rate_limits.five_hour/seven_day` (`used_percentage`,
    `resets_at`), typed stream-json `rate_limit_event` (`status`, `resetsAt`, `rateLimitType`,
    `utilization`, `isUsingOverage`). No CLI usage command exists; `agents --json` exposes no
    exhaustion state. `error=rate_limit` is also produced for generic 429 capacity errors, so it
    is never trusted alone.
  - **Bug fixed**: the M2C phrase allowlist never matched real Claude text ("You've hit your
    session limit · resets 3pm"), so `--probe` could not have detected real exhaustion. Now an exact
    "hit your <name> limit" matcher (session/weekly/usage/usage credit/monthly/monthly spend/Opus/
    Sonnet/Fable/fast/bare) tested against the literal strings; 429/"rate limit exceeded" never match.
  - **Policy** (`usage_policy.rs`, pure): EXHAUSTED = rejected `rate_limit_event` with a future
    reset and no overage (not contradicted by a newer fresh statusline), OR `StopFailure` + fresh
    statusline window ≥100% with future reset, OR opt-in phrase + the same corroboration.
    NEAR_LIMIT ≥90% (or 100% with no refusal), AVAILABLE below, UNKNOWN for stale/ambiguous;
    RESET_PENDING comes from the ledger (recorded exhausted, reset in the future). Model-scoped
    limits count only for a matching `--workload-model`; fast limit never blocks.
  - **Installer**: `relay integration claude install|status|uninstall [--dry-run]` per profile
    (`--profile` or explicit `--config-dir`). Preserves hooks, chains an existing statusLine (fail
    closed otherwise), backs up settings, records a manifest, uninstall restores byte for byte or
    surgically removes Relay's entries if settings changed since. Hidden `relay hook claude ...`
    commands never fail the calling session and record only closed metadata.
  - **Probe** demoted to an explicit diagnostic (stream-json, skipped for profiles already recorded
    exhausted, still spends a real request).
  - **Startup recovery**: `relay watch run` recovers the project's current incomplete transaction
    (under the orchestration lock, before detection or any probe) and returns `Recovered`, or exits
    non-zero `recovery_required` when ambiguous, or `TransactionInFlight` when another process holds
    it. Journals from older Relay schemas that are terminal, and superseded non-current journals, are
    history and never block (found live: a legacy journal blocked the disposable project).
  - **Capability gating**: per-capability Verified/Unverified/Unsupported with runtime checks
    (`--help` stream-json, `agents --json` shape); newer 2.1.x patches are Unverified (install
    refuses without `--allow-unverified-version`), other release lines Unsupported.
  - 243 tests pass (was 191). **Live validation** (disposable repos, Profile A/Profile B real adopted
    profiles, no quota exhausted): installed into both; real StopFailure/statusline payloads run
    through the installed hook commands; StopFailure alone, transient 429 at 40%, stale statusline,
    NearLimit 95% and statusline-100%-without-refusal all did NOT hand off; corroborated Profile A ->
    Profile B handed off automatically twice (evidence `stop_failure_corroborated`, reset recorded);
    RESET_PENDING excluded the exhausted profile from target selection; Relay SIGKILLed with a live
    `claude -p --resume` orphan, restart recovered (orphan stopped, transaction COMPLETE, lease
    moved, no second writer); uninstall restored both settings.json files byte for byte. The real
    statusline of the running Profile B session was captured with genuine `rate_limits` data.
  - **Profile B -> Profile A live-validated**: a subsequent `watch run` completed the reverse
    direction automatically (transaction `ho-18d68e5c2cc65a38-46090`, session id
    `ad32d31c-9036-4dfa-a27f-0a1038c45eba`) — same session id preserved, writer lease moved to
    Profile A, orchestration lock released, no duplicate writer remained. M2C.1 automatic handoff
    is now live-validated in both directions. A real refusal (`StopFailure` from an actually
    exhausted account) was never observed and no quota was burned.

- **M3 (approved): Herdr integration — thin plugin, no relay-core changes.**
  - **M3.0/M3.1**: researched Herdr's live plugin/CLI/socket surface (installed 0.9.0, current
    0.9.1, no breaking drift from the M0-era assumption), designed the integration contract
    (`relay-herdr` as a subprocess client of the existing `relay --json` CLI, profile mapping via
    an explicit `relay_profile`/`relay_profile_fallback` Herdr `tokens`-map token, re-validated
    live on every use, never cached blindly), and implemented status/doctor/recovery/watch/manual
    handoff actions plus an auto-retry/usage-plugin interoperability classifier. 32 tests against
    a scripted `relay` process; no live Herdr server touched yet.
  - **M3.2**: linked the plugin against a real Herdr 0.9.0 server on a disposable workspace/project
    and fixed what that found: `HERDR_PLUGIN_CONTEXT_JSON` is flat with no tokens/session id at all
    (a real follow-up `herdr pane get`/`workspace get` call is required — new `herdr_client`
    module); Herdr's own CLI is inconsistent about `--json` across subcommands (caught live, not
    guessed); `report-metadata` needs an undocumented `--source`; Herdr's built-in Claude
    integration only watches the default `~/.claude`, never an isolated `CLAUDE_CONFIG_DIR`
    (confirmed by reading its own installed hook script) — addressed with an explicit
    `relay_session_id` token fallback, same pattern as the profile token. Added an automatic
    `[[events]]` handler on `pane.agent_status_changed` (reuses the `watch` code path, relies
    entirely on Relay's own existing cooldown/ledger for throttling) and
    `relay integration herdr install|status|doctor|uninstall`, live-verified through a full
    install → doctor → uninstall → status → reinstall → doctor cycle.
  - **Live validation, full bidirectional controlled handoff** on the disposable project, through
    the real plugin and the real adopted profiles, using `--simulate-usage exhausted` (no real
    quota spent): profile A launched a real writer, handed off to profile B (`COMPLETE`, same
    session id, lease moved, lock released), the reverse direction correctly hit a genuine
    `target_stale_ancestor` conflict (profile A's own pre-handoff copy — resolved via
    `session conflict resolve --yes`, never force-discard), then completed back to profile A.
    Session id unchanged throughout; `claude agents --json` empty for both profiles afterward
    (single writer, no orphan); Herdr's own workspace list and server status unaffected.
  - **Noted, not fixed**: two of four `watch run`/`handoff run` attempts during that sequence hit
    `untracked_writer_detected` even though only the expected session was ever listed — resolved
    itself on retry both times. This is the existing (pre-M3) `ClaudeSourceLiveness` check, not
    something the Herdr integration introduced; per the M3 core-freeze rule it was not patched
    blind since it could not be reproduced deterministically enough to trust a regression test,
    and is recorded for a future dedicated investigation instead.
  - 43 new tests (34 actions/mapping + 11 install), all against scripted `relay`/`herdr` processes.
    Workspace total 243 -> 286 passing; fmt/clippy clean throughout. Zero changes to `relay-core`,
    `relay-provider-claude`, or the standalone handoff machinery itself.
  - **Not observed**: genuine (non-simulated) provider exhaustion end to end — this pass used
    fault injection throughout, exactly as M2C's original validation did.
  - Full account: `docs/herdr-integration.md`, `M3_FINAL_REPORT.md`.

## In progress

- Nothing in progress. M1.5, M2A, M2B, M2B.5, M2B.75 (with its soak test), M2C, M2C.1, and M3 (both
  M3.0/M3.1 and M3.2) are all complete.

## Blockers

- Compatibility is validated for exactly Claude Code 2.1.276, a single foreground `-p` session (for
  M2A/M2B's core session-continuation path) plus `--bg` (for M2B.5/M2B.75's launch/liveness/stop
  path); no subagents. The `-` project-path escaping convention and the subagent-sidecar naming
  assumption (`<session_id>-*.jsonl`) are both observed, not documented by Anthropic, and are not
  re-validated across versions.
- `relay handoff run` and `relay launch` both spend real API usage (a verification turn, and a
  real background session respectively); there is still no automatic/quota-triggered handoff, and
  none is planned without separate approval.
- The writer-liveness/stop logic has now been live-tested against two distinct real races
  (M2B.5's daemon PID reassignment, M2B.75's dormant-but-still-listed session) and corrected both
  times; this pattern — the real behavior of Claude's `--bg` daemon differing from what its own
  documentation/output implies — has not been exhaustively explored, so a third undiscovered edge
  case in this area cannot be ruled out from what has been tested so far.
- Resolved in M2C: the M2B.75 soak test's orphaned `claude -p --resume` child (Relay killed during
  `TARGET_STARTING`) is now discovered and stopped by `relay recover` before the target is
  re-verified. Residual: nothing reaps the orphan until `relay recover` runs, so it can spend its
  one short verification turn in the meantime, and recovery is an explicit command, not automatic.

- Claude Code auto-updated to 2.1.277 during M2C validation and `-p --resume`, `--bg`, `agents
  --json` and `stop` all behaved as on 2.1.276, but the compatibility pin above still names 2.1.276.
- Real usage detection (resolved in M2C.1 by hook/statusline/stream-json signals; the old note
  followed): unattended detection previously needed `--probe` (a small real API call per check) until one exists. This is the main remaining gap
  before unattended use. A killed orchestrator between spawn and the journal rename is now handled
  by the process-table scan, whose `ps -Eww` environment matching is macOS-specific.
- The M2A active-process guard is per profile, not per session, so an unrelated live Claude
  session under the source profile blocks a handoff (safe, but coarse).
- Relay run from inside a Claude Code tool shell sees `CLAUDE_CODE_MESSAGING_TOKEN` and a
  different `CLAUDE_CONFIG_DIR` and correctly reports `environment_override_conflict`; validation
  unset both for the Relay process only.

## Unresolved architecture questions

- What explicit confirmation and UX should a future alias override require, if aliases are supported at all?
- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

M3 (Herdr integration) is done through M3.2. Remaining before a further milestone: observe a
genuine (non-simulated) exhaustion end to end; investigate the intermittent
`untracked_writer_detected` observation from M3.2's live validation; decide whether to wire Herdr's
session-report hook into isolated profiles' `settings.json` (would make the `relay_session_id`
token fallback unnecessary); and decide packaging for a marketplace-style `herdr plugin install`
(both `relay-herdr-plugin`'s `relay` discovery and `relay integration herdr install`'s manifest
discovery currently require a local repo checkout).

Await explicit owner authorization before automatic fail-back, unattended (non-`--probe`) usage
detection, writing Herdr's session-report hook into any real profile's `settings.json`, or trusting
this path across any Claude Code version/layout other than 2.1.276/2.1.277 or any launch mode other
than foreground `-p` / `--bg`. Given M2B.75, M2C, and M3.2 each found real bugs or races during live
validation, treat any further Claude `--bg`/interactive daemon behavior assumption, and any further
Herdr CLI/schema assumption, as unverified until it has been tested live.
