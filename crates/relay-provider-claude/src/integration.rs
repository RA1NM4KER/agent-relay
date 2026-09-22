//! M2C.1: explicit, opt-in installation of Relay's usage integration into ONE Claude profile.
//!
//! Adds to that profile's `settings.json` only:
//! - a `hooks.StopFailure` group (matcher `rate_limit`) running `relay hook claude stop-failure`;
//! - a `statusLine` command running `relay hook claude statusline`, which records the
//!   `rate_limits` snapshot and then **chains** to any statusline the user already had, passing the
//!   same stdin through and the chained output back unchanged.
//!
//! Guarantees: existing user hooks and settings keys are preserved; the original file is backed up
//! and its hash recorded so uninstall restores it byte for byte; a statusLine Relay cannot chain
//! safely fails the install closed; nothing outside the chosen profile directory is touched.

use std::{
    fs,
    path::{Path, PathBuf},
};

use relay_core::{AtomicWrite, Error, FsAtomicWriter, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::usage_signals::{integration_dir, read_profile_signals};

const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_VERSION: u32 = 1;
const STOP_MARKER: &str = "hook claude stop-failure";
const STATUSLINE_MARKER: &str = "hook claude statusline";
/// The `UserPromptSubmit` hook that answers `/relay …` inside a Claude session without a model turn.
const PROMPT_MARKER: &str = "hook claude prompt";
/// First line of the `/relay` command files Relay installs; only files carrying it are ever
/// touched.
pub const COMMAND_FILE_MARKER: &str = "<!-- agent-relay:managed-command v1 -->";
const COMMAND_DIR: &str = "commands";
/// The bare `/relay` overview command lives at the top of `commands/`.
const COMMAND_FILE: &str = "relay.md";
/// The namespaced `/relay:*` commands live in this subdirectory, giving Claude
/// `commands/relay/<name>.md` -> `/relay:<name>`.
const NAMESPACE_DIR: &str = "relay";

/// One namespaced `/relay:<stem>` command: human-facing frontmatter only — never the internal
/// `agent-relay:managed-command` marker, which stays out of the visible command picker.
struct NamespacedCommand {
    stem: &'static str,
    description: &'static str,
    argument_hint: &'static str,
    fallback: &'static str,
}

const NAMESPACED_COMMANDS: &[NamespacedCommand] = &[
    NamespacedCommand {
        stem: "status",
        description: "See who owns this conversation",
        argument_hint: "",
        fallback: "relay status",
    },
    NamespacedCommand {
        stem: "switch",
        description: "Move this conversation to another profile",
        argument_hint: "[profile]",
        fallback: "relay switch",
    },
    NamespacedCommand {
        stem: "doctor",
        description: "Check whether automatic handoff is ready",
        argument_hint: "",
        fallback: "relay doctor",
    },
    NamespacedCommand {
        stem: "why",
        description: "Explain Relay's current decision/state",
        argument_hint: "",
        fallback: "relay why",
    },
    NamespacedCommand {
        stem: "adopt",
        description: "Bring this conversation under Relay",
        argument_hint: "",
        fallback: "relay adopt",
    },
];

/// The body every Relay command file shares. Claude's own `UserPromptSubmit` hook
/// (`relay hook claude prompt`) answers the real command before any model turn; this text is only
/// what would reach the model if that hook were somehow not running, and it deliberately gives the
/// model nothing to act on.
fn command_file_body(description: &str, argument_hint: &str, fallback: &str) -> String {
    let hint_line = if argument_hint.is_empty() {
        String::new()
    } else {
        format!("argument-hint: {argument_hint}\n")
    };
    format!(
        "{COMMAND_FILE_MARKER}\n---\ndescription: {description}\n{hint_line}---\nAgent Relay answers this command with its own hook before it reaches you. \
It did not run in this session, so do nothing except tell the user: \"Agent Relay's hook did not answer; \
run `{fallback}` in a terminal.\" Do not run commands or guess anything about sessions or profiles.\n"
    )
}

fn overview_command_file_contents() -> String {
    command_file_body(
        "Agent Relay: overview (status, switch, doctor, why, adopt)",
        "",
        "relay status",
    )
}

fn command_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(COMMAND_DIR).join(COMMAND_FILE)
}

fn namespaced_command_file_path(config_dir: &Path, stem: &str) -> PathBuf {
    config_dir
        .join(COMMAND_DIR)
        .join(NAMESPACE_DIR)
        .join(format!("{stem}.md"))
}

/// Every command file Relay wants installed: its path, the human label used in change
/// descriptions (`/relay`, `/relay:status`, ...), and its contents.
fn all_command_files(config_dir: &Path) -> Vec<(PathBuf, String, String)> {
    let mut files = vec![(
        command_file_path(config_dir),
        "/relay".to_owned(),
        overview_command_file_contents(),
    )];
    files.extend(NAMESPACED_COMMANDS.iter().map(|command| {
        (
            namespaced_command_file_path(config_dir, command.stem),
            format!("/relay:{}", command.stem),
            command_file_body(command.description, command.argument_hint, command.fallback),
        )
    }));
    files
}

/// What the install does with one command file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandFilePlan {
    /// Write (or refresh) Relay's own file.
    Write,
    /// Already current.
    Current,
    /// A file that is not Relay's already exists there: it is kept and that command is not
    /// installed.
    ForeignKept,
}

fn plan_command_file(path: &Path, contents: &str) -> CommandFilePlan {
    match fs::read_to_string(path) {
        Ok(existing) if existing == contents => CommandFilePlan::Current,
        Ok(existing) if existing.starts_with(COMMAND_FILE_MARKER) => CommandFilePlan::Write,
        Ok(_) => CommandFilePlan::ForeignKept,
        Err(_) => CommandFilePlan::Write,
    }
}

fn prompt_command(relay: &str, config_dir: &Path) -> String {
    format!(
        "{} {PROMPT_MARKER} --config-dir {}",
        shell_quote(relay),
        shell_quote(&config_dir.to_string_lossy())
    )
}

fn hook_group_is_ours(group: &Value, marker: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|inner| {
            inner.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command_is_ours(command, marker))
            })
        })
}

/// Adds (or refreshes in place) Relay's `UserPromptSubmit` group. Existing groups are kept.
fn ensure_prompt_hook(
    hooks: &mut Map<String, Value>,
    relay: &str,
    config_dir: &Path,
    changes: &mut Vec<String>,
) -> Result<()> {
    let groups = hooks
        .entry("UserPromptSubmit")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = groups.as_array_mut() else {
        return Err(refuse(
            "settings.json `hooks.UserPromptSubmit` is not an array",
        ));
    };
    let wanted = prompt_command(relay, config_dir);
    if groups
        .iter()
        .any(|group| hook_group_is_ours(group, PROMPT_MARKER))
    {
        for group in groups.iter_mut() {
            if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                for hook in inner {
                    if let Some(command) = hook.get_mut("command")
                        && command
                            .as_str()
                            .is_some_and(|text| command_is_ours(text, PROMPT_MARKER))
                        && command.as_str() != Some(wanted.as_str())
                    {
                        *command = Value::String(wanted.clone());
                        changes.push("update the Relay UserPromptSubmit hook command".to_owned());
                    }
                }
            }
        }
    } else {
        groups.push(json!({
            "hooks": [{"type": "command", "command": wanted, "timeout": 30}]
        }));
        changes.push(format!(
            "add hooks.UserPromptSubmit -> `{PROMPT_MARKER}` (answers `/relay …` locally; existing hooks kept)"
        ));
    }
    Ok(())
}

/// Removes only Relay's `UserPromptSubmit` entries.
fn remove_prompt_hook(hooks: &mut Map<String, Value>, changes: &mut Vec<String>) {
    if let Some(groups) = hooks
        .get_mut("UserPromptSubmit")
        .and_then(Value::as_array_mut)
    {
        for group in groups.iter_mut() {
            if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = inner.len();
                inner.retain(|hook| {
                    !hook
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| command_is_ours(command, PROMPT_MARKER))
                });
                if inner.len() != before {
                    changes.push("remove the Relay UserPromptSubmit hook".to_owned());
                }
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|inner| !inner.is_empty())
        });
        if groups.is_empty() {
            hooks.remove("UserPromptSubmit");
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum StatusLineMode {
    /// The profile had no statusLine; Relay added one.
    Added,
    /// The profile had a command statusLine; Relay wraps it and runs it unchanged.
    Chained { original: Value },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationManifest {
    pub version: u32,
    pub installed_unix_ms: u64,
    pub relay_executable: String,
    pub original_settings_present: bool,
    pub original_settings_sha256: Option<String>,
    pub installed_settings_sha256: String,
    pub backup_file: Option<String>,
    pub statusline: StatusLineMode,
}

#[derive(Clone, Debug)]
pub struct InstallPlan {
    pub config_dir: PathBuf,
    pub settings_path: PathBuf,
    pub already_installed: bool,
    /// Human-readable description of every change, for `--dry-run` preview.
    pub changes: Vec<String>,
    original: Option<Vec<u8>>,
    new_settings: Vec<u8>,
    relay_executable: String,
    statusline: StatusLineMode,
    command_files: Vec<(PathBuf, CommandFilePlan, String)>,
}

fn refuse(message: impl Into<String>) -> Error {
    Error::IntegrationRefused(message.into())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn stop_failure_command(relay: &str, config_dir: &Path) -> String {
    format!(
        "{} {STOP_MARKER} --config-dir {}",
        shell_quote(relay),
        shell_quote(&config_dir.to_string_lossy())
    )
}

fn statusline_command(relay: &str, config_dir: &Path, chain: Option<&str>) -> String {
    let mut command = format!(
        "{} {STATUSLINE_MARKER} --config-dir {}",
        shell_quote(relay),
        shell_quote(&config_dir.to_string_lossy())
    );
    if let Some(chain) = chain {
        command.push_str(" --chain ");
        command.push_str(&shell_quote(chain));
    }
    command
}

fn command_is_ours(command: &str, marker: &str) -> bool {
    command.contains(marker)
}

type SettingsRead = (Option<Vec<u8>>, Map<String, Value>);

fn read_settings(path: &Path) -> Result<SettingsRead> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(refuse("settings.json is a symbolic link"));
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(refuse("settings.json is not a regular file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, Map::new()));
        }
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| refuse("settings.json is not valid JSON; fix or move it first"))?;
    match value {
        Value::Object(map) => Ok((Some(bytes), map)),
        _ => Err(refuse("settings.json is not a JSON object")),
    }
}

fn serialize(map: &Map<String, Value>) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(map).map_err(|_| Error::SerializationFailed)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn check_profile_dir(config_dir: &Path) -> Result<()> {
    if !config_dir.is_absolute() {
        return Err(Error::PathNotAbsolute(config_dir.to_path_buf()));
    }
    match fs::symlink_metadata(config_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(Error::SymbolicLink(config_dir.to_path_buf()))
        }
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        _ => Err(Error::NotDirectory(config_dir.to_path_buf())),
    }
}

/// Builds the exact set of changes without writing anything.
pub fn plan_install(config_dir: &Path, relay_executable: &Path) -> Result<InstallPlan> {
    check_profile_dir(config_dir)?;
    let settings_path = config_dir.join("settings.json");
    let (original, mut settings) = read_settings(&settings_path)?;
    let relay = relay_executable.to_string_lossy().into_owned();
    let mut changes = Vec::new();

    if settings.get("disableAllHooks") == Some(&Value::Bool(true)) {
        return Err(refuse(
            "settings.json sets disableAllHooks=true; the StopFailure hook would never run",
        ));
    }

    // Hook.
    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(refuse("settings.json `hooks` is not an object"));
    };
    let groups = hooks
        .entry("StopFailure")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = groups.as_array_mut() else {
        return Err(refuse("settings.json `hooks.StopFailure` is not an array"));
    };
    let hook_present = groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|inner| {
                inner.iter().any(|hook| {
                    hook.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| command_is_ours(command, STOP_MARKER))
                })
            })
    });
    if hook_present {
        // Refresh the command in place if the relay path or profile changed.
        let wanted = stop_failure_command(&relay, config_dir);
        for group in groups.iter_mut() {
            if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                for hook in inner {
                    if let Some(command) = hook.get_mut("command")
                        && command
                            .as_str()
                            .is_some_and(|text| command_is_ours(text, STOP_MARKER))
                        && command.as_str() != Some(wanted.as_str())
                    {
                        *command = Value::String(wanted.clone());
                        changes.push("update the Relay StopFailure hook command".to_owned());
                    }
                }
            }
        }
    } else {
        groups.push(json!({
            "matcher": "rate_limit",
            "hooks": [{
                "type": "command",
                "command": stop_failure_command(&relay, config_dir),
                "timeout": 10
            }]
        }));
        changes.push(format!(
            "add hooks.StopFailure[matcher=rate_limit] -> `{STOP_MARKER}` (existing hooks kept)"
        ));
    }

    // `/relay` in-session control: a UserPromptSubmit hook plus the command file that makes the
    // command known to Claude.
    let hooks_map = settings
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| refuse("settings.json `hooks` is not an object"))?;
    ensure_prompt_hook(hooks_map, &relay, config_dir, &mut changes)?;
    let command_files: Vec<(PathBuf, CommandFilePlan, String)> = all_command_files(config_dir)
        .into_iter()
        .map(|(path, label, contents)| {
            let plan = plan_command_file(&path, &contents);
            match plan {
                CommandFilePlan::Write => changes.push(format!(
                    "add the `{label}` command ({}); it answers without a model turn",
                    path.display()
                )),
                CommandFilePlan::Current => {}
                CommandFilePlan::ForeignKept => changes.push(format!(
                    "keep your existing {} untouched (`{label}` is not installed)",
                    path.display()
                )),
            }
            (path, plan, contents)
        })
        .collect();

    // Status line.
    let mut mode = None;
    let existing = settings.get("statusLine").cloned();
    match existing {
        None => {
            settings.insert(
                "statusLine".to_owned(),
                json!({"type": "command", "command": statusline_command(&relay, config_dir, None)}),
            );
            mode = Some(StatusLineMode::Added);
            changes.push("add a statusLine that records rate_limits".to_owned());
        }
        Some(Value::Object(object)) => {
            let command = object
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    refuse(
                        "existing statusLine has no string `command`; it cannot be chained safely",
                    )
                })?;
            if object.get("type").and_then(Value::as_str) != Some("command") {
                return Err(refuse(
                    "existing statusLine is not `type: command`; it cannot be chained safely",
                ));
            }
            if command_is_ours(command, STATUSLINE_MARKER) {
                // Already Relay's (possibly chained). Keep whichever chain it carries, but
                // refresh the executable path if it moved.
                let chain = existing_chain(command);
                let wanted = statusline_command(&relay, config_dir, chain.as_deref());
                if command != wanted {
                    let mut updated = object.clone();
                    updated.insert("command".to_owned(), Value::String(wanted));
                    settings.insert("statusLine".to_owned(), Value::Object(updated));
                    changes.push("update the Relay statusLine command".to_owned());
                }
            } else {
                let mut wrapped = object.clone();
                wrapped.insert(
                    "command".to_owned(),
                    Value::String(statusline_command(&relay, config_dir, Some(command))),
                );
                settings.insert("statusLine".to_owned(), Value::Object(wrapped));
                mode = Some(StatusLineMode::Chained {
                    original: Value::Object(object),
                });
                changes.push(
                    "wrap the existing statusLine: Relay records rate_limits, then runs your \
                     command unchanged with the same input and shows its output"
                        .to_owned(),
                );
            }
        }
        Some(_) => {
            return Err(refuse(
                "existing statusLine is not an object; it cannot be chained safely",
            ));
        }
    }

    // Reuse the recorded mode when re-running install on an already-installed profile.
    let previous = load_manifest(config_dir).ok().flatten();
    let already_installed = previous.is_some() && changes.is_empty();
    let statusline = match (mode, &previous) {
        (Some(mode), _) => mode,
        (None, Some(previous)) => previous.statusline.clone(),
        (None, None) => StatusLineMode::Added,
    };
    let new_settings = serialize(&settings)?;
    if changes.is_empty() && previous.is_none() {
        changes.push("record the existing Relay hook/statusLine as installed".to_owned());
    }
    Ok(InstallPlan {
        config_dir: config_dir.to_path_buf(),
        settings_path,
        already_installed,
        changes,
        original,
        new_settings,
        relay_executable: relay,
        statusline,
        command_files,
    })
}

/// Extracts the `--chain '<cmd>'` argument from a Relay statusline command, if any.
fn existing_chain(command: &str) -> Option<String> {
    let marker = " --chain '";
    let start = command.find(marker)? + marker.len();
    let rest = &command[start..];
    // Undo shell_quote: the value ends at the final unescaped closing quote.
    let value = rest.strip_suffix('\'')?;
    Some(value.replace("'\\''", "'"))
}

pub fn apply_install(plan: &InstallPlan, now_unix_ms: u64) -> Result<()> {
    if plan.already_installed {
        return Ok(());
    }
    let directory = integration_dir(&plan.config_dir);
    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        path: directory.clone(),
        source,
    })?;
    set_private(&directory);

    let previous = load_manifest(&plan.config_dir).ok().flatten();
    // Keep the ORIGINAL backup across re-installs: it is what uninstall must restore.
    let (backup_file, original_hash, original_present) = match &previous {
        Some(previous) => (
            previous.backup_file.clone(),
            previous.original_settings_sha256.clone(),
            previous.original_settings_present,
        ),
        None => match &plan.original {
            Some(bytes) => {
                let name = format!("settings.json.backup-{now_unix_ms}");
                FsAtomicWriter.write_atomic(&directory.join(&name), bytes)?;
                (Some(name), Some(sha256_hex(bytes)), true)
            }
            None => (None, None, false),
        },
    };
    let manifest = IntegrationManifest {
        version: MANIFEST_VERSION,
        installed_unix_ms: now_unix_ms,
        relay_executable: plan.relay_executable.clone(),
        original_settings_present: original_present,
        original_settings_sha256: original_hash,
        installed_settings_sha256: sha256_hex(&plan.new_settings),
        backup_file,
        statusline: plan.statusline.clone(),
    };
    // Manifest first: if the settings write then fails, uninstall still knows what to restore.
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|_| Error::SerializationFailed)?;
    FsAtomicWriter.write_atomic(&directory.join(MANIFEST_FILE), &manifest_bytes)?;
    FsAtomicWriter.write_atomic(&plan.settings_path, &plan.new_settings)?;
    for (path, file_plan, contents) in &plan.command_files {
        if *file_plan == CommandFilePlan::Write {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            FsAtomicWriter.write_atomic(path, contents.as_bytes())?;
        }
    }
    Ok(())
}

fn set_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ignored = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub fn load_manifest(config_dir: &Path) -> Result<Option<IntegrationManifest>> {
    let path = integration_dir(config_dir).join(MANIFEST_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::CorruptedState),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { path, source }),
    }
}

#[derive(Clone, Debug)]
pub struct UninstallPlan {
    pub config_dir: PathBuf,
    pub installed: bool,
    pub changes: Vec<String>,
    action: UninstallAction,
}

#[derive(Clone, Debug)]
enum UninstallAction {
    Nothing,
    RestoreOriginal { backup: Option<PathBuf> },
    RemoveRelayEntries { new_settings: Vec<u8> },
}

pub fn plan_uninstall(config_dir: &Path) -> Result<UninstallPlan> {
    check_profile_dir(config_dir)?;
    let Some(manifest) = load_manifest(config_dir)? else {
        return Ok(UninstallPlan {
            config_dir: config_dir.to_path_buf(),
            installed: false,
            changes: vec!["nothing to do: the integration is not installed".to_owned()],
            action: UninstallAction::Nothing,
        });
    };
    let settings_path = config_dir.join("settings.json");
    let (current, mut settings) = read_settings(&settings_path)?;
    let unchanged = current
        .as_deref()
        .is_some_and(|bytes| sha256_hex(bytes) == manifest.installed_settings_sha256);
    if unchanged {
        let backup = manifest
            .backup_file
            .as_ref()
            .map(|name| integration_dir(config_dir).join(name));
        if manifest.original_settings_present && backup.is_none() {
            return Err(refuse("the recorded settings backup is missing"));
        }
        if let Some(backup) = &backup {
            let bytes = fs::read(backup).map_err(|source| Error::Io {
                path: backup.clone(),
                source,
            })?;
            if Some(sha256_hex(&bytes)) != manifest.original_settings_sha256 {
                return Err(refuse(
                    "the recorded settings backup does not match its hash",
                ));
            }
        }
        return Ok(UninstallPlan {
            config_dir: config_dir.to_path_buf(),
            installed: true,
            changes: vec![if manifest.original_settings_present {
                "restore settings.json byte for byte from the recorded backup".to_owned()
            } else {
                "remove settings.json (it did not exist before the integration)".to_owned()
            }],
            action: UninstallAction::RestoreOriginal { backup },
        });
    }

    // The user edited settings.json after install: remove only Relay's entries, keep their edits.
    let mut changes = Vec::new();
    if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
        if let Some(groups) = hooks.get_mut("StopFailure").and_then(Value::as_array_mut) {
            for group in groups.iter_mut() {
                if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = inner.len();
                    inner.retain(|hook| {
                        !hook
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| command_is_ours(command, STOP_MARKER))
                    });
                    if inner.len() != before {
                        changes.push("remove the Relay StopFailure hook".to_owned());
                    }
                }
            }
            groups.retain(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_none_or(|inner| !inner.is_empty())
            });
            if groups.is_empty() {
                hooks.remove("StopFailure");
            }
        }
        remove_prompt_hook(hooks, &mut changes);
        if hooks.is_empty() {
            settings.remove("hooks");
        }
    }
    let statusline_command_text = settings
        .get("statusLine")
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if statusline_command_text
        .as_deref()
        .is_some_and(|command| command_is_ours(command, STATUSLINE_MARKER))
    {
        match &manifest.statusline {
            StatusLineMode::Added => {
                settings.remove("statusLine");
                changes.push("remove the Relay statusLine".to_owned());
            }
            StatusLineMode::Chained { original } => {
                settings.insert("statusLine".to_owned(), original.clone());
                changes.push("restore your original statusLine".to_owned());
            }
        }
    }
    changes.push(
        "settings.json changed since install, so only Relay's entries are removed".to_owned(),
    );
    Ok(UninstallPlan {
        config_dir: config_dir.to_path_buf(),
        installed: true,
        changes,
        action: UninstallAction::RemoveRelayEntries {
            new_settings: serialize(&settings)?,
        },
    })
}

pub fn apply_uninstall(plan: &UninstallPlan) -> Result<()> {
    let settings_path = plan.config_dir.join("settings.json");
    match &plan.action {
        UninstallAction::Nothing => return Ok(()),
        UninstallAction::RestoreOriginal { backup } => match backup {
            Some(backup) => {
                let bytes = fs::read(backup).map_err(|source| Error::Io {
                    path: backup.clone(),
                    source,
                })?;
                FsAtomicWriter.write_atomic(&settings_path, &bytes)?;
            }
            None => match fs::remove_file(&settings_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::Io {
                        path: settings_path,
                        source,
                    });
                }
            },
        },
        UninstallAction::RemoveRelayEntries { new_settings } => {
            FsAtomicWriter.write_atomic(&settings_path, new_settings)?;
        }
    }
    // Only a command file that carries Relay's marker is ever removed — a foreign
    // `relay.md`/`relay/*.md` a user might have is always left alone.
    for (path, _, _) in all_command_files(&plan.config_dir) {
        if fs::read_to_string(&path).is_ok_and(|text| text.starts_with(COMMAND_FILE_MARKER)) {
            let _ignored = fs::remove_file(&path);
        }
    }
    // Tidy the namespace directory if Relay's removals left it empty; a non-empty directory
    // (e.g. it still holds a foreign command file) is left untouched.
    let _ignored = fs::remove_dir(plan.config_dir.join(COMMAND_DIR).join(NAMESPACE_DIR));
    // Retire the manifest and recorded signals; keep the settings backup so nothing is lost.
    let directory = integration_dir(&plan.config_dir);
    let _ignored = fs::remove_file(directory.join(MANIFEST_FILE));
    let _ignored = fs::remove_dir_all(directory.join("signals"));
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct IntegrationStatus {
    pub installed: bool,
    pub stop_failure_hook: bool,
    /// `none`, `relay`, `relay_chained` or `foreign` (someone else's statusLine).
    pub statusline: &'static str,
    pub settings_drifted_since_install: bool,
    pub hooks_disabled: bool,
    pub statusline_snapshot_present: bool,
    pub recorded_stop_failures: usize,
    pub recorded_rate_limit_events: usize,
}

pub fn integration_status(config_dir: &Path) -> Result<IntegrationStatus> {
    check_profile_dir(config_dir)?;
    let manifest = load_manifest(config_dir)?;
    let (current, settings) = read_settings(&config_dir.join("settings.json"))?;
    let hook_present = settings
        .get("hooks")
        .and_then(|hooks| hooks.get("StopFailure"))
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|inner| {
                        inner.iter().any(|hook| {
                            hook.get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|command| command_is_ours(command, STOP_MARKER))
                        })
                    })
            })
        });
    let statusline = match settings
        .get("statusLine")
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
    {
        None => "none",
        Some(command) if command_is_ours(command, STATUSLINE_MARKER) => {
            if command.contains(" --chain ") {
                "relay_chained"
            } else {
                "relay"
            }
        }
        Some(_) => "foreign",
    };
    let signals = read_profile_signals(config_dir);
    Ok(IntegrationStatus {
        installed: manifest.is_some() && hook_present,
        stop_failure_hook: hook_present,
        statusline,
        settings_drifted_since_install: manifest.as_ref().is_some_and(|manifest| {
            current
                .as_deref()
                .is_none_or(|bytes| sha256_hex(bytes) != manifest.installed_settings_sha256)
        }),
        hooks_disabled: settings.get("disableAllHooks") == Some(&Value::Bool(true)),
        statusline_snapshot_present: signals.statusline.is_some(),
        recorded_stop_failures: signals.stop_failures.len(),
        recorded_rate_limit_events: signals.rate_limit_events.len(),
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{
        apply_install, apply_uninstall, existing_chain, integration_status, load_manifest,
        plan_install, plan_uninstall,
    };

    const RELAY: &str = "/opt/relay/bin/relay";

    fn write(dir: &Path, value: &Value) -> Vec<u8> {
        let bytes = format!("{}\n", serde_json::to_string_pretty(value).unwrap()).into_bytes();
        fs::write(dir.join("settings.json"), &bytes).unwrap();
        bytes
    }

    fn install(dir: &Path) {
        let plan = plan_install(dir, Path::new(RELAY)).expect("plan");
        apply_install(&plan, 42).expect("apply");
    }

    fn settings(dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
    }

    #[test]
    fn dry_run_plan_writes_nothing() {
        let dir = tempdir().unwrap();
        let before = write(dir.path(), &json!({"theme": "dark"}));
        let plan = plan_install(dir.path(), Path::new(RELAY)).expect("plan");
        assert!(!plan.changes.is_empty());
        assert_eq!(fs::read(dir.path().join("settings.json")).unwrap(), before);
        assert!(!dir.path().join("relay-integration").exists());
    }

    #[test]
    fn install_preserves_existing_keys_and_hooks_and_is_idempotent() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            &json!({"theme": "dark", "hooks": {
                "StopFailure": [{"matcher": "billing_error", "hooks": [{"type": "command", "command": "echo mine"}]}],
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo pre"}]}]
            }}),
        );
        install(dir.path());
        let installed = settings(dir.path());
        assert_eq!(installed["theme"], "dark");
        assert_eq!(
            installed["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo pre"
        );
        let groups = installed["hooks"]["StopFailure"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], "echo mine");
        assert_eq!(groups[1]["matcher"], "rate_limit");
        let again = plan_install(dir.path(), Path::new(RELAY)).expect("plan");
        assert!(again.already_installed);
        apply_install(&again, 99).expect("noop");
        assert_eq!(settings(dir.path()), installed);
    }

    #[test]
    fn an_existing_statusline_is_chained_not_replaced_and_restored_on_uninstall() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            &json!({"statusLine": {"type": "command", "command": "my-status --fancy 'x y'", "padding": 2}}),
        );
        install(dir.path());
        let installed = settings(dir.path());
        let command = installed["statusLine"]["command"].as_str().unwrap();
        assert!(command.contains("hook claude statusline"));
        assert_eq!(installed["statusLine"]["padding"], 2);
        assert_eq!(
            existing_chain(command).as_deref(),
            Some("my-status --fancy 'x y'")
        );
        assert_eq!(
            integration_status(dir.path()).unwrap().statusline,
            "relay_chained"
        );

        // Simulate the user editing settings after install: uninstall must keep their edit.
        let mut edited = installed.clone();
        edited["theme"] = json!("light");
        write(dir.path(), &edited);
        let plan = plan_uninstall(dir.path()).expect("plan");
        apply_uninstall(&plan).expect("uninstall");
        let restored = settings(dir.path());
        assert_eq!(restored["theme"], "light");
        assert_eq!(restored["statusLine"]["command"], "my-status --fancy 'x y'");
        assert!(restored.get("hooks").is_none());
    }

    #[test]
    fn unchainable_statuslines_fail_closed() {
        for statusline in [
            json!({"type": "static", "text": "hi"}),
            json!({"type": "command"}),
            json!("just a string"),
        ] {
            let dir = tempdir().unwrap();
            let before = write(dir.path(), &json!({"statusLine": statusline}));
            let error = plan_install(dir.path(), Path::new(RELAY)).expect_err("must refuse");
            assert_eq!(error.code(), "integration_refused");
            assert_eq!(fs::read(dir.path().join("settings.json")).unwrap(), before);
        }
    }

    #[test]
    fn uninstall_restores_settings_byte_for_byte() {
        let dir = tempdir().unwrap();
        let before = write(dir.path(), &json!({"theme": "dark", "env": {"A": "1"}}));
        install(dir.path());
        assert_ne!(fs::read(dir.path().join("settings.json")).unwrap(), before);
        let plan = plan_uninstall(dir.path()).expect("plan");
        apply_uninstall(&plan).expect("uninstall");
        assert_eq!(fs::read(dir.path().join("settings.json")).unwrap(), before);
        assert!(load_manifest(dir.path()).unwrap().is_none());
    }

    #[test]
    fn uninstall_removes_a_settings_file_that_did_not_exist() {
        let dir = tempdir().unwrap();
        install(dir.path());
        assert!(dir.path().join("settings.json").exists());
        apply_uninstall(&plan_uninstall(dir.path()).unwrap()).expect("uninstall");
        assert!(!dir.path().join("settings.json").exists());
    }

    #[test]
    fn invalid_or_symlinked_settings_and_disabled_hooks_are_refused() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("settings.json"), b"{not json").unwrap();
        assert!(plan_install(dir.path(), Path::new(RELAY)).is_err());
        write(dir.path(), &json!({"disableAllHooks": true}));
        assert!(plan_install(dir.path(), Path::new(RELAY)).is_err());
        let other = tempdir().unwrap();
        fs::write(other.path().join("real.json"), b"{}").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                other.path().join("real.json"),
                other.path().join("settings.json"),
            )
            .unwrap();
            assert!(plan_install(other.path(), Path::new(RELAY)).is_err());
        }
    }

    #[test]
    fn profiles_are_independent() {
        let erika = tempdir().unwrap();
        let megan = tempdir().unwrap();
        install(erika.path());
        assert!(!integration_status(megan.path()).unwrap().installed);
        assert!(integration_status(erika.path()).unwrap().installed);
        assert!(!megan.path().join("settings.json").exists());
    }

    #[test]
    fn install_adds_the_prompt_hook_and_the_relay_command_and_keeps_user_prompt_hooks() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            &json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "mine"}]}]}}),
        );
        install(dir.path());
        let groups = settings(dir.path())["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            groups.len(),
            2,
            "the user's own hook is kept next to Relay's"
        );
        assert!(
            groups
                .iter()
                .any(|group| group.to_string().contains("hook claude prompt"))
        );
        let command = fs::read_to_string(dir.path().join("commands/relay.md")).unwrap();
        assert!(command.starts_with(super::COMMAND_FILE_MARKER));
        // Re-installing changes nothing.
        let plan = plan_install(dir.path(), Path::new(RELAY)).unwrap();
        assert!(plan.already_installed);
    }

    #[test]
    fn an_existing_foreign_relay_command_is_never_overwritten_or_removed() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::write(
            dir.path().join("commands/relay.md"),
            "my own /relay command\n",
        )
        .unwrap();
        install(dir.path());
        assert_eq!(
            fs::read_to_string(dir.path().join("commands/relay.md")).unwrap(),
            "my own /relay command\n"
        );
        let uninstall = plan_uninstall(dir.path()).unwrap();
        apply_uninstall(&uninstall).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("commands/relay.md")).unwrap(),
            "my own /relay command\n"
        );
    }

    #[test]
    fn uninstall_removes_relays_command_and_prompt_hook_only() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            &json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "mine"}]}]}}),
        );
        install(dir.path());
        assert!(dir.path().join("commands/relay.md").exists());
        // The user edits settings after install, so uninstall must strip only Relay's entries.
        let mut edited = settings(dir.path());
        edited["model"] = json!("opus");
        write(dir.path(), &edited);
        apply_uninstall(&plan_uninstall(dir.path()).unwrap()).unwrap();
        assert!(!dir.path().join("commands/relay.md").exists());
        let after = settings(dir.path());
        assert_eq!(after["model"], "opus");
        let hooks = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert!(hooks[0].to_string().contains("mine"));
        assert!(!after.to_string().contains("hook claude prompt"));
    }

    #[test]
    fn uninstall_without_install_is_a_noop() {
        let dir = tempdir().unwrap();
        let plan = plan_uninstall(dir.path()).unwrap();
        assert!(!plan.installed);
        apply_uninstall(&plan).unwrap();
    }

    #[test]
    fn install_writes_all_five_namespaced_commands_with_human_descriptions() {
        let dir = tempdir().unwrap();
        install(dir.path());
        for stem in ["status", "switch", "doctor", "why", "adopt"] {
            let path = dir.path().join("commands/relay").join(format!("{stem}.md"));
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("missing namespaced command file {}", path.display()));
            assert!(text.starts_with(super::COMMAND_FILE_MARKER));
            // The marker line is internal; everything a person sees in the picker must be short
            // and human, never the marker or long implementation detail.
            let description_line = text
                .lines()
                .find(|line| line.starts_with("description:"))
                .expect("description line");
            assert!(!description_line.contains("agent-relay:managed-command"));
            assert!(description_line.len() < 80);
        }
        // Re-installing changes nothing.
        let plan = plan_install(dir.path(), Path::new(RELAY)).unwrap();
        assert!(plan.already_installed);
    }

    #[test]
    fn switch_command_file_carries_a_profile_argument_hint() {
        let dir = tempdir().unwrap();
        install(dir.path());
        let text = fs::read_to_string(dir.path().join("commands/relay/switch.md")).unwrap();
        assert!(text.contains("argument-hint: [profile]"));
    }

    #[test]
    fn a_foreign_namespaced_command_is_never_overwritten_or_removed() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("commands/relay")).unwrap();
        fs::write(
            dir.path().join("commands/relay/status.md"),
            "my own status command\n",
        )
        .unwrap();
        install(dir.path());
        // The other four namespaced commands still get installed.
        assert!(dir.path().join("commands/relay/doctor.md").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("commands/relay/status.md")).unwrap(),
            "my own status command\n"
        );
        let uninstall = plan_uninstall(dir.path()).unwrap();
        apply_uninstall(&uninstall).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("commands/relay/status.md")).unwrap(),
            "my own status command\n"
        );
        // Relay's own namespaced files are removed on uninstall.
        assert!(!dir.path().join("commands/relay/doctor.md").exists());
    }

    #[test]
    fn uninstall_removes_the_namespaced_commands_and_their_now_empty_directory() {
        let dir = tempdir().unwrap();
        install(dir.path());
        assert!(dir.path().join("commands/relay/status.md").exists());
        apply_uninstall(&plan_uninstall(dir.path()).unwrap()).unwrap();
        assert!(!dir.path().join("commands/relay").exists());
        assert!(!dir.path().join("commands/relay.md").exists());
    }
}
