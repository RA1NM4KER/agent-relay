# M6 final report: Codex as a second provider

M6 generalized Agent Relay from "Claude only" to a provider-neutral core with Codex as a proven
second provider: `relay-core`'s handoff machinery now speaks in provider-neutral ports and a
`ContinuityType` (`SESSION_CONTINUATION` / `STATE_CONTINUATION` / `NATIVE_RESUME`) instead of
Claude-specific types, `relay-provider-claude` was adapted to implement those ports rather than
being the only implementation, and a new `relay-provider-codex` crate implements them for Codex.
`relay-cli` gained provider dispatch throughout (`relay setup`, `relay profile`, `relay status`,
`relay claude`), plus two new commands: `relay switch <target>` (cross-provider or cross-profile
handoff) and `relay resume <profile>` (reattach to the writer lease's own session/thread under its
exact provider — `codex resume <thread-id>` for Codex, an interactive `claude --resume <id>` for
Claude).

## Delivered (commits, `main..m6-codex`)

- `91b6c8b` feat: provider-neutral continuity/capability groundwork in `relay-core`
- `4f0b66e` feat: adapt `relay-provider-claude` to the generalized handoff ports
- `08689ea` feat: new `relay-provider-codex` crate
- `4e53243` fix: recovery must stop an orphan target with the TARGET's own stopper
- `7b78c86` feat: `relay-cli` provider dispatch, `relay switch`, `relay resume`
- `579c7f2` test: end-to-end mixed-provider CLI tests with `FakeClaude`/`FakeCodex`
- `2401056` feat: `relay setup`/`profiles`/`status`/`claude` become provider-aware
- `fa4b98c` test: interactive `relay setup` can register a Codex-only profile

## Dogfood-found bug: `relay claude` couldn't attach to its own background session

Found today, live, using Agent Relay on itself (not by a test): `relay claude` reported

```
No job matching '854a2a57'
```

for a background session that was, provably, still running — `relay resume erika` correctly
detected and described the exact same session (`854a2a57-1c41-47d2-b20b-85fa0118fcdd`, background
job `854a2a57`) as active, and manually running

```
CLAUDE_CONFIG_DIR="$HOME/.config/agent-relay/profiles/erika/claude" claude attach 854a2a57
```

worked immediately. The difference between the two: `relay resume` explicitly sets
`CLAUDE_CONFIG_DIR` before execing Claude; `relay claude`'s attach step did not.

### Root cause

`exec_claude_attach` (`crates/relay-cli/src/main.rs`), the function `relay claude` execs into for
its final "hand the user a live terminal" step, ran

```rust
std::process::Command::new(executable).arg("attach").arg(short_id).exec();
```

with no `CLAUDE_CONFIG_DIR` set on the child at all. `claude attach` then searched whichever config
directory this process happened to inherit from its parent shell — not necessarily anything, and
never guaranteed to be the isolated profile directory Relay itself had launched the background job
under — so it never found the job registry entry that job was actually recorded in. Every other
Claude-launching call site in the codebase (`exec_claude_resume`, `run_claude_auth_subcommand`,
`relay-provider-claude`'s own launch/usage/session-registry code) already sets `CLAUDE_CONFIG_DIR`
explicitly; `exec_claude_attach` was the one exception, introduced back in M4.4 and never caught
because no test exercised the attach step's environment — only its exit status.

### Fix

`exec_claude_attach` now takes the target `config_dir` and sets it on the child via
`Command::env("CLAUDE_CONFIG_DIR", config_dir)` — the child's environment only; this process's own
environment is never mutated, preserving credential isolation between profiles exactly as every
other call site already does.

At the call site (`run_claude`), the `config_dir` passed is resolved from `lease.owner_profile` —
looked up in the already-fetched `registered` profile list — never assumed to be the configured
primary profile. This matters specifically because `lease.owner_profile` is *not* always the
primary: after a `relay switch` (Claude A → Claude B handoff) completes, the writer lease's
`owner_profile` is the handoff target, which may be a fallback profile rather than primary, and
`run_claude`'s existing-lease-reuse branch reads that same `lease.json` regardless of whether the
lease reflects a fresh launch or a completed handoff. The fix reads the lease's actual owner every
time, so a post-handoff `relay claude` attaches under the correct profile automatically.

### Regression coverage

Two new tests in `crates/relay-cli/tests/m4.rs`:

- `claude_attach_execs_under_the_lease_owners_isolated_config_dir` — a fresh
  launch-then-attach (`relay claude`, no `--no-attach`) proves the fake `claude`'s logged `attach`
  invocation received `CLAUDE_CONFIG_DIR` equal to the profile's own registered config dir, not
  nothing.
- `claude_attach_after_a_handoff_uses_the_new_owners_config_dir_not_the_primarys` — sets primary =
  `alice`, fallback = `bob`, and makes `bob` (not the primary) the project's writer lease owner —
  the exact on-disk shape a completed Claude A → Claude B handoff leaves behind (`run_claude`'s
  existing-lease branch reads `lease.json` identically regardless of how it reached that state, so
  constructing it directly via `relay launch --profile bob` exercises the same code path a real
  `relay switch` would leave behind, without needing a full session-transfer transcript fixture).
  A plain `relay claude` (no `--profile` override) is then proven to (a) reuse the active lease
  rather than relaunch under the primary, and (b) attach under `bob`'s `CLAUDE_CONFIG_DIR`, never
  `alice`'s.

**LIVE VERIFIED**, both directions: both tests were run against a temporary revert of the fix
(`git stash` on `main.rs` alone) and confirmed to fail with the exact real-world symptom
(`config_dir` logged as empty string) before being confirmed to pass with the fix restored.

### fmt/clippy/tests

`cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`:
clean (one lint fixed along the way — `filter(..).next_back()` → `.rfind(..)` in the new test).
`cargo test --workspace`: **354/354, 0 failed**.

### Scope note

This report documents the dogfood-found bug above in full; it does not re-verify or re-attest the
correctness of the eight M6 feature commits listed above, which were built and tested in earlier
sessions — their own test coverage (`crates/relay-cli/tests/m6.rs`, unit tests in `relay-core` and
`relay-provider-codex`) is unchanged by this fix and remains green as part of the same
`cargo test --workspace` run.
