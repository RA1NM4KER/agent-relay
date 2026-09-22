use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use relay_core::{ClaudeConfigMode, Error, Result};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::AUTHENTICATION_OVERRIDE_VARIABLES;

const AUTH_OUTPUT_LIMIT: usize = 64 * 1024;
const VERSION_OUTPUT_LIMIT: usize = 4 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClaudeIdentityPin {
    pub schema_version: u32,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub organization_id: Option<String>,
    pub auth_method: String,
    pub api_provider: String,
}

impl ClaudeIdentityPin {
    #[must_use]
    pub fn matches(&self, observed: &Self) -> bool {
        let identity_matches = match (&self.account_id, &observed.account_id) {
            (Some(expected), Some(actual)) => expected == actual,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => {
                self.email == observed.email && self.organization_id == observed.organization_id
            }
        };
        identity_matches
            && self.auth_method == observed.auth_method
            && self.api_provider == observed.api_provider
    }

    /// Produces a provider-scoped, collision-safe key containing only pin fields.
    #[must_use]
    pub fn stable_id(&self) -> String {
        let mut stable_id = String::from("claude:v1");
        match &self.account_id {
            Some(account_id) => {
                push_identity_field(&mut stable_id, "account", account_id);
            }
            None => {
                push_identity_field(
                    &mut stable_id,
                    "email",
                    self.email.as_deref().unwrap_or_default(),
                );
                push_identity_field(
                    &mut stable_id,
                    "organization",
                    self.organization_id.as_deref().unwrap_or_default(),
                );
            }
        }
        push_identity_field(&mut stable_id, "auth", &self.auth_method);
        push_identity_field(&mut stable_id, "api", &self.api_provider);
        stable_id
    }
}

fn push_identity_field(target: &mut String, name: &str, value: &str) {
    use std::fmt::Write as _;

    write!(target, ":{name}:{}:{value}", value.len()).expect("writing to a String cannot fail");
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnvironmentVariableStatus {
    pub name: &'static str,
    pub present: bool,
    pub conflict: bool,
    pub category: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnvironmentOverrideStatus {
    pub safe: bool,
    pub variables: Vec<EnvironmentVariableStatus>,
}

impl EnvironmentOverrideStatus {
    #[must_use]
    pub fn conflicting_names(&self) -> Vec<&'static str> {
        self.variables
            .iter()
            .filter(|variable| variable.conflict)
            .map(|variable| variable.name)
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClaudeInspectionReport {
    pub schema_version: u32,
    pub provider: &'static str,
    pub config_dir: PathBuf,
    pub claude_version: String,
    pub authenticated: bool,
    pub identity_pin: Option<ClaudeIdentityPin>,
    pub environment_override_status: EnvironmentOverrideStatus,
    pub safe_to_adopt: bool,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub remove_environment: Vec<OsString>,
    pub timeout: Duration,
    pub output_limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult {
    pub success: bool,
    pub stdout: Vec<u8>,
    /// Captured for diagnostics only (see [`sanitize_diagnostic`]) — never returned to a caller
    /// raw, and never used as a data source for anything safety-relevant.
    pub stderr: Vec<u8>,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessResult>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessResult> {
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in &spec.remove_environment {
            command.env_remove(variable);
        }
        for (name, value) in &spec.environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|_| Error::ProviderCommandFailed)?;
        let stdout = child.stdout.take().ok_or(Error::ProviderCommandFailed)?;
        let stderr = child.stderr.take().ok_or(Error::ProviderCommandFailed)?;
        let limit = spec.output_limit;
        let stdout_reader = thread::spawn(move || read_limited(stdout, limit));
        let stderr_reader = thread::spawn(move || read_limited(stderr, limit));

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|_| Error::ProviderCommandFailed)? {
                break status;
            }
            if started.elapsed() >= spec.timeout {
                let _ignored = child.kill();
                let _ignored = child.wait();
                let _ignored = stdout_reader.join();
                let _ignored = stderr_reader.join();
                return Err(Error::ProviderCommandTimeout);
            }
            thread::sleep(Duration::from_millis(20));
        };

        let stdout = stdout_reader
            .join()
            .map_err(|_| Error::ProviderCommandFailed)??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| Error::ProviderCommandFailed)?
            .unwrap_or_default();
        Ok(ProcessResult {
            success: status.success(),
            stdout,
            stderr,
        })
    }
}

/// A bounded, redacted line of the command's own stderr, safe to put in an error message: long
/// token-shaped runs (anything that could be a credential/session id) are replaced with
/// `<redacted>`, and the whole thing is capped so a runaway or binary-garbage stream can never
/// blow up an error string. Never includes environment values — the caller only ever passes
/// captured stderr bytes here, nothing derived from `std::env`.
#[must_use]
pub fn sanitize_diagnostic(stderr: &[u8]) -> Option<String> {
    const MAX_LEN: usize = 300;
    let text = String::from_utf8_lossy(stderr);
    let first_line = text.lines().find(|line| !line.trim().is_empty())?;
    let mut redacted = String::new();
    for word in first_line.split_whitespace() {
        if !redacted.is_empty() {
            redacted.push(' ');
        }
        let looks_like_a_secret = word.len() >= 20
            && word
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/');
        redacted.push_str(if looks_like_a_secret {
            "<redacted>"
        } else {
            word
        });
    }
    if redacted.is_empty() {
        return None;
    }
    if redacted.chars().count() > MAX_LEN {
        redacted = redacted.chars().take(MAX_LEN).collect::<String>() + "…";
    }
    Some(redacted)
}

fn read_limited(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let take_limit = u64::try_from(limit)
        .map_err(|_| Error::MalformedProviderOutput)?
        .saturating_add(1);
    let mut bytes = Vec::new();
    reader
        .take(take_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::ProviderCommandFailed)?;
    if bytes.len() > limit {
        return Err(Error::MalformedProviderOutput);
    }
    Ok(bytes)
}

#[derive(Clone, Debug)]
pub struct ClaudeInspector<R = SystemCommandRunner> {
    executable: PathBuf,
    runner: R,
}

impl ClaudeInspector<SystemCommandRunner> {
    pub fn discover(requested: Option<&Path>) -> Result<Self> {
        Ok(Self {
            executable: resolve_executable(requested)?,
            runner: SystemCommandRunner,
        })
    }
}

impl<R: CommandRunner> ClaudeInspector<R> {
    pub fn with_runner(executable: PathBuf, runner: R) -> Result<Self> {
        Ok(Self {
            executable: validate_executable(&executable)?,
            runner,
        })
    }

    /// The validated, canonicalized executable this inspector resolved to — the same one that
    /// must be used for launching, so callers never re-derive a possibly-different path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn inspect(
        &self,
        config_dir: &Path,
        mode: ClaudeConfigMode,
        environment: EnvironmentOverrideStatus,
    ) -> Result<ClaudeInspectionReport> {
        if !environment.safe {
            return Err(Error::EnvironmentOverrideConflict);
        }
        if mode == ClaudeConfigMode::NativeDefault
            && config_dir != crate::native_default_dir().ok_or(Error::MissingEnvironment("HOME"))?
        {
            return Err(Error::ProviderProfileMismatch);
        }
        let version = self.inspect_version()?;
        if !is_supported_version(&version) {
            return Err(Error::UnsupportedProviderVersion);
        }

        let auth = self.run_auth_status(config_dir, mode)?;
        let mut reasons = Vec::new();
        let identity_pin = if auth.logged_in {
            match auth.identity_pin() {
                Some(identity) => Some(identity),
                None => {
                    reasons.push(
                        "authenticated profile has no safe account identity fields".to_owned(),
                    );
                    None
                }
            }
        } else {
            reasons.push("Claude reports that the profile is not authenticated".to_owned());
            None
        };
        let safe_to_adopt = auth.logged_in && identity_pin.is_some();
        Ok(ClaudeInspectionReport {
            schema_version: 1,
            provider: "claude",
            config_dir: config_dir.to_path_buf(),
            claude_version: version,
            authenticated: auth.logged_in,
            identity_pin,
            environment_override_status: environment,
            safe_to_adopt,
            reasons,
            warnings: vec![
                "Claude profile adoption is reference-only; session transfer remains unverified"
                    .to_owned(),
            ],
        })
    }

    pub fn inspect_version(&self) -> Result<String> {
        let result = self.runner.run(&ProcessSpec {
            executable: self.executable.clone(),
            arguments: vec![OsString::from("--version")],
            environment: BTreeMap::new(),
            remove_environment: override_names(),
            timeout: COMMAND_TIMEOUT,
            output_limit: VERSION_OUTPUT_LIMIT,
        })?;
        if !result.success {
            return Err(diagnostic("claude --version", &result.stderr));
        }
        parse_version(&result.stdout)
    }

    fn run_auth_status(&self, config_dir: &Path, mode: ClaudeConfigMode) -> Result<AuthStatus> {
        let environment = match mode {
            ClaudeConfigMode::Explicit => BTreeMap::from([(
                OsString::from("CLAUDE_CONFIG_DIR"),
                config_dir.as_os_str().to_os_string(),
            )]),
            // NativeDefault: CLAUDE_CONFIG_DIR must stay unset, not "set to config_dir" — the two
            // report different `loggedIn` results for the identical effective directory (see
            // crate::config_mode's doc comment). `remove_environment` below strips any inherited
            // value (e.g. from a parent Claude process of a different profile).
            ClaudeConfigMode::NativeDefault => BTreeMap::new(),
        };
        let mut remove_environment = override_names();
        if mode == ClaudeConfigMode::NativeDefault {
            remove_environment.push(OsString::from("CLAUDE_CONFIG_DIR"));
        }
        let result = self.runner.run(&ProcessSpec {
            executable: self.executable.clone(),
            arguments: vec![
                OsString::from("auth"),
                OsString::from("status"),
                OsString::from("--json"),
            ],
            environment,
            remove_environment,
            timeout: COMMAND_TIMEOUT,
            output_limit: AUTH_OUTPUT_LIMIT,
        })?;
        if !result.success {
            return Err(diagnostic("claude auth status --json", &result.stderr));
        }
        parse_auth_status(&result.stdout, config_dir)
    }
}

/// Turns a failed provider command into an actionable [`Error::ProviderDiagnostic`]: which
/// operation failed, and (when the command left anything on stderr worth showing) a sanitized
/// excerpt of it — never the raw text, never anything that looks like a token.
fn diagnostic(operation: &str, stderr: &[u8]) -> Error {
    let detail = sanitize_diagnostic(stderr)
        .unwrap_or_else(|| "the command exited with a non-zero status".to_owned());
    Error::ProviderDiagnostic {
        operation: operation.to_owned(),
        detail,
    }
}

fn override_names() -> Vec<OsString> {
    AUTHENTICATION_OVERRIDE_VARIABLES
        .iter()
        .map(OsString::from)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AuthStatus {
    logged_in: bool,
    auth_method: String,
    api_provider: String,
    account_id: Option<String>,
    email: Option<String>,
    organization_id: Option<String>,
}

impl AuthStatus {
    fn identity_pin(&self) -> Option<ClaudeIdentityPin> {
        if self.account_id.is_none() && self.email.is_none() {
            return None;
        }
        Some(ClaudeIdentityPin {
            schema_version: 1,
            account_id: self.account_id.clone(),
            email: self.email.as_deref().map(normalize_email),
            organization_id: self.organization_id.clone(),
            auth_method: self.auth_method.clone(),
            api_provider: self.api_provider.clone(),
        })
    }
}

fn parse_auth_status(bytes: &[u8], expected_config_dir: &Path) -> Result<AuthStatus> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::MalformedProviderOutput)?;
    let object = value.as_object().ok_or(Error::MalformedProviderOutput)?;
    validate_auth_keys(object)?;
    let logged_in = required_bool(object, "loggedIn")?;
    let auth_method = required_string(object, "authMethod")?;
    let api_provider = required_string(object, "apiProvider")?;
    let account_id = one_optional_string(object, "accountUuid", "accountId")?;
    let organization_id = one_optional_string(object, "orgId", "organizationId")?;
    let email = optional_string(object, "email")?;
    let _analytics_disabled = optional_bool(object, "analyticsDisabled")?;
    let _organization_name = optional_string(object, "orgName")?;
    let _subscription_type = optional_string(object, "subscriptionType")?;
    validate_reported_path(object, "configDirectory", expected_config_dir)?;
    validate_reported_path(
        object,
        "projectsDirectory",
        &expected_config_dir.join("projects"),
    )?;
    Ok(AuthStatus {
        logged_in,
        auth_method,
        api_provider,
        account_id,
        email,
        organization_id,
    })
}

fn validate_auth_keys(object: &Map<String, Value>) -> Result<()> {
    const KNOWN_KEYS: &[&str] = &[
        "loggedIn",
        "authMethod",
        "apiProvider",
        "accountUuid",
        "accountId",
        "email",
        "orgId",
        "organizationId",
        "subscriptionType",
        "analyticsDisabled",
        "configDirectory",
        "orgName",
        "projectsDirectory",
    ];
    let known: BTreeSet<&str> = KNOWN_KEYS.iter().copied().collect();
    if object.keys().any(|key| !known.contains(key.as_str())) {
        return Err(Error::UnsupportedProviderSchema);
    }
    Ok(())
}

fn required_bool(object: &Map<String, Value>, key: &str) -> Result<bool> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or(Error::MalformedProviderOutput)
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<Option<bool>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(Error::MalformedProviderOutput),
    }
}

fn validate_reported_path(object: &Map<String, Value>, key: &str, expected: &Path) -> Result<()> {
    if let Some(reported) = optional_string(object, key)?
        && !paths_refer_to_same_location(Path::new(&reported), expected)
    {
        return Err(Error::ProviderProfileMismatch);
    }
    Ok(())
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String> {
    optional_string(object, key)?.ok_or(Error::MalformedProviderOutput)
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() && value.len() <= 512 => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(Error::MalformedProviderOutput),
    }
}

fn one_optional_string(
    object: &Map<String, Value>,
    first: &str,
    second: &str,
) -> Result<Option<String>> {
    let first = optional_string(object, first)?;
    let second = optional_string(object, second)?;
    match (first, second) {
        (Some(_), Some(_)) => Err(Error::UnsupportedProviderSchema),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn parse_version(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::MalformedProviderOutput)?;
    let version = text
        .split_whitespace()
        .find(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
                && part.matches('.').count() == 2
        })
        .ok_or(Error::MalformedProviderOutput)?;
    Ok(version.to_owned())
}

pub(crate) fn is_supported_version(version: &str) -> bool {
    let mut parts = version.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("2"), Some("1"), Some(patch), None)
            if !patch.is_empty() && patch.bytes().all(|byte| byte.is_ascii_digit())
    )
}

pub fn inspect_environment(config_dir: &Path) -> EnvironmentOverrideStatus {
    inspect_environment_with(config_dir, |name| std::env::var_os(name))
}

pub fn inspect_environment_with(
    config_dir: &Path,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> EnvironmentOverrideStatus {
    let mut variables = Vec::with_capacity(AUTHENTICATION_OVERRIDE_VARIABLES.len() + 1);
    let selected_config = lookup("CLAUDE_CONFIG_DIR");
    let selected_conflict = selected_config
        .as_deref()
        .is_some_and(|value| !paths_refer_to_same_location(Path::new(value), config_dir));
    variables.push(EnvironmentVariableStatus {
        name: "CLAUDE_CONFIG_DIR",
        present: selected_config.is_some(),
        conflict: selected_conflict,
        category: "profile_selection",
    });
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        let present = lookup(variable).is_some();
        variables.push(EnvironmentVariableStatus {
            name: variable,
            present,
            conflict: present,
            category: override_category(variable),
        });
    }
    let safe = variables.iter().all(|variable| !variable.conflict);
    EnvironmentOverrideStatus { safe, variables }
}

fn paths_refer_to_same_location(first: &Path, second: &Path) -> bool {
    match (fs::canonicalize(first), fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => first == second,
    }
}

fn override_category(variable: &str) -> &'static str {
    if variable.starts_with("CLAUDE_CODE_USE_") {
        "provider_routing"
    } else if variable.contains("BASE_URL")
        || variable.contains("PROJECT_ID")
        || variable.contains("RESOURCE")
        || variable.contains("REGION")
    {
        "provider_endpoint"
    } else {
        "credential_or_identity"
    }
}

fn resolve_executable(requested: Option<&Path>) -> Result<PathBuf> {
    if let Some(requested) = requested {
        return validate_executable(requested);
    }
    let path = std::env::var_os("PATH").ok_or(Error::ProviderExecutableMissing)?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("claude");
        if candidate.is_file() {
            return validate_executable(&candidate);
        }
    }
    Err(Error::ProviderExecutableMissing)
}

fn validate_executable(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|_| Error::ProviderExecutableMissing)?;
    let metadata = fs::metadata(&canonical).map_err(|_| Error::ProviderExecutableMissing)?;
    if !metadata.is_file() {
        return Err(Error::UnsafeProviderExecutable);
    }
    validate_executable_platform(&metadata)?;
    Ok(canonical)
}

#[cfg(unix)]
fn validate_executable_platform(metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 || mode & 0o022 != 0 {
        return Err(Error::UnsafeProviderExecutable);
    }
    let home = std::env::var_os("HOME").ok_or(Error::MissingEnvironment("HOME"))?;
    let home_owner = fs::metadata(home)
        .map_err(|_| Error::UnsafeProviderExecutable)?
        .uid();
    if metadata.uid() != 0 && metadata.uid() != home_owner {
        return Err(Error::UnsafeProviderExecutable);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_executable_platform(_metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_supported_version, normalize_email, parse_auth_status, parse_version};
    use relay_core::Error;

    #[test]
    fn version_parser_is_strict() {
        assert_eq!(
            parse_version(b"2.1.276 (Claude Code)\n").expect("version"),
            "2.1.276"
        );
        assert!(is_supported_version("2.1.276"));
        assert!(!is_supported_version("2.2.0"));
        assert!(matches!(
            parse_version(b"secret"),
            Err(Error::MalformedProviderOutput)
        ));
    }

    #[test]
    fn email_normalization_is_deterministic() {
        assert_eq!(
            normalize_email(" Person@Example.COM "),
            "person@example.com"
        );
    }

    #[test]
    fn auth_parser_accepts_the_known_schema() {
        let parsed = parse_auth_status(
            br#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-1","email":"Person@example.com","orgId":"org-1","subscriptionType":"max"}"#,
            Path::new("/profile"),
        )
        .expect("known schema");
        let pin = parsed.identity_pin().expect("identity pin");
        assert_eq!(pin.account_id.as_deref(), Some("account-1"));
        assert_eq!(pin.email.as_deref(), Some("person@example.com"));
    }

    #[test]
    fn stable_id_follows_identity_pin_matching_rules() {
        let first = parse_auth_status(
            br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"Person@example.com","orgId":"org-1"}"#,
            Path::new("/profile"),
        )
        .expect("first status")
        .identity_pin()
        .expect("first pin");
        let same = parse_auth_status(
            br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"person@example.com","orgId":"org-1"}"#,
            Path::new("/profile"),
        )
        .expect("same status")
        .identity_pin()
        .expect("same pin");
        let different = parse_auth_status(
            br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"other@example.com","orgId":"org-1"}"#,
            Path::new("/profile"),
        )
        .expect("different status")
        .identity_pin()
        .expect("different pin");

        assert!(first.matches(&same));
        assert_eq!(first.stable_id(), same.stable_id());
        assert!(!first.matches(&different));
        assert_ne!(first.stable_id(), different.stable_id());
    }
}
