# M3 final report: Agent Relay × Herdr integration

Covers the full M3 arc: M3.0/M3.1 (research, contract design, first implementation slice — see
`M3_OVERNIGHT_REPORT.md` for that phase's own detailed record) and M3.2 (real Herdr server live
validation, session-resolution completion, full action set, automatic orchestration,
install/uninstall). Every claim below is marked **LIVE VERIFIED** (run against a real Herdr 0.9.0
server and/or the real adopted profiles), **TEST VERIFIED** (covered by the 286-test suite against
scripted fakes, not run live), or **UNVERIFIED / UPSTREAM LIMITATION** (a known gap).

## 1. Git/tag state

- Local `main`: `820bbaf`, 3 commits ahead of `origin/main` (`ef33543`, `0d03373`, `820bbaf`),
  plus this report and the doc updates described below, not yet committed as of writing this
  section (committed before push — see §22).
- `v0.1.0` tag: **untouched**, still points at `4107a5f`, the same commit as before M3 started.
- No force-push, no new tag, no version bump.

## 2. Herdr version live-tested

**LIVE VERIFIED.** Installed: `0.9.0` (client/server/protocol 22, `herdr status --json`). Latest
published: `v0.9.1` (GitHub release, 2026-09-16), 9 days newer; no breaking drift found between the
two for anything this integration uses. Herdr's own docs at the time state they apply to 0.9.1.

## 3. Observed context schema

**LIVE VERIFIED**, and it corrected a wrong M3.1-era assumption. `HERDR_PLUGIN_CONTEXT_JSON` for
both a real action invocation and a real event invocation is flat:

```json
{
  "workspace_id": "w7", "workspace_label": "...", "workspace_cwd": "...",
  "tab_id": "w7:t1", "tab_label": "1",
  "focused_pane_id": "w7:p5", "focused_pane_cwd": "...",
  "focused_pane_agent": "claude", "focused_pane_status": "working",
  "invocation_source": "cli", "correlation_id": "cli:plugin"
}
```

No `tokens` map, no `agent_session` field. The full env var set Herdr hands a plugin invocation
(also **LIVE VERIFIED**, via a temporary throwaway debug action, removed before finalizing):
`HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_ACTION_ID` (actions only) /
`HERDR_PLUGIN_EVENT`+`HERDR_PLUGIN_EVENT_JSON` (events only), `HERDR_ENV`, `HERDR_TAB_ID`,
`HERDR_PLUGIN_STATE_DIR`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_SOCKET_PATH`,
`HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_BIN_PATH`, `HERDR_WORKSPACE_ID`, `HERDR_PANE_ID`. Full detail
and the raw captures: `docs/herdr-integration.md`'s M3.2 section.

## 4. Profile resolution

**LIVE VERIFIED, unchanged in design from M3.1, corrected in one detail.** `relay_profile` /
`relay_profile_fallback` tokens on Herdr's own pane/workspace `tokens` map (`herdr
pane|workspace report-metadata --source agent-relay --token NAME=VALUE` — the `--source` flag was
undocumented in the M3.1 notes and is required), re-validated live against `relay profile status`
on every use (enabled, authenticated, identity-matched). Pane/workspace token disagreement is
`ProfileMappingAmbiguous`; anything else missing/stale is `ProfileMappingUnknown` — never a guess.
Live-tested: happy path, missing token, invalid/nonexistent token (both collapse to the same
`ProfileMappingUnknown`), non-Claude pane, and pane with no agent detected at all.

## 5. Session resolution

**LIVE VERIFIED — the one piece that genuinely changed shape from M3.1's design.**
`mapping::resolve_session_id`, in order:

1. Herdr's own `agent_session` from a real follow-up `herdr pane get <id>` call — but only a
   `kind: "id"` reference; `kind: "path"` is a hard `SessionReferenceNotAnId` refusal, never a
   silent downgrade to "no session" (the pane does have a session, just not one addressable as an
   id, and `relay --session` needs exactly that).
2. Failing that, an explicit `relay_session_id` pane/workspace token (same ambiguity rule as
   profile tokens) — the "smallest explicit binding mechanism" the M3 brief anticipated for
   exactly this case.

**Why step 2 is necessary, not optional** (**CONFIRMED, GENUINE UPSTREAM LIMITATION**): Herdr's
built-in Claude integration (`herdr integration install claude`) hard-codes the default
`~/.claude`; there is no per-profile or `--config-dir` form. Read directly (Herdr's own installed
`~/.claude/hooks/herdr-agent-state.sh`, read-only): the script itself has no `CLAUDE_CONFIG_DIR`
dependency at all — it only needs `HERDR_ENV`/`HERDR_SOCKET_PATH`/`HERDR_PANE_ID` (present
regardless of config dir) and Claude's own `SessionStart` hook stdin. So Herdr's installer, not the
hook mechanism itself, is what limits detection to the default account. A Claude session running
under an isolated Relay profile's own `CLAUDE_CONFIG_DIR` is therefore invisible to Herdr's
detection today, confirmed by directly reading Herdr's own configuration and source rather than
inferring it. Manually wiring the same hook into an isolated profile's `settings.json` would very
likely work but was deliberately not attempted live (writes to a real profile's Claude settings) —
see §25.

Never: scraping terminal output, choosing the newest transcript, inferring from cwd when multiple
sessions are possible, or silently choosing among candidates. Deterministic and fails closed by
construction (an explicit precedence order, an explicit ambiguity check, never a heuristic).

## 6. Plugin architecture

Unchanged core decision from M3.1, reinforced by M3.2: `relay-herdr` never links `relay-core`/
`relay-provider-claude`. It is a subprocess client of two separate stable CLIs — `relay --json`
(Relay's own, unchanged) and now also `herdr` itself (`herdr_client.rs`, new in M3.2, built on the
same generic `CommandRunner`/`ProcessSpec`/`SystemCommandRunner` infrastructure, just a different
envelope parser). **LIVE VERIFIED, and a real, corrected mistake**: Herdr's own CLI is not
consistent about `--json` — `pane get`/`workspace get`/`plugin link`/`plugin unlink` emit JSON
unconditionally and reject `--json` as an unknown flag; `plugin list` and `status` require it
explicitly. The original code guessed one behavior for all of them (based on the first few commands
tested) and was wrong for two of them — caught immediately as a clear `HerdrMalformedOutput`, not a
silent misbehavior, the first time each was actually exercised. Every call site in `herdr_client.rs`
now states which behavior it verified.

## 7. Actions implemented

All five, **LIVE VERIFIED** end to end against the real Herdr server, the real `relay` binary, and
the real adopted profiles on a disposable project (`~/repos/agent-relay-session-test`, a `w8`
Herdr workspace created and closed for this validation):

| Action | Relay calls | Live result |
|---|---|---|
| `status` | `profile status`, `lock status` | Composed real profile/lease/usage state correctly |
| `doctor` | `profile doctor` | `healthy: true`, all checks passed |
| `recovery` | `lock status`, `handoff status <id>` if on record | Correctly surfaced a real `COMPLETE` transaction, then correctly surfaced `RECOVERY_REQUIRED` mid-sequence |
| `watch` | `watch run ...` | Both the healthy no-op path and a genuine `EXHAUSTED` → `Handoff` outcome |
| `handoff` | `handoff run --from --to ...` | Completed a real transaction end to end |

Failure paths also **LIVE VERIFIED**: missing profile token, invalid/nonexistent profile, non-Claude
pane, malformed relay output (via a scripted fixture, not reproducible live on demand), relay
executable initially absent (the exact `RelayExecutableMissing` → discovery-fallback fix in §8), and
a real `target_artifact_diverges` refusal during the handoff sequence.

## 8. Automatic orchestration

**LIVE VERIFIED.** `[[events]]` on `pane.agent_status_changed` (field name is `on`, not `event` —
another thing corrected after Herdr rejected the first manifest draft with a clear parse error)
reuses the identical `watch` code path — confirmed the event invocation carries the same flat
`HERDR_PLUGIN_CONTEXT_JSON` shape as an action, so no separate parsing was needed. Fired live on a
real agent status transition (idle → working → done) in an **untagged** pane: completed in
~800ms, correctly no-opped with `ProfileMappingUnknown`, touched nothing else, did not interfere
with the underlying Claude turn. No client-side throttling was added; Relay's own cooldown
(30s)/per-hour cap (5)/known-exhausted ledger — all three exercised live during the handoff
sequence in §11 — are what actually prevent repeated evaluation from repeatedly handing off the
same session. This event subscribes globally (every Claude pane, not just Relay-managed ones),
matching how `herdr-claude-auto-retry`/`herdr-agent-usage` already do it; the cost for an opted-out
pane is two cheap local subprocess calls per status change, no relay/Claude API spend.

**Operationally note-worthy, not a defect**: leaving the plugin linked means this event handler now
fires for every Claude pane on the machine, including unrelated real workspaces, for as long as the
plugin stays linked. Overhead is small but continuous; flagged here so the decision to keep it
installed is an informed one (see §27).

## 9. Installation/uninstallation

**LIVE VERIFIED**, full cycle: `relay integration herdr install` → `doctor` (healthy: true) →
`uninstall` (removed: true) → `uninstall` again (idempotent, removed: false, no-op) → `status`
(plugin_registered: false) → `install` again (reinstall succeeds) → `doctor` (healthy: true again).
`doctor` validates the whole chain: Herdr reachable and version-compatible, plugin registered and
enabled, and the linked manifest's `min_herdr_version` not silently drifted from what the binary
expects. `uninstall` removes only Relay's own plugin registration (`herdr plugin unlink
agent-relay`) — never touches any other plugin or Herdr configuration.

**UNVERIFIED / UPSTREAM PACKAGING GAP, not solved**: `install` needs to find
`plugins/herdr/herdr-plugin.toml` on local disk (defaults to `./plugins/herdr`, i.e. running from
the repo checkout, or an explicit `--plugin-path`). A `cargo install`'d `relay` binary elsewhere on
the system has no way to locate the manifest automatically — the same open question as
`relay-herdr-plugin`'s own `relay` binary discovery (§6) for a marketplace-style `herdr plugin
install` as opposed to `link`. Not solved in M3; scoped as a real next-milestone item, not silently
worked around.

## 10. Interoperability behavior

**TEST VERIFIED** (3 unit tests) plus the design's own reasoning, unchanged from M3.1:
`usage_interop::acting_party` classifies Transient → `herdr-claude-auto-retry`'s job, `NEAR_LIMIT` →
informational only, `EXHAUSTED`/`RESET_PENDING` → Relay's job. This is display/gating only; Relay's
own `usage_policy.rs` (unchanged, `relay-core`) is the actual enforcement point regardless, so
there is exactly one place a transient event could ever trigger a real handoff, and this classifier
cannot bypass it even if used incorrectly. **UNVERIFIED**: UI-level coexistence (both plugins
toasting the same event) — a polish question, not attempted.

## 11. Real disposable smoke-test results

All 15 items from the M3 brief's smoke-test checklist, **LIVE VERIFIED** on the disposable
workspace/project:

1. plugin loads — `herdr plugin link` succeeded, schema validated by Herdr itself.
2. action appears — `herdr plugin action list`/`invoke` found all five real actions.
3. real `HERDR_PLUGIN_CONTEXT_JSON` parsed — confirmed via the debug-context capture, then via
   every subsequent successful action.
4. project resolves — `pane.foreground_cwd`/`cwd` matched the disposable repo throughout.
5. `relay_profile` resolves — confirmed for both real adopted profiles.
6. Claude session resolves — confirmed via both the Herdr-detected path (default-account test
   pane) and the `relay_session_id` token fallback (the actual handoff sequence).
7. profile status works — `status` action, live.
8. doctor works — `doctor` action, live, healthy.
9. watch works — both no-op and real-handoff outcomes, live.
10. missing profile fails closed — `ProfileMappingUnknown`, live.
11. invalid profile fails closed — same code, live (nonexistent profile name).
12. non-Claude pane fails safely — `NonClaudePane`, live, before any Relay call.
13. malformed Relay output fails safely — **TEST VERIFIED** only (not reproducible live on demand
    without deliberately corrupting a real binary's output).
14. recovery state is surfaced — live, including a real `RECOVERY_REQUIRED` reading mid-sequence.
15. no second writer authority is created — confirmed throughout: every action/event either reads
    Relay's own lease/lock or invokes `relay watch|handoff run` directly; nothing in `relay-herdr`
    holds a lock, a lease, or retries around one (also proven by the concurrency test —
    `two_simultaneous_evaluate_attempts_each_defer_to_relays_own_lock_outcome` — for the case a
    live two-writer race isn't safe to reproduce on demand).

## 12. Controlled Herdr → Relay handoff result

**LIVE VERIFIED, full success, both directions**, using `relay watch run --simulate-usage
exhausted` (fault injection, no real quota spent) on the disposable project between the two real
adopted profiles:

1. `relay launch --profile <A> --project-dir <disposable> "<harmless prompt>"` — real writer lease,
   real session id, one small real API turn (the launch prompt itself).
2. `<A> → <B>` via the real plugin action (`debug-watch-simulated`, a temporary manifest action
   added only for this validation and removed afterward — see §17): `COMPLETE`.
3. Reverse direction correctly hit `target_artifact_diverges` at first (via both the plugin and a
   direct `relay handoff run`), classified as `target_stale_ancestor` on inspection (safe —
   `<A>`'s pre-handoff copy was a strict prefix of `<B>`'s post-verification copy), resolved with
   `session conflict resolve --yes` (backup taken automatically first; `--force-discard-divergent`
   was never used, since this was never the divergent case).
4. `<B> → <A>` completed: `COMPLETE`.

## 13. Same-session result

**LIVE VERIFIED.** Session id identical across every step and both directions (confirmed in every
journal's `session_id` field and by the transferred-artifact hash chain). The `debug-context`/
`debug-watch-simulated` temporary manifest actions and their env dump were removed before
finalizing; no real session UUIDs are recorded in any tracked file (see §21).

## 14. Lease/single-writer result

**LIVE VERIFIED.** Final state after the full sequence: `lock status` shows `locked: false`, lease
owner `<A>` (back where it started, by design of doing both directions), `claude agents --json`
empty for both profiles. At no point during the sequence did more than one profile hold the lease,
and `relay launch`'s own guard (refuses a second writer for an already-held project) was never
challenged since each step waited for the prior one to reach `COMPLETE` first.

## 15. Dogfood workflow

Documented in `docs/herdr-integration.md`; summary:

1. Adopt Relay profiles as usual (`relay profile adopt ...`), same as standalone use.
2. `relay integration herdr install` once (per machine, from the repo checkout).
3. Open/create a Herdr workspace for the project; bind the pane once:
   `herdr pane report-metadata <pane_id> --source agent-relay --token relay_profile=<name>` (and
   `relay_profile_fallback=<other>` on the pane or workspace).
4. `relay launch --profile <name> --project-dir <dir> "<prompt>"` to start the managed writer (this
   step, not Herdr's own `agent start`, is what gives Relay a `WriterLease` to reason about).
5. If the launching shell was itself a Herdr pane, Herdr's own detection may pick up the session for
   the **default** account automatically; for an isolated profile, set `relay_session_id` once
   (§5) instead.
6. Herdr's `watch` action (manual) or the automatic `pane.agent_status_changed` event evaluates
   usage on real status changes; a genuine corroborated exhaustion triggers the existing
   transactional handoff automatically.
7. `status`/`doctor`/`recovery` actions stay available through Herdr for as long as the pane is
   open, regardless of which profile currently owns the writer lease.

## 16. Files changed

M3.0/M3.1 (see `M3_OVERNIGHT_REPORT.md` §4 for the itemized list): `docs/herdr-integration.md`
(created), `crates/relay-herdr/src/{error,client,mapping,actions,usage_interop}.rs` (created),
`crates/relay-herdr/src/bin/relay-herdr-plugin.rs` (created), `crates/relay-herdr/tests/actions.rs`
(created), `plugins/herdr/herdr-plugin.toml` (created), `crates/relay-herdr/{src/lib.rs,Cargo.toml}`
(changed).

M3.2 (this pass), commit `820bbaf`:

- New: `crates/relay-herdr/src/herdr_client.rs`, `crates/relay-herdr/src/install.rs`,
  `crates/relay-herdr/tests/install.rs`.
- Changed: `crates/relay-herdr/src/{lib,mapping,actions,error,client}.rs`,
  `crates/relay-herdr/src/bin/relay-herdr-plugin.rs`, `crates/relay-herdr/tests/actions.rs`
  (fixed to reflect the corrected session-token construction), `plugins/herdr/herdr-plugin.toml`
  (rewrote build/action command paths, added `[[events]]`, removed temporary debug actions before
  finalizing), `crates/relay-cli/{Cargo.toml,src/main.rs}` (new `integration herdr` subcommand),
  `Cargo.lock`.

Docs (this pass, staged separately — see §22): `docs/herdr-integration.md` (M3.2 section added),
`README.md`, `STATUS.md`, `CHANGELOG.md`, this file.

## 17. Any standalone-core changes and why

**None.** `relay-core`, `relay-provider-claude`, `relay-cli`'s existing commands, and the
transactional handoff machinery are byte-for-byte unchanged except for the new, purely additive
`integration herdr ...` subcommand tree in `relay-cli` (new commands, zero changes to existing
ones). The one real finding that touched an *existing* subsystem's behavior
(`untracked_writer_detected` firing intermittently, §25) was investigated but explicitly **not**
patched, per the M3 core-freeze rule: it could not be reproduced deterministically enough to trust
a regression test for the fix, so it is recorded for a dedicated future investigation instead of a
speculative change.

Two temporary debug artifacts were added and removed within this same pass, never committed: a
`debug-context` manifest action (`env > /tmp/...`, used once to capture the real
`HERDR_PLUGIN_CONTEXT_JSON`/env shape) and a `debug-watch-simulated` action/binary branch (exposed
`--simulate-usage` through the plugin for the controlled handoff proof in §12, since the shipped
`watch` action deliberately never exposes fault injection — a production action that could
fabricate exhaustion would be a real footgun). Both were removed, and the binary/manifest rebuilt
and relinked, before any commit.

## 18. Tests added

- M3.0/M3.1: 32 (29 `crates/relay-herdr/tests/actions.rs` + 3 `usage_interop` unit tests).
- M3.2: 43 net new/changed (`crates/relay-herdr/tests/actions.rs` grew to 34 with the session-token
  fallback tests; `crates/relay-herdr/tests/install.rs` added 11).
- All against `ScriptedCommandRunner` fakes — no real Claude account, Herdr socket, or profile
  registry in any automated test. Synthetic `alice`/`bob` names and `@example.com` identities only.

## 19. Total tests

**286 passing, 0 failed**, full workspace (`cargo test --workspace`). Was 243 before M3.

## 20. fmt/clippy

```
cargo fmt --all -- --check              → clean
cargo clippy --workspace --all-targets -- -D warnings   → clean, zero warnings
```

## 21. Privacy scan

Ran on every diff and every new/changed file, both before and after the live-validation work:

- No real profile owner names, emails, org/account IDs, OAuth/token/cookie/keychain content,
  Authorization headers, local usernames, or `/Users/<real-user>` paths in any tracked file.
- Raw `HERDR_PLUGIN_CONTEXT_JSON` and env dumps captured during live validation were written only
  to `/tmp` (outside the repo) and deleted immediately after use; none were committed. Doc excerpts
  use synthetic values (`w7:p5`, generic paths) reconstructed after the fact, not copy-pasted raw
  captures.
- Real session UUIDs generated during live testing (from `relay launch`, Herdr's own session
  detection, etc.) were kept out of every committed file; a grep for the UUID pattern across the
  diff found none. Test fixtures use obviously-synthetic UUIDs (`11111111-...`,
  `aaaaaaaa-bbbb-...`).
- No Schoolscape/internal information: the real Herdr server's other workspaces (real production
  work) were listed once for orientation, never opened, focused (beyond the one-time `workspace
  list` read), or referenced again; all testing used a dedicated disposable workspace created and
  closed for this purpose.

## 22. Commits

- `ef33543` — `feat: Agent Relay x Herdr integration, first safe slice (M3.0/M3.1)`
- `0d03373` — `docs: record M3 slice commit hash in overnight report`
- `820bbaf` — `feat: finish M3 Herdr integration - session resolution, full actions, automatic orchestration, install/uninstall (M3.2)`
- A final `docs:` commit for this report and the README/STATUS/CHANGELOG updates (see the commit
  immediately following this file's addition).

## 23. Push result

Deferred until you review this report. `main` is 4 commits ahead of `origin/main` (the three above
plus the final docs commit); pushed only if you confirm — see §28 for what I'd want your explicit
go-ahead on given the live-account/live-server nature of this work, even though the definition of
done in §27 is otherwise met.

## 24. Origin/main vs local main

`origin/main` remains at the same commit as the previous session's public push (`4107a5f`, matching
the pushed `v0.1.0` tag). Local `main` is ahead by the M3 commits above; nothing has been pushed
yet this session.

## 25. Remaining limitations

- Herdr's built-in Claude integration is default-account only; isolated profiles need the
  `relay_session_id` token fallback until/unless Herdr's hook is manually wired into their
  `settings.json` (not attempted live — writes to real profile config, deserves separate review).
- The intermittent `untracked_writer_detected` observation (§17) needs a dedicated, reproducible
  investigation before any `relay-core` change is justified.
- Marketplace-style `herdr plugin install` packaging (vs. `link`) is unsolved for both
  `relay-herdr-plugin`'s `relay` discovery and `relay integration herdr install`'s manifest
  discovery.
- UI-level coexistence with `herdr-agent-usage`/`herdr-claude-auto-retry` (double notifications) —
  a polish question.
- The automatic `pane.agent_status_changed` event fires globally once the plugin is linked,
  including for unrelated real workspaces (cheap no-op, but continuous) — see §8's note.
- Compatibility claims for both Claude Code (2.1.276/2.1.277) and Herdr (0.9.0/0.9.1) carry
  forward from before M3; neither was re-validated against a wider version matrix.

## 26. Genuine provider-exhaustion status

**Genuine provider exhaustion remains unobserved end-to-end.** Every handoff in this report,
tonight and in prior milestones, used `--simulate-usage exhausted` fault injection. No real account
was ever actually rate-limited, and none was intentionally pushed toward that state.

## 27. Ready for normal daily dogfooding?

**Yes, for the Herdr-driven manual workflow (status/doctor/recovery/manual handoff), with one
caveat for the fully automatic path.** The manual actions are thoroughly live-verified and safe:
they only ever read Relay's own state or invoke Relay's own already-tested commands, and every
failure mode fails closed with a clear, structured reason. The automatic `watch` event is also
live-verified and safe in isolation, but its continuous global firing (§8, §25) is a real
day-to-day cost worth being aware of, and genuine exhaustion detection through it has never been
observed for real (§26) — only simulated. Recommend: keep the plugin installed and use the manual
actions daily; treat the automatic event as "on and safe" but not yet proven against a real quota
limit.

## 28. Next milestone recommendation

1. Investigate the `untracked_writer_detected` intermittent observation properly — reproduce it
   deliberately (repeated `relay launch` + immediate `watch run` cycles) before touching
   `relay-core`.
2. Decide, with explicit sign-off, whether to wire Herdr's session-report hook into isolated
   profiles' `settings.json` — would remove the need for the `relay_session_id` token fallback
   entirely.
3. Observe a genuine (non-simulated) exhaustion end to end, opportunistically, without forcing it.
4. Only after both of the above: consider marketplace-style `herdr plugin install` packaging.

Before any of that: **your explicit review of this report**, and a decision on whether to push the
four local M3 commits to `origin/main` now (§23) — everything above meets the stated definition of
done, but the push itself, like the live handoff test, touches shared/public state and deserves an
explicit go rather than an assumed one, even under the broad authorization already given for this
session.
