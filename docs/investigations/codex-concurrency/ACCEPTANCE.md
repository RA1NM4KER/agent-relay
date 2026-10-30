# Acceptance matrix and proof obligations

This is a release gate, not a list of assertions already satisfied. See README.md for the exact
evidence boundary. **There is no implemented concurrent Relay design to certify.**

Phase 2 now includes real-backend controller and native-bridge-core prototypes, not a production
implementation. The evidence updates below supersede earlier evidence cells only to the stated
scope. Native TUI/host-wide enforcement and full handoff parity remain uncertified.

Status legend: O = observed narrow result; S = source established; H = historical evidence;
F = falsified candidate/baseline claim; U = unverified; B = blocked in this environment or missing
an external native capability. An O/H/S does not imply the proposed implementation passes.

## Required invariants

1. At most one Relay controller epoch has write authority for any concrete native conversation,
   regardless of project, home alias, frontend, or daemon connection.
2. Every automatic action names the exact Relay Session, native binding, controller epoch, and
   evidence generation. Stale evidence cannot mutate the successor.
3. Failure/ambiguity never becomes an exhaustion verdict, successful stop, verified target,
   guessed binding, or permission grant.
4. No target becomes the committed owner until the source is quiescent, context is correct,
   target identity/health is verified, and the transaction can recover after each durable step.
5. A's observation, switch, shutdown, crash, recovery, or upgrade cannot stop, adopt, or release B.
6. Credential ownership remains with Codex/Claude; no copied profiles, token routing, quota
   pooling, or shell environment dump is introduced to implement concurrency.
7. All supported native frontend settings and interaction semantics remain intact. Observation
   is passive except explicit ownership/switch operations; unknown coverage is visible.

## Scenario matrix

| ID | Scenario and required outcome | Verification method / decisive oracle | Evidence now |
| --- | --- | --- | --- |
| C01 | Two Relay Codex launches, same home/project, distinct roots, both supervised | Real native TUIs; independent structured IDs, leases, active epochs and events; no guessed mapping | O native embedded TUIs; U end-to-end Relay concurrency |
| C02 | Same home, different projects | Same as C01; no global profile refusal and no cross-project adoption | O native stdio runs; U Relay |
| C03 | 8/32 panes and simultaneous startup | Native client registry, one controller per thread, bounded startup/resource use | U; B live Herdr in this environment |
| C04 | Concurrent first-ever home initialization | All launches succeed, or one bounded maintenance gate serializes provider migration before user work | F naive 0.166.0 starts: 7/20 endpoints failed; fix U |
| C05 | Different native threads run simultaneously | Real per-thread turns/actions continue; A interrupt leaves B progress unchanged | O real shell actions in one server; U model/subagents |
| C06 | Same native thread, different projects | Atomic global reservation permits one controller; no duplicate lease or unverified frontend | S project-only Relay registry inadequate; O native separate-process writer conflict; U shared-daemon controller fence |
| C07 | Same thread, two connections to one daemon | Native join cannot grant a second Relay controller; view-only mode never writes | S resume can rejoin; B multiconnection live test here |
| C08 | Home aliases/symlinks/profile rename | Same underlying native ownership domain; no bypass or changed credential resolution | U; deterministic inode/path/rename adversarial tests |
| C09 | Global event stream receives unrelated top-level work | No rebind or exhaustion attribution to another supervisor | F unchanged profile watcher reproduced; U replacement |
| C10 | Another process forks the current thread | Ancestry alone never changes selected/current binding | O native B forked A; F ancestry-only attribution |
| C11 | An unchanged historical fork already exists | Initial observer remains on the actual current thread | F unchanged watcher adopted old fork, even one watcher |
| C12 | `/new` before first prompt | Exact new binding and reservation before first write; continuous supervision | O deferred hooks; B supported pre-write native carrier |
| C13 | `/fork` before first prompt | New concrete native ID, lineage recorded, no false same-thread continuation | O native fork/header and deferred hooks; U Relay binding transaction |
| C14 | Resume saved chat in TUI | Exact target selected without cwd/mtime inference; correct existing Relay mapping across projects | O embedded picker + next-prompt resume hook; U pre-write global resolution |
| C15 | Switch X -> Y -> X, already loaded, no prompt | Ordered client-scoped reports; stale reports rejected; no coverage gap | S native TUI owns local selection; B external selection API |
| C16 | Display subagent/read-only snapshot | Does not transfer primary controller or treat a child quota event as foreground selection | H native read-only support; U end-to-end root/display distinction |
| C17 | Old `SessionEnd` arrives after new selection | Does not terminate/clear current frontend/binding | O delayed old end observed; U replacement logic |
| C18 | Hooks missing, disabled, untrusted, altered, timed out, or out of order | No silent loss of safety; authoritative native integration remains or launch refuses capability | S hooks cannot be sole contract; U implemented behavior |
| Q01 | Exact session naturally exhausts quota | Scoped structured event -> exactly one fresh authoritative read -> its transaction | H 0.156.0 production proof; B new concurrent native quota test |
| Q02 | Two sessions exhaust together | Both independently verified/handed off; neither moves/stops the other | U; mock deterministic burst plus real account exhaustion |
| Q03 | Healthy observation idle for 30+ minutes | Zero periodic provider quota reads; measured process/RPC trace | H 18.5 minutes old design; U proposed design |
| Q04 | Numeric usage near 100%, generic errors, auth rejection, ambiguous read | No unsupported exhaustion inference; UNKNOWN remains fail closed | S existing adapter authority; U full concurrent regression run |
| Q05 | Event, reconnect, and scheduled read overlap | Single-flight, honest trigger trace; no loss of pending work or unbounded burst | H existing scheduler tests; U integration with new event generations |
| Q06 | Missing OS notification, partial record, long header, truncation/replacement | Catch up safely or mark coverage unknown; never claim healthy stale file | H existing regressions; U full new observer (cost prototype omits them) |
| Q07 | Observer disconnect/panic, native reconnect/daemon restart | Generation change detected; one authoritative reconciliation before healthy; no competing resume | S required native contract; U/B live multiclient restart |
| Q08 | Shared exhaustion ledger and target health | Concurrent writes remain locked/atomic; stale cache never authorizes unhealthy target | S existing global ledger lock; U multiple concurrent transactions |
| H01 | Concurrent handoffs to same healthy profile | Independent verified native roots, distinct leases/journals, no profile exclusion | U; fault-injected fake providers and real native clients |
| H02 | Source cannot be proven stopped | Retain reservation/recovery-required; no target commit; B untouched | S current port requirement; U scoped implementation |
| H03 | User input/steering races context capture | Admission fence fixes exact accepted boundary; no lost or duplicated accepted input | S pre-stop capture currently not itself a fence; B full public barrier |
| H04 | Queued turn/goal/scheduled task arrives during handoff | Drain or durable reroute with idempotent readback; retired epoch cannot execute | B public host-wide fence/drain not established |
| H05 | Claude -> Codex, Codex -> Claude, Codex -> different Codex home | Correct STATE_CONTINUATION source/context and new target identity, bounded notes | H existing product paths; U concurrent regression/integration |
| H06 | Same-profile native resume | Exact same thread, correct permission/configuration and lease owner | O native thread resume after writer death; U Relay migration path |
| H07 | Cross-provider transfer after `/new`/fork/resume | Bundle/excerpt comes from fenced current thread, never initial stale lease or neighbor | S watcher transition does not itself rebind lease; U implementation |
| H08 | Failure after source fence and before target success | Correct journal state; idempotent rollback/recovery without two active owners | U new epochs/fence receipt recovery; existing coordinator reused only after audit |
| R01 | TUI exits; unrelated TUI continues | Source execution authority properly detached/stopped as specified; neighbor untouched | O embedded concurrent TUIs; U shared-daemon runtime proof |
| R02 | SIGKILL supervisor/TUI/backend at each stage | Global reservation does not falsely certify descendants gone; recovery identifies exact runtime | F backend death implied quiescence; O orphan heartbeat survived |
| R03 | Active command interrupted with exact turn ID | Targeted work stops; unrelated thread work continues | O native `thread/shellCommand`; U ordinary tools, MCP, remote executors |
| R04 | Subagent and background terminal active during stop | Owned tree drain complete, other roots unaffected, no future mutation | U/B successful inference/subagents and native tree receipt |
| R05 | PID reused, fingerprint unavailable, reparented descendant | Never signal unrelated process; ambiguity blocks transfer | S ProcessIdentity intent; U durable runtime registry/containment |
| R06 | Crash between spawn and identity recording | Recovery resolves by durable creation token; no home-wide process kill | F broad old recovery unsuitable; U target-intent protocol |
| R07 | Stale offset, lease, lock file, transaction or epoch | No guessed ownership; real advisory lock/controller state and journal reconcile | H current lock/offset tests; U new global reservation |
| R08 | Two recovery commands race | Single transaction owner; idempotent native receipts; no duplicate target | U deterministic interleaving tests + live runtime |
| R09 | Filesystem I/O failure/disk full/native DB busy | Durable failure visible; old ownership retained or recovery-required | U fault injection; O fresh-home migration errors retained |
| U01 | Approval/sandbox/managed settings and passthrough flags | Byte/effective-policy equivalence with ordinary local Codex; no extra grants or stripping | H historical remote failure; S persists in 0.166.0; U proposed integration |
| U02 | Worktrees, add-dir, shell/MCP environment, trust | No changed root/credentials or lost per-client settings | U native parity corpus required |
| U03 | Shared daemon `agents`/remote workflows/native UI | Not disabled by silently injecting `--no-daemon`; native key/picker behavior preserved | F forced embedded mode fails full feature parity |
| U04 | Herdr pane identity and commands | Each pane retains its exact Relay/native binding; closing/switching A does not relabel B | U/B real Herdr integration |
| U05 | CLI JSON, titles, history, control skill, status/doctor | Existing contracts and per-session resolution preserved under concurrent writes | U; locked atomic skill installation required |
| S01 | Authentication/profile isolation | No auth file reads/copies, no raw environment dump or token/proxy log; provider resolves correct home | O all experiments isolated; U proposed bridge security review |
| S02 | Malicious/stale injected lifecycle report | Endpoint/peer/generation verified, replay rejected; prompt/skill cannot grant ownership | U private-channel negative tests |
| S03 | Same source shared worktree with unrelated agent edits | No workspace-wide ownership claim, destructive git mutation or unrelated stop; honest deterministic snapshot boundary | U; concurrent repo mutation/checkpoint tests |
| P01 | Idle 1/2/8/32 sessions, growing history | Budget CPU/RSS/FDs/wakeups; no O(sessions * all-history) recurring scan | O 1/2/8 synthetic current vs targeted; U native integration |
| P02 | Detection/handoff latency under burst/load | Record event, read, fence and commit monotonic times; p50/p95/p99; bounded queue | U proposed design latency; no claim from sleep-based probe |
| P03 | Large header/history, noisy neighbor, missed notifications | Bounded buffers, recovery correctness, no neighbor-induced starvation | H old offset/header tests; U new adapter stress tests |
| V01 | Unsupported/changed native version | Explicit capability/version refusal; no guessed IDs, stale parser coverage or silent supervision downgrade | U negotiated native capabilities and migration policy |
| V02 | Legacy Relay observer and new clients overlap | Compatibility gate prevents coexistence; existing lock inode never replaced | U mixed-binary multiprocess tests |
| V03 | Upgrade/downgrade with active sessions and recovery journals | Active old sessions unchanged; pending state remains recoverable or downgrade refuses clearly | U migration/crash compatibility suite |

## Proving the implementation rather than the harness

Use three independent test layers, with each row mapped to at least one integration layer:

1. Deterministic provider-neutral state-machine/fault-injection tests. Enumerate crash boundaries
   before/after intent persistence, client admission, native creation, process recording, binding
   publication, quiescence acceptance/receipt, history snapshot, target verification, lease commit,
   and source retirement. Assert reservations and journal state after every prefix. Model stale
   epoch events, cancellation of the caller, delayed native acknowledgements, and duplicate retry.
2. Real pinned Codex with a local deterministic mock inference backend. Exercise actual native
   TUI/App Server parsing, permission enforcement, commands, subagents, queues/goals, tool process
   ancestry and graceful/crash teardown. Drive native TUIs in PTYs or Herdr, but derive safety
   oracles from structured identities/receipts and controlled file mutation, not screen parsing.
   The mock must emit real structured usage-limit failures and track exact request/read counts.
3. Authenticated isolated-profile dogfood on supported macOS/Linux versions. Provider performs
   its own login; Relay never acquires credentials. Observe a natural exhaustion from each of two
   simultaneous sessions and real Claude/Codex transitions. Verify native permission equivalence
   and task continuation, not merely READY bootstrap success. Do not deliberately exhaust active
   user accounts. Obtain isolated authenticated test accounts through normal provider-owned login
   if this stage is authorized and available; none were used in this research.

Passing unit tests cannot replace native tests, and a successful mock model cannot establish real
provider quota semantics. Conversely, one natural exhaustion cannot establish concurrent recovery
or attribution. Preserve raw bounded structured evidence, versions, event/operation IDs, scripts,
negative trials and all ignored/skipped tests. Measure resource distributions without rerunning
until a desirable result appears.

## Performance acceptance before rollout

Record ordinary native Codex, existing Relay exclusive mode, and proposed concurrent Relay under
identical 1/2/8/32-session workloads. Define numerical budgets before selecting results. Native
integration should add no periodic usage polling while coverage is healthy and no recursive scan
per supervisor per 300 ms. Distinguish native backend cost from added Relay cost, and startup from
steady-state work. Measure CPU seconds, whole-runtime RSS, FDs, threads, wakeups, provider request
count, event-to-read, fence, handoff and recovery latency with confidence intervals or repeated
samples. Include notification storms and a stalled/noisy neighbor. The targeted cost prototype
does not satisfy this gate.

## Current guarantee-by-guarantee assessment

| Guarantee | What this investigation establishes | Certification of concurrent Relay |
| --- | --- | --- |
| Same-profile independent native conversations | Real storage/runtime coexistence and independent shell interrupt | Unverified |
| One active native persistence writer | Real independent-process conflict/release behavior | Partial; frontend/controller and process-work authority remain unverified |
| Exact per-session attribution | Current profile-domain assumption disproven; hooks insufficient | Blocked on authoritative native integration |
| Automatic event-driven handoff | Historical exclusive-mode proof retained; no new quota evidence | Unverified |
| `/new`/fork/resume continuity | Real native lifecycle/deferred hook behavior captured | Unverified; full binding/reservation transaction absent |
| Scoped stopping/crash safety | Independent native interrupt works; backend-death proof falsified | Blocked on scoped tree control/containment proof |
| Transactional recovery | Existing journal architecture identified; native target discovery gap remains | Unverified |
| Cross-provider correctness | Current transfer classes preserved in recommended design | Unverified under concurrency |
| Native TUI/permissions/Herdr | Forced remote/embedded substitutions have known parity risks | Unverified; no accepted shortcut |
| Credentials/security | Disposable experiments used no real credentials; new bridge has explicit boundary | Unverified until implementation/security review |
| Performance | Duplicated profile scanning scales poorly; exact-file cost promising | Unverified for full design |
| Future compatibility | Pinning and capability gates specified | Unverified; cannot guarantee arbitrary future provider versions |

Release requires all required rows passing within the declared supported envelope. An unresolved
row is not a reduced-functionality success; it blocks the first-class concurrency claim.

## Phase 2 experimental evidence update

See FEASIBILITY.md, DEFECTS.md and feasibility/evidence/. O below is intentionally scoped; it is
not a replacement for native-TUI, shared-daemon, model/subagent or authenticated handoff gates.

| Matrix IDs | New evidence | Remaining release obligation |
| --- | --- | --- |
| C01–C03, C05 | O eight logical controllers share a real backend/home, two projects, and simultaneous native commands | Eight rebuilt native TUIs and eight actual Herdr panes remain B/U |
| C04 | O independent comparison: cold 3/16 failed, warm 0/16, short bootstrap gate 0/16; quick_check ok | Interrupted migrations, upgrades, native fix and 8/32 starts remain U |
| C06–C08 | O pre-resume foreign refusal, eight rounds of eight independent registry contenders, canonical home alias equality, durable stale-epoch refusal | Native host controller epochs, authenticated peers, independent connections and alternate state roots remain U |
| C09–C11 | O actual response-bound identity avoids profile scanning; historical fork defect reproduced 8/8 | Host/TUI authoritative selection and separate production historical-fork correction remain U |
| C12–C15 | O real native fork/new/resume RPC equivalents with exact responses; source insertion core compiled; four source insertion files SHA-checked | Full patched TUI build/run, already-loaded selection, every reconnect path and pre-host-write admission remain B/U |
| C16–C18 | O display reporting alone does not grant ownership; unsupported mutations fail before native forwarding | Child/read-only frontend UX, full RPC classification, approval paths and secure replay-proof channel remain U |
| Q01–Q08 | O no account/quota polling in issued native method lists; 42 existing scheduler tests pass | Natural quota exhaustion, native quota wakeup/authority, simultaneous automatic handoffs, reconnect/replay remain B/U |
| H01–H08 | O full transfer refuses a native turn-drain-only receipt; lost native creation replies quarantine rather than guess | No general successful handoff, authoritative snapshot/fence boundary, cross-provider or full recovery has passed |
| R01–R05 | O exact native interruption stops A's parent while B–H continue; detached child survives turn completion; backend death leaves work | F process/turn death as quiescence; native executor-tree or platform containment receipt remains required |
| R06–R09 | O crash after intent, crash after actual native creation before binding, reopen and lost reply; neighbor existing work continues | Unknown creation currently blocks new adoption; host token recovery needed to avoid availability loss |
| U01–U05 | S source insertion forwards original typed requests and changes no permission params or remote flags; core runs against native backend | Effective native permission equivalence, shared-daemon functionality, CLI and Herdr UX remain U |
| S01–S03 | O disposable accounts/homes only; authentication RPC values do not cross callback; no profile process scan | Current callback owner IDs are not secure peer authentication; malicious/replayed source messages and worktree concurrency remain U |
| P01–P03 | O matched 3x eight-start comparisons with native diagnostics, CPU and partial RSS; no usage polling | Full TUI/executor process-tree resources, native quota/handoff latency, long-run coverage and optimized IPC remain U |
| V01–V03 | O patch rejects nonmatching upstream blobs; unclassified native RPC refuses | Complete supported-version matrix, capability negotiation, old/new compatibility and migration remain U |

Additional deterministic obligations now exercised: admitted-but-not-forwarded mutation versus
fence ordering, neighbor progress while that admission lock is held, and no empty turn drain when
an acknowledged native command's start event is still missing. Removing the admission lock in a
deliberately unsafe research mutant makes the native race test fail as expected.

**Current full-parity decision: NOT DEMONSTRATED.** The host accepts an unmediated mutation after
the frontend fence, and detached native descendants can still mutate after turn completion.
These are positive native counterexamples, not merely absent tests. The recommended production
boundary must fix both while retaining native TUI/permissions; neither prototype is shippable.

## Controlled engineering update

See ENGINEERING.md and engineering/evidence/integrated-transitions.json. Status
remains **NOT DEMONSTRATED**; no earlier release obligation is removed.

| IDs | Additional observed evidence | Still blocked/unverified |
| --- | --- | --- |
| C01–C03, C05, C12–C15 | Eight service clients execute real commands and concurrent fork/new/resume flows, with exact returned roots and 24 retained reservations | Native TUI patch build/run; shared-daemon connections; Herdr |
| C06–C08, S02 | Opaque service peers and token substitution refuse; one laboratory service lock retained through admitted-call close | Host-wide native enforcement, secure IPC, independent native processes and domain discovery |
| H02, R02–R08 | Committed lost reply replays without duplicate native execution; UNKNOWN never retries; reopen preserves statuses and rejects old generations | Native creation/token atomicity, real host crash recovery, complete executor fence and successful handoff |
| R04–R05 | Detached native child still writes; forged quiescence refuses; new Linux self-migration adversary supplied | Linux probe BLOCKED on macOS; no full native executor adapter or remote/MCP receipt |
| Q01–Q08 | 42 existing scheduler regressions pass; integrated service issues no account polling | New event-driven automatic quota/handoff path still unverified |
| U01–U05, P01–P03, V01–V03 | Production unchanged; pinned host admission seam inspected | Native permission/UX parity, whole-runtime costs, provider-upgrade/migration and mixed-client compatibility |

Additional required adversary: executors cannot migrate out of containment or
retain management authority/descriptors. An empty cgroup is insufficient if
work previously escaped it. Direct native requests must refuse after a fence;
the new service experiment still positively demonstrates that this is not true
for the unmodified provider dispatcher.

## Capability follow-up from c113acf

Checkpoint integrity is verified (bundle/hash and 90 mirrored files). Full native
build/shared-daemon/process-supervision validation remains blocked by DNS,
socket-listener and process-inspection restrictions. Remote backup is separately
blocked by GitHub write approval under a never-approval policy; no remote backup
is claimed. Exact setup and branch-only backup commands: LAB_SETUP.md. No native
acceptance row or previous test result is upgraded by these capability checks.

## Pinned native build follow-up from aa96aea

See NATIVE_BUILD.md and engineering/evidence/native-build-20261030/result.json.
C12–C15/U01–U05 advance only from source applicability to successful **native
client/TUI type-checking**, using the unchanged insertion and Rust 1.95.0.
External locked dependencies and permission parameters are unchanged. Full CLI
linking was stopped by a 1.5 GiB disk reserve, with no compiler diagnostic;
embedded/shared TUI runtime tests were **not run**. No selection, permission,
Herdr, host-admission, executor-quiescence or handoff runtime gate is passed.
Full parity remains NOT DEMONSTRATED. Earlier network/ps restrictions no longer
block this lab, but extra storage is required before another build/runtime run.
