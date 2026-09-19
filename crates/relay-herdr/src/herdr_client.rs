//! A thin, testable subprocess client for `herdr`'s own socket-backed CLI (`herdr pane get`,
//! `herdr workspace get`), used to fetch what `HERDR_PLUGIN_CONTEXT_JSON` does **not** carry.
//!
//! Live-confirmed against Herdr 0.9.0 (`docs/herdr-integration.md`'s M3.2 addendum):
//! `HERDR_PLUGIN_CONTEXT_JSON` is a **flat** object (`workspace_id`, `workspace_cwd`,
//! `focused_pane_id`, `focused_pane_cwd`, `focused_pane_agent`, `focused_pane_status`,
//! `tab_id`/`tab_label`, `invocation_source`, `correlation_id`) with **no `tokens` map and no
//! `agent_session`/session-id field at all** — both were incorrectly assumed to be inline in the
//! M3.1 slice. Getting them requires a real follow-up call to Herdr itself, exactly the way a
//! human operator would (`herdr pane get <id>`, `herdr workspace get <id>`), using the
//! `HERDR_BIN_PATH`/`HERDR_SOCKET_PATH` Herdr already hands every plugin invocation.
//!
//! Herdr's own CLI uses the same stdout(success)/stderr(failure) split as Relay's, confirmed live
//! (`herdr pane get w99:p99` → exit 1, `{"id","error":{"code":"pane_not_found",...}}` on
//! **stderr**, empty stdout), just a different envelope shape: `{"id", "result": ...}` on
//! success, `{"id", "error": {"code", "message"}}` on failure — no `ok` field at all.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::client::{CommandRunner, ProcessSpec, SystemCommandRunner};
use crate::error::HerdrIntegrationError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct AgentSessionView {
    pub kind: String,
    pub value: String,
}

impl AgentSessionView {
    /// `None` if there is genuinely no session reference; `Some(id)` only for a `kind: "id"`
    /// reference; a `kind: "path"` (or any other/future kind) reference is a hard refusal, not a
    /// silent downgrade to "no session" — the pane *does* have a session, it is just not
    /// addressable as an id, and Relay's `--session` flag needs exactly that.
    pub fn resolve_id(session: Option<&Self>) -> Result<Option<String>, HerdrIntegrationError> {
        match session {
            None => Ok(None),
            Some(session) if session.kind == "id" => Ok(Some(session.value.clone())),
            Some(_) => Err(HerdrIntegrationError::SessionReferenceNotAnId),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct PaneInfoView {
    pub agent: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub foreground_cwd: Option<PathBuf>,
    #[serde(default)]
    pub agent_session: Option<AgentSessionView>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct WorkspaceInfoView {
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct HerdrCliClient<R: CommandRunner = SystemCommandRunner> {
    executable: PathBuf,
    runner: R,
    timeout: Duration,
}

impl HerdrCliClient<SystemCommandRunner> {
    /// Resolves the `herdr` executable Herdr itself names via `HERDR_BIN_PATH` (preferred — it is
    /// exactly the binary that invoked this plugin, so there is no PATH-search ambiguity), or an
    /// explicit override, applying the same safety checks as `RelayClient`.
    pub fn discover(
        herdr_bin_path: Option<&std::path::Path>,
    ) -> Result<Self, HerdrIntegrationError> {
        let executable = crate::client::validate_executable_for(
            herdr_bin_path,
            "herdr",
            HerdrIntegrationError::HerdrExecutableMissing,
            HerdrIntegrationError::HerdrUnsafeExecutable,
        )?;
        Ok(Self {
            executable,
            runner: SystemCommandRunner,
            timeout: DEFAULT_TIMEOUT,
        })
    }
}

impl<R: CommandRunner> HerdrCliClient<R> {
    pub fn with_runner(executable: PathBuf, runner: R) -> Self {
        Self {
            executable,
            runner,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Live-confirmed (M3.2): Herdr's own subcommands are **not consistent** about `--json`.
    /// `pane get`/`workspace get` emit JSON-RPC-shaped output unconditionally and *reject*
    /// `--json` as an unknown flag; `plugin link` also emits it unconditionally and accepts no
    /// `--json` flag; but `plugin list` (and, by the same family pattern, `plugin unlink`) print
    /// **human-readable text** unless `--json` is passed explicitly. Each call site below states
    /// which behavior it verified rather than assuming one applies to all of them — the earlier
    /// M3.2 slice originally omitted `--json` here uniformly and it worked for `pane get`/
    /// `workspace get`/`plugin link` purely by chance, then failed silently (parsed a fallback
    /// error, not a crash) on `plugin list`, exactly the kind of gap only real invocation catches.
    fn run_json<T: serde::de::DeserializeOwned>(
        &self,
        args: &[&str],
        needs_json_flag: bool,
    ) -> Result<T, HerdrIntegrationError> {
        let mut arguments: Vec<OsString> = args.iter().map(OsString::from).collect();
        if needs_json_flag {
            arguments.push(OsString::from("--json"));
        }
        let result = self.runner.run(&ProcessSpec {
            executable: self.executable.clone(),
            arguments,
            timeout: self.timeout,
            output_limit: OUTPUT_LIMIT,
        })?;
        if result.success {
            let value: Value = serde_json::from_slice(&result.stdout)
                .map_err(|_| HerdrIntegrationError::HerdrMalformedOutput)?;
            let inner = value
                .get("result")
                .cloned()
                .ok_or(HerdrIntegrationError::HerdrMalformedOutput)?;
            serde_json::from_value(inner).map_err(|_| HerdrIntegrationError::HerdrMalformedOutput)
        } else {
            let value: Value = serde_json::from_slice(&result.stderr)
                .map_err(|_| HerdrIntegrationError::HerdrMalformedOutput)?;
            let error = value
                .get("error")
                .ok_or(HerdrIntegrationError::HerdrMalformedOutput)?;
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .ok_or(HerdrIntegrationError::HerdrMalformedOutput)?
                .to_owned();
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Err(HerdrIntegrationError::HerdrRefused { code, message })
        }
    }

    /// `herdr pane get <pane_id>` — result shape is `{"pane": PaneInfo, "type": "pane_info"}`.
    pub fn pane_get(&self, pane_id: &str) -> Result<PaneInfoView, HerdrIntegrationError> {
        #[derive(Deserialize)]
        struct Wrapper {
            pane: PaneInfoView,
        }
        let wrapper: Wrapper = self.run_json(&["pane", "get", pane_id], false)?;
        Ok(wrapper.pane)
    }

    /// `herdr workspace get <workspace_id>` — result shape is `{"workspace": WorkspaceInfo, ...}`.
    pub fn workspace_get(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceInfoView, HerdrIntegrationError> {
        #[derive(Deserialize)]
        struct Wrapper {
            workspace: WorkspaceInfoView,
        }
        let wrapper: Wrapper = self.run_json(&["workspace", "get", workspace_id], false)?;
        Ok(wrapper.workspace)
    }

    /// `herdr pane report-metadata <pane_id> --source <id> --token NAME=VALUE ...` — live-confirmed
    /// (M4) to print **nothing** on success, exit 0; no JSON envelope at all, unlike every other
    /// write in this client. Verifies success by exit code only, then re-reads the pane via
    /// [`Self::pane_get`] so the caller gets back a confirmed, not assumed, write.
    pub fn set_pane_tokens(
        &self,
        pane_id: &str,
        source: &str,
        tokens: &[(&str, &str)],
    ) -> Result<(), HerdrIntegrationError> {
        let mut args: Vec<String> = vec![
            "pane".to_owned(),
            "report-metadata".to_owned(),
            pane_id.to_owned(),
            "--source".to_owned(),
            source.to_owned(),
        ];
        for (name, value) in tokens {
            args.push("--token".to_owned());
            args.push(format!("{name}={value}"));
        }
        self.run_bare(&args)
    }

    /// `herdr workspace report-metadata <workspace_id> --source <id> --token NAME=VALUE ...` —
    /// same confirmed-by-exit-code-only shape as [`Self::set_pane_tokens`].
    pub fn set_workspace_tokens(
        &self,
        workspace_id: &str,
        source: &str,
        tokens: &[(&str, &str)],
    ) -> Result<(), HerdrIntegrationError> {
        let mut args: Vec<String> = vec![
            "workspace".to_owned(),
            "report-metadata".to_owned(),
            workspace_id.to_owned(),
            "--source".to_owned(),
            source.to_owned(),
        ];
        for (name, value) in tokens {
            args.push("--token".to_owned());
            args.push(format!("{name}={value}"));
        }
        self.run_bare(&args)
    }

    /// Runs a command that is confirmed to print nothing meaningful on success (exit code is the
    /// only signal) and, on failure, the same `{"id","error":{...}}` envelope on stderr every
    /// other command here uses.
    fn run_bare(&self, args: &[String]) -> Result<(), HerdrIntegrationError> {
        let arguments: Vec<OsString> = args.iter().map(OsString::from).collect();
        let result = self.runner.run(&ProcessSpec {
            executable: self.executable.clone(),
            arguments,
            timeout: self.timeout,
            output_limit: OUTPUT_LIMIT,
        })?;
        if result.success {
            return Ok(());
        }
        let value: Value = serde_json::from_slice(&result.stderr)
            .map_err(|_| HerdrIntegrationError::HerdrMalformedOutput)?;
        let error = value
            .get("error")
            .ok_or(HerdrIntegrationError::HerdrMalformedOutput)?;
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .ok_or(HerdrIntegrationError::HerdrMalformedOutput)?
            .to_owned();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Err(HerdrIntegrationError::HerdrRefused { code, message })
    }

    /// `herdr status` — live-confirmed to use a **different, non-JSON-RPC** envelope from every
    /// other subcommand here: no `{"id", ...}` wrapper at all, just
    /// `{"client": {...}, "server": {...}, "update": {...}}` directly on stdout, unconditionally
    /// (even a stopped server is reported this way, not as an error).
    pub fn status(&self) -> Result<HerdrStatus, HerdrIntegrationError> {
        let result = self.runner.run(&ProcessSpec {
            executable: self.executable.clone(),
            arguments: vec![OsString::from("status"), OsString::from("--json")],
            timeout: self.timeout,
            output_limit: OUTPUT_LIMIT,
        })?;
        if !result.success {
            return Err(HerdrIntegrationError::HerdrCommandFailed);
        }
        serde_json::from_slice(&result.stdout)
            .map_err(|_| HerdrIntegrationError::HerdrMalformedOutput)
    }

    /// `herdr plugin link <path>` — links a local plugin directory. Result shape is
    /// `{"plugin": {...}, "type": "plugin_linked"}`.
    pub fn plugin_link(
        &self,
        path: &std::path::Path,
    ) -> Result<PluginRecord, HerdrIntegrationError> {
        #[derive(Deserialize)]
        struct Wrapper {
            plugin: PluginRecord,
        }
        let path_str = path.to_string_lossy();
        let wrapper: Wrapper = self.run_json(&["plugin", "link", path_str.as_ref()], false)?;
        Ok(wrapper.plugin)
    }

    /// `herdr plugin list` — live-confirmed this one prints **human-readable text** unless
    /// `--json` is passed explicitly (unlike `plugin link`, `pane get`, `workspace get`). Result
    /// shape with the flag is `{"plugins": [...], "type": "plugin_list"}`.
    pub fn plugin_list(&self) -> Result<Vec<PluginRecord>, HerdrIntegrationError> {
        #[derive(Deserialize)]
        struct Wrapper {
            plugins: Vec<PluginRecord>,
        }
        let wrapper: Wrapper = self.run_json(&["plugin", "list"], true)?;
        Ok(wrapper.plugins)
    }

    /// `herdr plugin unlink <plugin_id>` — live-confirmed to match `plugin link`, not
    /// `plugin list`: JSON unconditionally, `--json` rejected as an unknown flag (usage error,
    /// exit 2). The earlier assumption that it followed `plugin list`'s pattern was wrong and was
    /// caught by actually running it, not by reasoning from the other two.
    pub fn plugin_unlink(&self, plugin_id: &str) -> Result<(), HerdrIntegrationError> {
        let _: serde_json::Value = self.run_json(&["plugin", "unlink", plugin_id], false)?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct HerdrClientInfo {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct HerdrServerInfo {
    pub status: String,
    pub running: bool,
    pub version: String,
    pub compatible: bool,
}

#[derive(Debug, Deserialize)]
pub struct HerdrStatus {
    pub client: HerdrClientInfo,
    pub server: HerdrServerInfo,
}

#[derive(Debug, Deserialize)]
pub struct PluginRecord {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub min_herdr_version: String,
    pub enabled: bool,
    pub manifest_path: PathBuf,
}
