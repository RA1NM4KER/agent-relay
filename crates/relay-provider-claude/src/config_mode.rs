//! The one real difference between Relay's native-default Claude profile and its isolated ones:
//! whether `CLAUDE_CONFIG_DIR` is set when Claude is invoked.
//!
//! Live-confirmed: `claude auth status --json` with `CLAUDE_CONFIG_DIR` **unset** reports the
//! logged-in native account, with `configDirectory: ~/.claude`. The identical command with
//! `CLAUDE_CONFIG_DIR=~/.claude` **explicitly set** reports logged **out**. The two are not
//! equivalent — Claude Code's own credential lookup differs — so this can never be special-cased
//! as `config_dir == "~/.claude"` anywhere in this codebase. Every Claude command Relay builds
//! must go through [`apply`] instead of hand-rolling `.env("CLAUDE_CONFIG_DIR", ...)`.

use std::path::{Path, PathBuf};

pub use relay_core::ClaudeConfigMode;

/// Sets or removes `CLAUDE_CONFIG_DIR` on a soon-to-be-spawned command according to `mode`.
/// `config_dir` is still used for everything else (file reads, `--cwd`-style arguments, ambient
/// project layout) regardless of mode — only the environment variable differs.
pub fn apply(command: &mut std::process::Command, mode: ClaudeConfigMode, config_dir: &Path) {
    match mode {
        ClaudeConfigMode::Explicit => {
            command.env("CLAUDE_CONFIG_DIR", config_dir);
        }
        ClaudeConfigMode::NativeDefault => {
            // Explicit removal, not merely "don't set it": a hook process inherits its parent
            // Claude's own environment, which may carry a DIFFERENT profile's CLAUDE_CONFIG_DIR.
            command.env_remove("CLAUDE_CONFIG_DIR");
        }
    }
}

/// `~/.claude` — the only directory a native-default profile may ever use. `None` only if `HOME`
/// itself cannot be resolved (never guessed, never defaulted to a relative path).
#[must_use]
pub fn native_default_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_default_never_sets_the_directory_even_when_the_parent_process_has_it() {
        let mut command = std::process::Command::new("true");
        command.env("CLAUDE_CONFIG_DIR", "/some/other/profile");
        apply(
            &mut command,
            ClaudeConfigMode::NativeDefault,
            Path::new("/Users/x/.claude"),
        );
        // `Command::env_remove` records an explicit removal as `(key, None)` in `get_envs()`
        // rather than dropping the key outright — that entry is exactly the proof the inherited
        // value was actively stripped, not merely left unset by this call.
        let entry = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"));
        assert_eq!(
            entry,
            Some((std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"), None)),
            "NativeDefault must explicitly remove an inherited CLAUDE_CONFIG_DIR, not merely \
             leave it unset"
        );
    }

    #[test]
    fn explicit_always_sets_the_directory_even_for_the_same_path_native_default_would_use() {
        let mut command = std::process::Command::new("true");
        let dir = Path::new("/Users/x/.claude");
        apply(&mut command, ClaudeConfigMode::Explicit, dir);
        let value = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"))
            .and_then(|(_, value)| value);
        assert_eq!(value, Some(dir.as_os_str()));
    }
}
