# Experiment ledger

## Cache cleanup and first rebuilt TUI execution

NATIVE_BUILD.md and engineering/evidence/native-tui-20261030/ retain this follow-up.
User-authorized cache cleanup reclaimed about 6 GiB; the pinned native CLI links
successfully with the unchanged insertion. Initial harness attempt timed out its
five-second version probe during overlapping cold execution; no TUI started.
After version verification completed, eight real embedded TUIs passed initial
pre-prompt selection reporting, then failed /new. The callback conflates owned
ephemeral/background work with foreground selection; supervision loss blocks
interrupt/cleanup too. Read-RPC classification is additionally incomplete.
Shared mode remains unrun. No safety or permission gate was loosened, and no
production source, credential or active session changed.

## Pinned native build continuation from aa96aea

NATIVE_BUILD.md and engineering/evidence/native-build-20261030/ retain every
stage. Exact 0.166.0 source fetched; official isolated Rust 1.95.0 installed;
source patch hashes matched unchanged. Initial --locked refusal was diagnosed
as 170 source-only version stamps, with no external dependency changes.
Native client/TUI cargo check then passed. Three CLI build stages stopped at
the explicit disk reserve; final code generation reached app-server after
native core compiled. Neither native TUI mode ran. All owned build process
groups are absent, production is unchanged, and full parity remains false.
Only this lab's redundant history/generated check metadata was reclaimed;
all pinned source trees and original evidence are preserved.

Phase 2 is recorded in [FEASIBILITY.md](FEASIBILITY.md), [DEFECTS.md](DEFECTS.md), and the additional
ledger below. Prior phase 1 evidence remains unchanged, including failed/blocked experiments.

All roots below were created for this investigation. Full native transient artifacts stay in
those roots; bounded structured evidence is retained here. Existing accounts/worktrees were
never used. README.md distinguishes native tests, current Relay code, and incomplete prototypes.

| Experiment | Reproduction/evidence | Observation and interpretation |
| --- | --- | --- |
| Environment probes | Python socket bind on loopback TCP and a new Unix path; `pty.openpty`; `ps -p $$ -o pid=,lstart=` | Listener creation and ps denied (`EPERM`); PTYs work. Shared daemon/mock HTTP tests cannot be run in this sandbox. Shell curl also cannot resolve GitHub; connector retrieval works. |
| Native initial attempt | `stdio_probe.py` before offline-turn materialization; `stdio-lazy-persistence-first-attempt.json` | Both threads started but foreign read/resume/fork had no persisted rollout. This did NOT prove lack of concurrency or successful lock exclusion. |
| Native same-project | Final `stdio_probe.py`; `stdio-same-project.json`; transient `/private/tmp/relay-concurrency-sdij0vuy` | Two processes, same home/project, distinct roots. Offline turns started/interrupted. Foreign read works; foreign resume refused while A lives; B forks A; after A SIGKILL, B resumes A; B's original thread still readable. |
| Native different-projects | `stdio_probe.py --different-projects`; `stdio-across-projects.json`; transient `/private/tmp/relay-concurrency-jmk_jpm0` | Same conclusions across two separate directories. No real inference or same-daemon frontend exclusivity claim. |
| Initial TUI input failure | `tui-input-harness-failed-attempt.json`; `/private/tmp/relay-tui-si4eb6vh/*.terminal` | Text and Enter were coalesced; intended prompt was not submitted, and later interrupt exited A. Not counted as a lifecycle success. Harness corrected to separate text from Enter. |
| First-use TUI database failure | `tui-fresh-home-failure.json`; `/private/tmp/relay-tui-dtgjd92n/B.terminal` | Concurrent startup on a new home produced migration 18 `no such table: stage1_outputs`; B exited. This is a failed startup, not a passing steady-state test. |
| Warmed embedded TUI intermediate run | `/private/tmp/relay-tui-q2p7umoz/results.json` and terminal diagnostics | Two embedded TUIs, correct separate launch markers; initial and new-chat start hooks with prompts; fork visually/native-metadata confirmed but no prompt yet, so no fork hook. Extended rather than declaring complete coverage. |
| Warmed embedded TUI final run | `tui_probe.py`; `tui-embedded-transitions.json`; `/private/tmp/relay-tui-vjs967qd` | Initial selection has no start hook before input; A new/fork/resume receive lifecycle hooks at next prompt; B remains separate; delayed old-root end appears after navigation. Native fork has new concrete/root IDs in this build. This disproves hook-only selection coverage even when embedded provenance is correct. |
| First-use database race repeated | `bootstrap_race.py`; `bootstrap-race.json`; `/private/tmp/relay-bootstrap-bto3wa0b` | Ten independent homes, two simultaneous processes per home; 7 of 20 endpoints failed initialization. Recorded as an upstream/platform initialization risk; not fixed by this research. EOF handling was later improved so the harness need not wait a full response timeout after an exited server. |
| Exact-thread cancellation | `shell_probe.py`; `shell-interruption-and-crash.json`; `/private/tmp/relay-shell-m0d7_e2f` | One real server, two native threads, two bounded heartbeat commands. Exact `turn/interrupt` stops A while B continues. Proves this shell-command path only. |
| Backend death versus tool lifetime | Same shell run | Server SIGKILL releases its backend but B's heartbeat continues. Commands self-expire, so no unidentified child process is killed. Backend/process-lock release is not sufficient stop evidence. |
| Production watcher cross-talk | `watcher-probe 2 0 1`; `watcher-misattribution.json` | Both watchers report one injected exhaustion; second reports transition into first's thread. Invokes unchanged production library directly; production launch lock remains intact. |
| Production historical-fork bug | `watcher-probe 1 0 1 idle production historical-fork`; `watcher-historical-fork.json` | One watcher changes to an unchanged pre-existing fork without any new activity. This falsifies ancestry-as-selection even under profile exclusivity. |
| Exact-file cost prototype | `watcher-probe 2 0 1 inject targeted`; `watcher-targeted-isolation.json` | Only the explicitly bound target receives its synthetic exhaustion. No native identity mechanism, recovery implementation or full health proof is supplied by this prototype. |
| Current watcher cost | `measure_watchers.py production`; `watcher-production-measurements.json` | 1/2/8 watchers x 0/1000 histories, three repetitions. Production library measured. CPU totals include setup/cleanup. |
| Targeted cost | `measure_watchers.py targeted`; `watcher-targeted-measurements.json` | Same fixture counts/window; much less history-dependent recurring work. Includes measured peak RSS. Prototype excludes production durability/identity/fencing. |
| Fixture-only cost | `watcher-fixture-baseline.json` | Zero watchers with 0/1000 fixture files. Separates file-creation/deletion contribution from reader work; do not interpret total benchmark CPU as idle CPU. |

## Repository verification

No tracked production source changed. The pre-existing untracked root AGENTS.md was left alone.

- `cargo fmt --check`: passed.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: passed.
- `cargo test --workspace --offline`: failed in relay-cli binary tests, 215 passed / 4 failed;
  later workspace targets were not all run by that command. Failures concern process ancestry,
  fingerprint-based supervisor detection, and supervisor-record publication. `ps` is denied in
  this sandbox and process identity resolves unknown. This is an environment limitation; do not
  weaken identity checks or claim this suite was green.
- `cargo test -p relay-core --offline`: library tests 125 passed / 5 failed, all in process identity/
  ancestry checks; process queries returned `None` rather than expected true/false. This is
  consistent with the denied ps probe. Later package targets were not all executed after failure.
- `cargo test -p relay-provider-codex --offline`: 72 passed / 5 failed / 1 ignored. The five failures
  are rollout discovery/transition timing assertions, several observing transition without the
  following exhaustion within a fixed 600 ms wait. A representative transition test also failed
  in isolation. Do not dismiss these as a passing flaky suite or attribute them all to ps. Current
  worker ordering includes another 300 ms wait before reading a newly rebound tail, which is
  consistent with a boundary timing failure, but the full cause is not conclusively established.
  The research probes with longer observation windows captured the expected eventual records.
- Research Rust prototype builds in release/offline mode and is rustfmt-formatted.
- Python harnesses compile; `verify_evidence.py` passes 21 narrow checks of retained observations.
  This checker explicitly reports `zero_regression_certification: false`.

`evidence/validation.json` records test summaries and failure names. These checks characterize
the current baseline in this environment. They are not evidence that a proposed concurrent
implementation has passed production tests, and no production fixes were attempted.

## Explicitly blocked/unperformed experiments

1. Multiple connections/native TUIs against a disposable shared local daemon: Unix/TCP listener
   creation forbidden. Existing user daemon was deliberately not contacted.
2. Successful inference, model-spawned subagents/background tools, mock structured quota failures:
   cannot host a local HTTP backend here; no actual account used as a substitute.
3. Authenticated natural simultaneous exhaustion and Claude/Codex handoff: no disposable logged-in
   account was provisioned, and active accounts were excluded by scope.
4. Herdr pane lifecycle integration: no production panes touched; no claim of native UI validation
   beyond the isolated PTYs.
5. Complete native quiescence/controller protocol: no suitable supported external contract has
   been established, and no upstream/native host patch was built here.
6. Performance of that unimplemented protocol, isolated-runtime full-memory overhead, Linux
   containment and mixed-version migration: not measured. Acceptance matrix specifies the lab.

These limits leave the best architectural direction defensible, but full parity unverified.
The next experiment must target the missing native contract, rather than accumulating more
observer-only tests that cannot establish client admission or runtime quiescence.

## Phase 2: working candidates and falsification

All additional programs live under feasibility/, use new temporary homes, and leave production
behavior untouched. The source insertion is applied only to a disposable upstream source fixture.

| Experiment | Evidence file / command | Outcome |
| --- | --- | --- |
| Eight native logical controllers | `run_native.py`; `native-eight-clients.json` | 15 native assertions pass: simultaneous commands, exact fork/new/resume responses, old-root ownership retained, foreign/stale/fenced refusal, scoped observation, crash preservation and no quota calls. NOT eight native TUIs. |
| Detached-child falsification | Same run | A setsid child writes after its parent turn completes native interruption. Full transfer refuses. |
| Host-bypass falsification | Same run | An unmediated native shell request writes after frontend fence; proves the client-only boundary is insufficient. |
| Two-phase native insertion core | `run_bridge.py`; `native-bridge-core.json` | Compiled bridge core surrounds actual native operations. Eight callbacks bind before frontend continuation; conflicts/refused methods do not forward. Unresolved creation preserves ownership but blocks new adoption. |
| Source insertion applicability | `patch_native.py`; `source-applicability.json`; `native-insertion.patch` | Pinned blobs/anchors verified and patch applied to disposable source fixture. Complete upstream TUI was NOT compiled or run. |
| Admission-order/lost-reply faults | `faults_native.py`; `native-faults.json` | Same-thread fence waits for admitted request, B progresses, native creation reply loss quarantines target, no guessed adoption. |
| Unsafe admission mutant | `faults_native.py --unsafe-research-mutant`; `unsafe-admission-mutant.json` | EXPECTED FAILURE: fence overtakes authorized request when per-thread admission lock is removed. Not a passing implementation test. |
| Registry multiprocess stress | `stress_registry.py`; `registry-stress.json` | Eight independent 8-way races; exactly one winner in each; aliases, stale epochs and crash-after-intent checked. Registry proof only. |
| Deterministic obligations | `python3 -m unittest discover -s .../feasibility -p test_controller.py -v` | Eight tests pass, including acknowledged command without start event and disconnected drain boundary. Not a native quiescence proof. |
| Independent defects | `defects.py`; `independent-defects.json` | Historical-fork misbinding 8/8; cold startup 3/16 failed versus warm/gated 0/16 each; completed DB quick_check ok. |
| Matched native cost | `measure.py`; `matched-costs.json` | Three repetitions per direct/controller/bridge mode; native RSS via server diagnostics, worker peak RSS/CPU and exact method lists. Per-request bridge is materially more expensive. |
| Linux containment alternative | `linux_containment.py`; `linux-containment-blocker.json` | BLOCKED on macOS; explicit-delegation bounded lab script supplied, not run with guessed/existing cgroups. |

A code-review correction added per-thread admission locking before the definitive fault runs;
the unsafe mutant experimentally demonstrates its necessity. Another review correction accounts
for accepted-but-not-yet-started commands before returning a turn-drain-only receipt. Neither
correction is a production change or proof of host-wide admission.

### Phase 2 verification

- Eight deterministic obligation tests pass; native candidates/fault/stress evidence is retained
  with scope labels and no full-parity assertion.
- Compiled Rust bridge core release build and Clippy pass; root formatting and workspace Clippy
  pass. Python harnesses compile. Patch applicability and revision refusal are separate from full
  native workspace compilation.
- Existing scheduler regressions: 42 passed. Existing new-fork regression: 1 passed. Existing
  stale-offset regressions: 2 passed. These protect historical behavior; they do not validate
  concurrency or fix historical-fork adoption.
- Repeated full workspace test run again fails the same four process-identity/supervisor binary
  tests under denied process inspection: 215 passed / 4 failed. Later targets are not counted as
  green after this failure. No safety check was loosened to make these pass.
- Complete rebuilt TUI/shared-daemon, successful inference/subagents, authenticated quota,
  Claude/Codex handoffs, Linux containment and real Herdr remain blocked/unverified. Runnable
  bounded lab procedures are in FEASIBILITY.md; these outcomes must not be marked passed.

## Controlled engineering increment

Full commands, source findings and interpretation: ENGINEERING.md.

| Experiment | Retained evidence | Result |
| --- | --- | --- |
| Preserve original packet before changes | Dedicated branch commit afabe33; verified bundle + original SHA-256 inventory in workspace checkpoints/ | 77 research files durably committed; original main and AGENTS.md untouched |
| Full pinned source fetch | engineering/evidence/environment.json | FAILED/BLOCKED: DNS, exit 128; full TUI not built/run |
| Listener preflight | Same environment record | TCP and Unix bind EPERM; mock/shared-daemon lab remains blocked |
| Integrated service/native contract | engineering/run_integrated.py; integrated-initial.json and integrated-transitions.json | Initial 20 and final 22 narrow checks; eight logical clients, actual native commands/navigation, replay/epochs/reopen; full_parity false |
| Bypass and detached work | Final integrated record | Both counterexamples reproduced; native host/executor integration remains required |
| Contract interleavings | engineering/test_host_contract.py | 2 tests pass; concurrent replay forwards once; close holds service lock until admitted work finishes |
| Containment self-migration adversary | engineering/containment_escape.py; containment-escape-blocked.json | BLOCKED, direct command exit 2 on macOS; no measured Linux escape or containment claim |
| Existing deterministic/regression evidence | controller tests, codex_poll, both retained-evidence validators | 8 controller + 42 scheduler tests pass; 21 phase-1 checks and phase-2 validator pass |

Pre-commit result-loss injection in the new service is not a native process kill.
The committed-reply retry result is not proof of atomic native creation tokens.
No successful model/subagent, quota handoff, eight native TUIs or Herdr result was
added. D1/D2 remain separate open defects; neither was quietly fixed.
