//! Small, generic helpers with no natural home in a single command or domain module: time,
//! terminal prompts, UUID/shell-quoting utilities, and the Herdr pane-token bridge. Each is used
//! by more than one command module — anything used by exactly one stays local to it instead.

use std::path::{Path, PathBuf};

use relay_core::{Error, ProfileName};
use relay_herdr::herdr_client::HerdrCliClient;

/// A short, human-friendly project name for the terminal banner (`relay claude`/`relay resume`) —
/// never the full path, and never a native session UUID (per the M6 UX contract: normal output
/// shows people and projects, not internal identifiers).
pub(crate) fn project_display_name(canonical_project: &Path) -> String {
    canonical_project
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| canonical_project.to_string_lossy().into_owned())
}

pub(crate) fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub(crate) fn prompt_line(question: &str, default: Option<&str>) -> Result<String, Error> {
    use std::io::Write as _;
    match default {
        Some(default) => print!("{question} [{default}]: "),
        None => print!("{question}: "),
    }
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let bytes_read = std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| Error::MissingEnvironment("stdin"))?;
    if bytes_read == 0 {
        // True EOF (closed/exhausted stdin), not just an empty line: never loop forever waiting
        // for input that will never come.
        return Err(Error::MissingEnvironment("stdin"));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        if let Some(default) = default {
            return Ok(default.to_owned());
        }
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool, Error> {
    use std::io::Write as _;
    let hint = if default_yes { "Y/n" } else { "y/N" };
    print!("{question} [{hint}]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let bytes_read = std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| Error::MissingEnvironment("stdin"))?;
    if bytes_read == 0 {
        return Err(Error::MissingEnvironment("stdin"));
    }
    Ok(match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    })
}

pub(crate) fn resolve_initial_message(args_message: &[String]) -> Result<String, Error> {
    if !args_message.is_empty() {
        return Ok(args_message.join(" "));
    }
    loop {
        let line = prompt_line("What would you like Claude to help with?", None)?;
        if !line.trim().is_empty() {
            return Ok(line);
        }
        println!("(please enter a message)");
    }
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Automatic Herdr metadata, only when actually running inside a Herdr pane. Returns whether the
/// pane tokens were written.
pub(crate) fn bind_herdr_pane(
    primary: &ProfileName,
    fallback: &[ProfileName],
    session_id: &str,
) -> bool {
    let herdr_env = std::env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"));
    if !herdr_env {
        return false;
    }
    let Ok(pane_id) = std::env::var("HERDR_PANE_ID") else {
        return false;
    };
    let herdr_bin = std::env::var_os("HERDR_BIN_PATH").map(PathBuf::from);
    let Ok(herdr_client) = HerdrCliClient::discover(herdr_bin.as_deref()) else {
        return false;
    };
    let fallback_value = fallback
        .iter()
        .map(ProfileName::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut pane_tokens: Vec<(&str, &str)> = vec![("relay_profile", primary.as_str())];
    if !fallback_value.is_empty() {
        pane_tokens.push(("relay_profile_fallback", fallback_value.as_str()));
    }
    pane_tokens.push(("relay_session_id", session_id));
    herdr_client
        .set_pane_tokens(&pane_id, "agent-relay", &pane_tokens)
        .is_ok()
}

/// A random RFC 4122 version-4 UUID, as `claude --session-id` requires.
pub(crate) fn new_session_uuid() -> Result<String, Error> {
    use std::io::Read as _;
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|source| Error::Io {
            path: PathBuf::from("/dev/urandom"),
            source,
        })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}
