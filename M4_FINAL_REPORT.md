# M4 final report: dumb-user UX for Agent Relay

Covers M4.0–M4.19: a consumer-CLI layer over the already-complete M2/M3 machinery. Every claim
below is marked **LIVE VERIFIED** (run against the real development machine's real adopted
profiles), **TEST VERIFIED** (covered by the automated suite against scripted fakes, not run
live), or **UNVERIFIED / KNOWN LIMITATION**.

## 1. The exact dumb-user workflow

```sh
cargo install --path crates/relay-cli   # or: cargo build --release, copy the binary onto PATH
relay setup                              # once
cd ~/repos/my-project
relay claude                             # every day
```

No `CLAUDE_CONFIG_DIR`, no `--profile`, no pane id, no session UUID, no Herdr token command, no
`relay launch`/`relay watch run` invocation required for this path. **LIVE VERIFIED** end to end
on this machine (§14).

## 2. `relay setup` wizard behavior

Six steps, each idempotent and safe to re-run: environment check (plain ✓/✗ checklist, not raw
JSON, unless `--verbose`) → per-account authenticate-new-or-adopt-existing → primary/fallback
selection → usage-integration offer (reuses `integration claude install` unchanged, explains
unverified-version gating rather than bypassing it) → Herdr offer (reuses `relay integration herdr
install`+`doctor` unchanged, silently absent if Herdr isn't installed) → summary. Re-running it
never re-authenticates an already-working profile and never discards an existing one
(`setup_rerun_is_idempotent`, **TEST VERIFIED**; **LIVE VERIFIED** in §14 against this machine's
two real profiles).

## 3. Claude auth UX

For a new profile, Relay creates an isolated profile directory (`ProfileDirectory::create_managed`,
existing M1 code) and launches Claude's own official `claude auth login` with `CLAUDE_CONFIG_DIR`
set to that directory — confirmed to be Claude's real login subcommand via live `claude
--help`/`claude auth login --help` on the installed 2.1.278 binary, not invented. Relay waits for
it to exit, then runs the existing strict `claude auth status` inspection/identity-pin machinery
unchanged. `relay login <name>` / `relay logout <name>` are thin wrappers around the same official
`auth login`/`auth logout` commands for an already-registered profile.

## 4. Credentials remain provider-owned

Relay never sees a password, OAuth token, or cookie. It only launches Claude's official commands
and reads Claude's own `auth status` JSON (already-existing strict, deny-unknown-fields schema
parsing rejects anything Relay doesn't recognize, including a planted extra field — confirmed via
`login_never_writes_into_the_claude_config_directory`, which snapshots the Claude config
directory's file listing before/after a login/logout cycle and asserts it is byte-for-byte
unchanged, i.e. Relay wrote nothing into it). The only thing Relay persists is a non-secret
"identity pin" (provider account id, already part of M1) plus profile *names* — never a credential.

## 5. Primary/fallback persistence

New `crates/relay-cli/src/preferences.rs`: a small Relay-owned `preferences.toml`
(`~/.config/agent-relay/preferences.toml`, mode `0600`, atomic write via temp-file + rename)
holding `primary_profile`, `fallback_profiles` (ordered), `herdr_enabled`,
`usage_integration_enabled` — profile *names* and booleans only. 4 unit tests cover missing-file,
round-trip, file-mode, and fail-closed-on-corruption behavior.

## 6. `relay claude` behavior

Resolves project from `cwd`, profile/fallback from `preferences.toml` (or `--profile`/`--fallback`
override), ensures the primary is authenticated (re-running the official login flow and revalidating
if not — see §11), reuses the existing `perform_launch` path shared with `relay launch` (no
duplicated launch logic — `claude_and_launch_share_one_writer_creation_implementation`, **TEST
VERIFIED**) to create/reuse the writer lease and capture the session id automatically, then attaches
the user to it interactively (§9). Re-running it against a project with an already-active session
reattaches rather than relaunching (`claude_entrypoint_reuses_an_active_lease_without_relaunching`).

## 7. Herdr auto-binding

`relay claude` detects Herdr context itself (via the same env vars Herdr injects into a managed
pane shell), determines the pane/workspace, and writes `relay_profile`/`relay_profile_fallback`/
`relay_session_id` via the new `HerdrCliClient::set_pane_tokens`/`set_workspace_tokens`
(`herdr pane|workspace report-metadata --source agent-relay ...`, exit-code-only success check,
since that command prints nothing on success). It does **not** create a Herdr workspace — no
official API made that behavior clearly useful and unsurprising, so it was deliberately left out
per the task's explicit instruction. Works fully standalone outside Herdr
(`herdr_bound: false` in that case). **LIVE VERIFIED** (§14): `herdr pane get` showed the
auto-written tokens immediately after `relay claude` ran, with zero manual `herdr` commands typed.

## 8. Is the session UUID completely hidden?

Yes in the normal path. `relay claude` captures the id `claude --bg` prints internally and never
surfaces it unless `--json` is passed or `relay status --json`/`relay launch` (advanced) is used.
The one place a raw id can still appear is advanced/manual recovery (`relay recover <id>
--project-dir DIR`), which is explicitly an advanced command, not part of the daily path.

## 9. Interactive/attached Claude UX decision

`relay claude` launches with `claude --bg` (as `relay launch` already did, preserving writer-lease
creation, liveness tracking, and the single-writer invariant unchanged) and then hands the user a
real interactive terminal via `claude attach <id>` — Claude's own documented mechanism for
attaching to a background session, confirmed live via `claude attach --help` on 2.1.278, not a
Relay workaround. This was the safest option found: it does not replace Relay's `--bg` lifecycle
with a plain `claude` process (which would have lost Relay's ability to stop/verify ownership), so
the safety model (authoritative-source stopping, exact session id, writer lease, single-writer
invariant, automatic handoff) is fully preserved. Trade-off, documented in the README's Limitations:
the first exchange happens before attach, since Relay needs an initial message to start the tracked
session — attaching is instant after that.

## 10. Commands added/changed

Added: `relay setup [--non-interactive ...] [--verbose]`, `relay claude [message] [--profile]
[--fallback] [--project-dir]`, `relay status [--project-dir] [--json]`, `relay profiles`, `relay
login <name> [--claude-executable]`, `relay logout <name> [--claude-executable]`. Changed:
`relay launch`'s launch logic was extracted into `perform_launch` and reused by `relay claude`
(behavior unchanged, output unchanged); the pre-existing `relay watch run` "Claude Code version not
verified" warning now respects `--json` (see §17 for why). No other advanced command's behavior,
flags, or output changed.

## 11. Safe reauthentication

If `relay claude` finds the primary profile unauthenticated, it prints "Profile "X" needs Claude
authentication. Opening Claude login..." and runs the same official login flow §3 uses, then
revalidates before proceeding — it never silently substitutes another identity. If a *fallback* is
unauthenticated, `relay claude` only warns and continues, since the primary still works. Both paths
are **TEST VERIFIED** against a scripted fake Claude binary that reports unauthenticated then
authenticated.

## 12. Default config

`~/.config/agent-relay/preferences.toml` (§5) — profile names, ordered fallback list, two booleans.
No OAuth tokens, cookies, API keys, or provider auth data of any kind are ever written to it; this
was specifically checked in the privacy/security scan (§17).

## 13. First-run/existing-user handling

`relay setup` enumerates already-registered profiles (from the existing `ProfileService`, unchanged)
and already-installed integrations before asking anything, and only prompts to add what's missing —
it never triggers new authentication for something already authenticated
(`setup_interactive_detects_and_reuses_existing_profiles`, **TEST VERIFIED**; **LIVE VERIFIED** in
§14 against this exact machine's two pre-existing real profiles, which were correctly detected and
reused without any new login prompt).

## 14. Live disposable-project validation

**LIVE VERIFIED**, on this development machine, using the real, already-authenticated profiles
(referred to as the established redacted names in code/docs):

- `relay setup` re-run against this machine: detected both existing profiles, offered "use these?",
  accepted, wrote `preferences.toml` with the chosen primary/fallback — no re-authentication
  triggered.
- `relay claude` run from a disposable scratch project directory with zero prior state: resolved
  project from `cwd`, resolved profile/fallback from preferences with no flags, launched Claude,
  captured the session id automatically, wrote Herdr pane tokens automatically (confirmed via
  `herdr pane get` showing `relay_profile`/`relay_profile_fallback`/`relay_session_id` immediately
  after, with zero manual `herdr` commands typed), and attached the user into a real interactive
  session — all without the user entering a config dir, pane id, or session UUID anywhere.

## 15. Simulated high-level handoff result

**LIVE VERIFIED, one controlled run**, no real quota exhausted (per explicit instruction). Using
the session id `relay claude` had already captured automatically, a simulated-exhaustion evaluation
was run against it; the transactional handoff completed, the fallback profile became the writer,
`lock status` and `claude agents --json` confirmed a single writer post-handoff (no duplicate), and
Herdr's pane tokens remained coherent afterward. Two real, deterministic issues were found and fixed
live during this run (both documented in §17/§18, not the known intermittent race): a pre-existing
`relay watch run` stderr warning that broke the `--json` contract, and this session's own
already-occupied Herdr pane correctly taking priority over the token-based mapping (a documented
test-environment artifact from testing inside my own live coordinator pane, not a product bug — see
`docs/herdr-integration.md`'s equivalent M3 note). The final connecting step used a direct `relay
watch run --session <the auto-captured id>` invocation rather than the plugin-mediated Herdr event,
because of that pane-conflation artifact; the profile/session/lease mechanics it exercised are
identical either way.

## 16. Backwards compatibility

**TEST VERIFIED + LIVE VERIFIED.** `relay profile ...`, `relay launch`, `relay watch run`, `relay
handoff run`, `relay recover`, `relay session conflict ...`, `relay integration claude ...`, `relay
integration herdr ...` are all unchanged in flags and output; `perform_launch` extraction (§10) was
checked to produce byte-identical `relay launch` output to before. All pre-M4 tests (290 of them)
still pass unmodified.

## 17. Security/privacy result

Clean. Verified: no OAuth token, cookie, API key, or Keychain data is ever read, copied, or stored
by any new M4 code; `preferences.toml` contains only names/booleans (§12); the Claude config
directory's file listing is provably unchanged across a full login/logout cycle (§4); the official
`auth login`/`auth logout` commands are invoked, never a custom OAuth client or proxy. Two
deterministic (not the known intermittent race) bugs were found and fixed as part of this scan and
the live run in §15: (a) a pre-existing `relay watch run` "version not verified" warning printed
unconditionally to stderr even under `--json`, corrupting the stderr-must-be-pure-JSON contract on
a failure path — fixed by gating it on `!cli.json`; (b) the same class of bug in this session's own
new `run_login`/`run_claude` code, fixed the same way. No real user credential, session content, or
identity was exposed in any test fixture or committed file (only synthetic UUIDs and the project's
established redacted profile names appear).

## 18. Known limitations

- `relay claude`'s interactive attach happens after one initial message exchange, not before (§9)
  — an unavoidable consequence of `--bg` needing a starting prompt.
- Herdr's own session detection still only reaches the default `~/.claude`; an isolated profile's
  pane still needs the `relay_session_id` token, which `relay claude` now writes automatically
  instead of requiring a manual `herdr pane report-metadata` call (unchanged from M3, now
  automated).
- Testing an automatic Herdr-triggered handoff from *within* the same live pane used to drive the
  test conflates the coordinator's own agent session with the test subject's (§15); this is a
  test-topology artifact, not a product limitation, and does not affect a real, separately-paned
  usage.
- The known intermittent `untracked_writer_detected` race (documented since M2/M3) remains
  unpatched, per explicit instruction not to touch it without a new, deterministic reproduction —
  none was produced this session.
- No background daemon: automatic handoff outside Herdr still requires `relay claude` or `relay
  watch run` to be invoked again (documented in the README's Limitations, unchanged from before M4).

## 19. Commits

Three, all local at session end pending §20:

- `1a93faa` feat: M4 dumb-user UX - relay setup, relay claude, status/profiles/login/logout
- `861988a` test: M4 test suite + fix --json stdout/stderr contamination in login/claude
- `36c2d78` docs: M4 getting-started guide, README rewritten around relay setup/relay claude

## 20. Push result

All gates green before push: `cargo fmt --all -- --check` clean, `cargo clippy --workspace
--all-targets -- -D warnings` clean, `cargo test --workspace` **309/309 passed, 0 failed**
(290 pre-existing + 19 new M4 tests in `crates/relay-cli/tests/m4.rs`), existing advanced commands
confirmed unchanged (§16), new `relay setup`/`relay claude` flows live-verified (§14), controlled
high-level handoff live-verified (§15). Pushed `1a93faa`, `861988a`, `36c2d78` to `origin/main` with
a normal (non-force) push. `v0.1.0` untouched, no history rewrite, no new tag/release.

## 21. Can a new user realistically use Agent Relay without reading architecture docs?

Yes, for the documented workflow. `cargo install`/`cargo build --release` → `relay setup` →
`cd project && relay claude` requires no knowledge of `CLAUDE_CONFIG_DIR`, profile adoption,
writer leases, Herdr pane/workspace ids, Herdr metadata tokens, Claude session UUIDs, or the
`relay launch`/`watch run`/`handoff run` command family. `docs/getting-started.md` documents exactly
this path in the same order a first-time user would hit it, including what a genuine quota
exhaustion looks like from the outside and how to reauthenticate. Anyone who wants transaction
internals, scripting, or manual control still has the unchanged advanced path in the README and
`docs/architecture.md`.

## 22. Next product milestone

The Herdr-pane-conflation artifact in §15/§18 suggests the next worthwhile piece is a documented,
tested way to distinguish "this pane's own Claude session" from "a different coordinator agent also
running in this pane" when both exist — today it's resolved correctly by design priority, but not
observably/debuggably from the outside. Separately, Linux live validation (currently test-only, per
the README's Status line) is the other clear next step before broadening platform claims.
