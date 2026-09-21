# Security model

Status: initial M0 threat model. It must be reviewed at every milestone that adds a mutation surface.

## Security objectives

1. Agent Relay never becomes a credential vault.
2. At most one Relay-managed writer can mutate a canonical project.
3. A handoff cannot be reported complete without positive target verification.
4. Interrupted work remains recoverable from durable, minimally sensitive state.
5. Project-controlled input cannot choose arbitrary executables, escape configured directories, or become shell code.
6. Logs and machine output do not expose provider secrets or conversation contents by default.

## Assets

- Claude and Codex credentials held by the provider's config directories (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`) and, for Claude, the OS Keychain.
- Source code, uncommitted work, and repository metadata.
- Claude transcripts and Codex thread history, which may contain source, prompts, tool output, and secrets.
- Profile identity and usage metadata.
- Handoff journal integrity and writer ownership.
- Local process-control authority.

## Trust boundaries

- **User config/state:** trusted only after ownership and permission validation.
- **Project checkout and `.relay`:** untrusted; a cloned repository may contain malicious files or symlinks.
- **Provider CLI and hooks:** privileged integration boundary; structured output is validated against the launched process and expected paths.
- **Provider terminal output:** untrusted text, even when it resembles a status message.
- **Herdr/plugin environment:** trusted only for documented fields after validation; plugins are unsandboxed local programs.
- **Operating system process table and locks:** authoritative when queried through platform adapters, with known race limitations.
- **Third-party optional plugins:** unavailable or adversarial from core correctness's perspective.

## Non-negotiable invariants

- Credentials are never copied between profiles or written into Relay state.
- A profile launch always uses an absolute validated config path and a controlled executable resolution.
- Provider authentication overrides cannot silently supersede the selected profile.
- Project identity uses canonical paths and a trusted state-directory lock.
- A healthy writer lock is never silently broken.
- Source and target agents are never simultaneously live as Relay-authorized writers.
- An unverified target is stopped before retry, fallback, or source restoration.
- Native continuation is never inferred from process exit status or matching text alone.

## Threats and mitigations

### Credential leakage

Threats include logging environment variables, copying provider credential files, reading Keychain values, overly permissive config directories, crash dumps, or passing secrets on command lines.

Mitigations:

- invoke provider-owned authentication; do not accept pasted tokens;
- never enumerate or persist Keychain secret values;
- create profile directories mode 0700 and state files mode 0600;
- redact variable values and token-shaped text from diagnostics;
- pass only required environment entries to helpers;
- store no full environment snapshots;
- prohibit credentials, cookies, and auth headers in events;
- test serialization with planted canary secrets.

### Wrong-profile authentication

Environment variables, cloud-provider modes, a stale login, or selecting the wrong browser account can cause the target to use an unexpected identity.

Mitigations:

- enforce a provider-specific authentication environment policy;
- call `claude auth status --json` under exactly the launch environment;
- pin a non-secret stable identity where available and require confirmation on mismatch;
- compare pins across profiles and never infer distinct accounts from distinct directory or profile names;
- display the active profile and verified identity on every handoff;
- treat identity uncertainty as `AUTH_REQUIRED` or `UNKNOWN`, never available.

### Two simultaneous writers

Races can occur between two Relay processes, between a handoff and recovery, or through a manually launched provider outside Relay.

Mitigations:

- OS advisory writer lock plus a separate orchestration lock;
- trusted user-state lock path derived from canonical project identity;
- coordinator reservation across the stopped-source gap;
- PID and process-start identity metadata;
- atomic, revisioned journal updates;
- verify source death before target start;
- clearly state that processes launched outside Relay cannot be technically prevented, and detect/report known provider processes where safely possible.

### Stale locks and PID reuse

A crash can leave metadata while the OS lock is released; a PID can later identify a different process.

Mitigations:

- treat the OS lock, not metadata age, as authority;
- bind metadata to process start time or platform process identity;
- recover only after proving absence or mismatch;
- refuse automatic recovery when the platform cannot establish identity.

### Malicious project configuration

A repository can contain symlinks, path traversal, huge files, invalid JSON, executable-looking packet content, or a configured binary path.

Mitigations:

- treat `.relay` as data, cap file sizes, and validate schema/version;
- open files without following symlinks where supported and verify inode/path after open;
- keep security-critical locks outside the repository;
- forbid project config from overriding provider/Relay executable paths;
- constrain profile names and resolve paths beneath approved roots;
- never execute or interpolate `handoff.md`;
- ignore unknown fields only where forward compatibility is explicitly safe.

### Path alias and symlink attacks

Different spellings of a path could obtain separate locks or redirect an atomic write.

Mitigations:

- canonicalize the existing project root before ID derivation;
- record device/inode identity on Unix where useful;
- validate destination parents immediately before staged rename;
- reject profile/state roots that are symlinks or owned by another user;
- test spaces, Unicode, symlink aliases, and parent replacement races.

### Shell and command injection

Profile names, paths, pane titles, session IDs, or provider output could be interpreted as shell syntax.

Mitigations:

- use structured process APIs and argv arrays exclusively;
- never use `sh -c`, `eval`, or generated command strings;
- validate opaque IDs by provider-specific grammar and length;
- use Herdr manifest command arrays;
- keep display escaping separate from process arguments.

### Provider-output injection and false limits

A project or agent can print “Usage limit reached,” fake a session ID, or emit control sequences.

Mitigations:

- prefer authenticated process-associated hooks and official JSON output;
- bind hook events to a per-launch nonce and expected process/session context;
- treat terminal parsing as low-confidence advisory input;
- require paired/versioned patterns and fixtures for fallback parsers;
- strip control sequences and cap captured output;
- never trigger a destructive transition from display text alone.

### Session corruption or divergence

Copying a live, partial, or independently advanced transcript can corrupt history or erase target work.

Mitigations:

- stop and verify the source before discovery/copy;
- compute artifact hashes and sizes before and after copy;
- stage on the destination filesystem, sync, and atomically rename;
- detect an existing destination and refuse if it is not byte-identical;
- retain the source and use backups for Relay-created destination artifacts;
- version-gate layouts and refuse unknown schemas;
- test crash points around every artifact transition.

### Conversation-content leakage

Transcripts and handoff packets may reveal sensitive code or prompts to another account or to version control.

Mitigations:

- require explicit target selection and show that conversation content will cross the profile boundary;
- copy only required session artifacts;
- keep `.relay` gitignored and files restrictive;
- default packets to filenames and operator-supplied summaries, not diffs or transcript excerpts;
- provide inspection and deletion commands before release;
- never include conversation contents in JSONL events.

### Plugin compromise and inherited environment

Herdr plugins are unsandboxed and can inherit secrets. A malicious plugin could invoke Relay with forged context.

Mitigations:

- keep the Herdr plugin minimal and auditable;
- use documented CLI JSON and validate focused pane/project identity;
- require the same core locks/preflight regardless of caller;
- do not grant plugin calls a bypass flag;
- send a minimal environment to managed panes;
- feature-check Herdr versions and fail closed.

### Adoption and the in-agent control channel

Adopting a conversation (`relay claude --resume`, `/relay adopt`) creates a writer lease, so it is held
to the same standard as starting one. Identity is never taken from model output, terminal text or "the
most recent" heuristics: it is the hook payload Claude writes (`session_id`, `cwd`, `transcript_path`), the
`CLAUDE_*` environment of the Claude process, and Claude's own live-session registry, which must agree and
name a running process; the profile is the one registered profile whose config directory *is* the running
Claude's, with its identity pin checked in a separate process with credential-override variables removed.
Any disagreement refuses adoption and changes nothing; a live Relay writer refuses it too. The lease is
written once, atomically, under the orchestration lock.

`/relay switch` uses a control directory inside the project's own Relay state (mode 0700, same user
only — no socket, daemon or network). The supervisor honours a request only if it names the current
lease's session and owner and comes from the exact process it launched; stale (older than 60 seconds) or
mismatched requests are refused, and the transaction itself is the ordinary `relay switch`. The hook
runs with the user's own privileges, so the trust boundary is unchanged: anything that can write the
project's Relay state can already write its lease.

### Dangerous automatic handoff

An incorrect limit signal or target choice could stop useful work or appear to evade provider limits.

Mitigations:

- omit AUTO from v0.1;
- require explicit target selection in the first product milestone;
- keep usage data advisory and explain every recommendation;
- if added later, make AUTO per-project opt-in, immediately disableable, and subject to the identical transaction and audit path.

### Denial of service and resource exhaustion

Huge transcripts, hung children, event floods, or lock contention can make Relay unavailable.

Mitigations:

- size/time limits with explicit override for legitimate large sessions;
- bounded output buffers and event rates;
- graceful-stop deadline followed by an explicit escalation policy;
- no silent lock stealing;
- recoverable state before waiting on external processes.

## Event and diagnostic policy

Allowed by default:

- profile-local name and provider;
- project ID and canonical path (path can be redacted in exported diagnostics);
- session ID;
- process identity metadata;
- state transitions, timestamps, reason categories, file hashes, and changed filenames;
- availability state and reset time with source/freshness.

Forbidden by default:

- tokens, cookies, auth headers, credential objects, and environment values;
- transcript contents, prompts, model responses, diffs, and file contents;
- raw provider output;
- Keychain data;
- full hook payloads when they contain assistant messages.

`StopFailure.last_assistant_message` is specifically discarded before persistence.

## Permissions and platform posture

- macOS arm64 and Linux x86_64 are v0.1 release gates.
- Platform-specific permissions, process identity, and locking live behind adapters.
- Relay follows existing user umask only when it is at least as restrictive as required; otherwise it sets restrictive modes explicitly.
- Windows semantics are designed but not claimed until implemented and tested.

## Existing Claude profile inspection

The read-only adoption preflight enforces the following order:

1. canonicalize the supplied directory and reject missing paths, terminal symlinks, managed-root escapes, wrong ownership, or group/other permission bits;
2. canonicalize the Claude executable and require a regular executable owned by root or the current user and not group/world writable;
3. report authentication-related environment variables as `present` or `absent`, never their values;
4. reject any conflicting credential, identity, endpoint, or cloud-provider routing override before process launch;
5. run only `claude --version` and `claude auth status --json` with bounded output and timeout;
6. discard stderr and map failures to closed error categories;
7. accept only the tested auth-status schema and extract whitelisted non-secret identity fields.

Unknown fields are not logged and do not become forward-compatible by accident. A secret-like unexpected field therefore causes `unsupported_provider_schema` without its key value or content appearing in output.

Directory mode 0755 is intentionally rejected even if individual credential files might be more restrictive: filenames, session layout, and future provider files would otherwise be exposed to group or other users. Relay does not repair permissions during inspection or dry-run.

Real validation against Claude Code 2.1.276 initially caught a wrong-account condition because the Profile A and Profile B directories reported the same pin. After Profile A was corrected, the profiles reported distinct normalized email and organization IDs. Duplicate provider-scoped pins are now rejected at the core registration boundary and by adoption dry-run; there is no implicit alias mode.

Adoption is a registry operation, not credential migration. The only permanent write is Relay's global `profiles.toml`; atomic replacement also uses a transient sibling. No Relay marker or identity file is placed in the Claude directory.

## Recovery safety

Recovery is a reconciler, not an undo script. It observes locks, processes, artifact hashes, and journal revision before choosing an action. It does not blindly rerun the last command.

When evidence is contradictory, recovery leaves the project reserved and prints the exact observations requiring human resolution. Availability is less important than preventing a second writer or destroying the only valid session copy.

## Security validation gates

Before M3 completion:

- threat-model review against the implemented code;
- planted-secret log tests;
- symlink and path-race tests;
- competing-process and PID-reuse tests;
- crash injection at every handoff state;
- malformed hook/output fuzzing;
- transcript divergence and partial-copy tests;
- dependency and license audit;
- documented manual recovery drill.
