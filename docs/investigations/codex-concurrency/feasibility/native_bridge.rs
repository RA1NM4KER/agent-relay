//! Source-insertion experiment. No production Relay linkage.
//! Synchronous bounded pipe callbacks intentionally expose prototype overhead.
//! Not a native scheduler/tool-tree fence or a complete RPC policy.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{
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

pub fn requires_gate(method: &str) -> io::Result<bool> {
    if !ENABLED.load(Ordering::SeqCst) {
        return Ok(false);
    }
    if FAILED.load(Ordering::SeqCst) {
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
    // Do not route authentication frames or their values through the callback.
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
                | "thread/unsubscribe"
                | "turn/interrupt"
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
    Err(io::Error::other(format!(
        "unclassified research RPC: {method}"
    )))
}

pub struct Ticket {
    child: Child,
    frames: mpsc::Receiver<io::Result<Value>>,
}

impl Ticket {
    pub fn begin(method: &str, thread: Option<&str>) -> io::Result<Self> {
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
        ticket.send(&json!({"method":method,"thread":thread,
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
    pub fn complete(&mut self, outcome: &str, thread: Option<&str>) -> io::Result<()> {
        self.send(&json!({"outcome":outcome,"thread":thread}))?;
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
    let result = Ticket::begin("selection", Some(displayed));
    let _ = primary; // Kept distinct at the future wire-contract boundary.
    if result.is_err() {
        FAILED.store(true, Ordering::SeqCst);
    }
    result.map(|_| ())
}
