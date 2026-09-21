# Getting started with Agent Relay

This is for someone who has never used Agent Relay before and doesn't need to know how it works
internally. If you want the internals — writer leases, transaction states, Herdr tokens — those
live in [`docs/architecture.md`](architecture.md) and [`docs/automatic-handoff.md`](automatic-handoff.md).
Here, you only need three ideas:

- You have one or more Claude accounts.
- One is your **primary**; the others are **fallbacks**.
- You run Claude *through* Agent Relay, and Relay moves your session to a fallback if your primary
  genuinely runs out of quota.

## 1. Install

```sh
brew install RA1NM4KER/tap/agent-relay
```

No Rust, no `git clone`, no repository checkout. You also need
[Claude Code](https://docs.claude.com/en/docs/claude-code) itself installed (`claude --version`
should print something in the `2.1.x` range) and `git`. [Herdr](https://herdr.dev) is optional —
everything below works without it.

Not on macOS, or don't want Homebrew? See the README's
["Advanced / manual setup / building from source"](../README.md#advanced--manual-setup--building-from-source)
section — it builds the same two binaries (`relay`, `relay-herdr-plugin`) from source with
`cargo build --release`.

## 2. `relay setup`

Run:

```sh
relay setup
```

This is a one-time interactive wizard. It walks through six things:

1. **Environment check.** It looks for Claude Code, Herdr (if you have it), and Agent Relay's own
   config directory, and shows a simple checklist — no raw JSON unless you pass `--verbose`.
2. **Claude accounts.** For each account you want Relay to manage, you can either:
   - **Authenticate a new one.** You give it a short name (e.g. `work`); Relay creates a private,
     isolated directory for it and opens Claude's own official login (a browser window, or
     whatever Claude's login flow normally shows you). **Relay never sees your password or your
     Claude token** — it only waits for the login to finish and then asks Claude "are you
     authenticated now?" the same way `relay profile doctor` already does.
   - **Reuse one that's already logged in.** If you've already set up an isolated Claude profile
     by hand, point Relay at its config directory once and it adopts it.

   If you already have profiles from before (including from before you ever ran `relay setup`),
   it detects them and asks if you want to keep using them — it never re-authenticates something
   that's already working.
3. **Primary and fallback.** If you have two or more accounts, you pick which is primary and the
   priority order for the rest. This is saved to a small file Agent Relay owns
   (`~/.config/agent-relay/preferences.toml`) — just profile *names*, never anything that could
   authenticate as one of your accounts.
4. **Automatic quota detection** (recommended, default yes). This installs a couple of small,
   already-existing Claude Code hooks (`StopFailure` and a statusline addition) into each chosen
   profile, so Relay can tell when an account is genuinely out of quota without spending an extra
   API call to check. If your Claude Code version is newer than what Relay has explicitly
   verified, it says so and asks before installing anyway — it never does this silently.
5. **Herdr** (if installed, recommended, default yes). Wires the same status/doctor/watch/handoff
   behavior into Herdr so a Claude pane inside Herdr can trigger them without a separate terminal.
   If Herdr isn't installed, Relay just says so and moves on — it works fine standalone.
6. **Done.** It prints your primary/fallback and reminds you of the two commands you actually need
   day to day: `relay claude` to start, `relay resume` to continue.

You can re-run `relay setup` any time — to add another account, change the primary, or toggle an
integration. It's safe: it never re-authenticates something that's already logged in, and it never
throws away an existing profile.

## 3. `relay claude`, `relay codex` and `relay resume`

The mental model: **`relay claude` = `claude` + Relay supervision.** It always starts something
new; it never silently reattaches you to something old. Continuing is a separate, explicit command.

```sh
cd ~/repos/my-project
relay claude
```

This is the command you use to **start** a conversation. It:

- figures out the project from your current directory,
- figures out which account to use from what `relay setup` saved (no `--profile` needed),
- starts a **new** Claude conversation under that account, tracked from the first message,
- if you're inside a Herdr pane, tells Herdr which account/session this pane belongs to
  automatically (you never type a pane id or copy a session UUID anywhere),
- and hands you a normal, interactive Claude terminal.

It needs an opening message — either give it on the command line
(`relay claude "let's refactor the auth module"`) or it asks you once.

`relay codex` is the same command for Codex: it starts a **new** Relay-managed Codex conversation
under your highest-priority configured Codex profile (`relay claude` picks the highest-priority
Claude profile). Neither command prompts for a profile; `--profile <name>` overrides the choice and
must name a profile of the right provider. Both support `--new` and `--no-attach`, refuse to create
a second writer, and open the supervised terminal, so automatic handoff stays active. `relay resume`
continues whichever provider currently owns the conversation.

### Passing options to Claude or Codex

Options for the provider CLI go after `--`; everything before it is Relay's:

```sh
relay claude -- --dangerously-skip-permissions
relay claude --profile claude-backup -- --model opus
relay codex -- --sandbox workspace-write
```

They are forwarded verbatim (no shell re-parsing), remembered for this project, and reused only for
the same provider — including after a handoff to another profile of that provider. Options are
never translated between CLIs: after a Claude → Codex handoff the Codex session gets Codex's own
stored options (if any), never `--dangerously-skip-permissions`. Flags that would replace something
Relay owns — the working directory, the session/thread being resumed, headless/machine-readable
output, backgrounding — are rejected with a clear error.

If a Relay-managed session is *already* active for this project, `relay claude` refuses rather than
guessing what you meant:

```
A Relay-managed session is already active for this project.

Current profile: erika

Run:
  relay resume
to continue it.

Or:
  relay claude --new
to stop the existing managed session and start a fresh one.
```

```sh
relay resume
```

This is the command you use to **continue** a conversation later — same project, same session,
picked back up under whichever profile actually owns it (you don't need to know or type a profile
name; that's only for the advanced explicit form, `relay resume <profile>`). If nothing is
running, it says so clearly instead of starting something you didn't ask for.

```sh
relay claude --new
```

The explicit escape hatch: safely stop the active managed session (the same authoritative
stop-and-verify machinery `relay switch` and recovery already use — never a raw kill, never two
writers coexisting even for an instant) and start a genuinely fresh conversation in its place. Use
this when you actually want a clean slate, not a continuation.

## 4. What happens when quota runs out

If your primary account genuinely, verifiably runs out of quota (not a transient rate-limit blip —
Relay is deliberately conservative about that distinction), Agent Relay:

1. safely stops your current session (the conversation itself is preserved, not discarded),
2. moves it to your next fallback account,
3. resumes it there — same conversation, same context, same session, just under a different
   account,
4. and you keep working. The moment Claude reports a rate limit, the usage integration's hook
   starts one short-lived evaluation for that project — Relay is not a background daemon and never
   polls. If you're attached through `relay claude` or `relay resume`, your terminal continues on
   the fallback account on its own ("continuing this conversation on '<profile>'"); Herdr's
   status event and a manual `relay watch run` are additional triggers.

You are never asked to copy a session id, look up a pane id, or run a lower-level handoff command
for this to work.

## 5. Checking on things: `relay status` and `relay profiles`

```sh
relay status
```

```
Project: ~/repos/my-project
Claude session: active
Current profile: work
Fallback: backup
Primary profile auth: authenticated
Automatic handoff: enabled
Herdr: connected
```

```sh
relay profiles
```

```
work         primary    authenticated
backup       fallback   authenticated
```

Neither of these prints transaction ids, session UUIDs, or config paths unless you ask for the
`--json` form (meant for scripts, not for reading).

## 6. Recovery basics

If Relay (or your machine) was interrupted mid-handoff, `relay claude`/`relay watch run`
automatically detect and recover an incomplete transaction before doing anything else — you don't
need to know what "recovery" means for this to work. If it can't safely decide on its own, it
tells you plainly and points at `relay recover <id> --project-dir DIR` (an advanced command — see
below) rather than guessing.

## 7. Reauthenticating

If Claude logs you out of an account (it happens), `relay claude` notices automatically:

```
Profile "work" needs Claude authentication.

Opening Claude login...
```

It opens the same official login flow `relay setup` used, waits, and continues once you're signed
in again. If a *fallback* account is the one that's logged out, Relay just warns you — your
primary keeps working — since you might not have gotten around to setting that one up yet.

You can also do this directly: `relay login work` / `relay logout work`.

## 8. The advanced/manual path

Everything above is a friendly layer over commands that still work exactly as they did before:
`relay profile ...`, `relay launch`, `relay watch run`, `relay handoff run`, `relay recover`,
`relay session conflict ...`, `relay integration claude ...`, `relay integration herdr ...`. If you
want to script Agent Relay, inspect a transaction's internals, or understand exactly what's
happening under the hood, start with [`docs/architecture.md`](architecture.md) and
[`docs/automatic-handoff.md`](automatic-handoff.md) — and the main [README](../README.md)'s
"Advanced / manual setup / building from source" section shows the equivalent manual commands for
everything `relay setup`/`relay claude` do automatically.

## 9. Using Claude Code to help install Agent Relay

This is a secondary convenience, not the recommended way to install — `brew install
RA1NM4KER/tap/agent-relay` (step 1 above) is simpler and doesn't involve an LLM in the loop at all.
If you'd rather have Claude Code do it for you (e.g. you're already in a Claude Code session and
want to set this up without leaving it), a safe prompt is:

> Install Agent Relay from the official RA1NM4KER/agent-relay repository using the documented
> installation method. Do not manually edit or copy Claude credential files. After installation,
> run `relay setup` and let Agent Relay use Claude's official authentication flow.

The important part is the constraint, not the convenience: Agent Relay's own installer and setup
wizard never touch Claude's credentials directly, and neither should any assistant helping you
install it.
