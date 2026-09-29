# Automatic handoff: day-to-day use

Relay moves work automatically when the current writer is exhausted, for both providers — with a
different trigger for each. **Claude** reports a rate limit as an event, so this page's main flow is the
Claude usage integration (a hook Relay installs into a Claude profile). **Codex** has no such event;
Relay reads its structured rate-limit state directly, before entering a Codex terminal and
periodically while it is supervised (see [Codex as the exhausted writer](#codex-as-the-exhausted-writer)).

For Claude, automatic handoff is **opt-in**: nothing runs unless you install the integration into a
profile. Once installed, the profile's `StopFailure` hook is the trigger: when Claude reports a rate limit
for the session Relay manages for that project, the hook starts one short-lived, detached
`relay watch auto` (a hidden command) that runs exactly the evaluation `relay watch run` runs,
retrying only while it answers "no action needed" (the statusline snapshot that corroborates a
limit can land just after the failure). It makes a short 300ms/1s/2s corroboration burst, then
uses the normal 20-second cadence for a bounded roughly two-minute window. It logs each attempt to
`<state>/projects/<project>/auto-handoff.log`. Relay is not a daemon and never polls; you can still
invoke `relay watch run` by hand at any time. Only the exact conversation named by an active Relay
Session's lease can trigger it, and every safety rule below (cooldown, per-window cap,
known-exhausted ledger, the session's own lock) applies unchanged — **per Relay Session**: two
sessions on the same profile each run their own evaluation and transaction (they may land on the
same fallback), and neither can stop or move the other.

For an automatic Claude handoff, that 0600 log also records sanitized wall-clock correlation
points for the hook receipt, detached watcher spawn, first evaluation, corroboration, and the
foreground supervisor noticing Claude exit. The handoff journal records the corresponding
transaction state transitions and monotonic phase durations. These are diagnostics only; they do
not alter ownership or recovery decisions.

## One-time setup (per profile)

```sh
relay integration claude install --profile profile-a --dry-run   # preview every change
relay integration claude install --profile profile-a
relay integration claude install --profile profile-b
relay integration claude status  --profile profile-a

# or, for every registered Claude profile in one command (e.g. after a dogfood upgrade):
relay integration claude install --all
```

What gets installed, in that profile's `settings.json` only (never `~/.claude` unless you pass
`--config-dir ~/.claude`):

- a `hooks.StopFailure` group (matcher `rate_limit`) running `relay hook claude stop-failure`;
- a `statusLine` running `relay hook claude statusline`, which records the `rate_limits` snapshot
  and then **runs your existing statusLine unchanged** (chained; a statusLine Relay cannot chain
  safely makes the install fail closed);
- `relay-integration/` inside the profile: a manifest, a backup of your original `settings.json`,
  and a `signals/` folder holding only structured metadata (never message text).

Existing hooks are preserved. Install refuses on an unverified/unsupported Claude Code version
(`--allow-unverified-version` accepts a newer 2.1.x patch; another release line is always refused).

An isolated Claude profile starts with its own settings; it never silently inherits the default
account's `~/.claude/settings.json`. During interactive `relay setup`, Relay detects a compatible
default-account command status line and, only when the isolated profile has no status line, asks
whether to copy and wrap it. This is opt-in and reversible: declining uses Relay's compact usage
line, and uninstall returns the isolated profile to its own original settings.

## Daily use

```sh
relay watch run \
  --profile profile-a --fallback profile-b \
  --project ~/repos/foo --session <session-id> \
  [--workload-model opus]
```

Run it from cron or a shell loop (`watch -n 60 …`). Each run is one evaluation.

## When switching happens

Only when the writer's profile is `EXHAUSTED`, which requires one of:

1. a `rate_limit_event` with `status=rejected`, a reset time in the future, not using overage; or
2. a `StopFailure(rate_limit)` **and** a fresh (≤10 min) statusline with a 5-hour or 7-day window
   at ≥100% whose reset is still in the future; or
3. a named `StopFailure` for the account's `session` or `weekly` limit and a fresh, same-native-
   session pre-failure statusline showing the matching 5-hour or 7-day window at ≥90%, with its
   reset still in the future. This narrowly covers Claude's limit modal, which can redraw the
   current statusline with null usage windows; or
4. (only with `--probe`) a real limit message ("You've hit your session limit …") **plus** the same
   statusline corroboration.

`NEAR_LIMIT` (≥90%), `AVAILABLE`, and anything stale or ambiguous (`UNKNOWN`) never trigger a
handoff. A bare `rate_limit` can be a transient 429 capacity error, so it is never enough alone.

Relay stores at most 20 sanitized statusline snapshots per Claude profile in its integration
directory. History is bounded and only used when the `StopFailure` and the selected snapshot have
the same native Claude session ID; a stale snapshot, missing reset, mismatched window, or bare
`rate_limit` remains `UNKNOWN`.

When strong exhaustion evidence includes a future reset, Relay also records a bounded durable
provider-account fact under its state root, keyed by provider plus the registered profile's
currently verified stable identity. A new Relay Session using that exact identity reads it as
`RESET_PENDING` until the reset passes; another profile/account is unaffected. `relay watch clear`
clears only project-session automation state. To explicitly clear one durable account record, use
`relay watch clear --project <path> --provider-account <profile>`.
Model-specific limits (Opus/Sonnet/Fable) only count when `--workload-model` names that family;
a fast-mode limit never counts.

### Superseding a stale durable record

The durable record's reset time is Relay's own clock-based estimate, not a live guarantee — a real
incident found a genuine, correctly-detected exhaustion (rule 3, corroborated by a second
independent session reading the identical 100% usage minutes earlier) invalidated less than a
minute later by an out-of-band, mid-window provider-side usage reset the record's own clock had no
way to know about. With no automatic supersession, Relay kept treating the now-healthy account as
`RESET_PENDING` for three more days, and — because every configured fallback was exhausted for
independent, legitimate reasons at the same time — the resulting "no eligible fallback" standoff
(below) ran silently rather than surfacing the mismatch.

`apply_provider_exhaustion` (`relay-cli`) now checks the *fresh* observation before ever falling
back to the durable record: if the account is freshly and positively read as `AVAILABLE` or
`NEAR_LIMIT` (a real statusline snapshot, not merely the absence of a signal), that clears the
stale durable record for this identity and wins outright, rather than being silently overridden by
the older stored fact. A fresh reading of `UNKNOWN` (no real signal at all) still defers to the
durable record exactly as before — the fix is scoped to genuine, positive contradicting evidence,
never to weakening the fail-closed default when there is nothing new to go on.

The handoff itself is Relay's transactional handoff (the same one `relay switch` uses). The target is the first `--fallback`
that is healthy, has a different identity, and is not recorded exhausted.

When an interactive terminal is waiting for a detached automatic handoff that has acquired its
orchestration lock, Relay shows a terminal-safe progress indicator. It names the source while the
transaction is in progress, names the target only after the completed lease proves ownership, and
then reports that it is continuing there. `--json` remains silent; detailed phase timings belong
in the handoff journal rather than normal terminal output (see [benchmarks](benchmarks.md#per-handoff-phase-diagnostics)).

## Reset windows

An exhaustion records its reset time. Until then that profile is `RESET_PENDING` and is never chosen
as a target. After the reset it may be a target again, but Relay **never fails back on its own**.
`relay watch clear --project …` forgets recorded exhaustion.

## No eligible fallback

When the writer is exhausted and every configured fallback is also exhausted, disabled, unhealthy,
or shares the writer's own identity, `watch run`/`watch auto` reach the terminal
`waiting_for_capacity` outcome. This is not a transient state to retry into silently: it names why
each candidate was rejected, and is recorded in the session's own automation ledger so a later,
unrelated evaluation can tell "the exact same standoff as last time" from "something changed."

The writer keeps the lease; nothing here weakens single-writer ownership or attempts a handoff to
an ineligible target. A supervised terminal (the Codex periodic poll, or the `StopFailure`-hook
trigger) prints it once the first time it is reached:

```
[Relay] codex-main is exhausted, but no fallback is currently eligible.
  claude-main: known exhausted
  claude-backup: known exhausted
```

It does not repeat on every later poll while the same standoff holds, and reports again only if the
facts actually change (a fallback becomes eligible, then everything is exhausted again for some
other reason). `--json` output carries the same facts structurally (`rejected`, `newly_reported`)
instead of the printed line. `relay why` explains the same decision on demand at any time.

## Recovery

Every `watch run` first inspects the project's current transaction. An interrupted one is recovered
(orphan target process stopped and confirmed, target re-verified, transcript turns kept) **before
anything new starts**, and that round ends with `Recovered`; run again to continue. If recovery is
ambiguous the command exits non-zero with `recovery_required` and starts nothing — run
`relay recover <id> --project-dir …` (or `--acknowledge` after confirming no target is running).

### Claude profile round-trips

For a native Claude `SESSION_CONTINUATION`, returning through profiles is supported: for example
`claude-main → claude-backup → claude-main`. Relay may replace an existing target transcript only
when every transferable artifact is either missing, byte-identical, or proven to be an exact stale
ancestor of the stopped source, and the target session is proven dormant immediately before each
replacement. The displaced target artifacts are atomically backed up, hash-verified, and recorded
in the handoff journal before Relay resumes the native session. This is not conflict merging:
ahead, divergent, active, unreadable, or ambiguous artifacts fail closed.

## What never happens automatically

Fail-back, quota pooling, editing settings outside install/uninstall, discarding transcript turns,
starting a second writer, or a real API request (the `--probe` diagnostic spends one and is never
run against a profile already recorded exhausted).

## Uninstall

```sh
relay integration claude uninstall --profile profile-a --dry-run
relay integration claude uninstall --profile profile-a
```

Restores `settings.json` byte for byte if unchanged since install; otherwise removes only Relay's
entries and restores your original statusLine. The backup file is kept.

## Codex as the exhausted writer

Codex reports usage through `codex app-server` (typed JSON-RPC over stdio), which Relay queries
read-only under the profile's own isolated `CODEX_HOME`: `initialize` (its response names the
Codex home the server really used; a mismatch fails closed), `account/rateLimits/read`
(`ordinaryUsageAllowed` is the authoritative allowed/blocked verdict; the window `resetsAt` values
feed the same known-exhausted ledger as Claude), and `thread/read` (proves a same-profile resume
target exists before `codex resume` runs). Anything unavailable, contradictory or not a plan-based
account is `UNKNOWN`. Relay never reads `auth.json` and never matches English error text.

Because Codex has no hook for "the limit was hit", a supervised Codex terminal (`relay resume`,
`relay switch`) starts the same bounded one-shot evaluation the Claude hook starts, while that
terminal is open. The interval is adaptive (GitHub #13): 2 minutes at comfortable usage (the
default, unchanged), tightening to 15s once a trustworthy read reports 90%+ used and 5s at 98%+ —
never inferring exhaustion from a percentage itself, only how soon to look again; the
`ordinaryUsageAllowed == false` verdict above remains the sole authoritative signal, and a fresh
lower reading after a reset relaxes the cadence back to 2 minutes. `RELAY_CODEX_POLL_SECS=0` still
disables polling entirely; `RELAY_CODEX_POLL_SECS=N` (`N > 0`) is still a deterministic fixed
interval that disables adaptive cadence for that process. See
`crates/relay-cli/src/codex_poll.rs`. An unsupervised Codex session is only evaluated by an
explicit `relay watch run` or the Herdr event.
Codex → Claude and Codex → other Codex profiles are `STATE_CONTINUATION` (a new session seeded from
the state bundle); cross-profile native resume is not supported.

### Experimental: event-driven wake-up (GitHub #16), opt-in only

Set `RELAY_CODEX_EVENT_DRIVEN=1` to let a supervised Codex terminal wake up immediately on Codex's
own structured turn-failure error (`usageLimitExceeded`) or its `account/rateLimits/updated`
notification, rather than only ever finding out on the next poll above. Unset (the default),
nothing here changes: every launch/resume/switch behaves exactly as already described. `#13`'s
polling is never disabled or reduced by this — it remains the watchdog either way, and this
feature never becomes authoritative on its own: either signal, and a successful (re)connect (see
below), only ever triggers the same `account/rateLimits/read` evaluation, and
`ordinaryUsageAllowed == false` remains the sole exhaustion verdict.

Mechanically: enabling this ensures Codex's own shared local app-server daemon is running for the
profile's `CODEX_HOME` (`codex app-server daemon start` — the same officially supported
multi-client backend `codex agents` already relies on to list every session across simultaneous
local invocations), then attaches Relay's own passive observer to that daemon's well-known control
socket in the background — this happens immediately and never blocks the interactive command from
starting. **The interactive command itself is never modified: no `--remote` flag, no other
argument is ever added.** An earlier version of this feature spawned a *private*
`codex app-server --listen unix://…` and appended `--remote unix://…` to the interactive command
so it would attach to that private runtime; a real dogfood session found this silently broke
persisted permission arguments (`--yolo`, `--sandbox`, etc.) — Codex's own TUI (live-verified
against `codex-cli 0.155.0`) explicitly refuses to carry a CLI permission override into a
`--remote`-mode resume, restoring the resumed thread's saved settings instead, which is not the
same as an ordinary local `--yolo` invocation. An ordinary local `codex resume` with **no**
`--remote` flag at all auto-discovers and reuses this exact same daemon on its own, and Codex
treats a connection it made itself to it as an ordinary local session for permission purposes —
so every already-resolved provider argument keeps meaning exactly what it always did.

- **A resume** (Relay's own lease already names the thread) attaches directly.
- **A fresh launch** (no thread exists yet) waits for the real `thread/started` broadcast to learn
  the id, then retries `thread/resume` on a cheap local interval — live-verified (GitHub #17) that
  this succeeds as soon as the thread's *first* turn has *started*, not completed, so the observer
  reliably catches that first turn's own outcome (including a real `usageLimitExceeded`, were the
  account already exhausted) rather than being excluded the way it was in #16.
- **If the observer's connection dies** (crash, dropped connection, anything short of the
  app-server itself dying) — live-verified with a genuine abrupt disconnect, including mid-turn —
  Relay detects this locally (no provider call) and reconnects on the same cheap local interval,
  reusing the now-known thread id (never re-learned; the broadcast only fires once). **Every
  successful (re)attach — the first one or any later reconnect — triggers exactly one authoritative
  `account/rateLimits/read`** before resuming event-driven operation, so a gap of any length is
  always reconciled against current truth rather than assumed benign. The retry/reconnect interval
  never grows large enough to matter (bounded at a few seconds) and is entirely local socket
  activity — never a provider call — so it costs nothing meaningful even across an arbitrarily long
  wait for a real user's first message.

Only the observer (or its still-retrying background worker) is torn down as part of normal session
cleanup — the daemon itself is never terminated by Relay, on a normal exit or otherwise: it is
shared and Codex-owned, potentially serving other sessions or tools with nothing to do with this
Relay Session, and must keep running exactly as if Relay had never been involved. What this still
deliberately does **not** attempt:

- **Periodic provider polling has not been removed.** #13's adaptive cadence table is unchanged and
  remains the only mechanism whenever this feature is off, fails to attach, or hasn't reconnected
  yet — see GitHub #17/#18 for the research behind why removing it entirely may eventually be safe,
  and why that has not happened yet (it requires a real natural exhaustion observed end-to-end
  through this path first).

Only activates for a Codex CLI version this was live-verified against
(`relay_provider_codex::VERIFIED_VERSIONS`); any other version silently declines and #13 alone
covers that session. See `crates/relay-provider-codex/src/{events,observer,runtime}.rs` and
`crates/relay-cli/src/codex_runtime.rs`.

## Working state (Issue #5)

Relay can carry a small, durable, per-Relay-Session working-state snapshot across a handoff, in
addition to the deterministic repo facts and recent-conversation excerpt every `STATE_CONTINUATION`
bundle already carries: a goal, the current subtask, decisions, failed attempts (so a later
provider does not repeat known-wasted work), relevant files (with why they matter, not their
contents), and next actions. It lives at `sessions/<id>/working_state.json`, one flat snapshot —
not a transcript, not an event log — because one writer per Relay Session means there is never a
concurrent-write problem an event log would exist to solve.

The active agent maintains it explicitly at meaningful checkpoints, never automatically and never
through an extra model call:

```
relay state show
relay state update '{"goal": "...", "add_decisions": [{"summary": "...", "rationale": "..."}], "next_actions": ["..."]}'
relay state update -   # reads the same JSON from stdin
```

Every field is optional and additive; `next_actions` replaces wholesale on each update (a stale
next-action is actively misleading), everything else is appended and bounded (20 decisions, 20
failed attempts, 20 relevant files, 10 next actions, ~500 characters per entry, 64 KiB total — an
update that would exceed a bound is rejected with a clear error, never silently truncated). Only
the current active writer may update it; a session that has never called `relay state update`
simply has no working state, which is a normal condition everywhere it is read, not an error.

**This state is advisory, never authority.** It is rendered into a `STATE_CONTINUATION` bootstrap
prompt labeled "Durable working notes from the previous agent — advisory only," explicitly stating
it cannot override user instructions, permissions, Relay policy, or security constraints. Nothing
in Relay's own ownership, lease, provider-eligibility, or exhaustion-decision code ever reads it —
it flows in exactly one direction, into text a future model turn sees, the same one-way channel
recent-conversation excerpts already use. Corrupt or unreadable working state degrades to "none
recorded" rather than blocking a handoff: the real ownership transaction never depends on this
purely advisory artifact being healthy. It also never appears in a same-provider native resume
(`SESSION_CONTINUATION`) bootstrap, since native conversation history already supplies that
continuity — it is persisted regardless, so it is available the next time this session *does*
cross providers.

## Execution mode (Issue #3)

Every Relay Session carries an execution mode, `interactive` (the default — normal conversational
confirmation) or `autonomous` (continue pursuing the current task without asking merely because a
step, or a handoff, occurred; still stops for a genuine blocker: missing information, an action
needing explicit authorization, or an unsafe/ambiguous situation). **This is behavioral intent
only — never a permission grant.** It never maps onto, sets, or implies a Claude permission flag, a
Codex sandbox/approval setting, `--yolo`, or any passthrough provider argument; those stay exactly
whatever you pass them as, completely independently of execution mode.

Three equivalent ways to set it, all converging on the same durable field:

```
relay claude --autonomous ...          # at launch
relay codex --autonomous ...           # at launch
relay resume --autonomous              # while resuming a closed session (or --interactive)
relay mode autonomous                  # live, on the current session (or `relay mode` to show it)
```

The same live change is also reachable from inside a managed conversation — `/relay:mode
autonomous`/`/relay:mode interactive` for Claude, `$relay mode autonomous`/`$relay mode
interactive` for Codex — and it takes effect immediately: unlike every other `/relay:*` command,
which Relay answers without the model ever seeing the prompt, a `/relay:mode` change that actually
changes something lets the prompt through with the new mode's meaning added as context, specifically
so the already-running model acts on it starting with its very next step, not merely on the next
handoff.

Execution mode is set once at launch and otherwise changes only through one of the above — never as
a side effect of a handoff, which always preserves whatever mode the session already had onto its
new owner. An old session recorded before this field existed reads as `interactive`, the
least-surprising default. `relay status --json` reports it (`execution_intent`, on every session
row and on `current_session`) as a plain local field — never a reason for `status` to perform a
live provider check.

## Limitations

- The statusline only refreshes while an interactive Claude session is drawing it; headless
  (`-p`/`--bg`) sessions record nothing there, so the snapshot goes stale (≤10 min) and detection
  falls back to `UNKNOWN` rather than guessing.
- `rate_limit_event` capture happens only for headless processes Relay itself runs (the probe).
- The hook/statusline commands embed the path of the `relay` binary that installed them; re-run
  install after moving or upgrading it.
- The handoff still refuses while the source profile has any live Claude process (per profile).
- Validated on Claude Code 2.1.276 and 2.1.277.
