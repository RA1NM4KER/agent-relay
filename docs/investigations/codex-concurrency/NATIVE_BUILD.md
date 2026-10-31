# Pinned native build continuation, 2026-10-30

## Latest native evidence (2026-10-31)

The role/cancellation repair is built into the pinned native CLI. Eight actual
embedded TUIs pass nine narrow identity/navigation/foreground-interruption/crash
checks. Twenty-two Python obligations and two Rust policy tests pass; the
standalone bridge passes Clippy with warnings denied and research Rust formatting.
The complete native lab package now builds. Its shared daemon starts and eight
TUIs bind exact roots, then the disk-reserve guard stops the incomplete suite.
Shared validation remains disk-blocked. See [CONTINUATION.md](CONTINUATION.md) and
`engineering/evidence/native-roles-20261031/` for current results and preserved
failures. Earlier “not built/run” and failed embedded `/new` statements below are
historical. Full native host enforcement, executor quiescence, successful Relay
handoff and zero regressions remain unproven; production locks remain unchanged.

This increment continues `aa96aea`; it does not restart the architecture study.
Production Relay, ProfileSessionLock, installed binaries, real provider homes,
and live Herdr panes remain unchanged. Full parity remains unproven.

## Complete disposable daemon package

The bare debug CLI cannot start a managed daemon: native `prepare_install.rs`
requires a complete package. Build the real `codex-code-mode-host` and
`codex-exec-server-cli` binaries; the executor binary is named `codex-exec-server`.
The first package-name mistake and generic V8 download failure are retained.
Generic Cargo invokes the fork's default denoland V8 URL, which returns 404.
The pinned repository's `codex_package.v8.resolve_codex_v8_cargo_env` supplies the
Codex release artifacts after checking their manifest against the SHA256 trust
anchor in `third_party/v8/`. That path succeeds without dependency/pin changes.

Lab helpers and reproducible command scripts are archived in the latest evidence
directory. Run them from the existing lab root through `run_stage.py`; they keep
Rust, Cargo, downloads and HOME isolated. Upstream package construction and
validation are used, with only executable copying replaced by macOS APFS clones
(independent inodes, exact hash checks) to conserve physical storage. The native
runtime/validator is unchanged. The copied ripgrep is an existing local executable,
not source-built here; its hash is recorded. No provider data is copied.

The package lives at `target/research-package` inside the lab. Its manifest is
explicitly `0.166.0-relay-research`, so native initial-start policy does not follow
the production update channel; the actual CLI still reports `0.166.0`. The staged
CLI hash exactly matches the successful embedded binary. The shared harness now
requires an isolated managed daemon PID/socket and matching executable hash before
launching the eight normal TUIs, with no injected remote/permission flags.

## Storage recovery and first native runtime attempt

This follow-up supersedes the disk blocker below, retaining all earlier failures.
The user explicitly authorized removal of unnecessary caches. Removed inactive
Relay debug dependency/incremental/build metadata and package-download metadata;
kept the native source/toolchain/target cache, Relay entry/release binaries,
personal files, provider homes and running application caches. Available disk
increased from approximately 1.7 to 7.6 GiB (about 6 GiB reclaimed). The detailed
cleanup inventory is local-only under workspace checkpoints.

The cached `cargo build --locked -p codex-cli --bin codex` then **passed** in
146.4s, using the original source insertion, pin and audited lockfile. The linked
binary reports `codex-cli 0.166.0`. Native build is no longer an environment
blocker. Exact stages/results and binary hash are in
`engineering/evidence/native-tui-20261030/`.

The first harness attempt overlapped the binary's slow initial version check:
the standalone check took 16.0s, while NativeRPC's five-second version probe
timed out. That attempt started no TUI. Retained it, then reran only after the
successful version check completed; no timeout or version gate was weakened.

The next embedded run launched eight actual source-built native TUIs across
two disposable projects and acknowledged eight exact initial selections before
any prompt. It then **failed** `eight native new selections before their prompts`.
No fork/resume/crash success is implied; shared mode has not been run while the
embedded gate remains unresolved.

### Runtime failure diagnosis

The pinned TUI legitimately creates ephemeral temporary structured-request
threads (`tui/src/temporary_structured_request.rs`, `start_temporary_thread`),
separate from the displayed foreground root. The callback reserves their exact
returned IDs, but its generic mutation path requires every owned root to equal
the frontend's display selection. It does not retain/propagate the native
ephemeral/background role. This conflicts with ordinary native background work.

The run retained eight foreground selections, sixteen bound creation intents,
and sixteen subsequently UNKNOWN reservations. Native logs report supervision
loss in temporary-thread cleanup and foreground `turn/interrupt`. The source
bridge checks its global FAILED flag before even its nominally ungated interrupt
path, so callback refusal also disables cancellation. The foreground task remains
in progress and native `/new` refuses accordingly. Terminal text is diagnostic
only; no identity or ownership is inferred from it. The registry, structured
native logs, rollout metadata and pinned source establish the relevant paths.

Startup also reports unclassified `hooks/list` and `thread/attachment/list` RPCs.
They require separate source-level classification and parity tests; do not
blindly allow unknown methods to make the harness green. No native hooks,
temporary tasks or user permission features were disabled as a workaround.

Next engineering increment: propagate explicit primary/display/background roles
from native response-bound identity, preserve reservations and event scope for
background work, and provide narrowly owned cancellation/cleanup after admission
loss without reopening writes or releasing uncertain ownership. Add deterministic
negative/interleaving tests, rebuild, and rerun embedded before shared mode.
The harness must observe completed cancellation through structured evidence before
issuing navigation; changing a sleep or weakening selection assertions is not a
fix. Host-wide admission and executor quiescence still remain separate foundations.

## Exact source and isolated tools

- Upstream: `rust-v0.166.0`, commit
  `6ff1e1ca3b30fc8b914720681fcf5fe62e95f856`.
- Disposable lab: `/private/tmp/relay-native-build-20261030-9fw2zv7w`.
- Research: `/private/tmp/agent-relay-native-validation`, branch
  `research/codex-concurrency-native-validation`.
- The existing four-file source patch matched every expected Git blob and
  applied unchanged. Its SHA-256 remains
  `48184de3a16a04921801a3ec7e768f5f0869b42276b5d32e327c05645552a828`.
- Installed Rust 1.95.0 with clippy, rustfmt and rust-src beneath the lab's
  `tooling/` directory. The downloaded official rustup-init executable was
  checked against its official SHA-256. No shell profile/PATH installation or
  system Rust replacement occurred. `rustup show` confirms the source's pin.
- Builds use a dedicated HOME, CARGO_HOME, RUSTUP_HOME and target directory,
  debug symbols disabled and incremental compilation off. The CLI build uses
  one worker to limit memory/swap pressure. No features or permissions removed.

The former network/listener/process-inspection environment restrictions no
longer prevent this lab. This does not supply executor containment or prove
native safety; those remain independent engineering requirements.

## Release-tag lockfile mismatch

The first `cargo check --locked -p codex-app-server-client -p codex-tui` stopped
before compilation because the release tag's Cargo.lock describes 170 local
workspace packages as version `0.0.0`, while its workspace manifest declares
`0.166.0`. The original lockfile was retained. Offline Cargo metadata produced
the proposed normalization, then stopped because registry archives had not yet
been downloaded.

A structural TOML comparison verifies the entire normalization: only those 170
source-only workspace package version fields changed. External package entries,
versions, Git revisions, checksums and dependency edges remain identical.
All subsequent Cargo commands retain `--locked`. The exact diff, hashes and
changed-package list are retained in `engineering/evidence/native-build-20261030/`.
This is a release-stamping adjustment, not permission to update dependencies.

## Completed build gate

`cargo check --locked -p codex-app-server-client -p codex-tui` **passed** with
the original source insertion on Rust 1.95.0 (375.6 seconds after dependency
fetch/lock diagnosis). No source fix was required to get this check green.
This advances the earlier source-applicability result to actual native workspace
type checking. It is not a linked CLI, a running native TUI, host-wide admission,
an executor fence, or successful handoff evidence.

## Resource failure and recovery

The machine initially had roughly 9 GiB free. Dependency fetching includes
V8/Chromium submodule history; native compilation also competes with macOS swap.
The first CLI build was deliberately stopped by the lab runner's 1.5 GiB free
space reserve. The recorded exit is SIGTERM from that guard, not a compiler
error. The runner signals only the new process group it created for that stage.

Compacted only this lab's downloaded V8 and ICU Git histories and bare Cargo
cache entries to their exact pinned commits. Verified HEAD, Git tree and clean
tracked source before/after; no source or dependency identity changed. Initial
checkout compaction alone did not free space because the Cargo bare cache held
hardlinks to the old objects. Compacting those bare caches reclaimed about
1.8 GiB. Both passes and the interrupted build remain recorded.

The resumed CLI build again reached the reserve. With all build processes
stopped, reclaimed 651 MiB of completed check-only `.rmeta` files without linked
`.rlib`/`.dylib` counterparts and compacted the remaining lab Git histories.
Verified source HEAD/tree/clean status throughout. This is generated-cache
reclamation, not removal of experimental evidence or a changed dependency set.

The final resumed `cargo build --locked -p codex-cli --bin codex` compiled
codex-core successfully and reached codex-app-server, then hit the same reserve.
All three CLI attempts ended with the guard's SIGTERM, not a compiler diagnostic.
All owned build process groups were verified absent afterward. No linked
`target/debug/codex` exists; **neither embedded nor shared native TUI runtime
suite was run**. No source/compiler error was observed, but a complete linked
build is still unproven. The successful check result is retained separately.

The current blocker is disk capacity, replacing the earlier DNS/listener/ps
blockers. Approximately 1.7 GiB remained when recording the final result;
the source/toolchain/dependency/build caches remain usable for continuation.
Do not repeat the build until additional storage is available. Prefer several
additional GiB for linking and sufficient further headroom for eight native
runtimes, swap and test artifacts; 15–20 GiB free is a conservative lab setup
target, not a measured exact minimum. Do not remove unrelated user files or
relax native features, admission checks or permission gates to fit this volume.

## Remote durability

Rechecked remote main, the absence of the research ref, and the four workflow
files against the main commit. Workflow contents match the local audit:
research pushes trigger no CI deployment, dogfood, release or Pages publication.
Pushed the existing research tip `aa96aea` to its named research branch and
verified the remote ref equals that exact tip. This resolves the prior backup
attempt's blocker; no main/tag/PR/release/deployment was created.

## Evidence and continuation

`engineering/evidence/native-build-20261030/` retains source/toolchain identity,
initial patch, workflow audit, lockfile comparison, build commands/statuses,
resource-compaction records and logs. Large disposable source/toolchain/build
caches remain in the lab directory, not in Git. `run_stage.py` records exact
argv, timing, worker settings, exit code and free disk before/after each stage.

Once capacity is available, resume without repeating source/toolchain setup:

```sh
python3 /private/tmp/relay-native-build-20261030-9fw2zv7w/run_stage.py build-cli-after-capacity cargo build --locked -p codex-cli --bin codex
```

The stage name is new so no previous log is overwritten. The runner selects
the isolated tools, pinned source cwd, one worker, existing target cache and
the same disk reserve. If moving the lab, update the explicit path references
in lab helpers; do not copy live provider homes into it.

After a successful linked CLI build, run the existing native_tui_lab in both
embedded and shared modes against that explicit source-built path, with fresh
HOME/CODEX_HOME and projects. Preserve failed runs and diagnose their real
compiler/transport/callback/selection/permission failures. Do not weaken gates,
drop permission flags, inject --remote or claim eight logical clients are eight
native TUIs. Host-wide admission and executor quiescence remain mandatory even
if the frontend laboratory passes.

`result.json` is the machine-readable result for this increment. Earlier raw
evidence remains untouched; no successful native runtime, host admission,
executor quiescence, quota or handoff acceptance is added by this build work.

## Relay baseline verification

Required `cargo fmt --check` and workspace/all-targets Clippy with warnings denied
pass using the unchanged Relay checkout and its existing cached toolchain.
`cargo test --workspace --offline` fails in the existing PTY presentation test
after earlier targets pass, including all 219 CLI unit tests and 74 automatic
handoff integration tests. The old process-inspection failures are absent from
the completed CLI unit target. Do not call the full workspace suite green.

The presentation test passes on one isolated rerun. An independent, fixed
25-case diagnostic reproduced PTY EOF before waitable exit twice, followed by
successful exit within approximately 50 microseconds; every command's color
policy matched. DEFECTS.md records this as separate D3; no production/test fix
is included. Remaining non-CLI workspace packages passed separately: 507 passed,
zero failed, two ignored. Cargo stopped before subsequent CLI test targets, so
those targets are not represented as passing. Exact statuses are retained in
relay-validation.json and relay-followup-validation.json. A retry passing cannot
erase the first failure, and earlier provider timing failures are not considered
fixed merely because this invocation passed all 77 nonignored provider tests.

Raw logs and unified patches are saved as deterministic `.gz` records;
compressed-records.json supplies their
uncompressed SHA-256 values. Source/evidence integrity is preserved without
rewriting raw diff context or installer output to satisfy text whitespace checks.

## Shared runtime and reserve-guard outcome

The package's staged CLI passes --version (first run 22.0s); the timed-out native
installer's copied CLI subsequently passes in 0.35s. The 90-second native package
preparation attempt is retained. A one-second sample of the next attempt shows
`prepare_from_package -> package_tree -> blake3` before daemon launch. The bounded
180-second attempt starts an exact rebuilt managed daemon and eight shared TUIs,
then the 1.5-GiB guard sends SIGTERM after 110.3s. No full shared suite passes.
Recovered SQLite/event evidence is explicitly partial; no original successful
result/check list is fabricated. Two reservations become UNKNOWN, fourteen remain
ACTIVE. They are not released or reused.

Native stop then times out at 60s. The exact owned daemon PID, recorded process
start time and full isolated executable path are verified before SIGTERM. No lab
processes remain. This is laboratory cleanup, not an executor-quiescence receipt.
The harness now routes SIGTERM through evidence/cleanup and fails on native daemon
cleanup errors, using a bounded 180s debug-stop window. An actual eight-TUI injected
SIGTERM confirms cleanup with zero surviving frontends; shared cleanup with that
new window is still unverified.

Only completed crate-download archives and hash-verified duplicate failed/stopped
lab package caches were removed. The APFS-cloned validated package and compiled
artifacts remain. Before the next fresh shared run, provide at least 4 GiB free;
about 2.5 GiB remains after cleanup. Do not use another reserve-stopping retry as
proof or erase the failure evidence to reclaim space.
