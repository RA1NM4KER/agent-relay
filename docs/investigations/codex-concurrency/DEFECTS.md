# Independent follow-up work

These defects remain separate from the concurrency prototypes. No production fix is applied.

## D1: Historical fork adopted without current activity

**Reproduction:** `feasibility/defects.py` invokes the unchanged production watcher eight times,
each with a current thread and an unchanged pre-existing fork. All eight runs adopt `old-fork`.
There is one watcher, no concurrent writer, and no post-attach append. The existing regression
for a genuinely new fork also passes; it does not cover this historical-file case.

**Cause:** `rollout.rs` treats `forked_from_id == current_thread_id` as sufficient even when the
candidate was present at attach and never grew. Lineage is persistent historical metadata, not
evidence of a new fork operation or current TUI selection.

**Severity:** high attribution correctness concern / must be fixed before treating watcher
binding as authoritative. It can persist an incorrect observed thread and report healthy
coverage while following the wrong file. No duplicate writer, credential exposure or false
completed handoff was demonstrated by this reproduction; do not inflate the evidence.

**Narrow existing-architecture fix to review separately:** do not accept a pre-existing unchanged
fork solely because of its ancestry. Require eligible post-attach structural activity or exact
native lifecycle evidence. A newly created header already constitutes post-watch bytes and should
retain the new-fork regression. A watch-state restart must not resurrect a stale historical fork.

**Required tests:** unchanged historical fork; chains/multiple historical forks; real newly
created fork before its next turn; existing fork actually resumed and appended; restart with
old offsets; historical exhaustion not replayed; new current-thread exhaustion still caught.
This fix does not make profile-growth attribution safe under concurrency. The long-term
explicit native binding replaces that inference rather than generalizing it.

## D2: Concurrent first-use native SQLite initialization

**New controlled comparison, Codex 0.166.0:** eight trials per treatment, two endpoints each:

| Treatment | Failed endpoints |
| --- | --- |
| Cold parallel initialization | 3 / 16 |
| Serialized warmup, then parallel startup | 0 / 16 |
| Short bootstrap gate around initialization/start only | 0 / 16 |

The earlier independent run had 7/20 failures. These are finite observations, not an estimated
population failure rate or proof that warmup eliminates all future failures. All completed
database files in the new run returned `PRAGMA quick_check = ok`. An earlier TUI failure named
migration 18 / missing `stage1_outputs`; the newer initialization failures did not expose enough
inner detail to establish an identical SQL race in each endpoint.

**Severity:** significant fresh-profile/upgrade startup availability risk. Persistent physical
database corruption was not established. Current Relay profile exclusivity prevents its own
simultaneous startup path; relaxing it must not reintroduce this race.

**Required native fix:** serialize schema/version checks and migration across processes for
the actual configured SQLite domain, then publish completion. Test interruption of migration,
mixed-version clients, custom SQLite locations, a busy database, and retry after a failed starter.
Native initialization opens several database domains; a guard around only one SQL statement
is not sufficient without source-level migration-order proof. No auth read, long-lived session
exclusion, quota polling or credential duplication is needed.

**Possible Relay/native integration mitigation to validate:** a short provider-owned bootstrap/
maintenance gate or one authoritative shared-backend initializer. The experiment demonstrates
the narrow gate, not a production implementation or proof against unmanaged native starters.
Never clear tables, remove a live database, bypass native migration checks or label a profile
permanently safe merely because a marker file says it was once initialized.

**Required tests:** 2/8/32 simultaneous cold starts; current-version warm starts; concurrent
upgrades; killed initializer before/after each migration commit; successful later retry;
semantic migration-state verification as well as quick_check; no effect on unrelated home.

Primary source additionally inspected: pinned `codex-rs/state/src/runtime.rs`, blob
`2ecd9592c69aa2c7aade3f083a5bb95b5dfd8233`. The experiment establishes the startup race;
it does not claim a complete root-cause proof for every failure inside the native migration code.

## D3: Baseline PTY presentation test conflates EOF with process exit

Found during the native-build continuation's required Relay baseline check.
The unchanged `relay-cli --test presentation` case
`real_terminal_obeys_color_no_color_dumb_and_json_policy` reported a ten-second
terminal-command timeout after its entire three-test target took only 0.13s.
The script breaks on PTY EOF/EIO, immediately checks `process.poll()`, and kills
the child if its exit status is not observable yet, without waiting for the
remaining deadline. PTY closure can precede waitable process exit.

The original test passed on one isolated rerun; the original full workspace
command remains failed. A separate, predetermined 25-case diagnostic (five
trials across tty/no-color/dumb/json/control) observed two early-EOF cases with
poll=None followed by successful exit in about 50 microseconds. All 25 exited
successfully and matched the color policy. A preliminary five-case tty-only
diagnostic did not observe the race and is retained too. This diagnoses a
false-timeout mechanism, not a native concurrency regression or a production fix.

Evidence: `engineering/evidence/native-build-20261030/pty-modes-exit-diagnosis.json`
and the original/isolated Relay test logs. No production/test source changed.
Review separately: after terminal EOF, wait for the exact owned child with the
remaining monotonic deadline; only kill/reap after a real timeout. Add a
deterministic child that closes its terminal before a delayed successful exit,
and a child that never exits, so both normal exit and the real deadline remain
covered. Do not mask nonzero exits or weaken output/color assertions.
