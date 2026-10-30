# Continuation checkpoint

Date: 2026-10-30. This is a research/engineering branch, not a release.

## Pinned native build follow-up from aa96aea

[NATIVE_BUILD.md](NATIVE_BUILD.md) records the now-successful isolated Rust
1.95.0 setup, exact upstream source and actual native client/TUI type-check.
The source insertion compiled without changes. A release-tag lockfile mismatch
required only 170 local package version stamps to match 0.166.0; all external
dependencies/checksums/edges are unchanged and builds retain --locked.

The linked CLI build remains **BLOCKED_DISK_RESERVE**, not compiler-failed:
three bounded attempts ended at the lab's 1.5 GiB reserve, even after reclaiming
only generated lab caches/history. Native core compiled; the final attempt
reached app-server. No CLI binary exists, and no embedded/shared TUI runtime
suite was run. All owned build process groups are gone. Production is untouched.

The complete lab and compiled cache remain at
`/private/tmp/relay-native-build-20261030-9fw2zv7w`. First provide additional
storage, then use NATIVE_BUILD.md's new-stage resume command; do not repeat
already completed research or source/toolchain downloads. Host admission and
executor quiescence obligations remain unchanged. The original research tip
aa96aea was backed up remotely after a fresh workflow audit; final new checkpoint
identity/bundle verification are recorded outside the bundle in checkpoint.json.

The sections below retain earlier historical state and blockers. Their DNS,
listener and ps failures are superseded only by the new capability/build evidence.

## Latest capability follow-up

The continuation from c113acf verified its bundle/hash and all 90 mirrored files.
Current DNS, listener and own-process inspection probes remain blocked; PTYs work.
No native suites were rerun. The remote research ref was not found, and remote
backup via GitHub create_blob was rejected because write approval is required
while session policy is never. No remote backup exists from that attempt.

[LAB_SETUP.md](LAB_SETUP.md) contains precise required capability changes, the
verified Rust 1.95.0 pin, safe branch-only push instructions, and the native build/
integration sequence for an authorized disposable lab. The deployment workflows
were checked against remote blob hashes; research backup matches none of their
deployment triggers. The research checkout's origin is the local original
workspace, so use the explicit GitHub URL when backing up remotely.

This follow-up stops at the environmental boundary as requested. It changes no
prototype/production implementation or guarantee status. A new documentation/
evidence commit follows c113acf; exact tip and bundle hash remain in checkpoint.json.

## Durable state

- Original workspace: `/Users/kefasmanda/repos/agent-relay`, still on main at
  `7a6466667ddf1aa3196a9351d5f87c90d9527c77`; no production source modified.
- Development checkout: `/private/tmp/agent-relay-native-validation`.
- Branch: `research/codex-concurrency-native-validation`.
- Prior research checkpoint: `afabe33dda0e3ff358041ff9336914e4c12cf692`.
- Integrated engineering increment: `53c6a64` (full hash in bundle/commit log).
- A subsequent documentation commit adds this continuation record. The exact
  final tip, bundle SHA-256 and verification are in the original workspace's
  `docs/investigations/checkpoints/checkpoint.json`.
- Bundle: `docs/investigations/checkpoints/codex-concurrency-native-validation.bundle`
  in the original workspace. It requires base `7a6466667ddf1aa3196a9351d5f87c90d9527c77`,
  which is already present in the original repository. Original research checksum
  inventory: `docs/investigations/checkpoints/research-inventory.json`.
- Research documents/code/evidence are mirrored back to the original workspace.
  AGENTS.md remains separate/untracked. No push, merge, installation or deployment.

If the temporary checkout disappears, restore into a new disposable checkout:

```sh
LAB=$(mktemp -d)
git clone --no-local /Users/kefasmanda/repos/agent-relay "$LAB/relay"
git -C "$LAB/relay" fetch /Users/kefasmanda/repos/agent-relay/docs/investigations/checkpoints/codex-concurrency-native-validation.bundle refs/heads/research/codex-concurrency-native-validation:refs/heads/research/codex-concurrency-native-validation
git -C "$LAB/relay" switch research/codex-concurrency-native-validation
```

Do not commit a bundle into itself. Keep bundles/checkpoint metadata outside the
branch's codex-concurrency research directory. Retain earlier raw evidence rather
than overwriting it when running a revised experiment.

## Completed and observed

Read FEASIBILITY, ACCEPTANCE, EXPERIMENTS, DEFECTS; inspected relevant prototypes,
production profile-lock/watch integration and regression tests; rechecked the
observer-writer and remote-permission historical incidents.

New code: engineering/host_contract.py, run_integrated.py, test_host_contract.py,
containment_escape.py. New primary source: pinned native TurnAdmission + blob
manifest. Detailed analysis and production-capable blueprint: ENGINEERING.md.

Two real-backend integrated runs retained; final 22 narrow checks pass. Eight
logical controllers execute native commands and fork/new/resume concurrently.
Operation replay avoids duplicate native creation after a committed lost reply;
uncertain results retain ownership; service reopen rejects stale generations.
Stopping A's turn leaves B–H progressing. Detached A and raw-native bypass remain
positive counterexamples. Full handoff refuses; full_parity is explicitly false.

Tests: 2 new contract interleavings, 8 existing controller obligations, 42 existing
production scheduler regressions pass. Both prior evidence validators and Python
compilation pass. Production Rust has no diff. No new full workspace certification;
earlier documented failing/ignored tests remain outstanding.

## Exact blockers

1. Full Codex checkout fetch failed DNS (exit 128). Selected source is not a
   complete build workspace. Full patched native TUI has NOT been built/run.
2. TCP and Unix listeners both fail EPERM. Shared-daemon multiconnection and
   deterministic inference backend tests cannot run here.
3. Native dispatcher and native root persistence do not contain the laboratory
   token/epoch contract. External wrapping is not host-wide enforcement.
4. No platform/native executor quiescence adapter demonstrated. Linux cgroup
   escape probe exits BLOCKED/2 on macOS. Do not turn that into a passing test.
5. No disposable authenticated provider accounts, real quota exhaustion or live
   Herdr testing used. No credential access is authorized by this checkpoint.

## Next executable work, in order

1. On a disposable development host with source/dependencies available, run the
   full pinned-source patch/check/build and both native_tui_lab modes exactly as
   listed in FEASIBILITY.md. Capture compiler errors and actual structured IDs.
   Do not treat source applicability or bridge-core compilation as this result.
2. Port the operation contract into native thread creation/store and scoped
   admission. Extend existing TurnAdmission rather than adding a second writer.
   Source evidence identifies queue rechecks and extension admission seams; the
   no-thread trait and per-process state require actual changes. Persist token
   and binding atomically in native creation, not in a later Relay transaction.
3. Re-run run_integrated.py against that actual native enforcement, changing its
   bypass oracle only when raw native requests demonstrably refuse. Add multiple
   independent clients/processes, stale approval/queued work and real host kills
   at every persistence prefix. Never remove the current falsification oracle
   merely because a cooperative gateway passes.
4. On explicitly delegated disposable Linux, run containment_escape.py, then the
   earlier linux_containment.py. Address self-migration/management authority,
   descriptor inheritance and namespace escape before accepting an empty-group
   receipt. Develop a separate macOS adapter; retain reservations if unavailable.
5. Integrate native executor accounting (shell/background/subagents/MCP/remote)
   and durable fence milestones before enabling any ownership transfer. Then
   test the actual Relay journal/snapshot/target/recovery chain and quota events.
6. Complete eight native TUI/Herdr, natural quota, cross-provider, performance and
   mixed-version acceptance gates. Preserve production ProfileSessionLock until
   all supported modes pass. D1/D2 remain separate follow-ups in DEFECTS.md.

The strongest architecture remains native host epochs + atomic creation tokens +
global admission + executor authority, with a thin native TUI binding. This
checkpoint advances its test substrate, not proof that its critical boundaries
are solved. No zero-regression or full-parity claim is justified.
