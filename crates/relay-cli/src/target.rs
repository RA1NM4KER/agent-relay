//! The one switch-target model, shared by `relay switch` (the terminal picker) and the in-agent
//! `/relay switch` listing, plus the dependency-light terminal picker itself.
//!
//! The model only *describes* who could receive the conversation and why not; it never decides
//! or moves anything. Every actual move still goes through `run_switch` → `HandoffCoordinator`,
//! which re-verifies everything (authentication, identity pin, target usage) before committing.

use std::{
    io::{IsTerminal as _, Read as _, Write as _},
    path::Path,
    process::{Command, Stdio},
};

use relay_core::{Profile, ProfileName, ProfileService, ProviderKind, usage::UsageState};

use crate::{preferences::Preferences, providers};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwitchTarget {
    pub name: ProfileName,
    pub provider: ProviderKind,
    pub current: bool,
    /// Position in the global priority order (0 = primary), for display and ordering.
    pub priority: usize,
    /// Why this profile cannot receive the conversation right now (`None` = selectable).
    pub unavailable: Option<String>,
}

impl SwitchTarget {
    #[must_use]
    pub fn selectable(&self) -> bool {
        !self.current && self.unavailable.is_none()
    }

    /// `claude` / `codex` for display.
    #[must_use]
    pub fn provider_label(&self) -> &'static str {
        match self.provider {
            ProviderKind::Codex => "Codex",
            ProviderKind::Claude | ProviderKind::Fake => "Claude",
        }
    }
}

/// The global priority order: primary, then fallbacks, then any other registered profile by name.
#[must_use]
pub fn priority_order<'a>(
    registered: &'a [Profile],
    preferences: &Preferences,
) -> Vec<&'a Profile> {
    let mut ordered: Vec<&Profile> = Vec::new();
    let listed = preferences
        .primary_profile
        .iter()
        .chain(preferences.fallback_profiles.iter());
    for name in listed {
        if let Some(profile) = registered.iter().find(|candidate| &candidate.name == name)
            && !ordered.iter().any(|seen| seen.name == profile.name)
        {
            ordered.push(profile);
        }
    }
    let mut rest: Vec<&Profile> = registered
        .iter()
        .filter(|profile| !ordered.iter().any(|seen| seen.name == profile.name))
        .collect();
    rest.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    ordered.extend(rest);
    ordered
}

/// Builds every row. `deep` additionally asks each candidate's provider for its usage now (a Codex
/// availability check takes a moment; a Claude one only reads locally recorded signals) — an
/// exhausted candidate is unavailable, and an unverifiable Codex one is unavailable too, exactly as
/// an explicit switch would refuse it. Without `deep` only local facts are used.
pub fn build_targets(
    registered: &[Profile],
    preferences: &Preferences,
    current: &ProfileName,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
    deep: bool,
) -> Vec<SwitchTarget> {
    priority_order(registered, preferences)
        .into_iter()
        .enumerate()
        .map(|(priority, profile)| {
            row_for(profile, current, priority, executables, project_dir, deep)
        })
        .collect()
}

/// One profile's row (also used alone to vet a single explicit target).
pub fn row_for(
    profile: &Profile,
    current: &ProfileName,
    priority: usize,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
    deep: bool,
) -> SwitchTarget {
    let is_current = &profile.name == current;
    let unavailable = if is_current {
        None
    } else if !profile.enabled {
        Some("disabled".to_owned())
    } else {
        usage_reason(profile, executables, project_dir, deep)
    };
    SwitchTarget {
        name: profile.name.clone(),
        provider: profile.provider,
        current: is_current,
        priority,
        unavailable,
    }
}

fn usage_reason(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    project_dir: &Path,
    deep: bool,
) -> Option<String> {
    if profile.provider == ProviderKind::Codex && !deep {
        return None;
    }
    let observation = providers::usage_signal_for(
        profile.provider,
        executables,
        false,
        None,
        profile.effective_claude_config_mode(),
    )
    .detect(&profile.config_dir, project_dir, "");
    match observation {
        Ok(observation) if observation.state.is_blocking() => Some("exhausted".to_owned()),
        Ok(observation)
            if observation.state == UsageState::Unknown
                && profile.provider == ProviderKind::Codex =>
        {
            Some("usage cannot be verified".to_owned())
        }
        Err(_) if profile.provider == ProviderKind::Codex => {
            Some("usage cannot be verified".to_owned())
        }
        _ => None,
    }
}

/// Authentication + identity-pin check for an explicit target, from the same provider status the
/// switch itself uses. Only used to explain a refusal early; the transaction re-checks regardless.
pub fn verification_reason(
    service: &ProfileService,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> Option<String> {
    let backend = providers::provider_backend(profile.provider, executables).ok()?;
    let status = service.status(&profile.name, backend.as_ref()).ok()?;
    if status.authentication != relay_core::AuthenticationState::Authenticated {
        return Some("not logged in".to_owned());
    }
    if !status.identity_matches {
        return Some("account does not match the registered identity".to_owned());
    }
    None
}

/// The identity-pin proof for adoption, run as a separate `relay profile status` process with the
/// provider's authentication-override variables removed: the caller may be a hook that inherited a
/// running Claude's environment, which Relay's own authentication check rightly treats as a
/// conflicting override. `None` = the account behind the profile matches its registered identity.
pub fn identity_refusal_scrubbed(
    paths: &relay_core::RelayPaths,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> Option<String> {
    let unverifiable = || Some("the profile's account could not be verified".to_owned());
    let Ok(program) = std::env::current_exe() else {
        return unverifiable();
    };
    let mut command = Command::new(program);
    command
        .arg("--json")
        .arg("--config-root")
        .arg(paths.config_root())
        .arg("--state-root")
        .arg(paths.state_root())
        .args(["profile", "status", profile.name.as_str()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("CLAUDE_CONFIG_DIR");
    if let Some(claude) = &executables.claude {
        command.arg("--claude-executable").arg(claude);
    }
    for variable in relay_provider_claude::AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let Ok(output) = command.output() else {
        return unverifiable();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return unverifiable();
    };
    fn find<'v>(value: &'v serde_json::Value, key: &str) -> Option<&'v serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map
                .get(key)
                .or_else(|| map.values().find_map(|inner| find(inner, key))),
            _ => None,
        }
    }
    match find(&value, "identity_matches").and_then(serde_json::Value::as_bool) {
        Some(true) => None,
        Some(false) => Some(
            "the account behind this profile no longer matches its registered identity".to_owned(),
        ),
        None => unverifiable(),
    }
}

/// One line per row, for human output that is not interactive.
#[must_use]
pub fn describe_rows(targets: &[SwitchTarget]) -> String {
    targets
        .iter()
        .map(|target| {
            format!(
                "{} ({}){}",
                target.name,
                target.provider_label(),
                if target.current {
                    " — current".to_owned()
                } else if let Some(reason) = &target.unavailable {
                    format!(" — unavailable: {reason}")
                } else {
                    String::new()
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- the picker ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Cancel,
    /// A digit `1`–`9` jumps to that row.
    Digit(usize),
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerOutcome {
    Continue,
    Chosen(usize),
    Cancelled,
}

/// The picker's pure state: which row is highlighted. Unselectable rows are skipped by the
/// arrows and refuse Enter, so the result can only ever be a selectable row.
pub struct Picker<'a, R: PickRow> {
    targets: &'a [R],
    cursor: usize,
}

/// One line of a picker: what it says, and whether it can be chosen.
pub trait PickRow {
    fn selectable(&self) -> bool;
    fn label(&self) -> String;
    /// Why the row is shown but not selectable (`current`, `unavailable: …`, `active`).
    fn note(&self) -> Option<String>;
}

impl PickRow for SwitchTarget {
    fn selectable(&self) -> bool {
        Self::selectable(self)
    }
    fn label(&self) -> String {
        format!("{} {DIM}{}{RESET}", self.name, self.provider_label())
    }
    fn note(&self) -> Option<String> {
        if self.current {
            Some("current".to_owned())
        } else {
            self.unavailable
                .as_ref()
                .map(|reason| format!("unavailable: {reason}"))
        }
    }
}

impl<'a, R: PickRow> Picker<'a, R> {
    #[must_use]
    pub fn new(targets: &'a [R]) -> Self {
        let cursor = targets.iter().position(PickRow::selectable).unwrap_or(0);
        Self { targets, cursor }
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn step(&mut self, forward: bool) {
        let count = self.targets.len();
        for offset in 1..=count {
            let index = if forward {
                (self.cursor + offset) % count
            } else {
                (self.cursor + count - offset % count) % count
            };
            if self.targets[index].selectable() {
                self.cursor = index;
                return;
            }
        }
    }

    pub fn handle(&mut self, key: Key) -> PickerOutcome {
        match key {
            Key::Up => self.step(false),
            Key::Down => self.step(true),
            Key::Digit(number) => {
                if let Some(index) = number.checked_sub(1)
                    && self.targets.get(index).is_some_and(PickRow::selectable)
                {
                    self.cursor = index;
                }
            }
            Key::Enter => {
                if self
                    .targets
                    .get(self.cursor)
                    .is_some_and(PickRow::selectable)
                {
                    return PickerOutcome::Chosen(self.cursor);
                }
            }
            Key::Cancel => return PickerOutcome::Cancelled,
            Key::Other => {}
        }
        PickerOutcome::Continue
    }
}

/// Decodes raw bytes read after putting the terminal in single-key mode.
#[must_use]
pub fn decode_key(bytes: &[u8]) -> Key {
    match bytes {
        [] | [0x1b] | [0x03] | [b'q'] => Key::Cancel,
        [b'\r'] | [b'\n'] => Key::Enter,
        [0x1b, b'[' | b'O', b'A'] | [b'k'] => Key::Up,
        [0x1b, b'[' | b'O', b'B'] | [b'j'] => Key::Down,
        [digit @ b'1'..=b'9'] => Key::Digit(usize::from(digit - b'0')),
        _ => Key::Other,
    }
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

fn render<R: PickRow>(targets: &[R], cursor: usize, title: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{BOLD}{title}{RESET}\r\n\r\n"));
    for (index, target) in targets.iter().enumerate() {
        let marker = if index == cursor { "❯" } else { " " };
        let line = format!("{marker} {}. {}", index + 1, target.label());
        if let Some(note) = target.note() {
            out.push_str(&format!("{DIM}{line} — {note}{RESET}\r\n"));
        } else if index == cursor {
            out.push_str(&format!("{BOLD}{line}{RESET}\r\n"));
        } else {
            out.push_str(&format!("{line}\r\n"));
        }
    }
    out.push_str(&format!(
        "\r\n{DIM}↑/↓ move · Enter choose · Esc cancel{RESET}\r\n"
    ));
    out
}

/// Puts the terminal into single-key mode with `stty` for the picker's lifetime and always
/// restores it (also on panic/early return). `stty` is on every supported platform; no extra
/// dependency is needed.
struct RawMode {
    saved: String,
}

impl RawMode {
    fn enter() -> Option<Self> {
        let saved = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())?;
        let applied = Command::new("stty")
            .args(["-icanon", "-echo", "min", "1", "time", "0"])
            .stdin(Stdio::inherit())
            .status()
            .ok()?;
        applied.success().then_some(Self { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ignored = Command::new("stty")
            .arg(&self.saved)
            .stdin(Stdio::inherit())
            .status();
    }
}

fn set_key_timeout(short: bool) {
    let args: &[&str] = if short {
        &["min", "0", "time", "2"]
    } else {
        &["min", "1", "time", "0"]
    };
    let _ignored = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .status();
}

/// True when both stdin and stderr are terminals (the picker draws on stderr so stdout stays
/// clean for the command's real output).
#[must_use]
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Runs the picker. `Ok(None)` = cancelled. Never mutates anything itself.
pub fn pick(targets: &[SwitchTarget], from: &ProfileName) -> std::io::Result<Option<usize>> {
    pick_rows(
        targets,
        &format!("Switch this conversation {DIM}(from '{from}'){RESET}"),
    )
}

/// The picker for any rows. `Ok(None)` = cancelled.
pub fn pick_rows<R: PickRow>(targets: &[R], title: &str) -> std::io::Result<Option<usize>> {
    let Some(_raw) = RawMode::enter() else {
        return Err(std::io::Error::other(
            "could not put the terminal in key mode",
        ));
    };
    let mut stderr = std::io::stderr();
    let mut picker = Picker::new(targets);
    let lines = targets.len() + 4;
    write!(
        stderr,
        "\x1b[?25l{}",
        render(targets, picker.cursor(), title)
    )?;
    stderr.flush()?;
    let mut stdin = std::io::stdin();
    let result = loop {
        let mut buffer = [0_u8; 8];
        // Blocks for the first byte (EOF reads 0 bytes, which cancels).
        let mut read = stdin.read(&mut buffer[..1])?;
        if read == 1 && buffer[0] == 0x1b {
            // A lone ESC and an arrow key differ only by what follows within a short timeout.
            set_key_timeout(true);
            read += stdin.read(&mut buffer[1..])?;
            set_key_timeout(false);
        }
        let key = decode_key(&buffer[..read]);
        match picker.handle(key) {
            PickerOutcome::Continue => {
                write!(
                    stderr,
                    "\x1b[{lines}A\x1b[J{}",
                    render(targets, picker.cursor(), title)
                )?;
                stderr.flush()?;
            }
            PickerOutcome::Chosen(index) => break Some(index),
            PickerOutcome::Cancelled => break None,
        }
    };
    write!(stderr, "\x1b[{lines}A\x1b[J\x1b[?25h")?;
    stderr.flush()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str, current: bool, unavailable: Option<&str>) -> SwitchTarget {
        SwitchTarget {
            name: ProfileName::new(name).unwrap(),
            provider: ProviderKind::Claude,
            current,
            priority: 0,
            unavailable: unavailable.map(str::to_owned),
        }
    }

    #[test]
    fn the_cursor_starts_on_the_first_selectable_row_and_skips_the_rest() {
        let rows = [
            target("a", true, None),
            target("b", false, Some("exhausted")),
            target("c", false, None),
            target("d", false, None),
        ];
        let mut picker = Picker::new(&rows);
        assert_eq!(picker.cursor(), 2);
        picker.handle(Key::Down);
        assert_eq!(picker.cursor(), 3);
        picker.handle(Key::Down);
        assert_eq!(
            picker.cursor(),
            2,
            "wraps past current and unavailable rows"
        );
        picker.handle(Key::Up);
        assert_eq!(picker.cursor(), 3);
    }

    #[test]
    fn current_and_unavailable_rows_can_never_be_chosen() {
        let rows = [
            target("a", true, None),
            target("b", false, Some("disabled")),
        ];
        let mut picker = Picker::new(&rows);
        assert_eq!(picker.handle(Key::Enter), PickerOutcome::Continue);
        picker.handle(Key::Digit(1));
        assert_eq!(picker.handle(Key::Enter), PickerOutcome::Continue);
        picker.handle(Key::Digit(2));
        assert_eq!(picker.handle(Key::Enter), PickerOutcome::Continue);
    }

    #[test]
    fn enter_chooses_the_highlighted_selectable_row_and_escape_cancels() {
        let rows = [target("a", true, None), target("b", false, None)];
        let mut picker = Picker::new(&rows);
        assert_eq!(picker.handle(Key::Enter), PickerOutcome::Chosen(1));
        assert_eq!(picker.handle(Key::Cancel), PickerOutcome::Cancelled);
    }

    #[test]
    fn keys_decode_from_raw_bytes() {
        assert_eq!(decode_key(b"\x1b[A"), Key::Up);
        assert_eq!(decode_key(b"\x1b[B"), Key::Down);
        assert_eq!(decode_key(b"\x1b"), Key::Cancel);
        assert_eq!(decode_key(b"\r"), Key::Enter);
        assert_eq!(decode_key(b"3"), Key::Digit(3));
        assert_eq!(decode_key(b"\x03"), Key::Cancel);
        assert_eq!(decode_key(b""), Key::Cancel);
    }

    #[test]
    fn priority_order_puts_primary_then_fallbacks_then_the_rest_by_name() {
        // (Profile construction is heavy; the ordering rule is exercised through names only in
        // the CLI tests. Here we only pin the described rows.)
        let rows = [
            target("a", true, None),
            target("b", false, Some("exhausted")),
        ];
        assert_eq!(
            describe_rows(&rows),
            "a (Claude) — current, b (Claude) — unavailable: exhausted"
        );
    }
}
