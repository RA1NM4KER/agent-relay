//! Provider passthrough arguments: `relay claude [relay options] -- [claude options]` and
//! `relay codex [relay options] -- [codex options]`.
//!
//! Everything after `--` is the *user's own* provider CLI arguments. Relay does not mirror either
//! CLI's flags; it stores the argv it was given, exactly, and appends it only where it is
//! semantically the user's session preference. The rules, per launch path:
//!
//! | path | user arguments |
//! |---|---|
//! | fresh `relay claude` (`claude --bg …`) | Claude arguments, after Relay's own arguments and the prompt |
//! | fresh `relay codex` | never on the bootstrap `codex exec` (a fixed, content-free turn that only creates the thread); Codex arguments go to the interactive `codex resume` that follows |
//! | `relay resume` / continuation of the *same* provider | that provider's arguments on `claude --resume <id>` / `codex resume <id>` |
//! | `claude attach <job>` | none — the background job already carries the arguments it was launched with |
//! | handoff verification/bootstrap turns (`claude -p …`, `codex exec …`) | none — Relay's own canary/bootstrap turns use Relay's arguments only |
//! | cross-provider handoff | the *target* provider's stored arguments only; arguments are never translated between CLIs |
//!
//! The arguments live in project-scoped state (`provider_args.json`, an atomic private write like
//! every other Relay-owned file) so the supervised terminal — and a detached automatic handoff in
//! a different process — continue with the right arguments after ownership changes. The file holds
//! a structured argv list per provider (never a shell string, never credentials).
//!
//! Relay stays authoritative for what makes a session *managed*: the working directory, the
//! provider config home (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`, always set by environment), the
//! session/thread identity, headless/machine-readable output and background/detach. A small,
//! explicit set of flags that would replace one of those is rejected with a clear error instead
//! of being silently overridden or passed through.

use std::path::Path;

use relay_core::{AtomicWrite, Error, FsAtomicWriter, ProviderKind, Result};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "provider_args.json";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderArgs {
    version: u32,
    #[serde(default)]
    claude: Vec<String>,
    #[serde(default)]
    codex: Vec<String>,
}

impl ProviderArgs {
    /// Missing file is normal (no passthrough was ever given); a corrupt one is an error rather
    /// than silently dropping the user's arguments.
    pub fn load(project_state_dir: &Path) -> Result<Self> {
        let path = project_state_dir.join(FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let parsed: Self =
                    serde_json::from_str(&text).map_err(|_| Error::UnsupportedStateVersion(0))?;
                if parsed.version != VERSION {
                    return Err(Error::UnsupportedStateVersion(parsed.version));
                }
                Ok(parsed)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    pub fn save(&self, project_state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(project_state_dir).map_err(|source| Error::Io {
            path: project_state_dir.to_path_buf(),
            source,
        })?;
        let mut stored = self.clone();
        stored.version = VERSION;
        let text = serde_json::to_string_pretty(&stored).map_err(|_| Error::SerializationFailed)?;
        FsAtomicWriter.write_atomic(&project_state_dir.join(FILE_NAME), text.as_bytes())
    }

    /// The stored arguments for one provider; `Fake` is treated as Claude (as everywhere else).
    #[must_use]
    pub fn for_provider(&self, provider: ProviderKind) -> &[String] {
        match provider {
            ProviderKind::Codex => &self.codex,
            ProviderKind::Claude | ProviderKind::Fake => &self.claude,
        }
    }

    /// Replaces one provider's arguments; the other provider's are left exactly as they were.
    pub fn set(&mut self, provider: ProviderKind, args: Vec<String>) {
        match provider {
            ProviderKind::Codex => self.codex = args,
            ProviderKind::Claude | ProviderKind::Fake => self.claude = args,
        }
    }

    /// A brand-new managed conversation: only the launching provider's arguments carry over to
    /// it; anything stored for another provider belonged to the previous conversation.
    #[must_use]
    pub fn fresh_for(provider: ProviderKind, args: Vec<String>) -> Self {
        let mut fresh = Self::default();
        fresh.set(provider, args);
        fresh
    }
}

/// (flag, why) — matched against `--flag`, `--flag=value` and, for the short forms listed here,
/// `-X` with or without an attached value. Deliberately small: only flags that would replace
/// something Relay must own for a managed session.
const CLAUDE_REJECTED: &[(&str, &str)] = &[
    (
        "--bg",
        "Relay starts and tracks the background session itself",
    ),
    (
        "--background",
        "Relay starts and tracks the background session itself",
    ),
    (
        "-p",
        "headless print mode replaces the interactive managed session",
    ),
    (
        "--print",
        "headless print mode replaces the interactive managed session",
    ),
    (
        "--output-format",
        "Relay needs its own machine-readable output",
    ),
    (
        "--input-format",
        "Relay needs its own machine-readable input",
    ),
    (
        "-r",
        "to adopt an existing Claude conversation use `relay claude --resume`; to continue the one Relay manages use `relay resume`",
    ),
    (
        "--resume",
        "to adopt an existing Claude conversation use `relay claude --resume`; to continue the one Relay manages use `relay resume`",
    ),
    ("-c", "Relay decides which session to continue"),
    ("--continue", "Relay decides which session to continue"),
    (
        "--fork-session",
        "it would create a different session than the one Relay tracks",
    ),
    (
        "--from-pr",
        "it would select a different session than the one Relay tracks",
    ),
    ("--session-id", "Relay owns the session identity"),
    (
        "--teleport",
        "it would select a different session than the one Relay tracks",
    ),
    (
        "--cloud",
        "a cloud session is not a local Relay-managed session",
    ),
    (
        "--environment",
        "a cloud session is not a local Relay-managed session",
    ),
    (
        "-w",
        "a worktree changes the project directory Relay manages",
    ),
    (
        "--worktree",
        "a worktree changes the project directory Relay manages",
    ),
    (
        "--tmux",
        "it detaches the session from Relay's supervised terminal",
    ),
    (
        "--no-session-persistence",
        "Relay must be able to resume the session",
    ),
    // Audited against `claude --help` (2.1.x): these disable the hooks / user settings that
    // Relay's installed integration (StopFailure hook + status line) lives in, which would leave a
    // session presented as Relay-managed but silently unable to hand off automatically.
    (
        "--bare",
        "it disables the hooks Agent Relay requires for automatic handoff (and Claude's OAuth/keychain login)",
    ),
    (
        "--safe-mode",
        "it disables all hooks and customizations, including the ones Agent Relay requires for automatic handoff",
    ),
    (
        "--restricted",
        "it ignores the user settings file where Agent Relay's automatic-handoff hooks are installed",
    ),
];

const SETTING_SOURCES_WITHOUT_USER: &str = "it excludes the `user` settings source where Agent Relay's automatic-handoff hooks are installed";

const CODEX_REJECTED: &[(&str, &str)] = &[
    ("-C", "Relay owns the working directory"),
    ("--cd", "Relay owns the working directory"),
    (
        "--worktree",
        "a worktree changes the project directory Relay manages",
    ),
    (
        "--ephemeral",
        "an ephemeral thread cannot be resumed by Relay",
    ),
    ("--json", "Relay needs its own machine-readable output"),
    ("-o", "Relay needs its own machine-readable output"),
    (
        "--output-last-message",
        "Relay needs its own machine-readable output",
    ),
    (
        "--output-schema",
        "Relay needs its own machine-readable output",
    ),
    ("--last", "Relay decides which thread to resume"),
    ("--all", "Relay decides which thread to resume"),
    (
        "--include-non-interactive",
        "Relay decides which thread to resume",
    ),
    (
        "--remote",
        "it would attach to a different Codex server than the profile's own home",
    ),
    (
        "--remote-auth-token-env",
        "it would attach to a different Codex server than the profile's own home",
    ),
];

/// Rejects only the flags above; every other argument is the user's and is forwarded verbatim.
/// A lone `--` inside the provider arguments ends flag parsing for the provider CLI itself, so
/// nothing after it is inspected.
pub fn validate(provider: ProviderKind, args: &[String]) -> Result<()> {
    let table = match provider {
        ProviderKind::Codex => CODEX_REJECTED,
        ProviderKind::Claude | ProviderKind::Fake => CLAUDE_REJECTED,
    };
    for (index, argument) in args.iter().enumerate() {
        if argument == "--" {
            break;
        }
        for (flag, why) in table {
            if matches_flag(argument, flag) {
                return Err(Error::ProviderArgumentRejected(argument.clone(), why));
            }
        }
        // Claude only: `--setting-sources` is fine as long as it still loads the `user` source
        // (Relay's hooks and status line are installed in the profile's user settings).
        if !matches!(provider, ProviderKind::Codex) {
            let value = if argument == "--setting-sources" {
                args.get(index + 1).map(String::as_str)
            } else {
                argument.strip_prefix("--setting-sources=")
            };
            if let Some(value) = value
                && !value.split(',').any(|source| source.trim() == "user")
            {
                return Err(Error::ProviderArgumentRejected(
                    argument.clone(),
                    SETTING_SOURCES_WITHOUT_USER,
                ));
            }
        }
    }
    Ok(())
}

fn matches_flag(argument: &str, flag: &str) -> bool {
    if flag.starts_with("--") {
        return argument == flag
            || argument
                .strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('='));
    }
    // A short flag: `-C`, `-C dir` (separate token), or `-Cdir` / `-C=dir` (attached value).
    // Never a long flag that merely shares its first letters, and never another short flag.
    argument.starts_with(flag) && !argument.starts_with("--")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn arguments_round_trip_exactly_and_stay_provider_scoped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude = strings(&[
            "--model",
            "opus",
            "--append-system-prompt",
            "be brief; \"quoted\" & spaced",
            "--add-dir=/a b",
            "--add-dir",
            "/c",
            "-",
            "",
        ]);
        let mut args = ProviderArgs::default();
        args.set(ProviderKind::Claude, claude.clone());
        args.set(
            ProviderKind::Codex,
            strings(&["--sandbox", "workspace-write"]),
        );
        args.save(dir.path()).expect("save");
        let loaded = ProviderArgs::load(dir.path()).expect("load");
        assert_eq!(loaded.for_provider(ProviderKind::Claude), claude.as_slice());
        assert_eq!(
            loaded.for_provider(ProviderKind::Codex),
            strings(&["--sandbox", "workspace-write"]).as_slice()
        );
    }

    #[test]
    fn a_missing_file_is_empty_and_a_corrupt_one_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            ProviderArgs::load(dir.path())
                .expect("empty")
                .claude
                .is_empty()
        );
        std::fs::write(dir.path().join(FILE_NAME), "not json").expect("corrupt");
        assert!(ProviderArgs::load(dir.path()).is_err());
    }

    #[test]
    fn a_new_conversation_keeps_only_the_launching_providers_arguments() {
        let fresh = ProviderArgs::fresh_for(ProviderKind::Codex, strings(&["--oss"]));
        assert_eq!(
            fresh.for_provider(ProviderKind::Codex),
            strings(&["--oss"]).as_slice()
        );
        assert!(fresh.for_provider(ProviderKind::Claude).is_empty());
    }

    #[test]
    fn ordinary_and_unknown_provider_flags_are_forwarded_untouched() {
        validate(
            ProviderKind::Claude,
            &strings(&[
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--some-future-flag=x",
                "--allowedTools",
                "Bash",
                "-d",
            ]),
        )
        .expect("claude ok");
        validate(
            ProviderKind::Codex,
            &strings(&[
                "--sandbox",
                "workspace-write",
                "-m",
                "o3",
                "-c",
                "model=\"o3\"",
                "--search",
            ]),
        )
        .expect("codex ok");
        validate(ProviderKind::Claude, &[]).expect("empty ok");
    }

    #[test]
    fn only_flags_that_replace_what_relay_owns_are_rejected() {
        for (provider, bad) in [
            (ProviderKind::Claude, "--bg"),
            (ProviderKind::Claude, "-p"),
            (ProviderKind::Claude, "--resume"),
            (ProviderKind::Claude, "--resume=abc"),
            (ProviderKind::Claude, "--session-id"),
            (ProviderKind::Claude, "--worktree"),
            (ProviderKind::Claude, "-w"),
            (ProviderKind::Claude, "--output-format=json"),
            (ProviderKind::Codex, "-C"),
            (ProviderKind::Codex, "--cd=/elsewhere"),
            (ProviderKind::Codex, "-C/elsewhere"),
            (ProviderKind::Codex, "--ephemeral"),
            (ProviderKind::Codex, "--json"),
            (ProviderKind::Codex, "--last"),
            (ProviderKind::Codex, "--remote"),
        ] {
            assert!(
                matches!(
                    validate(provider, &[bad.to_owned()]),
                    Err(Error::ProviderArgumentRejected(..))
                ),
                "{bad} must be rejected for {provider:?}"
            );
        }
    }

    #[test]
    fn flags_that_disable_relays_hooks_are_rejected_for_claude_only() {
        for bad in ["--bare", "--safe-mode", "--restricted"] {
            let error = validate(ProviderKind::Claude, &strings(&["--model", "opus", bad]))
                .expect_err("must be rejected");
            assert!(
                matches!(error, Error::ProviderArgumentRejected(ref flag, why)
                    if flag == bad && why.contains("Agent Relay")),
                "{bad}: {error}"
            );
            // the human message names the flag and explains why
            assert!(error.to_string().contains(bad));
            assert!(
                error.to_string().contains("automatic handoff")
                    || error.to_string().contains("settings")
            );
        }
        // Codex has no such flags, so the same words pass through to Codex untouched.
        validate(ProviderKind::Codex, &strings(&["--bare", "--safe-mode"])).expect("codex ok");
    }

    #[test]
    fn setting_sources_is_only_rejected_when_it_drops_the_user_source() {
        validate(
            ProviderKind::Claude,
            &strings(&["--setting-sources", "user,project"]),
        )
        .expect("has user");
        validate(
            ProviderKind::Claude,
            &strings(&["--setting-sources=local, user"]),
        )
        .expect("has user");
        for bad in [
            strings(&["--setting-sources", "project,local"]),
            strings(&["--setting-sources=project"]),
            strings(&["--setting-sources", ""]),
        ] {
            assert!(
                matches!(
                    validate(ProviderKind::Claude, &bad),
                    Err(Error::ProviderArgumentRejected(..))
                ),
                "{bad:?}"
            );
        }
        validate(
            ProviderKind::Codex,
            &strings(&["--setting-sources", "project"]),
        )
        .expect("codex ok");
    }

    #[test]
    fn a_similar_looking_flag_or_a_value_after_double_dash_is_not_rejected() {
        // `--print-timing`-style long flags sharing a prefix, and `--cdx`, are not the banned flags
        validate(ProviderKind::Claude, &strings(&["--print-something"])).expect("prefix only");
        validate(ProviderKind::Codex, &strings(&["--cdx", "--json-ish"])).expect("prefix only");
        // after a bare `--` the provider CLI treats everything as positional, so it is not a flag
        validate(ProviderKind::Claude, &strings(&["--", "--bg", "-p"])).expect("after --");
    }
}
