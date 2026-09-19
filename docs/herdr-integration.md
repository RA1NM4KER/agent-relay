# M3.0: current Herdr integration research

This document re-verifies Herdr's plugin/CLI/socket surface **live**, against the actually
installed Herdr binary and the current `herdr.dev` documentation, rather than trusting the
Herdr-related notes in `docs/research.md` (written during M0). Where the two agree that is called
out explicitly; where they disagree, this document is authoritative for M3 design decisions.

All examples below use synthetic data (`alice`/`bob` profile names, `/home/alice/project` paths).
No real pane titles, real project paths, or real session data seen while inspecting the local live
Herdr server were copied into this file or anywhere else in the repository.

## Version findings vs. the M0 assumption

- **Locally installed Herdr:** `0.9.0` (`herdr status` → `client.version: 0.9.0`,
  `server.version: 0.9.0`, protocol `22`). The local `herdr update` was deliberately **not** run —
  that would mutate a live, currently-running Herdr server, which M3.0 is explicitly not allowed to
  touch.
- **Latest published Herdr release:** `v0.9.1`, tagged 2026-09-16 on
  [`herdrdev/herdr`](https://github.com/herdrdev/herdr/releases) (confirmed via
  `gh api repos/herdrdev/herdr/releases`). `v0.9.0` was tagged 2026-09-07 — the two stable releases
  are 9 days apart.
- **Official docs** at [`herdr.dev/docs/plugins/`](https://herdr.dev/docs/plugins/) state they apply
  to **Herdr 0.9.1 (latest)**.
- **Contrast with M0 research:** `docs/research.md` already anticipated this — it recorded
  `"Herdr 0.9.0 (the inspected upstream source is 0.9.1)"`. That call turned out to be correct: the
  version M0 targeted for compatibility (`0.9.0`) is still a currently-shipped stable release, and
  the newer `0.9.1` documentation did not change the manifest schema, CLI surface, or socket schema
  in any way relevant to this integration (verified by diffing the concepts below against
  `docs/research.md`'s existing Herdr notes). **No breaking drift was found.** The one operational
  note worth carrying forward: this machine's Herdr server itself is one release behind latest, so
  Relay's plugin should feature-check `min_herdr_version` rather than assume the newest CLI surface
  is present everywhere it runs.

## Plugin manifest (`herdr-plugin.toml`)

Source: [`herdr.dev/docs/plugins/`](https://herdr.dev/docs/plugins/), cross-checked against the
current manifests of `mo-arvan/herdr-claude-auto-retry` and `senna-lang/herdr-agent-usage` (fetched
2026-09-19).

Required top-level fields:

- `id` — ASCII letters/digits/`.`/`:`/`_`/`-`.
- `name` — display name.
- `version` — semver.
- `min_herdr_version` — oldest compatible Herdr release; `install`/`link` refuse a plugin whose
  `min_herdr_version` exceeds the running binary.

Optional top-level fields: `description`, `platforms` (subset of `["linux","macos","windows"]`,
defaults to all).

All commands are argv arrays — Herdr executes them directly, **never through a shell**. Both
reference plugins work around this by shelling out to a `launch.sh`/`bin/*.sh` wrapper themselves
(because Herdr spawns plugin commands with the server's own `PATH`, which usually lacks
nvm/homebrew/mise dirs) — a real, current gotcha worth carrying into the Relay plugin's own
packaging.

Sections:

- **`[[build]]`** — runs once during `plugin install` (not `plugin link`), after user confirmation,
  before registration; a failure aborts install. `senna-lang/herdr-agent-usage` uses this to compile
  its own binary post-checkout, since `plugin install` always replaces the managed directory with a
  fresh checkout.
- **`[[startup]]`** — runs once per enabled plugin after session restoration and socket readiness,
  env `HERDR_PLUGIN_EVENT=startup`. A failure here does not halt the Herdr server.
- **`[[actions]]`** — invocable workflows. `id` (ASCII letters/digits/`:`/`_`/`-`, no dots — unlike
  the top-level plugin `id`), `title`, `contexts` (`"global"`, `"workspace"`, `"pane"` are all used
  by the reference plugins), `command`. The action receives `HERDR_PLUGIN_ACTION_ID`. Current
  plugins use **no parameters on actions** — every actioned command discovers its own target from
  context env vars (see below), matching what `docs/architecture.md` already assumed
  ("Herdr actions do not accept parameters").
- **`[[events]]`** — fire on Herdr events; both reference plugins subscribe to
  `pane.agent_status_changed` and `pane.focused`; `herdr-claude-auto-retry` also subscribes to
  `pane.agent_detected`. Handlers receive `HERDR_PLUGIN_EVENT` and `HERDR_PLUGIN_EVENT_JSON`.
- **`[[panes]]`** — plugin-owned terminal UI surfaces (`placement`: `overlay` default, `popup`,
  `split`, `tab`, `zoomed`). `herdr-agent-usage` uses a `split` pane for its limits view.
- **`[[link_handlers]]`** — routes Control+click terminal URLs (regex `pattern` → local `action`).
  Not needed for the Relay slice.

Runtime env vars available to every plugin command: `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`,
`HERDR_ENV=1`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_CONFIG_DIR`,
`HERDR_PLUGIN_STATE_DIR`, `HERDR_PLUGIN_CONTEXT_JSON` (workspace/tab/pane/worktree/agent/selection
context, shape depends on where the action was invoked from), and optionally `HERDR_WORKSPACE_ID`,
`HERDR_TAB_ID`, `HERDR_PANE_ID`.

**State storage:** Herdr creates `HERDR_PLUGIN_CONFIG_DIR` and `HERDR_PLUGIN_STATE_DIR` but does not
validate, sync, or delete their contents — there is no Herdr-provided storage API. Relay already has
its own state directory conventions (`relay-core::paths`); the plugin should keep any Herdr-specific
mapping cache there or in `HERDR_PLUGIN_STATE_DIR`, not invent a third location.

## Socket API vs. CLI

Both remain current. The CLI (`herdr agent|workspace|pane|plugin ...`) wraps the same socket calls
the hook scripts and plugins use directly. `herdr api schema --json` prints the full JSON Schema for
every socket request/response/event (dumped and inspected locally, ~11k lines) — this is the
authoritative live schema, more reliable than any cached doc. `herdr api snapshot` prints the current
live session snapshot (not used here beyond confirming shape, to avoid pulling real session data into
context).

Relevant CLI surface (from `herdr <cmd> --help`, all confirmed against the installed `0.9.0`
binary):

- `herdr agent list` — lists all panes hosting a detected agent.
- `herdr agent get <target>` — one agent, by pane id or agent name.
- `herdr agent explain [target] --json` — explains detection state; useful for diagnosing "why does
  Relay not see this pane as Claude" without guessing.
- `herdr agent start <name> --kind claude --pane <id>` — start an interactive agent in an existing
  pane (`claude` is a supported `--kind`, alongside codex/gemini/cursor/etc.).
- `herdr workspace list` / `herdr workspace create --cwd PATH ...` / `herdr workspace
  report-metadata <id> --token NAME=VALUE ...`
- `herdr pane report-metadata <pane_id> --token NAME=VALUE --state-label STATUS=TEXT ...`
- `herdr plugin install <owner>/<repo>[/subdir] [--ref REF] [--yes]`, `plugin list [--json]`,
  `plugin uninstall <id>`, `plugin link <path> [--disabled]`, `plugin enable/disable <id>`.
- `herdr integration install|uninstall|status` — manages the **built-in** per-agent hook
  integrations (see below); separate from the plugin system.

### `pane`/`workspace` metadata shape (from `herdr api schema --json`)

`PaneInfo` (required fields marked `*`): `pane_id*`, `terminal_id*`, `workspace_id*`, `tab_id*`,
`focused*`, `agent_status*` (`idle|working|blocked|done|unknown`), `revision*`, plus optional
`agent` (string|null), `agent_session` (nullable `AgentSessionInfo`), `cwd`, `foreground_cwd`,
`display_agent`, `label`, `title`, `terminal_title`, `state_labels` (map), and **`tokens`** — a
free-form `{string: string}` map, ≤32 entries, keys matching `^[A-Za-z0-9_-]{1,32}$`, settable via
`herdr pane report-metadata <id> --token NAME=VALUE`. `WorkspaceInfo` carries the same kind of
`tokens` map plus `workspace_id`, `label`, `pane_count`, `tab_count`, `agent_status`, etc.

`AgentSessionInfo`: `{source, agent, kind: "id"|"path", value}` — e.g. Claude reports
`{"source":"herdr:claude","agent":"claude","kind":"id","value":"<claude-session-uuid>"}`.

**Confirmed finding, not previously documented in `docs/research.md`: `PaneInfo` has no
`CLAUDE_CONFIG_DIR` field, no process id, and no provider-identity field of any kind.** Herdr knows
a pane's `cwd` and the Claude session id/kind, and nothing about which authenticated Claude account
or config directory produced it. Any Relay/Herdr integration has to establish that mapping itself —
see the open design note below. The `tokens` map on both `PaneInfo` and `WorkspaceInfo` is the one
Herdr-native place designed for exactly this kind of external-tool metadata and is the natural place
for a Relay plugin to stamp `relay_profile=<name>` once it has resolved the mapping once, so it does
not have to re-derive it on every action.

## How Claude session identity actually reaches Herdr (read from the installed integration)

`herdr integration status` shows Herdr's **built-in** Claude integration (separate from the plugin
system) installed at `~/.claude/hooks/herdr-agent-state.sh`, current version 9. Reading that script
(read-only; it is Herdr's own file, not modified) shows the exact mechanism:

1. Claude Code's own documented `SessionStart` hook fires and pipes its standard JSON hook payload
   to the script on stdin.
2. The script extracts `hook_input.session_id` and `hook_input.transcript_path` directly from that
   payload — the same fields Agent Relay's own `relay-provider-claude` code already reads from
   Claude's hook/`agents --json` surface.
3. It skips subagent sessions (`hook_input.agent_id` present) and non-Claude wrappers (Cursor).
4. It opens `$HERDR_SOCKET_PATH` as a raw Unix domain socket and sends one JSON-RPC-shaped line:
   `{"id": "...", "method": "pane.report_agent_session", "params": {"pane_id", "source":
   "herdr:claude", "agent": "claude", "seq", "agent_session_id", "agent_session_path"}}`.

This confirms: Herdr's Claude awareness is **entirely** driven by Claude Code's own hook payload,
exactly like Relay's usage-detection hooks (`docs/automatic-handoff.md`). There is no separate
"Claude Code API" Herdr talks to. This also confirms Relay and Herdr's built-in integration hook the
same Claude Code hook surface **independently** — Herdr uses `SessionStart`, Relay's existing
integration installer uses `StopFailure` + `statusLine` (`docs/automatic-handoff.md`). They are
different hook types in the same `settings.json`, and Relay's installer already documents that it
"preserves existing hooks" and "chains an existing statusLine" — so the two should already coexist
without conflict, but this has not been live-verified with both integrations installed
simultaneously (flagged as a concrete thing to verify live, not simulate).

**Design implication:** because neither Herdr nor its Claude integration ever sees
`CLAUDE_CONFIG_DIR`, mapping "this Herdr pane" → "this Relay profile" cannot be read directly off
any Herdr API. Two realistic strategies, to be decided in M3.1:

1. **Environment-derived:** read the actual `CLAUDE_CONFIG_DIR` of the pane's foreground process via
   a process-table scan, reusing the `ps -Eww` matching Relay's M2C orphan-detection code
   (`relay-provider-claude/src/handoff_adapters.rs`) already implements for a different purpose.
   Requires Herdr to expose (or the plugin to independently discover) a pid for the pane, which
   `PaneInfo` does not currently carry — would need `terminal_id`-based process discovery instead.
2. **Explicit registration, cached via Herdr tokens:** the user (or a one-time setup action) tells
   Relay which profile a given pane/workspace belongs to; the plugin stamps that as a
   `relay_profile` token via `herdr pane report-metadata`/`herdr workspace report-metadata` so
   later actions read it back instead of re-resolving it, and Relay independently confirms the
   token against its own registered identity pin before ever trusting it (fail closed on mismatch).

Strategy 2 is safer (it never guesses) and uses a Herdr-native extension point instead of relying on
process-table scanning across two different programs (Herdr's server + Relay's CLI), so it is the
one carried into M3.1's contract design.

## Install/update/uninstall conventions

- **Discovery:** the marketplace at [`herdr.dev/plugins/`](https://herdr.dev/plugins/) is an
  automatic index of public, non-fork GitHub repositories tagged with the `herdr-plugin` topic that
  contain a parseable `herdr-plugin.toml` (root or subdirectory). Confirmed unchanged from M0.
- **Install:** `herdr plugin install <owner>/<repo>[/subdir]` — accepts GitHub shorthand only,
  refuses on `min_herdr_version` mismatch, runs `[[build]]` after confirmation. Update is the same
  command re-run (`senna-lang/herdr-agent-usage`'s manifest comment confirms: "there is no separate
  update command... Install replaces the whole managed directory with a fresh checkout").
- **Uninstall:** `herdr plugin uninstall <plugin_id|owner/repo[/subdir]>`.
- **Local development:** `herdr plugin link <path> [--disabled]` — links a local directory instead
  of a GitHub checkout; skips `[[build]]`. This is the path M3 testing/dev should use, and it is
  explicitly **not** run against the live server in this session per the M3 safety boundary.

## Reference plugins (patterns only, no code copied)

All three are still active, unarchived, MIT-licensed (confirmed via `gh api repos/<owner>/<repo>`,
pushed within the last ~2 weeks of 2026-09-19):

- **`mo-arvan/herdr-claude-auto-retry`** (v1.3.0, `min_herdr_version = "0.7.5"`): a pure
  wait-and-resume tool. `[[startup]]` re-arms monitors after a restore; `pane.agent_detected` and
  `pane.agent_status_changed` events (re)arm a per-pane monitor; `actions`: `watch-all`, `arm`,
  `status`, `stop`, `logs`, all parameterless, `contexts` of `"global"` or `"pane"`. It never changes
  profiles or accounts — it retries in place.
- **`senna-lang/herdr-agent-usage`** (v0.5.14, `min_herdr_version = "0.7.5"`): sidebar usage meters
  and rate-limit toasts across many providers. `[[build]]` compiles its own Go binary post-checkout;
  `[[startup]]` restores cached state; events on `pane.agent_status_changed` and `pane.focused`
  refresh/notify; a `[[panes]]` split view (`limits`) shows the detail. It is a dashboard, not an
  actor — it never moves a session.
- **`wilbeibi/herdr-catchup`**: (per M0 notes, unchanged here) restores/resumes a known Claude
  session (`claude --resume <id>`) inside a Herdr-managed pane using the session id Herdr's own
  Claude integration already captured; it does not transfer a transcript between config
  directories/accounts, and has no notion of profile identity.

None of the three implement anything resembling cross-account/cross-profile handoff, writer
ownership, or transactional safety — confirming the M0 conclusion that this space is open and that
Agent Relay is not duplicating an existing plugin's job.

## Why Agent Relay belongs in Herdr

Herdr is where a developer actually watches and drives long-running coding agents day to day: it
already knows which pane is running Claude, what its session id is, and what the developer is
looking at right now. That is exactly the moment a real account exhaustion becomes visible and
exactly where the developer wants the recovery to happen — not in a separate terminal running a CLI
they have to remember to invoke. Agent Relay's transactional handoff is real, tested (243 tests,
live-validated in both directions — see `STATUS.md`), and already correctly scoped as an
opt-in, explicit, single-writer-safe operation. Wiring it behind a Herdr action/event means the
developer keeps working in the same pane, in the same workspace, and gets the exhaustion → handoff
→ resumed-writer sequence surfaced as part of the tool they are already watching, instead of a
side-channel they must poll by hand. Herdr's existing ecosystem already proves the appetite for this
kind of integration (`herdr-agent-usage` for visibility, `herdr-claude-auto-retry` for transient
retries) — Relay fills the one gap neither of them touches: what happens when the account is
genuinely, corroboratedly out of quota and the work needs to continue under a different identity.

## Responsibilities we deliberately do not duplicate

Herdr owns, and Relay's Herdr adapter must keep treating as Herdr's exclusive responsibility:

- **Workspaces, tabs, and panes** — creation, layout, focus, and restoration across restarts.
- **Terminal/process lifecycle** — keeping the underlying agent process alive after detach, PTY
  management, session restoration.
- **Agent lifecycle visibility** — `agent_status` (idle/working/blocked/done/unknown), detection,
  and the UI surfaces (sidebars, toasts, panes) that show it to the developer.
- **Session ↔ pane association** — Herdr's own `AgentSessionInfo`/hook-driven bookkeeping of which
  pane is running which agent session.

Agent Relay owns, and the Herdr adapter must **never** reimplement, shadow, or race with:

- **Profile identity/isolation** — which Claude account (config directory) a profile refers to, and
  the non-secret identity pin that proves two profiles are distinct.
- **Usage/exhaustion decision policy** — the `AVAILABLE / NEAR_LIMIT / EXHAUSTED / RESET_PENDING /
  UNKNOWN` state machine in `relay-core::usage` / `usage_policy.rs`.
- **Single-writer authority** — the `WriterLease` and `OrchestrationLock`; there is exactly one
  source of truth for "who owns this project's writer seat," and it is Relay's, not a second
  Herdr-side lock.
- **Transactional handoff** — the `PREPARING → ... → COMPLETE` state machine in
  `relay-core::handoff`.
- **Source-stop verification** — `SessionStopper`'s multi-observation quiescence check.
- **Session transfer** — hash-verified transcript staging (`session_transfer`).
- **Target verification** — the post-launch verification turn before a handoff is trusted complete.
- **Conflict resolution** — `session conflict inspect/resolve/rollback`.
- **Recovery** — `relay recover`'s interrupted-transaction handling.

The Herdr adapter's only job is to (a) tell Relay, as accurately as it can and never by guessing,
which profile/session/project a given pane corresponds to, and (b) invoke Relay's existing, already-
tested CLI/API surface to act on it. If a mapping is ambiguous or unconfirmed, the adapter must
refuse and fail closed rather than pick a plausible-looking profile — an incorrect guess here is
exactly the kind of cross-account mistake Relay's whole safety model exists to prevent.

## M3.1: the integration contract that was built

Implemented in `crates/relay-herdr` (library) and `crates/relay-herdr/src/bin/relay-herdr-plugin.rs`
(the argv-invoked plugin binary), plus `plugins/herdr/herdr-plugin.toml`.

**Transport decision.** `relay-herdr` never links `relay-core`/`relay-provider-claude`. It shells
out to the `relay` binary with `--json` and parses the same stable envelope
(`{"schema_version","ok","command","data"}` on stdout for success, `{"schema_version","ok":false,
"error":{"code","message"}}` on **stderr** for failure — confirmed from `relay-cli::main()`, not
assumed) that the CLI already treats as its stable contract. This was a deliberate, evidence-based
choice, not a default: whatever Herdr's own plugin execution model turns out to be for a given
platform (a native binary today; Herdr's manifest schema does not preclude a different one later),
a compiled adapter that shells out to a stable CLI works underneath it, and it keeps `relay-herdr`
from ever needing to track `relay-core`'s internal types. It also means adding this integration
required **zero changes** to `relay-core` or `relay-provider-claude` — see M3_OVERNIGHT_REPORT.md.

**Profile mapping** (`relay-herdr::mapping::resolve_profile`) follows Strategy 2 above exactly:

1. Refuse immediately if the pane is not a recognized Claude pane (`agent != Some("claude")`,
   including "no agent detected at all").
2. Read the `relay_profile` token from the pane's own `tokens` map, falling back to the
   workspace's if the pane has none. If both are present and disagree, refuse
   (`ProfileMappingAmbiguous`) rather than pick one.
3. Re-confirm the named profile with a live `relay profile status <name> --json`: it must still
   exist, be enabled, be authenticated, and have `identity_matches: true`. Anything else —
   including the profile simply not existing (`profile_not_found`) — collapses to
   `ProfileMappingUnknown`; a cached token is a hint, never a trusted fact.

**Fallback/target resolution** (`relay-herdr::mapping::resolve_fallback_profiles`) reads a second
token, `relay_profile_fallback` (comma-separated priority list), the same way — because every
current Herdr plugin action takes **no parameters** (confirmed live above), so "which profile
should this pane fail over to" cannot be a dialog the action shows; it has to be configured once,
the same way `relay_profile` is.

**Actions implemented** (`relay-herdr::actions`), each doing nothing but map + invoke + report:

| Action (manifest `id`) | Relay CLI calls | Notes |
|---|---|---|
| `status` | `profile status`, `lock status` | Composite read-only health + writer/lock view |
| `doctor` | `profile doctor` | Verbatim `DoctorReport` |
| `recovery` | `lock status`, then `handoff status <id>` if a transaction is on record | Project-scoped only — deliberately does **not** require a resolved profile, so it still works when the `relay_profile` token is missing/stale |
| `watch` | `watch run --profile <mapped> --fallback <fallback...> --project <pane.cwd> --session <pane.session>` | Requires a session id and at least one fallback; the tagged `outcome` field is Relay's own, reported verbatim |
| `handoff` | `handoff run --from <mapped> --to <fallback[0]>` | Manual, explicit; still refuses without a session id |

None of these hold a lock, a lease, retry a call, or race a second attempt themselves — see the
concurrency test (`two_simultaneous_evaluate_attempts_each_defer_to_relays_own_lock_outcome`) in
`crates/relay-herdr/tests/actions.rs`, which runs two `watch_evaluate` calls in parallel against
two independently scripted `relay` responses (one `handoff`, one `transaction_in_flight`) and
asserts the adapter reports each outcome exactly as given, proving it never suppresses, merges, or
retries around whatever Relay's own orchestration lock decided.

## Auto-retry / usage-plugin interoperability

Agent Relay must not become another generic 429-retry mechanism (`herdr-claude-auto-retry`'s job)
or another quota dashboard (`herdr-agent-usage`'s job). `relay-herdr::usage_interop` documents and
implements the precedence:

| Condition | Owner | Why |
|---|---|---|
| Transient rate limit / 5xx / overload, no corroborating fresh signal | `herdr-claude-auto-retry` | Relay's own `usage_policy.rs` already refuses to treat a bare `error=rate_limit` as exhaustion — it is also produced for generic 429 capacity errors — so Relay has nothing to do here even if asked |
| `NEAR_LIMIT` | Informational only (a `herdr-agent-usage`-style meter) | No migration on an unconfirmed approach to a limit |
| `EXHAUSTED` / `RESET_PENDING`, corroborated, future reset window | Agent Relay | The one condition its transactional handoff exists for |

The important failure-mode property: this classifier is **display/gating only**. Even if a caller
ignored it entirely and invoked `relay watch run` unconditionally, Relay's own policy in
`usage_policy.rs` would still refuse to hand off on a transient or near-limit reading — there is
exactly one enforcement point (Relay's CLI), not two. This means Relay and `herdr-claude-auto-retry`
cannot race each other into both acting on the same transient event: Relay's own policy is the
backstop even if the Herdr-side gating were somehow bypassed. What is *not* yet resolved is UI-level
coexistence (e.g. both plugins showing a toast for the same event) — that is a product polish
question for a later slice, not a safety one.

## What is explicitly deferred to a later M3 slice

- **Live verification of `HERDR_PLUGIN_CONTEXT_JSON`'s exact field layout** for a `pane`-context
  action. `relay-herdr-plugin`'s parsing is written from the documented/schema-derived shape above
  but was never run against a real Herdr server tonight (disallowed by the M3 safety boundary). See
  `M3_OVERNIGHT_REPORT.md` for the exact steps to verify this by hand.
- **Coexistence of Relay's `StopFailure`/`statusLine` hooks with Herdr's own `SessionStart` hook**
  in the same profile's `settings.json` — expected to be fine (different hook types, and Relay's
  installer already documents preserving/chaining existing hooks) but not live-verified with both
  installed simultaneously.
- **UI-level coexistence** with `herdr-agent-usage`/`herdr-claude-auto-retry` (toast/sidebar
  double-notification on the same event) — a polish question, not a safety one.
- **Environment-derived mapping (Strategy 1)** — reusing the M2C orphan-detection process-table
  scan to read a pane's actual `CLAUDE_CONFIG_DIR` — was considered and explicitly not chosen for
  this slice; Strategy 2 (Herdr-native tokens) needed no `relay-core`/`relay-provider-claude`
  changes and matches Herdr's own extension point. Revisit only if the explicit-token UX proves too
  manual in practice.
