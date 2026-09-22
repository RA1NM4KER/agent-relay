# M3 overnight report: Agent Relay × Herdr integration, first safe slice

Scope: the non-destructive, non-live-touching portion of M3 (research → contract design →
first implementation slice → tests → quality gate), run autonomously per the M3 task brief.
Nothing was pushed, no live Herdr/Claude state was touched, no version was bumped, no history was
rewritten. Everything below is local commits on `main` only.

## 1. Herdr version/API researched

- Locally installed Herdr: `0.9.0` (client/server/protocol confirmed via `herdr status`).
  Deliberately **not** updated (would mutate a live server — out of scope tonight).
- Latest published release: `v0.9.1` (GitHub, tagged 2026-09-16), docs at `herdr.dev/docs/plugins/`
  state they apply to 0.9.1.
- **The M0-era assumption in `docs/research.md` ("Herdr 0.9.0, inspected upstream 0.9.1") held up
  exactly** — no breaking drift in manifest schema, CLI surface, or socket schema was found.
- **New finding not in the old research**: `PaneInfo`/`WorkspaceInfo` (from live
  `herdr api schema --json`) have **no `CLAUDE_CONFIG_DIR` field, no pid, and no provider-identity
  field of any kind**. This changed the mapping design from what M0's notes implied was possible —
  see §2 and §6.
- Confirmed, by reading Herdr's own installed Claude integration script (`herdr-agent-state.sh`,
  read-only) rather than guessing: Herdr's Claude awareness comes entirely from Claude Code's own
  `SessionStart` hook payload relayed over Herdr's Unix socket (`pane.report_agent_session`) — the
  same class of mechanism Relay's own usage-integration hooks already use, just a different hook
  type in the same `settings.json`.
- Full write-up, sources, and the manifest/CLI/socket schema detail: `docs/herdr-integration.md`.

## 2. Architecture selected

- **Transport**: `relay-herdr` shells out to the `relay` CLI's `--json` envelope
  (`relay-cli::main()`'s stable `{schema_version, ok, command, data}` / `{ok:false, error:{code,
  message}}` contract — success on stdout, **error on stderr**, verified from source, not assumed).
  Zero linking against `relay-core`/`relay-provider-claude` internals. This means Herdr's own
  plugin execution model (whatever it turns out to be) sits underneath a stable CLI boundary that
  already existed, rather than a new one invented for this integration.
- **Profile mapping**: Herdr's own `tokens` extension point on `PaneInfo`/`WorkspaceInfo`
  (`relay_profile` token, `relay_profile_fallback` token), re-validated live against
  `relay profile status` on every use — never a cached/trusted guess. This was chosen over the
  alternative (deriving `CLAUDE_CONFIG_DIR` from a process-table scan, reusing M2C's orphan-scan
  code) because it requires **zero changes to `relay-core`/`relay-provider-claude`**, uses a
  Herdr-native mechanism the research explicitly flagged as designed for exactly this, and never
  guesses. See `docs/herdr-integration.md`'s "M3.1: the integration contract that was built".

## 3. Why Herdr integration is worthwhile

Herdr is where a developer already watches and drives long-running Claude sessions; it already
knows the focused pane and its session id. Wiring Relay's existing, tested, opt-in transactional
handoff behind a Herdr action/event means a genuine account exhaustion can be resolved without the
developer switching to a separate terminal and remembering CLI invocations. Neither existing
Herdr plugin (`herdr-agent-usage`, visibility; `herdr-claude-auto-retry`, transient retries) touches
cross-account handoff — Relay fills exactly that gap. Full argument in
`docs/herdr-integration.md#why-agent-relay-belongs-in-herdr`.

## 4. Files added/changed

New:
- `docs/herdr-integration.md` — M3.0 research + M3.1 contract design + auto-retry interop.
- `crates/relay-herdr/src/error.rs` — `HerdrIntegrationError`.
- `crates/relay-herdr/src/client.rs` — subprocess `RelayClient`/`CommandRunner`, `ScriptedCommandRunner` test double.
- `crates/relay-herdr/src/mapping.rs` — `resolve_profile`, `resolve_fallback_profiles`.
- `crates/relay-herdr/src/actions.rs` — `status`, `doctor`, `recovery_status`, `watch_evaluate`, `handoff_manual`.
- `crates/relay-herdr/src/usage_interop.rs` — auto-retry/Relay precedence classifier + unit tests.
- `crates/relay-herdr/src/bin/relay-herdr-plugin.rs` — the argv-invoked plugin binary.
- `crates/relay-herdr/tests/actions.rs` — 29 integration tests against a scripted `relay` process.
- `plugins/herdr/herdr-plugin.toml` — the plugin manifest (not yet linked/installed anywhere).
- `M3_OVERNIGHT_REPORT.md` — this file.

Changed:
- `crates/relay-herdr/src/lib.rs` — redesigned `HerdrPaneContext` (added `agent`, `pane_tokens`,
  `workspace_tokens`; removed the placeholder `PathBuf`-only shape), `HerdrAdapter` now returns
  `HerdrIntegrationError` instead of `relay_core::Result`.
- `crates/relay-herdr/Cargo.toml` — dropped the now-unused `relay-core` dependency, added
  `serde_json`/`thiserror`.
- `Cargo.lock` — reflects the above.

**Not changed**: `relay-core`, `relay-provider-claude`, `relay-cli`, `relay-testkit` — no files
under any of those crates were touched. See §13.

## 5. Implemented actions/workflows

| Manifest action | Relay calls | Requires |
|---|---|---|
| `status` | `profile status`, `lock status` | resolved profile |
| `doctor` | `profile doctor` | resolved profile |
| `recovery` | `lock status`, then `handoff status <id>` if on record | project dir only — **no profile mapping required** |
| `watch` | `watch run --profile --fallback... --project --session` | resolved profile, session id, ≥1 fallback |
| `handoff` | `handoff run --from --to --project --session` | resolved profile, session id, ≥1 fallback (first is the target) |

None hold a lock/lease or retry around a refusal — see the concurrency test in §8.

## 6. Session/profile mapping design

Herdr's `PaneInfo` exposes no `CLAUDE_CONFIG_DIR`/pid (confirmed live, see §1), so mapping cannot
be derived from environment/process state without a new Relay-side hook — which would touch the
integration-installer surface (`relay-provider-claude`) and was judged out of scope for tonight's
"do not refactor unless integration exposes a concrete bug" boundary. Instead:

1. `resolve_profile` reads a `relay_profile` token from the pane (falling back to the workspace);
   pane/workspace disagreement is `ProfileMappingAmbiguous`, absence is `ProfileMappingUnknown`.
2. It re-confirms the named profile is enabled, authenticated, and identity-matched via a live
   `relay profile status` call — a stale/renamed/deleted profile collapses to the same
   `ProfileMappingUnknown`, never a guess.
3. `resolve_fallback_profiles` reads a second token, `relay_profile_fallback` (comma-separated),
   for the same reason every current Herdr action is parameterless (confirmed live): there is no
   dialog for the user to name a target profile per invocation, so it is configured once via
   `herdr pane|workspace report-metadata --token ...`.

This is a deliberate architecture decision (the two realistic strategies are recorded in
`docs/herdr-integration.md`) rather than a default — flagging it per the M3 stop-condition
guidance even though it did not require stopping, since a different, equally reasonable team could
have chosen the process-scan strategy instead.

## 7. Auto-retry interoperability design

`relay-herdr::usage_interop::acting_party` classifies a reported usage state into
`AutoRetry` / `InformationalOnly` / `AgentRelay` / `Unknown` per:

- Transient 429/5xx/no corroboration → `herdr-claude-auto-retry`'s job, Relay does nothing.
- `NEAR_LIMIT` → informational only, no migration.
- `EXHAUSTED`/`RESET_PENDING`, corroborated → Relay's job.

This is a **display/gating helper only** — the real enforcement point remains Relay's own
`usage_policy.rs`, which already refuses to hand off on anything but a corroborated exhaustion.
Full precedence table and failure-mode reasoning: `docs/herdr-integration.md`'s "Auto-retry /
usage-plugin interoperability" section.

## 8. Tests added and totals

- 29 new integration tests (`crates/relay-herdr/tests/actions.rs`) + 3 new unit tests
  (`usage_interop`), all against `ScriptedCommandRunner` — no real Claude account, no real Herdr
  socket, no real profile registry. Synthetic `alice`/`bob` names and `@example.com` identities
  only.
- Covers every scenario the M3 brief listed: happy-path mapping, unknown profile, ambiguous
  mapping (pane/workspace token conflict — see §6 for why this replaced "ambiguous config-dir"),
  missing session identity, non-Claude pane (including "no agent detected"), healthy/UNKNOWN/
  NEAR_LIMIT/EXHAUSTED/WAITING_FOR_CAPACITY usage passthrough, target conflict
  (`target_artifact_diverges`), recovery-required state, malformed JSON (both stdout-side and
  stderr-side), Relay executable absent, Herdr metadata unavailable, and two simultaneous
  `watch_evaluate` attempts each deferring to Relay's own lock outcome verbatim.
- Workspace total: **275 tests pass** (was 243 before M3; +32).
- A manual, non-automated smoke test additionally exercised the compiled
  `relay-herdr-plugin` binary end to end (env parsing → mapping → scripted `relay` subprocess →
  JSON stdout) for the `status` action's happy path and three failure paths (non-Claude pane,
  missing `HERDR_PLUGIN_CONTEXT_JSON`, missing `relay` executable) — not part of `cargo test`, but
  confirms the binary itself wires together correctly. Fixture files were created under `/tmp` and
  deleted immediately after.

## 9. fmt/clippy/test results

```
cargo fmt --all -- --check       → clean
cargo clippy --workspace --all-targets -- -D warnings   → clean, zero warnings
cargo test --workspace           → 275 passed, 0 failed
```

## 10. Local commits created

- `ef33543` — `feat: Agent Relay x Herdr integration, first safe slice (M3.0/M3.1)`

Local only — `main` has not been pushed and the `v0.1.0` tag (still `4107a5f`) was not touched.

## 11. Anything blocked

Nothing was blocked outright, but one thing was deliberately **not done** because it requires live
Herdr access this session is not allowed to use: `HERDR_PLUGIN_CONTEXT_JSON`'s exact field layout
for a `pane`-context action was written from the documented/schema-derived shape, not captured
byte-for-byte from a real invocation. See §12.

## 12. Exact live validation steps for tomorrow

All on a **disposable workspace, no real account**, exactly mirroring how M2A/M2B/M2C were
originally live-validated:

1. `herdr plugin link ~/repos/agent-relay/plugins/herdr` (links the local manifest; does not touch
   the marketplace or any real workspace yet).
2. Open a disposable Herdr workspace, start a Claude pane in it (`herdr agent start alice --kind
   claude --pane <id>` or via the UI), let Herdr's built-in Claude integration detect it.
3. `herdr pane report-metadata <pane_id> --token relay_profile=<a real, disposable, adopted Relay
   profile name>` (and `relay_profile_fallback=<other-profile>` if you want to test `watch`/
   `handoff` too).
4. Trigger the `status` action from Herdr's UI (or run `relay-herdr-plugin status` directly with
   `HERDR_PLUGIN_CONTEXT_JSON`/`HERDR_PANE_ID` set the way Herdr actually sets them — capture that
   real env var value first with a temporary `env > /tmp/ctx.txt` inside a throwaway manifest
   action, so the exact shape is known before trusting the parser).
5. Compare the captured real `HERDR_PLUGIN_CONTEXT_JSON` against
   `crates/relay-herdr/src/bin/relay-herdr-plugin.rs`'s `PluginContext`/`PaneInfo`/
   `AgentSessionInfo`/`WorkspaceInfo` structs. Fix field names/nesting if they differ — the parser
   is written to fail closed (non-zero exit, clear stderr message) rather than silently misreading
   a field, so a mismatch will surface as an error, not a wrong action.
6. Only once that parsing is confirmed correct: try `doctor`, `recovery`, then `watch --dry-run`
   equivalent behavior isn't wired as a flag in the manifest today — if you want to test `watch`
   without spending real API usage, use `relay watch run`'s own `--simulate-usage` fault injection
   by testing the underlying `relay-herdr::actions::watch_evaluate` function directly in a Rust
   test/script rather than through the live binary, since the manifest's `watch` action always
   calls the real (non-dry-run) path today.
7. Verify Relay's `StopFailure`/`statusLine` hooks and Herdr's `SessionStart` hook coexist cleanly
   in the same profile's `settings.json` (install both, confirm neither clobbers the other) — this
   is expected to work (different hook types; Relay's installer preserves/chains existing hooks)
   but was not live-verified together tonight.

## 13. Whether any core Agent Relay change was necessary

**No.** `relay-core`, `relay-provider-claude`, `relay-cli`, and `relay-testkit` are byte-for-byte
unchanged. The entire integration is additive, in `relay-herdr` and `plugins/herdr/` only, exactly
matching the brief's "do not refactor the standalone handoff machinery unless integration exposes
a concrete bug" — no bug was found, so nothing there was touched.

## 14. Recommendation for the next M3 slice

1. Run the live validation steps in §12 (highest priority — it's the one real unknown left).
2. Once the context-JSON parsing is confirmed, add a `--dry-run`-equivalent manifest action or CLI
   flag for `watch` so it can be exercised live without spending real API usage, mirroring
   `relay watch run --dry-run`/`--simulate-usage`.
3. Decide on a UI-polish pass for auto-retry/usage-plugin coexistence (toast/sidebar
   double-notification) — explicitly deferred in `docs/herdr-integration.md`, not a safety
   question.
4. Only after both of the above: consider `herdr plugin install` (marketplace-style) packaging,
   which still requires explicit owner authorization before any real account is involved.
