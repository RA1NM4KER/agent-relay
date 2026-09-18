# M0 research report

Research date: 2026-09-18

This report records source-level research used to choose Agent Relay's architecture. Repositories were cloned into a temporary directory outside this repository. No upstream source was copied into Agent Relay, and no real Claude authentication or global configuration was changed.

Post-M0 validation note: the project owner separately validated Claude Code 2.1.276 authentication persistence in isolated Erika and Megan directories and confirmed that the default Claude account remained unaffected. M1.5 subsequently inspected both profiles read-only. This strengthens the profile-isolation evidence but does not validate cross-profile session transfer.

## Executive conclusion

Claude Code officially supports side-by-side isolated environments through `CLAUDE_CONFIG_DIR`. On current macOS builds, authentication is scoped to the configuration directory through a directory-derived Keychain service, with a mode-0600 credentials file as a documented fallback. This is sufficient to keep profile A and profile B authenticated without logging the whole machine out.

Native cross-profile session continuation is possible today, but it is not an official transfer API. Claude stores conversation transcripts locally under the active configuration directory, and `--resume` only discovers transcripts visible there. The current practical mechanism is therefore:

1. fully stop and quiesce the source session;
2. copy the transcript and required session sidecars to the target profile's matching project store using staged, verified writes;
3. launch the target profile with `claude --resume <session-id>`; and
4. verify the target's `SessionStart` event, identity, project, session ID, and transcript location.

Nemo's account switcher provides current working precedent for this mechanism. However, Claude's transcript layout and schema are undocumented implementation details. Agent Relay must version-gate this capability and fall back to an explicitly labelled **STATE CONTINUATION** packet whenever compatibility or verification is uncertain. A copied session keeps its local conversation ID, but provider-side prompt caches and any account-scoped server state do not transfer.

The three Herdr ecosystem plugins solve useful adjacent problems but do not supply a stable end-to-end cross-account transaction. Agent Relay should remain self-contained for correctness and expose optional, capability-detected integrations.

## Method

For each upstream project, the default branch was inspected at the exact revision below, together with releases/tags, manifests, relevant source, and documentation. Installed CLI behavior was checked locally using read-only commands:

- Claude Code `2.1.276`
- Herdr `0.9.0` (the inspected upstream source is `0.9.1`)

A safe isolation probe ran `claude auth status --json` with two fresh temporary `CLAUDE_CONFIG_DIR` values. Each environment resolved a distinct config and projects directory, reported `loggedIn: false`, and created only mode-0600 local metadata inside its temporary directory. This proves configuration-directory resolution; it does not constitute a two-account transfer test.

## Decision matrix

| Component | Decision | Reason |
|---|---|---|
| Nemo account switcher | Borrow concepts; independently implement the narrow transfer adapter | Strong current evidence, but the file layout and Keychain behavior are provider internals and the project exposes no stable library API |
| Cody Hutson account switcher | Borrow checkpoint, redaction, and rollback concepts | It targets Claude Desktop and global process switching rather than isolated Claude Code profiles |
| `cswap` | Do not depend or copy its rotation architecture | Credential rotation and threshold-driven switching conflict with Relay's explicit-handoff model |
| `herdr-catchup` | Optional transcript/state-continuation integration | It identifies sessions well, but does not transfer a Claude transcript between config profiles |
| `herdr-agent-usage` | Optional usage adapter | It has excellent multi-profile attribution, but its collection command is not documented as a stable public API |
| `herdr-claude-auto-retry` | Coexist; borrow parser/test ideas only | Current Claude hooks provide a better structured hard-limit signal; this plugin retries in an existing pane rather than handing off |
| Herdr | First-class adapter through documented CLI/plugin APIs | Herdr provides pane/session identity and lifecycle, while Relay retains handoff policy and safety |

## Upstream projects

### Nemo-Illusionist/claude-code-account-switcher

- Repository: <https://github.com/Nemo-Illusionist/claude-code-account-switcher>
- License: MIT
- Revision inspected: `eb445dfe590a979f5d606f70caed7e9ce8f51e57`
- Release inspected: `v0.18.2` (2026-09-17)
- Implementation: Rust

Relevant behavior:

- A profile is a complete Claude configuration directory selected with `CLAUDE_CONFIG_DIR`.
- Login invokes Claude's normal `auth login` flow inside that directory.
- Before launching Claude, the process environment removes credential overrides including `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, and the Bedrock bearer token.
- Session files are found at `<config-dir>/projects/<cwd-slug>/<session-id>.jsonl`.
- A session copy stages the transcript as `.jsonl.part`, renames it into place, and recursively copies the optional `<session-id>/subagents` sidecar directory.
- The destination retains the same session ID. Source and target copies subsequently diverge, so an unqualified resume can become ambiguous.
- The first request after copying has a cold prompt cache and may resend substantial history.
- On macOS, current Claude credentials are stored under a Keychain service derived from the absolute config-directory path. The implementation also contains legacy fallbacks and defensive restoration because Keychain behavior has changed over time.
- Usage polling reads credentials and calls an undocumented Anthropic OAuth usage endpoint.

API stability and brittle assumptions:

- The CLI is not a supported library interface for Relay.
- Project slugging, transcript filenames, sidecar structure, Keychain service naming, and the usage endpoint are reverse-engineered.
- Staging makes the main transcript copy safer, but the recursive sidecar copy is not one atomic transaction.
- Copying over a destination can erase or fork an independently advanced session unless divergence is detected first.

Useful ideas:

- Full config-directory isolation rather than credential swapping.
- Sanitize authentication override variables before launch.
- Pin non-secret account identity and detect a wrong login.
- Quiesce, stage, checksum, rename, and then verify a copied session.
- Treat a copied session as a forkable local artifact and surface ambiguity.

Security concerns:

- Transcripts can contain source code, tool output, secrets, and user messages.
- Credential import/re-key logic necessarily handles secrets and is out of scope for Relay.
- Direct OAuth usage calls increase secret exposure and depend on an undocumented API.

Agent Relay decision: do not depend on the executable and do not copy credential-management code. Independently implement a narrow, versioned Claude session-store adapter after real-account approval. If any MIT code is later adapted, preserve its notice and record the exact files in `THIRD_PARTY_NOTICES.md`.

Primary evidence: [session documentation](https://github.com/Nemo-Illusionist/claude-code-account-switcher/blob/eb445dfe590a979f5d606f70caed7e9ce8f51e57/docs/sessions.md), [session copy source](https://github.com/Nemo-Illusionist/claude-code-account-switcher/blob/eb445dfe590a979f5d606f70caed7e9ce8f51e57/src/sessions.rs), and [launch environment policy](https://github.com/Nemo-Illusionist/claude-code-account-switcher/blob/eb445dfe590a979f5d606f70caed7e9ce8f51e57/src/environment.rs).

### cody-hutson/claude-account-switcher

- Repository: <https://github.com/cody-hutson/claude-account-switcher>
- License: MIT
- Revision inspected: `d0803316cb72d6ff0145900dbc5d86edaa66aa7c`
- Release inspected: no tagged release at the inspected revision
- Implementation: Electron/macOS application

Relevant behavior:

- Launches separate Claude Desktop instances with per-account `--user-data-dir` values.
- Shares the normal `~/.claude` Code transcript tree.
- Produces a privacy-oriented `SWAP_HANDOFF.md` containing Git state and operational context.
- Attempts a graceful application quit, then escalates through process signals with PID checks.
- Redacts common token-shaped values in its handoff output.

Limitations for Relay:

- It targets Claude Desktop rather than Claude Code's supported config-directory mechanism.
- Its process model is application-wide and macOS-specific.
- It does not implement a transactional, per-project single-writer lease or verified cross-config transcript transfer.

Agent Relay decision: borrow the checkpoint, redaction, visible handoff, and rollback concepts. Do not depend on it and do not copy its application-wide process-switching strategy.

### danpoyar/cswap

- Repository: <https://github.com/danpoyar/cswap>
- License: MIT
- Revision inspected: `c14ab714adf23a7e273a1bdb2a571d65c09fcfb5e`
- Version inspected: `0.24.0b1`
- Implementation: Python

Relevant behavior:

- Maintains multiple account slots and session profiles.
- Polls an undocumented OAuth usage endpoint and selects accounts using thresholds, hysteresis, and cooldowns.
- Handles macOS Keychain records, credentials-file locks, refresh-token lineage, and live-session safeguards.
- Can symlink a shared projects history tree across profiles.

What must not be copied:

- Automatic credential rotation and threshold-driven quota aggregation.
- Mutating a default/live credential store to switch an already running workload.
- Refresh-token handling or local token custody.
- A shared writable transcript tree, which makes profile ownership and rollback ambiguous.

Useful concepts:

- Fail closed to `UNKNOWN` when usage cannot be established.
- Cooldown and hysteresis are relevant only to a much later, opt-in automatic mode.
- Credential and session-store lock ordering demonstrates how easy deadlocks and partial mutations are in this domain.

Agent Relay decision: no dependency. Borrow only general reliability ideas. Relay's explicit profile handoff must remain visibly different from `cswap`'s account-rotation model.

### wilbeibi/herdr-catchup

- Repository: <https://github.com/wilbeibi/herdr-catchup>
- License: MIT
- Revision inspected: `51f690964aa491447baa39c732c5333ed0de315a`
- Manifest version inspected: `0.6.0`; latest published GitHub release observed: `v0.4.0`
- Minimum Herdr version: `0.7.5`

How it identifies sessions:

- It reads the focused Herdr pane, calls `herdr agent get`, and uses the exact `agent_session.value` when available.
- If identity is unavailable, its underlying `catchup` tool can fall back to the newest transcript in a directory. Relay must not use this heuristic for a destructive operation.

How handoff/resume works:

- Same-agent Claude forking runs `claude --resume <id> --fork-session` in the currently selected environment.
- Cross-agent handoff renders a transcript into plugin state and instructs a target agent to read it. That is state continuation, not native conversation continuation.
- Pane and workspace actions use Herdr's documented CLI and managed-pane interfaces.

Can Relay call it?

Yes, optionally, for transcript rendering or a user-selected state-continuation experience. It cannot be the correctness foundation because the external `catchup` binary and plugin version become additional failure surfaces.

Can it move a Claude session between authenticated config profiles?

No. Its native Claude resume assumes the session is already visible in the active `CLAUDE_CONFIG_DIR`; it does not copy the session store or change authentication profiles.

Agent Relay decision: optional integration only. Reuse the exact-session-ID and managed-pane concepts, and keep Relay's own minimal packet fallback so recovery has no plugin dependency.

The underlying [`catchup`](https://github.com/wilbeibi/catchup) tool was also inspected at `ec551b22ae302788aa1b803e3bfd25180c10883a` under MIT. It explicitly parses undocumented Claude JSONL and therefore does not turn the format into a stable API.

### senna-lang/herdr-agent-usage

- Repository: <https://github.com/senna-lang/herdr-agent-usage>
- License: MIT
- Revision inspected: `cfd84237738c3e653274b4f0090cf1ab779234e8`
- Release inspected: `v0.5.14` (2026-09-15)
- Minimum Herdr version: `0.7.5`
- Implementation: Go

Multiple-account model:

- Claude profiles have an ID, label, `config_dir`, and optional `.claude.json` path.
- Attribution requires a strict resolved-path match. Unmatched profiles remain unassigned instead of being guessed.
- Each profile owns independent cache, state, and transcript roots.

Usage information:

- For Claude it primarily consumes local `cachedUsageUtilization` information supplied through Claude's status-line input and local `.claude.json` cache.
- Its `usagebar collect` command emits JSON suitable for diagnostics, including providers and API usage. The project does not document that command as a compatibility-stable API for other plugins.
- Missing or stale data remains visibly unknown.

Agent Relay decision: offer a capability-detected optional adapter after standalone usage works. Do not link its internal Go packages and do not make handoff safety depend on it. Prefer Claude hooks/status-line data for Relay's own hard-limit state and preserve `UNKNOWN` when confidence is low.

### mo-arvan/herdr-claude-auto-retry

- Repository: <https://github.com/mo-arvan/herdr-claude-auto-retry>
- License: MIT
- Revision inspected: `23ad3448b7f77d46756e5901b583c372ac33e72a`
- Release inspected: `v1.3.0` (2026-09-02)
- Minimum Herdr version: `0.7.5`

Relevant behavior:

- Parses the latest Claude output block and status/footer using paired limit and reset-time patterns to reduce false positives.
- Distinguishes transient failures, parses absolute and relative reset times, handles time zones, and provides a configurable fallback wait.
- Waits and then sends a retry prompt to the existing Herdr pane. It does not change profiles or reconstruct a stopped session.

Current reuse decision:

Claude Code now exposes a structured `StopFailure` hook whose `error` can be `rate_limit`. Relay should prefer that provider signal. A terminal-output parser can exist as an isolated, fixture-tested fallback, drawing on this plugin's conservative pattern strategy. Agent Relay should coexist with the plugin but not depend on it.

### Herdr core and official documentation

- Repository: <https://github.com/herdrdev/herdr>
- License: Apache-2.0
- Revision inspected: `da6bcd5969779bfe0396bcf89a8025d4375d611e`
- Source version inspected: `0.9.1` (2026-09-18)
- Locally installed version: `0.9.0`

Plugin API findings:

- `herdr-plugin.toml` declares `id`, `name`, `version`, `min_herdr_version`, actions, event hooks, managed panes, and startup behavior.
- Commands are argument arrays, avoiding shell interpolation. Plugin processes are not sandboxed and inherit their environment, so the plugin remains a meaningful trust boundary.
- The CLI is the recommended integration API. The raw socket API is appropriate only when a long-lived event subscription is required; its schema can be queried with `herdr api schema --json`.
- Action parameters are not currently supported. Relay actions should therefore open an interactive selector or call a deterministic configured command.
- Managed panes can receive a working directory and explicit environment entries. Herdr-owned environment variables remain authoritative.
- The official Claude integration reports `SessionStart` data including session ID and transcript path. Herdr can restore a known Claude session with `claude --resume <id>`, but only when that transcript is visible in the launched profile.
- Herdr's native session restore handles server/pane reconstruction; it does not transfer a session file or authenticated identity between config directories.
- Marketplace discovery indexes public, non-fork GitHub repositories carrying the `herdr-plugin` topic and a valid manifest. Agent Relay will not publish or add the topic during development.

Integration decision:

- Use the documented Herdr CLI with JSON output and feature detection.
- Let Relay own the transaction, lock, profile environment, and verification.
- Use focused-pane context to identify project/session and a managed replacement pane for target launch.
- Stop and verify the source before target launch. If same-pane replacement is unsafe, keep the stopped pane as evidence until the new pane verifies, then close it.
- Target Herdr `0.9.0` initially, because that is the locally testable stable surface; lower compatibility can be added after fixture testing.

References: [session state](https://herdr.dev/docs/session-state/), [integrations](https://herdr.dev/docs/integrations/), [socket API](https://herdr.dev/docs/socket-api/), [CLI reference](https://herdr.dev/docs/cli-reference/), and [marketplace](https://herdr.dev/docs/marketplace/).

### ogulcancelik/herdr-plugin-examples

- Repository: <https://github.com/ogulcancelik/herdr-plugin-examples>
- License: no license file or SPDX declaration found at the inspected revision
- Revision inspected: `18709cdc851dd63ed0543eb8388343a5446fd8d8`
- Release inspected: none

The examples demonstrate the expected directory layout and Herdr 0.7-era manifest shapes. They are a useful packaging reference, but absent an explicit license Agent Relay must not copy their source or manifest text. Current official Herdr documentation and schema remain authoritative.

## Current Claude Code behavior

### Officially supported

- `CLAUDE_CONFIG_DIR` moves the configuration, session-history, and plugin directories and is explicitly documented for side-by-side account use.
- `claude auth login`, `auth status --json`, and `auth logout` operate against the selected environment.
- On macOS, credentials normally live in Keychain; the service is scoped to the config path on current versions. The documented fallback is `<config-dir>/.credentials.json` with mode 0600. Linux uses the protected credentials file.
- Authentication can be overridden by environment variables or cloud-provider modes. A Relay profile is not trustworthy unless these overrides are rejected or explicitly surfaced.
- `--resume [session]` resumes a chosen local conversation, `--continue` selects the latest conversation in the current directory, and `--fork-session` creates a new ID when resuming.
- Session transcripts are local plaintext JSONL and can include full messages, tool calls, and tool outputs.
- `SessionStart` hooks receive `session_id`, `transcript_path`, `cwd`, and a source such as `resume`.
- `StopFailure` hooks provide a structured error classification, including `rate_limit`. A normal `Stop` hook is not emitted for every user interrupt, and `SessionEnd` is time constrained.
- `CLAUDE_CODE_SESSION_ID` is made available to hook/tool subprocesses and matches the resumed session ID.

Official references: [environment variables](https://code.claude.com/docs/en/env-vars), [authentication](https://code.claude.com/docs/en/authentication), [sessions](https://code.claude.com/docs/en/how-claude-code-works), [the `.claude` directory](https://code.claude.com/docs/en/claude-directory), and [hooks](https://code.claude.com/docs/en/hooks).

### Reverse-engineered or unsupported

- The exact projects-directory slug algorithm, JSONL schema, sidecar layout, Keychain service-name algorithm, and retention interactions are not public compatibility contracts.
- Copying a transcript between two config directories is not an official Claude command.
- The OAuth usage endpoint used by account switchers is undocumented.
- A copied transcript does not move prompt-cache state. The first target request may resend the full conversation and consume more tokens than expected.
- Account-specific server-side artifacts, if any, cannot be assumed to transfer just because the local session ID is retained.

### Answer to the primary architectural question

**Can profile A hand the same project and Claude conversation to profile B without a machine-wide logout?**

Qualified yes:

- Authentication isolation: **sound and officially supported** through distinct absolute `CLAUDE_CONFIG_DIR` values, provided Relay neutralizes higher-precedence authentication overrides and verifies target identity.
- Local conversation transfer: **technically demonstrated by current third-party implementations, but unsupported as a stable provider API**. It needs a versioned compatibility adapter, source quiescence, complete artifact discovery, checksums, divergence checks, and target-hook verification.
- Semantic continuity: **best effort**. The local transcript and session ID can be preserved; provider-side caches cannot.
- Product guarantee: Relay must report `SESSION CONTINUATION` only after verification. Every other successful fallback is `STATE CONTINUATION`.

The real cross-account **session-transfer** experiment remains deliberately undone. M1.5 authorizes only read-only authentication inspection and adoption dry-runs; it does not test transcript movement or resume behavior.

## Authentication and profile-isolation decision

Each Relay profile points to a dedicated directory owned by the current user and mode 0700. Relay invokes Claude's normal browser authentication flow in that directory and never asks for or copies a token. After login it runs `claude auth status --json` under the same sanitized environment and stores only a non-secret identity pin sufficient to detect a wrong-account login.

Before every launch, Relay:

1. resolves and validates the absolute profile directory;
2. rejects symlink escapes and unsafe permissions;
3. removes or reports provider-selection and credential environment overrides;
4. runs an authentication preflight;
5. verifies the observed identity against the profile's pin; and
6. passes `CLAUDE_CONFIG_DIR` structurally to the child process.

Relay never imports credentials, enumerates Keychain secret values, or copies `.credentials.json`.

## M1.5 real-profile adoption validation

Agent Relay now implements a read-only `claude auth status --json` adapter for Claude Code 2.1.x. It validates the executable, enforces a ten-second timeout and output bounds, discards stderr, parses a closed JSON schema, and never persists raw output. Supported credential, identity, endpoint, and cloud-routing environment variables are reported by name and presence only; any conflict prevents Claude from launching.

The non-secret identity pin is versioned and contains:

- provider account UUID/ID when supplied, as the primary stable identifier;
- normalized account email as a fallback and human-auditable identity;
- organization ID when supplied, to scope the email fallback;
- authentication method and API provider as context that must remain consistent.

Account ID takes precedence when available. Without an account ID, normalized email plus organization ID is used. If neither account ID nor email is present, identity cannot be established and adoption fails closed. Unknown top-level fields, conflicting aliases, malformed types, or a future Claude version also fail closed until fixtures and the compatibility table are updated.

After the owner corrected the profile preconditions, Relay validated both mode-0700 directories on 2026-09-18. Claude Code 2.1.276 returned these top-level fields for each profile: `analyticsDisabled` (boolean), `apiProvider` (string), `authMethod` (string), `configDirectory` (string), `email` (string), `loggedIn` (boolean), `orgId` (string), `orgName` (string), `projectsDirectory` (string), and `subscriptionType` (string). Relay added this exact schema to its fixtures and now verifies the two reported directory paths against the selected canonical profile directory.

Both inspections and both adoption dry-runs passed their individual safety checks. Neither result supplied an account UUID, so the identity pin used normalized email plus organization ID, with authentication method and API provider as context. The observed identity for **both** directories was:

- email: `megan@schoolscape.co.za`;
- organization ID: `d9e018d0-04b9-4edf-9749-5bc832f036f5`;
- authentication method: `claude.ai`;
- API provider: `firstParty`.

This is a material safety finding: Erika and Megan currently resolve to the same pinned Claude identity. It demonstrates that pin comparison works, but it does not establish two distinct authenticated accounts. Relay must not treat these directories as distinct-account fallbacks unless Erika is intentionally an alias and that limitation is made explicit. No raw provider output was persisted or surfaced, no credential or Keychain contents were inspected, and recursive filesystem metadata for both profile trees and the default `~/.claude` tree was unchanged before and after validation. Cross-profile session transfer remains unverified.

## Usage decision

Usage is advisory and cannot be allowed to weaken handoff safety.

Priority order:

1. structured official provider events such as `StopFailure(error = rate_limit)`;
2. documented/local status-line or cache signals with freshness metadata;
3. optional `herdr-agent-usage` adapter when a compatible interface is detected;
4. versioned terminal-output parsing as a last resort; and
5. `UNKNOWN`.

Relay will not store OAuth tokens or call an undocumented usage endpoint in v0.1. It will never synthesize percentages from weak signals.

## Licensing implications

Original Agent Relay code is licensed under Apache-2.0, which is compatible with using MIT-licensed dependencies or adapted code while providing an explicit patent grant for Relay contributors. Any copied or modified MIT source must retain the upstream copyright and permission notice. Researching behavior and independently implementing an interface does not copy code, but attribution remains appropriate.

The Herdr core is Apache-2.0. The three inspected Herdr plugins, Nemo's switcher, Cody Hutson's switcher, `cswap`, and `catchup` are MIT. The plugin-examples repository has no detected license and is reference-only. No upstream code has been incorporated at M0.

## Research limitations and validation gates

- No real profile was authenticated or logged out.
- No live two-account transcript was transferred.
- Claude's version can change independently of Relay. Native transfer must use a tested compatibility table, not an open-ended semver assumption.
- A successful `claude --resume` process start is insufficient verification; the structured target event and identity checks are required.
- The architecture must continue to provide safe state continuation if native transfer disappears entirely.
