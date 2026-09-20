# Automatic handoff: day-to-day use

Automatic handoff is **opt-in**. Nothing runs unless you install the integration into a profile.
Once installed, the profile's `StopFailure` hook is the trigger: when Claude reports a rate limit
for the session Relay manages for that project, the hook starts one short-lived, detached
`relay watch auto` (a hidden command) that runs exactly the evaluation `relay watch run` runs,
retrying for about two minutes only while it answers "no action needed" (the statusline snapshot
that corroborates a limit can land just after the failure). It logs each attempt to
`<state>/projects/<project>/auto-handoff.log`. Relay is not a daemon and never polls; you can still
invoke `relay watch run` by hand at any time. Only the exact session named by the project's writer
lease can trigger it, and every safety rule below (cooldown, per-window cap, known-exhausted
ledger, single-writer lock) applies unchanged.

## One-time setup (per profile)

```sh
relay integration claude install --profile profile-a --dry-run   # preview every change
relay integration claude install --profile profile-a
relay integration claude install --profile profile-b
relay integration claude status  --profile profile-a
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
3. (only with `--probe`) a real limit message ("You've hit your session limit …") **plus** the same
   statusline corroboration.

`NEAR_LIMIT` (≥90%), `AVAILABLE`, and anything stale or ambiguous (`UNKNOWN`) never trigger a
handoff. A bare `rate_limit` can be a transient 429 capacity error, so it is never enough alone.
Model-specific limits (Opus/Sonnet/Fable) only count when `--workload-model` names that family;
a fast-mode limit never counts.

The handoff itself is the unchanged transactional M2B handoff. The target is the first `--fallback`
that is healthy, has a different identity, and is not recorded exhausted.

## Reset windows

An exhaustion records its reset time. Until then that profile is `RESET_PENDING` and is never chosen
as a target. After the reset it may be a target again, but Relay **never fails back on its own**.
`relay watch clear --project …` forgets recorded exhaustion.

## Recovery

Every `watch run` first inspects the project's current transaction. An interrupted one is recovered
(orphan target process stopped and confirmed, target re-verified, transcript turns kept) **before
anything new starts**, and that round ends with `Recovered`; run again to continue. If recovery is
ambiguous the command exits non-zero with `recovery_required` and starts nothing — run
`relay recover <id> --project-dir …` (or `--acknowledge` after confirming no target is running).

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

## Limitations

- The statusline only refreshes while an interactive Claude session is drawing it; headless
  (`-p`/`--bg`) sessions record nothing there, so the snapshot goes stale (≤10 min) and detection
  falls back to `UNKNOWN` rather than guessing.
- `rate_limit_event` capture happens only for headless processes Relay itself runs (the probe).
- The hook/statusline commands embed the path of the `relay` binary that installed them; re-run
  install after moving or upgrading it.
- The handoff still refuses while the source profile has any live Claude process (per profile).
- Validated on Claude Code 2.1.276 and 2.1.277.
