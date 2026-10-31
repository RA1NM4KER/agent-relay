# Concurrent Codex sessions: investigation and decision record

## Shared-daemon follow-up from 19d3c29 (2026-10-31)

The user approved the proposed Playwright cache cleanup. Three inactive caches
were removed (758 MiB); chromium-1248 was retained because NotificationCenter
still maps a resource from it. Lossless APFS compression reclaimed 3.56 GiB from
2,723 inactive native dependency artifacts with per-file decoded SHA256,
permissions and timestamp verification. Required compiled artifacts remain
available; two stopped lab package copies were retained as verified APFS clones.
Free space reached approximately 7 GiB. The 4 GiB entry gate and 1.5 GiB runtime
reserve remain unchanged. No native rebuild or repeated toolchain download ran.

Actual shared validation exposed two harness mistakes, retained as failures:
`app-server proxy --sock` is a raw byte bridge, while native Unix transport uses
WebSocket framing; Ctrl-C opens the shared task-exit menu rather than directly
interrupting. The observer now uses pinned websockets 15.0.1 over Unix, only for
initialize/loaded-root/turn-status reads. A real empty-daemon smoke test passes,
including mutation refusal and cleanup. Escape supplies the native interrupt.

The first Escape run passes 16 checks but fails to submit its third prompt after
resume; all evidence is retained. Explicit bracketed paste avoids the pinned
composer's Enter-as-newline burst heuristic. The final fresh shared run
`relay-native-tui-lab-zqnciiy7` passes all **26 narrow checks** against the actual
managed daemon: eight simultaneous native TUIs, exact in-memory ownership roots,
concurrent held response turns, interruption/disconnect, `/new`, `/fork`, exact
original-root resume, and A cancellation/frontend death while seven exact native
streams continue. All foreground statuses come from that daemon, not persisted
history or embedded fallback. This uses a local synthetic Responses provider,
not credentialed inference or tool/approval parity.

Cleanup exits zero, all frontend processes exit, the daemon PID is absent and its
socket refuses connections. **The native stop command forces shutdown after its
60-second grace period** while synthetic ephemeral helper requests are still
held. This is successful owned-process cleanup, NOT graceful drain or executor
quiescence proof. Shell snapshot validation and local MCP session-delete errors
in daemon stderr are retained as unresolved diagnostics; no native fix is claimed.

A separate pinned native Unix-host adversary uses two independent WebSocket
connections. The external contract fence refuses cooperating stale writes, but a
raw peer resumes the same root and executes a write anyway. A detached setsid
child continues effects after exact turn completion AND host SIGKILL. Forged
quiescence cannot release reservations. Reopening the contract service recovers
committed identity without replay, while its global generation also rejects B's
old admission: scoped recovery remains missing, even though B's existing command
continues. These are confirmed counterexamples, never parity passes.

The containment adversary records BLOCKED with exit 2 on this macOS host: its
candidate requires Linux plus explicit delegated research cgroups. No process-group
kill, quiet file, caller boolean or dead daemon is substituted for containment.
No handoff/ownership transfer is safe to validate until executor authority exists.

All failed runtimes cleaned up their eight frontends and owned managed daemon;
PID absence, refused socket connections and provider closure are recorded.
Evidence: `engineering/evidence/shared-daemon-20261031/`. All 27 feasibility Python
tests pass. Native host-wide fencing, descendant quiescence, transfer/recovery,
credentialed inference and full zero-regression acceptance remain unproven.
Production Relay, ProfileSessionLock, installed binaries and credentials remain
untouched. Earlier preflight/capacity statements below are historical.


**Continuation from 45670f2:** shared runtime remains blocked below the 4 GiB
minimum; broader cache deletion awaits approval. No shared success is claimed.
The existing native package is intact. Held-response native cancellation and
cleanup regressions pass; the stronger shared proof harness is prepared but not
shared-validated. [CONTINUATION.md](CONTINUATION.md) contains the current checkpoint.

**2026-10-31 native role repair:** the pinned CLI rebuild passes. Eight rebuilt
embedded TUIs pass nine narrow gates, including foreground turn completion,
`/new`, `/fork`, exact picker resume and surviving neighbor frontends. The complete lab package builds; its shared daemon starts and eight TUIs bind
exact roots, but the disk-reserve guard stops the suite. Full shared validation
remains disk-blocked; the harness now passes an actual SIGTERM cleanup check.
[CONTINUATION.md](CONTINUATION.md) records the current result. Earlier failures
below remain historical evidence. No production change or full-parity claim.

**First rebuilt native run:** cache cleanup unblocked full CLI linking. Eight
embedded source-built TUIs report initial selections, but /new fails on the
prototype's background-root/cancellation handling. NATIVE_BUILD.md retains this
failure and next engineering obligations; full parity remains unproven.

**Pinned build follow-up:** [NATIVE_BUILD.md](NATIVE_BUILD.md) records an actual
successful native client/TUI type-check with Rust 1.95.0. CLI linking reached
the disk reserve; native TUI runtime/permission and enforcement gates remain
unverified. Production profile exclusivity remains unchanged.

**Phase 2 experimental update:** [FEASIBILITY.md](FEASIBILITY.md) contains two working competing
prototypes, real eight-controller native execution, crash/admission falsification, measurements,
and the native-source insertion/lab procedure. [DEFECTS.md](DEFECTS.md) independently characterizes
the historical-fork and cold database defects. Full parity remains unproven. The sections below
retain the phase 1 research record; phase 2 supersedes its "Next decision" work item.

Research date: 2026-10-30. Relay checkout: `7a64666` (production code unchanged).
Real executable tested: `codex-cli 0.166.0`, macOS 26.5.2, arm64. Relay's existing
rollout verification list is 0.155.0/0.156.0; these are different evidence domains.

## Decision

Same-home concurrency is feasible at the native storage/runtime level. It is **not yet
proven safe as a transparent Relay feature with the complete requested parity**.
The current profile lock is an observation-domain exclusion mechanism, not a necessary
credential/account restriction. Removing it would expose both event misattribution and
profile-wide stopping/recovery. We reproduced the observation failure using the unchanged
production watcher and independently exercised real Codex's per-thread writer exclusion.

The recommended long-term boundary is **a provider-supported native TUI/controller integration
with exact client-to-thread bindings, exclusive control admission, and durable per-execution-tree
quiescence**. Preserve the shared native backend and native permission/configuration resolution.
Do not start another writer merely to observe. Use its structured event stream, or a read-only
exact-thread observer after authoritative binding is established. A shared observer service is
not required for correctness and should not be added without measured benefit.

Hooks alone, disabling the shared daemon, and a replacement UI do not meet the complete target.
They were investigated rather than accepted as shortcuts. A narrow upstream integration is
preferable to a permanent Codex fork, but a version-pinned fork/wrapper is technically another
way to implement the same missing contract. Neither is implemented or validated here.

**No zero-regression claim is made.** This packet includes negative results, unresolved issues,
reproducible probes, and release gates. No production lock, active profile, real credential,
real project worktree, release, GitHub issue, or branch was modified.

## Evidence conventions and scope

- **Observed:** real executable or unchanged Relay code exercised in disposable environments.
- **Source:** source inspection at a recorded version/blob, not a live behavior assertion.
- **Historical:** issue/commit evidence from prior work; not rerun here.
- **Unverified:** a proposed property without adequate experimental evidence.
- **Blocked:** required test cannot run here or needs an absent provider capability.
- **Failed:** an experiment falsified the relevant claim; not necessarily a production regression.

The sandbox denies TCP and Unix-socket listener creation (`EPERM`) and shell DNS resolution.
Pseudo-terminals and stdio subprocesses work. GitHub's connected integration supplied source,
issue bodies, and discussions; it does not relax local process/network restrictions. No mock
HTTP inference server, shared-daemon socket, successful model response, authenticated quota read,
real exhaustion, live Claude transition, or Herdr UI workflow was exercised. These are explicitly
unverified, not silently replaced by passing mocks.

All executable probes construct a new HOME and CODEX_HOME and use a minimal environment without
authentication overrides. Their custom provider points only to unreachable `127.0.0.1:1`, with
`requires_openai_auth=false`. They neither load nor copy a real account's credentials. TUI hook
trust bypass is confined to a researcher-created hook in that disposable home. Experiments may
create failed/interrupted offline turns; they do not consume a real provider's quota.

## Root cause and lock lifecycle

`crates/relay-core/src/handoff/lock.rs::ProfileSessionLock` nonblockingly acquires an OS advisory
lock at `<CODEX_HOME>/.relay/session.lock`. The directory is private; the lock file symlink is
rejected. A temporary acquisition probe precedes the retained fd-lock guard. Guard Drop explicitly
unlocks. Its leaked backing allocation/file handle is retained; ordinary refusals avoid that leak,
but a narrow race can still leak an attempted allocation. A replacement frequently acquired lock
should use an owned RAII implementation instead of reproducing this successful-acquisition leak.

`commands/codex.rs::perform_codex_launch` acquires before `launch_new_thread`/`codex exec`, then
passes the guard into terminal supervision. `terminal_session.rs` otherwise acquires before the
interactive TUI, independently of event observation/version gates, and releases after terminal
supervision ends and the watcher stops. `CodexTargetLauncher::launch_and_verify` separately locks
before bootstrap and releases at the end of that call; later attach reacquires. That bootstrap/
attach interval is not one continuously held profile lock, although transactional session
reservation continues separately. None of these locks is an account quota lock.

`RolloutWatcher` knows the initial thread from Relay's launch/resume. It verifies filename/header
identity, tracks complete records and generation-aware offsets, and treats exhaustion as a wakeup
hint. Only `ordinaryUsageAllowed == false` from the authoritative usage adapter permits the quota
verdict. Its profile-domain rebinding rules are:

1. Exclude structurally identified subagents/ephemeral records.
2. Follow `forked_from_id == current_thread_id`, even without growth.
3. Otherwise follow a top-level candidate that grows beyond its watch-start baseline.

Rule 3 needs the assumption that only this TUI can cause another top-level thread's activity.
Two TUIs invalidate it even in unrelated projects. Rule 2 proves ancestry, not the requesting
client or current selection. Real Codex allowed server B to fork A while server A still held its
writer lock. Neither cwd, timestamps, only-one-new-file assumptions, argv, title text, nor
creation originator can replace missing client identity.

There is an information boundary: exchange A and B's client-side `/new` actions while preserving
the same global thread/rollout events. Without a client-scoped carrier, the profile observer has
the same evidence in two executions requiring different associations. More frequent scanning
cannot solve that indistinguishability.

## Historical decisions, now independently retrieved

See `evidence/relay-issue-{15,16,17,18}.md` and local commits. Issue 18's original body prohibited
rollout parsing, but subsequent incident-driven commits explicitly superseded that design. Do
not elevate the early issue text above current code or erase the failure that changed the choice.

| Change | Evidence and reason |
| --- | --- |
| `b15d68d` | Ownership became per Relay Session, permitting many conversations per project. |
| `b2320f5` | Private app-server + `--remote` broke permission passthrough: overrides were rejected/removed on resume. |
| `00893fd` | Shared-daemon lifecycle broadcasts lacked client attribution; observer exclusivity introduced. |
| `65dd11f` | macOS CI contradicted the prior unlock strategy; explicit guard Drop retained. |
| `8a6e693` | Refusing only a second observer did not stop a second TUI contaminating the first observer's domain. |
| `75ba067` | Real 2026-10-13 observer `thread/resume` raced the TUI and produced an active-writer conflict; observer removed. |
| `5c09bcd` | Fixed silent record skipping from bad initial/recovery offsets and partial lines. |
| `92fea8d` | Locked before bootstrap creation; profile-domain rebinding added. |
| `deeac2f` | Real 22,275-byte session header defeated the former 8 KiB discovery ceiling. |
| `6810a50` | Natural exhaustion on 0.156.0 validated event -> one authoritative read -> handoff with no periodic reads while healthy. |

Issue 18 records an earlier natural exhaustion whose timing was consistent with the periodic
timer: it was correctly NOT accepted as event-causality proof. Its final closure records 18.5
minutes of healthy zero-poll observation, followed by the event-originated read and handoff.
The new acceptance criteria preserve that evidentiary standard.

The live second-TUI refusal integration test remains ignored on Linux after documented CI hangs;
the trigger was not established. Do not convert this into either proof of Linux correctness or
proof that concurrency caused the hangs.

## Additional baseline problems found

1. **Profile-domain stopping:** `CodexSourceLiveness` regards any matching home process as active.
   `CodexSessionStopper::stop_and_verify` scans/stops matching home processes in addition to the
   recorded process; `stop_unrecorded_targets` uses the broad scan. The native session parameters
   are not used to scope those operations. Relaxing the lock could kill an unrelated session.
   Commit `5414fdd` expanded these checks after a real stop-verification failure; replacing them
   must retain that failure coverage rather than simply deleting the fallback.
2. **Historical fork misbinding:** the unchanged watcher adopted a pre-existing, unchanged fork
   of its current thread, with only one watcher and no post-attach activity. This is an existing
   attribution defect, not a concurrency implementation introduced by this research.
   `evidence/watcher-historical-fork.json` reproduces it. Exclusivity alone does not prove selection.
3. **Partial binding propagation:** `EventDrivenRuntime::tick` logs `ThreadTransitioned(id)` and
   requests reconciliation; it does not update the lease there. The scheduler's plan closure
   reads the existing lease. Observer continuity must not be mistaken for transactional update
   of resume/context-capture identity.
4. **Project scope:** watch state currently lives in the project diagnostic directory. Native-ID
   uniqueness in `SessionStore` is enforced within a project's registry. Neither supplies a
   machine-wide same-home/native-thread reservation across projects.
5. **Other shared files:** Codex skill and marker installation includes ordinary file writes;
   concurrent upgrades/installations need short-lived coordination and atomic publication.
   The provider-identity exhaustion ledger already has a separate lock and atomic writer; retain
   this existing protection rather than assuming it is another unprotected per-session file.
6. **Capture boundary:** the coordinator captures the state bundle before stopping the source.
   A new admission fence must establish a stable native context boundary while retaining source
   reservation. Do not move capture after releasing ownership or silently lose intervening turns.

## Actual Codex interfaces and experiments

Source manifest: `evidence/source-manifest.json`. Sources were fetched from `rust-v0.166.0` and
recorded by Git blob SHA. A future default-branch observation is not substituted for these files.

Native `LocalThreadStore` uses `WriterLockCoordinator` under `CODEX_HOME/thread-writer-locks`.
Two real stdio app-server processes started different threads under the same home, both in one
project and in different projects. A read-only foreign `thread/read` worked after persistence;
foreign `thread/resume` failed while the original writer lived and succeeded after its SIGKILL.
B's original thread remained readable. This is storage-writer exclusion, not proof that two
clients of one daemon have exclusive frontend control or that descendants have stopped.

The first experiment omitted turns and saw `no rollout found`: native persistence is lazy.
The revised probe starts then interrupts offline turns to materialize history. The failed initial
attempt is retained (`stdio-lazy-persistence-first-attempt.json`), not counted as a writer-lock test.

Real B also forked live A successfully. In these ordinary-root fork experiments, the fork had a
new `id` AND a new root `sessionId`, plus `forkedFromId=A`. This corrects the earlier overly broad
inference that ordinary TUI forks necessarily share the root session ID. Source separately defines
`Session::session_id()` as runtime-family identity; descendant hooks can share it. Do not use it
as a universally interchangeable thread ID. Hook provenance/timing failures stand independently
of this distinction.

The generated protocol offers thread/turn operations and metadata reads, but no external API
established here combines (a) exact selected TUI identity, (b) exclusive controller admission,
and (c) durable execution-tree quiescence. `thread/unsubscribe` detaches a subscriber; documented
unload delay and continuing work make it a poor shutdown proof. `thread/loaded/list` does not
identify which TUI selected a thread. Attachment ownership describes stored attachments, not
frontend control. `originator` explicitly describes creation origin, not the current executor.

The native core has `suspend_turn_and_shutdown`. Its own contract says descendant cleanup is not
awaited and queued input/request waiters may be discarded. It is not a proven external App Server
quiescence protocol. Existing native internals are promising implementation building blocks, not
evidence that the required public contract is already present.

### Real TUI and hook results

Two `--no-daemon` TUIs ran concurrently in disposable PTYs. A exercised initial prompt, `/new`,
another prompt, `/fork`, another prompt, and saved-chat resume through the native picker. B
continued its independent offline turn. The successful run's structured hooks and metadata are
in `tui-embedded-transitions.json`; synthetic prompts only, no real transcript content.

Before the initial prompt: zero hooks. After `/new` and before the next prompt: no new start hook.
After `/fork` and before a prompt: the fork existed on disk but its start hook had not fired.
After the next prompt: `SessionStart(source=fork)` and prompt hooks appeared. Resume likewise
emitted its start hook with the next prompt. An earlier thread's delayed `SessionEnd` also arrived
after the TUI had moved elsewhere. Thus hook end is not TUI exit, and hook start is not every
selection transition. Embedded hooks carried distinct A/B launch markers correctly; this does
not establish shared-daemon provenance.

The first PTY harness accidentally coalesced pasted text and Enter, so it did not submit its
intended prompt. We corrected input separation and reran. One simultaneous fresh-home TUI launch
also failed a SQLite migration (`no such table: stage1_outputs`). Serially preparing the database
separated this bootstrap issue from steady-state tests. A separate 10-trial concurrent first-use
stdio probe observed 7 initialization failures among 20 endpoints. This is an observed 0.166.0
fresh-home risk in this sandbox, not a finding that mature profiles always fail or that Relay
caused it. Raw structured outcomes are in `bootstrap-race.json`.

Upstream reports independently support the boundaries, but are reports rather than our live proof:

- [#48500](https://github.com/openai/codex/issues/48500), [#44902](https://github.com/openai/codex/issues/44902): hooks inherit the first daemon client's environment, causing wrong-pane routing.
- [#48880](https://github.com/openai/codex/issues/48880): exact displayed-thread identity is requested separately from hook provenance. Closed status does not establish implementation; retrieved discussion supplied no implementation proof.
- [#51360](https://github.com/openai/codex/issues/51360): deferred resume start hook and delayed old-thread end.
- [#43253](https://github.com/openai/codex/pull/43253): read-only TUI fallback on writer conflict; snapshot viewing does not confer ownership.
- [#44449](https://github.com/openai/codex/issues/44449): an alive daemon can retain idle writer locks; absence of current phone activity is not writer release.
- [#46802](https://github.com/openai/codex/issues/46802): requests host-enforced admission fencing and scheduled-work drain for handoff.

### Native command interruption and crash results

In one real stdio server, A and B each ran `thread/shellCommand` with a bounded heartbeat command.
`turn/interrupt(A, exact_turn_id)` stopped A's heartbeat; B continued. Killing the server then left
B's heartbeat running. Each heartbeat self-expired after 12 seconds; no guessed descendant PID
or user process was killed. See `shell-interruption-and-crash.json`.

This proves a narrow independent cancellation success and falsifies backend-PID-death as a general
quiescence proof. It does not prove cancellation of all model tools, subagents, queues, goals,
remote executors, detached shell descendants, or external side effects. Native persistence writer
release, no-more-model-work admission, and no-more-workspace-mutation are separate properties.

## Architectures challenged

| Architecture | What it solves | Why it is insufficient or conditional | Cost/maintenance |
| --- | --- | --- | --- |
| Remove/narrow profile lock alone | Launches coexist | Wrong rollout adoption; broad stop/recovery; cross-project duplicate native ownership | Small diff, unsafe |
| Fixed initial-thread watcher + native locks | Initial attribution; native writer conflicts | TUI changes invisibly; healthy stale observer misses current exhaustion | Cheap, incomplete |
| Shared profile watcher/demultiplexer | Avoids repeated tree scans | Same missing TUI association; broker does not create evidence; adds lifecycle/failure domain | Moderate, no identity solution |
| Hook-only integration | Structured lifecycle and turn callbacks | Deferred starts, delayed ends, shared-daemon provenance, non-universal admission, disabled/untrusted hooks | Low-medium; not full parity |
| Per-launch config/instruction/creation marker | Labels some creation events | Persisted creation marker is not current-client identity after arbitrary resume; instructions alter prompts; custom config may change backend selection | Fragile and incomplete |
| PID/argv/open-file/cwd/title/PTY/log inference | Sometimes useful diagnostics | Shared daemon and already-loaded navigation defeat attribution; display parsing cannot authorize ownership | Brittle, rejected |
| Inject `--no-daemon` with hooks | Embedded ancestry and per-runtime environment, normal local permission mode | Does not fix deferred hooks or descendant crash cleanup; changes shared command center/remote behavior; duplicates runtime resources | Credible partial design, fails full requirements |
| Isolated runtime, same home, custom local socket | Backend lifetime can be separate | Public custom endpoint uses remote semantics; no proven local-mode custom routing; isolation alone cannot fence arbitrary native transitions | Medium-high; needs upstream support |
| Protocol intermediary on actual TUI connection | Correlates requests/responses without observer resume; can gate known transitions | No established transparent local-TUI insertion point; remote permission failure remains; reconnect/multiclient/background effects require proof; auth frames expand security boundary | High; viable only after explicit transport/permission work |
| Relay sole-writer App Server UI | Explicit identity/control; structured events | Replaces native TUI behavior unless native frontend can attach safely; still needs host quiescence/containment | Feasible different product, high parity burden |
| Embed/version-pin native TUI and host | Can add exact callbacks and controller fences within native path | Private Rust internals, substantial dependency/release coupling; quiescence still needs real implementation | Technically credible fallback, largest maintenance burden |
| Per-runtime home/store overlays or credential sharing | Physical observation separation | Changes literal home/storage semantics, authentication boundary, config/history behavior; filesystem namespace support differs by OS | Not the requested capability |
| OS tracing/namespace containment | Can expose connections or contain descendants | Privilege/platform requirements, races, escaped jobs; macOS/Linux mismatch; not a supported portable identity interface | High complexity; supplementary only |
| Native provider control/observation contract | Exact TUI identity, pre-write reservations, structured events, scoped quiescence while preserving native UI | Requires upstream/native integration and full validation; display-only event alone is insufficient | Strongest long-term design |

An RPC intermediary is distinct from the rejected *second observer writer*: it forwards the
actual writer's connection and never issues its own resume just to subscribe. That resolves the
specific old observer race architecturally, but not the remote permission/configuration problem,
credential-routing boundary, or native process cleanup. Do not call it a ready solution.

## Recommended native integration contract

Keep profile-owned authentication and one shared backend. Add a private local control capability
without exposing tokens or introducing a network listener in Relay:

1. **Exact client identity:** supervisor instance/generation and endpoint bound to verified local
   frontend origin. Peer identity and launch registration authenticate the association. No model
   instruction or prompt text grants authority. Persist identities, not secret capability values.
2. **Explicit bindings:** report primary controllable thread, displayed thread, execution-tree
   root, and controller separately. Viewing a subagent/read-only snapshot must not transfer root
   ownership. Carry full IDs, ordered sequence/generation, and successful/failed operation result.
3. **Admission:** before a managed frontend can write a newly selected native root, acquire its
   global reservation. Existing native writer locks remain in force. Same-daemon joins must not
   bypass Relay controller exclusivity. Refusal preserves the old binding; no second writer or
   unverified target is left running. Crash recovery queries a durable operation token.
4. **Events:** authoritative, scoped structured events with bounded replay/cursor or equivalent
   reconciliation. Subscriptions do not start/resume a thread. New background roots, subagents,
   user-navigation transitions, and scheduled activity are distinguishable.
5. **Quiesce:** epoch-bound admission fence and completion receipt for a specific owned execution
   tree: active turns, queued inputs, goals, subagents, approvals, background terminals, scheduled
   deliveries, and tracked tools. Flush history before capture/commit. Acknowledged fence survives
   observer disconnect; retries and rollback are idempotent. Unknown -> no ownership transfer.
6. **Crash containment:** an execution-owner registry/containment boundary records work before it
   can mutate the workspace. PID reuse and reparenting cannot turn an unrecorded surviving child
   into a successful stop proof. Graceful cancellation plus exact runtime receipts is preferable
   to process scans. OS-specific containment is needed if claiming revocation of independently
   daemonized programs. Define that boundary explicitly; existing Relay PID checks do not prove
   arbitrary externally detached descendants stopped either.
7. **Permissions:** reuse native local config/managed-policy resolution. Do not strip permission
   arguments or translate remote settings by guesswork. The `Remote` resume stripping is still
   present in inspected 0.166.0 code; localhost is not automatically equivalent to embedded mode.

An upstream TUI lifecycle FD/callback plus host-side control/fence API could realize this without
a new generic broker. Its shape is a proposed contract, not an existing Codex flag/API. A
display-selection notification solves observation only; the pre-write admission and quiescence
parts are independently necessary for the full requirement.

### Conversation navigation policy must be explicit

A frontend, a displayed transcript, a Relay Session, and a native execution tree are not the same
thing. On `/new` or fork, record a deliberate native binding transition under reservation; on
resume of a thread already mapped to a Relay Session, resolve that existing mapping atomically
across projects rather than creating a second lease. Never turn a fork into native same-thread
continuation. If prior roots retain background work, keep their ownership/observation until the
provider reports them drained, or represent them as separately managed sessions. A new foreground
selection cannot simply erase their supervision. Whether the native version retains such work
on each navigation path remains a required integration test, not an assumption made here.

## Required Relay implementation and migration

1. Land a design/contract and capability negotiation before any production lock change. Establish
   a global lock order across native reservations, session registry, session orchestration, and
   short profile maintenance operations. Never hold a global registry lock during provider I/O.
2. Introduce native-conversation keys `(provider, canonical home identity, native thread ID)`
   independent of project. Validate aliases/symlinks and keep a stable controller epoch. Do not
   key by cwd, profile display name, PID alone, or provider session-tree ID alone.
3. Add durable execution-owner and native-binding records, plus intended creation tokens before
   bootstrap. A target that exists before PID/native ID recording must remain discoverable by
   its transaction token; otherwise recovery stops, rather than killing the entire home.
4. Transactionally propagate binding to lease, observer, context capture, resume target, control
   endpoints, history, and diagnostics. Include the generation in asynchronous callbacks so a
   delayed previous-TUI event cannot overwrite current state.
5. Replace `CodexSourceLiveness`/`CodexSessionStopper` broad scans with the scoped host contract.
   Keep source reservation through quiescence, snapshot, target verification, and commit. Journal
   fence receipts and target recovery identity. Do not create a profile unlock/relock substitution
   for a native-conversation reservation. Fail closed on an unknown stop.
6. Preserve STATE_CONTINUATION as a new native session for every Codex/cross-provider transfer;
   preserve same-profile native resume. Capture the exact fenced source, bounded recent context,
   working state, and deterministic repo/git facts, without claiming provider cache transfer.
7. Move observation offsets to session/binding-generation state. Metadata validation must use
   concrete thread identity, with explicit handling of session-family ID differences. Retain
   partial-line, inode/generation, truncation, old-history, and missed-notification protections.
8. Keep quota wakeups scoped; authoritative usage remains profile/account information, never
   evidence of which conversation emitted an event. Preserve UNKNOWN behavior, target health
   checks, single-flight evaluation, cooldowns, caps, and the existing locked exhaustion ledger.
   Coalesce overlapping reads only with explicit freshness and the same authority semantics.
9. Make profile skill maintenance atomic and short-lived. Do not duplicate every profile-wide
   scan in every supervisor. Prefer the native scoped event stream; exact-file observation is
   an alternative only after binding and coverage are proven.
10. Use an exclusive legacy/shared compatibility gate: old profile-domain observers and new
    concurrent sessions must not coexist. A retained shared lock on the existing lock namespace
    could coordinate upgraded participants with legacy exclusive holders, but its acquisition,
    process lifetime, aliases, and older bootstrap behavior must be tested before adoption.
    Never remove/replace a live lock inode. Do not hot-migrate an active conversation.
11. Reconcile dormant records and pending journals without moving ownership automatically.
    Upgrade new launches only after active old sessions finish or explicitly resume into the
    new contract. Unsupported versions retain existing safe behavior and visibly explain the
    unsupported concurrency capability; this is a release gate, not an accepted degraded mode.
12. Publish only after the full acceptance matrix passes. Concurrency is then the normal default
    for supported native versions, with no user-created aliases/homes, disabled supervision, or
    special Herdr mode. A downgrade must refuse incompatible pending state rather than guess.

## Performance: measurements and limits

Real production `RolloutWatcher`, release build; synthetic valid headers; idle observation window
3 seconds plus 0.4-second settle, setup and cleanup included in CPU totals; median of 3 runs.
Targeted prototype uses exact-path notify + incremental reads, one thread per watcher. It omits
durable recovery, binding, and fencing, so **these are cost experiments, not parity validation**.

| Historical files | Watchers | Production CPU seconds | Targeted CPU seconds | Production attach ms | Targeted attach ms |
| --- | --- | --- | --- | --- | --- |
| 0 | 1 | 0.0248 | 0.0232 | 5.21 | 6.53 |
| 0 | 2 | 0.0404 | 0.0244 | 7.39 | 6.56 |
| 0 | 8 | 0.1311 | 0.0338 | 13.07 | 8.38 |
| 1000 | 1 | 0.5150 | 0.1637 | 14.01 | 2.70 |
| 1000 | 2 | 0.8899 | 0.1637 | 26.74 | 2.82 |
| 1000 | 8 | 3.0488 | 0.1862 | 92.68 | 4.31 |

Creating/deleting the 1000 fixtures contributes to both CPU totals; do not describe their entire
difference as steady-state CPU or subtract independent medians as an exact accounting identity.
`watcher-fixture-baseline.json` separately measures zero readers. Targeted process peak RSS was
approximately 7.5-8.3 MB (macOS `wait4` bytes); this is a tiny research executable, not full Codex
runtime RSS. No added provider polling exists in either watcher experiment.

These measurements reject naive duplication of profile-tree scans and show exact-file costs are
promising. They do not measure the proposed upstream integration, shared-daemon overhead, full
Herdr launch latency, inference latency, runtime-isolation memory, or production p95 exhaustion
latency. Set performance budgets before implementation using repeated native runs with 1/2/8/32
sessions and 0/1k/10k histories, notification bursts, long records, and missed notifications. Count
actual provider reads, not inferred timer settings. Compare warm/cold launch, idle CPU/wakeups,
whole-runtime RSS/FDs/threads, event-to-read and event-to-handoff p50/p95/p99, shutdown latency,
and crash recovery. No finite test can prove future-version performance or an absolute theorem
of zero regressions; the supported version/OS envelope must be explicit.

## Reproduction

From the repository root, with Python 3 and the real Codex CLI on PATH:

```sh
python3 docs/investigations/codex-concurrency/experiments/stdio_probe.py
python3 docs/investigations/codex-concurrency/experiments/stdio_probe.py --different-projects
python3 docs/investigations/codex-concurrency/experiments/tui_probe.py
python3 docs/investigations/codex-concurrency/experiments/shell_probe.py
python3 docs/investigations/codex-concurrency/experiments/bootstrap_race.py
cargo build --offline --release --manifest-path docs/investigations/codex-concurrency/experiments/watcher-probe/Cargo.toml --target-dir /private/tmp/relay-codex-concurrency/target
/private/tmp/relay-codex-concurrency/target/release/relay-concurrency-watcher-probe 2 0 1
/private/tmp/relay-codex-concurrency/target/release/relay-concurrency-watcher-probe 1 0 1 idle production historical-fork
/private/tmp/relay-codex-concurrency/target/release/relay-concurrency-watcher-probe 2 0 1 inject targeted
python3 docs/investigations/codex-concurrency/experiments/measure_watchers.py production
python3 docs/investigations/codex-concurrency/experiments/measure_watchers.py targeted
```

The scripts retain fresh temporary roots and print their locations. They do not connect to the
user's daemon. `shell_probe` takes extra time to let bounded owned commands expire even after a
crash. Fresh-home initialization is intentionally raced only in `bootstrap_race`; the final TUI
probe warms the home first. First-ever offline Cargo builds need cached dependencies. Native
startup, keyboard handling, and hook behavior are version-specific; a script exit alone is not
proof all intended actions succeeded. Validate recorded IDs, responses, events, and terminal
diagnostics. Read `ACCEPTANCE.md` before turning a probe result into a safety claim.

## Next decision

The root cause is defensible, the cheap workarounds are disproven, and the long-term missing
contract is concrete. The next engineering stage should prototype that native integration with
a pinned Codex version in a listener-capable disposable lab, while retaining the current lock.
Do not implement a production hook-only concurrency mode as an interim success. If upstream
declines the contract, explicitly choose between maintaining a thin native integration/fork and
changing the product requirements; no observer heuristic meets the stated target.
