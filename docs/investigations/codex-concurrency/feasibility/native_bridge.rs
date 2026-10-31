//! Source-insertion experiment. No production Relay linkage.
//! Synchronous bounded pipe callbacks intentionally expose prototype overhead.
//! Not a native scheduler/tool-tree fence or a complete RPC policy.
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn enable_from_env() -> io::Result<()> {
    if std::env::var_os("RELAY_RESEARCH_CALLBACK").is_some() {
        for name in [
            "RELAY_RESEARCH_CLIENT",
            "RELAY_RESEARCH_REGISTRY",
            "CODEX_HOME",
        ] {
            std::env::var(name).map_err(io::Error::other)?;
        }
        ENABLED.store(true, Ordering::SeqCst);
    }
    Ok(())
}

fn gate_policy(method: &str, enabled: bool, failed: bool) -> io::Result<bool> {
    if !enabled {
        return Ok(false);
    }
    // Cleanup remains gated by exact pre-existing ownership, including after
    // quarantine. It cannot grant admission, reactivate or release a root.
    if matches!(method, "turn/interrupt" | "thread/unsubscribe") {
        return Ok(true);
    }
    // Provider-owned auth frames never cross the callback. These metadata
    // queries were inspected in the pinned native processor; no hook execution
    // or attachment mutation is admitted by their list operations.
    if method.starts_with("account/")
        || matches!(
            method,
            "initialize"
                | "model/list"
                | "thread/read"
                | "thread/list"
                | "thread/loaded/list"
                | "thread/turns/list"
                | "thread/items/list"
                | "thread/attachment/list"
                | "hooks/list"
                | "config/read"
                | "configRequirements/read"
                | "skills/list"
                | "plugin/list"
                | "permissionProfile/list"
                | "mcpServerStatus/list"
                | "collaborationMode/list"
        )
    {
        return Ok(false);
    }
    if failed {
        return Err(io::Error::other("research supervision lost"));
    }
    if matches!(
        method,
        "thread/start"
            | "thread/resume"
            | "thread/fork"
            | "turn/start"
            | "turn/steer"
            | "thread/shellCommand"
    ) {
        return Ok(true);
    }
    Err(io::Error::other(format!(
        "unclassified research RPC: {method}"
    )))
}

pub fn requires_gate(method: &str) -> io::Result<bool> {
    gate_policy(
        method,
        ENABLED.load(Ordering::SeqCst),
        FAILED.load(Ordering::SeqCst),
    )
}

static EVENTS: OnceLock<Mutex<Option<File>>> = OnceLock::new();

// Bounded lab observation from the actual client stream, not an observer resume
// and not a durable executor/quiescence receipt. No content/auth frames recorded.
pub fn record_turn(kind: &str, thread: &str, turn: &str) -> io::Result<()> {
    if !ENABLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let result = (|| {
        let client = std::env::var("RELAY_RESEARCH_CLIENT").map_err(io::Error::other)?;
        if client.is_empty()
            || client.len() > 64
            || !client
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(io::Error::other("invalid lab event client identity"));
        }
        let directory = std::path::PathBuf::from(
            std::env::var("RELAY_RESEARCH_REGISTRY").map_err(io::Error::other)?,
        );
        if !directory.is_absolute() {
            return Err(io::Error::other("lab registry must be absolute"));
        }
        let mut guard = EVENTS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| io::Error::other("research event log mutex poisoned"))?;
        if guard.is_none() {
            let path = directory.join(format!("{client}-{}.events.jsonl", std::process::id()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            *guard = Some(options.open(path)?);
        }
        let record = json!({"client":client,"pid":std::process::id(),"kind":kind,
            "thread":thread,"turn":turn,"sequence":SEQUENCE.fetch_add(1,Ordering::SeqCst)});
        if let Some(file) = guard.as_mut() {
            writeln!(file, "{record}")?;
        }
        Ok(())
    })();
    if result.is_err() {
        FAILED.store(true, Ordering::SeqCst);
    }
    result
}

pub fn coverage_lost() -> io::Result<()> {
    if !ENABLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    FAILED.store(true, Ordering::SeqCst);
    record_turn("coverage-lost", "", "")
}

pub struct Ticket {
    child: Child,
    frames: mpsc::Receiver<io::Result<Value>>,
}

impl Ticket {
    pub fn begin(method: &str, thread: Option<&str>) -> io::Result<Self> {
        Self::begin_identity(method, thread, None)
    }
    fn begin_identity(
        method: &str,
        thread: Option<&str>,
        primary: Option<&str>,
    ) -> io::Result<Self> {
        let executable = std::env::var("RELAY_RESEARCH_CALLBACK").map_err(io::Error::other)?;
        if !std::path::Path::new(&executable).is_absolute() {
            return Err(io::Error::other(
                "callback must be an absolute research executable",
            ));
        }
        let mut child = Command::new(executable)
            .env_clear()
            .env("PYTHONNOUSERSITE", "1")
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("callback stdout"))?;
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let result = reader
                    .by_ref()
                    .take(4097)
                    .read_until(b'\n', &mut bytes)
                    .and_then(|n| {
                        if n == 0 || n > 4096 {
                            Err(io::Error::other("callback EOF/limit"))
                        } else {
                            serde_json::from_slice(&bytes).map_err(io::Error::other)
                        }
                    });
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut ticket = Self { child, frames };
        ticket.send(&json!({"method":method,"thread":thread,"primary":primary,
            "operation":SEQUENCE.fetch_add(1,Ordering::SeqCst),
            "client":std::env::var("RELAY_RESEARCH_CLIENT").map_err(io::Error::other)?,
            "registry":std::env::var("RELAY_RESEARCH_REGISTRY").map_err(io::Error::other)?,
            "home":std::env::var("CODEX_HOME").map_err(io::Error::other)?}))?;
        ticket.ack()?;
        Ok(ticket)
    }
    fn send(&mut self, value: &Value) -> io::Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("callback stdin"))?;
        writeln!(stdin, "{value}")?;
        stdin.flush()
    }
    fn ack(&self) -> io::Result<()> {
        let value = match self.frames.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(value)) => value,
            _ => {
                FAILED.store(true, Ordering::SeqCst);
                return Err(io::Error::other("research callback lost/timed out"));
            }
        };
        if value.get("allow").and_then(Value::as_bool) != Some(true) {
            FAILED.store(true, Ordering::SeqCst);
            return Err(io::Error::other("research controller refused operation"));
        }
        Ok(())
    }
    pub fn complete(
        &mut self,
        outcome: &str,
        thread: Option<&str>,
        ephemeral: Option<bool>,
    ) -> io::Result<()> {
        self.send(&json!({"outcome":outcome,"thread":thread,"ephemeral":ephemeral}))?;
        self.ack()
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn selection(primary: Option<&str>, displayed: &str) -> io::Result<()> {
    if !ENABLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    // This reports display state only. It intentionally does not claim ownership.
    let result = Ticket::begin_identity("selection", Some(displayed), primary);
    if result.is_err() {
        FAILED.store(true, Ordering::SeqCst);
    }
    result.map(|_| ())
}

#[cfg(test)]
mod policy_tests {
    use super::gate_policy;
    #[test]
    fn failure_keeps_owned_cleanup_gated_and_writes_closed() {
        for method in ["turn/interrupt", "thread/unsubscribe"] {
            assert!(gate_policy(method, true, true).unwrap());
        }
        for method in [
            "turn/start",
            "thread/start",
            "thread/shellCommand",
            "process/spawn",
        ] {
            assert!(gate_policy(method, true, true).is_err());
        }
        assert!(!gate_policy("thread/read", true, true).unwrap());
    }
    #[test]
    fn metadata_inventory_does_not_allow_mutations_or_change_ordinary_clients() {
        for method in ["hooks/list", "thread/attachment/list"] {
            assert!(!gate_policy(method, true, false).unwrap());
        }
        for method in ["hooks/update", "thread/attachment/add", "process/spawn"] {
            assert!(gate_policy(method, true, false).is_err());
            assert!(!gate_policy(method, false, false).unwrap());
        }
    }
}
