# Integration lab setup and capability boundary

Continuation from `c113acf`, checked 2026-10-30. This is an execution runbook,
not a new architectural investigation. Do not rerun completed native experiments
until an implementation change or a newly available capability justifies it.

## Current decision

**Stop native integration in this execution environment.** The selected native
sources and bridge-core binary are not a complete Codex workspace. DNS fails for
GitHub, the Cargo index and Rust toolchain host. Loopback TCP/Unix listeners and
even `/bin/ps` on this process fail with EPERM. PTYs work. These are independently
required capabilities; fixing DNS alone will not enable the integration suite.

Raw current results: `engineering/evidence/capabilities-followup.json`. The first
capability probe aborted on the denied process spawn before saving its results;
the corrected probe catches that error and retains the full record. No native
experiment or source clone was retried in this continuation.

## Checkpoint and remote backup

Verified the original checkpoint's branch/head, bundle prerequisites and SHA-256,
and every one of its 90 mirrored research files. Original bundle SHA-256:
`9084b8a485170adb43b23ad2860cb1b39da4d40315a0caa72aae1e0a7c3fdb83`.
The research checkout has no tracked edits at that checkpoint; AGENTS.md remains
untracked. Original main and production sources remain untouched.

The remote `research/codex-concurrency-native-validation` ref was not found.
The four remote workflow blobs match the inspected local files exactly:

| Workflow | Trigger relevant to a research backup |
| --- | --- |
| CI | Pull requests or pushes to main; no research push trigger |
| Dogfood | Successful push-triggered main CI workflow_run; not this branch |
| Release | v* tag push or explicit dispatch; neither is used |
| Pages stable version | Published stable release; no release is created |

An exact-bundle backup via the GitHub connector was attempted because shell
network access is unavailable. The connector rejected `github_create_blob`:
**"MCP tool call requires approval, but approval policy is never"**. No blob,
branch or remote backup was created. This is a write-authorization blocker in
addition to shell networking. Do not report the local bundle as a remote backup.
The blocked attempt and workflow hashes are retained in
`engineering/evidence/remote-backup-followup.json`.

On an authorized development environment, back up the actual research branch
using ordinary Git. Use the explicit GitHub URL: the disposable checkout's
`origin` may point to the original *local* workspace. No force, tags, PR, merge or
workflow dispatch is needed:

```sh
RESEARCH=/private/tmp/agent-relay-native-validation
git -C "$RESEARCH" status --short
git -C "$RESEARCH" branch --show-current
git -C "$RESEARCH" rev-parse HEAD
git -C "$RESEARCH" -c push.followTags=false push https://github.com/RA1NM4KER/agent-relay.git HEAD:refs/heads/research/codex-concurrency-native-validation
git ls-remote https://github.com/RA1NM4KER/agent-relay.git refs/heads/research/codex-concurrency-native-validation
```

Before pushing, verify the checkout is on the research branch and recheck remote
workflow triggers if they changed. The last command must name exactly the local
HEAD. If a remote branch has appeared, fetch/inspect it and reconcile without
force-overwriting other work. Use provider-owned Git authentication; do not copy
or print credentials. If the temporary checkout is gone, restore from the verified
bundle using CONTINUATION.md first. Latest local tip/bundle hash are in the
workspace's `docs/investigations/checkpoints/checkpoint.json`.

## Required environment changes

Use a dedicated disposable lab account/VM or a new appropriately authorized
execution environment. Merely opening another shell under the same restrictive
execution policy does not change these capabilities. No changes to active Relay
or Codex sessions are required.

| Capability | Required change and verification |
| --- | --- |
| Source/dependencies | Working DNS + outbound HTTPS for GitHub and pinned Git dependencies, Cargo registries/download hosts, and Rust toolchains; alternatively supply the complete verified source, toolchain and dependency closure offline |
| Local transports | Permit binding/listening/connecting on new private Unix sockets and ephemeral 127.0.0.1 TCP ports for the isolated daemon and Responses mock |
| Process supervision | Permit spawning and inspecting owned children, start-time fingerprints, exact-child signal/wait, process groups, fork/setsid and PTYs; do not grant authority over unrelated user sessions |
| Build tools | Rust 1.95.0 with clippy, rustfmt and rust-src per the pinned Codex toolchain; native prerequisites declared by that source; writable disposable source/Cargo/target caches; verify rustup show and actual Cargo build |
| Profiles/worktrees | Fresh homes and dedicated projects created by the harness; no mounted live provider profile or credential files; no installation into PATH |
| macOS parity | A macOS lab for native TUI/permissions/Herdr and a separately designed executor containment adapter; Linux results cannot certify macOS quiescence |
| Linux containment | cgroup v2 with explicit research delegation and cgroup.kill, plus an enforceable separation of manager and executor management authority; the self-migration adversary must be exercised |
| Remote durability | Git write authorization for the named research branch, with deployment-trigger audit; current connector policy cannot authorize writes |

The toolchain path/version was verified through the connector against
[the pinned native toolchain](https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/rust-toolchain.toml),
blob `910c65114ec128a8317ee8257428451f3cd28230`; this read does not supply
the full source/dependency closure needed for compilation.

Needed dependency hosts include github.com, the hosts referenced by its pinned
Git dependencies, index.crates.io, static.crates.io and static.rust-lang.org.
The definitive host/tool list is the pinned workspace and lockfile; selected
cached source does not establish a complete dependency closure. No HTTP proxy
or provider credential routing is proposed.

The required capabilities should apply to the **agent's test execution**, not
only an interactive terminal elsewhere. Supply resulting artifacts/logs if a
separate lab operator runs the procedures. Do not claim a capability is enabled
until its small probe succeeds. Record failed probes once; do not repeatedly
invoke native suites against a known blocked prerequisite.

## Native build and first integration gate

Run in the disposable lab account. Paths below never replace installed Codex.
`rust-v0.166.0` is the investigation pin; this does not expand Relay's production
supported-version declaration.

```sh
LAB=$(mktemp -d)
RELAY=/path/to/research-checkout
git clone --depth 1 --branch rust-v0.166.0 https://github.com/openai/codex.git "$LAB/codex"
git -C "$LAB/codex" rev-parse HEAD
cat "$LAB/codex/codex-rs/rust-toolchain.toml"
python3 "$RELAY/docs/investigations/codex-concurrency/feasibility/patch_native.py" "$LAB/codex" --apply --diff "$LAB/native.patch"
cd "$LAB/codex/codex-rs"
rustup show
cargo check -p codex-app-server-client -p codex-tui --target-dir "$LAB/target"
cargo build -p codex-cli --target-dir "$LAB/target"
cd "$RELAY"
python3 docs/investigations/codex-concurrency/feasibility/native_tui_lab.py --codex "$LAB/target/debug/codex" --mode embedded
python3 docs/investigations/codex-concurrency/feasibility/native_tui_lab.py --codex "$LAB/target/debug/codex" --mode shared
```

Execute each stage only if the preceding command succeeds. Preserve compiler
logs, exact upstream commit, patch, command statuses and harness result roots.
If rust-toolchain.toml is elsewhere in the full pinned checkout, use that
workspace's declared toolchain; do not silently substitute the Relay toolchain.
The patch checks four upstream blob hashes before editing anything.

The existing frontend lab is intentionally incomplete. A compile error, unknown
RPC, callback failure or failed selection oracle is a failure to investigate,
not permission to weaken the gate. Embedded mode is a comparison only; shared
mode must also pass. Never force --remote or drop native permission overrides.
The lab's offline provider cannot certify successful inference, subagents or
quota. A passing TUI gate alone does not prove host admission or quiescence.

## Subsequent enforcement gates

After the first build runs, implement native token/epoch enforcement at creation,
dispatch, dequeue and executor start. Test raw unmediated calls, stale approval
replies, goal/queue/subagent work, lost replies and actual host crashes. Keep the
existing bypass test until it demonstrably refuses inside native enforcement.
External service-only fencing remains a failed candidate.

On a disposable Linux containment lab with explicitly provided delegation:

```sh
python3 docs/investigations/codex-concurrency/engineering/containment_escape.py --delegated-parent /explicit/research/cgroup --output /tmp/relay-cgroup-escape.json
python3 docs/investigations/codex-concurrency/feasibility/linux_containment.py --delegated-parent /explicit/research/cgroup
```

An observed escape is a falsification. One denied escape path is not complete
containment. These scripts do not yet implement a native executor adapter.
Do not release the old owner until every accepted execution domain has an
authoritative fence/quiescence receipt, including shared/external tool services.
Only then integrate the Relay handoff journal, scoped observation, snapshots,
target verification and recovery on the research branch.

## Guarantee state at this boundary

| Guarantee | Current state |
| --- | --- |
| Checkpoint integrity | Proven narrowly: bundle verifies; recorded hash and 90 mirrored files match |
| Remote backup | Blocked: required GitHub write approval unavailable; not completed |
| Exact native binding/replay | Prior narrow backend/service tests observed; full native-TUI binding unverified |
| Authoritative native admission | Failed external-wrapper boundary; native implementation unverified |
| Executor quiescence | Failed turn/process-death implication; native/platform adapters unverified |
| Automatic handoffs, cross-provider recovery, native UX, Herdr, whole-runtime costs | Unverified for concurrency; existing production behavior unchanged |
| Eight fully supervised Relay sessions / zero regressions | Unverified; no production integration enabled |

ACCEPTANCE.md remains the complete per-scenario release matrix. This continuation
does not upgrade any native safety claim or rerun old tests merely to add counts.
