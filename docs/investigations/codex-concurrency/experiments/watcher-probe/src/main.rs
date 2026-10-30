//! Isolated research: invokes the unchanged production watcher without launch orchestration.
//! This deliberately tests assumptions the production ProfileSessionLock currently prevents.
use notify::Watcher;
use relay_provider_codex::rollout::RolloutWatcher;
use std::{
    fs,
    io::Write,
    thread,
    time::{Duration, Instant},
};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::mpsc,
};

// COST PROTOTYPE ONLY: identity is supplied by the harness, never discovered.
// Missing production requirements include durable offsets, replacement recovery,
// lifecycle binding, health transitions, and runtime fencing. No parity claim.
struct Targeted {
    stop: mpsc::Sender<()>,
    events: mpsc::Receiver<String>,
    worker: thread::JoinHandle<()>,
}
impl Targeted {
    fn attach(path: &Path, expected: &str) -> Self {
        let bytes = fs::read(path).unwrap();
        let first = bytes.split(|b| *b == b'\n').next().unwrap();
        let value: serde_json::Value = serde_json::from_slice(first).unwrap();
        assert_eq!(value["payload"]["session_id"], expected);
        let mut file = fs::File::open(path).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        let mut watch = notify::recommended_watcher(move |_| {
            let _ = wake_tx.send(());
        })
        .unwrap();
        watch
            .watch(path, notify::RecursiveMode::NonRecursive)
            .unwrap();
        let (stop, stopped) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _watch = watch;
            let mut pending = Vec::new();
            let mut signaled = false;
            loop {
                if stopped.try_recv().is_ok() {
                    break;
                }
                let _ = wake_rx.recv_timeout(Duration::from_millis(300));
                file.read_to_end(&mut pending).unwrap();
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let v: serde_json::Value = serde_json::from_slice(&pending[..end]).unwrap();
                    pending.drain(..=end);
                    if !signaled
                        && v["type"] == "event_msg"
                        && v["payload"]["type"] == "task_complete"
                        && v["payload"]["error"]["codex_error_info"] == "usage_limit_exceeded"
                    {
                        signaled = true;
                        let _ = tx.send("UsageLimitExceeded".to_owned());
                    }
                }
            }
        });
        Self {
            stop,
            events,
            worker,
        }
    }
    fn stop(self) {
        let _ = self.stop.send(());
        self.worker.join().unwrap();
    }
}
enum Probe {
    Production(RolloutWatcher),
    Targeted(Targeted),
}
impl Probe {
    fn events(&self) -> Vec<String> {
        match self {
            Self::Production(w) => w.drain_events().iter().map(|e| format!("{e:?}")).collect(),
            Self::Targeted(w) => w.events.try_iter().collect(),
        }
    }
    fn healthy(&self) -> bool {
        match self {
            Self::Production(w) => w.is_healthy(),
            Self::Targeted(_) => true,
        }
    }
    fn stop(self) {
        match self {
            Self::Production(w) => w.stop(),
            Self::Targeted(w) => w.stop(),
        }
    }
}

fn header(id: &str) -> String {
    serde_json::json!({"type":"session_meta", "payload":{"session_id":id}}).to_string() + "\n"
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let watchers: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    let history: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let seconds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2);
    let inject = args.get(4).is_none_or(|s| s != "idle");
    let targeted = args.get(5).is_some_and(|s| s == "targeted");
    let historical_fork = args.get(6).is_some_and(|s| s == "historical-fork");
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let sessions = home.join("sessions/2026/10/30");
    fs::create_dir_all(&sessions).unwrap();
    for i in 0..history {
        fs::write(
            sessions.join(format!("rollout-2026-10-30T00-00-00-history-{i}.jsonl")),
            header(&format!("history-{i}")),
        )
        .unwrap();
    }
    let mut paths = Vec::new();
    for i in 0..watchers {
        let id = format!("thread-{i}");
        let path = sessions.join(format!("rollout-2026-10-30T00-00-00-{id}.jsonl"));
        fs::write(&path, header(&id)).unwrap();
        paths.push(path);
    }
    if historical_fork {
        fs::write(sessions.join("rollout-2026-10-30T00-00-00-old-fork.jsonl"),
            serde_json::json!({"type":"session_meta", "payload":{"session_id":"old-fork","forked_from_id":"thread-0"}}).to_string() + "\n").unwrap();
    }
    let started = Instant::now();
    let mut active = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        active.push(if targeted {
            Probe::Targeted(Targeted::attach(path, &format!("thread-{i}")))
        } else {
            Probe::Production(
                RolloutWatcher::attach(
                    &home,
                    &format!("thread-{i}"),
                    &root.path().join(format!("state-{i}.json")),
                )
                .unwrap(),
            )
        });
    }
    let attach_ms = started.elapsed().as_secs_f64() * 1000.0;
    thread::sleep(Duration::from_millis(400));
    let injected = Instant::now();
    if inject {
        let record = serde_json::json!({"type":"event_msg", "ordinal":1,"payload":{"type":"task_complete","error":{"codex_error_info":"usage_limit_exceeded", "message":"synthetic research"}}});
        writeln!(
            fs::OpenOptions::new().append(true).open(&paths[0]).unwrap(),
            "{record}"
        )
        .unwrap();
    }
    thread::sleep(Duration::from_secs(seconds));
    let outcomes: Vec<_> = active
        .iter()
        .enumerate()
        .map(|(i, w)| serde_json::json!({"watcher":i,"healthy":w.healthy(),"events":w.events()}))
        .collect();
    println!(
        "{}",
        serde_json::json!({"mode":if targeted {"targeted-cost-prototype"} else {"production"},"watchers":watchers,"history":history,"attach_ms":attach_ms,"observation_ms":injected.elapsed().as_secs_f64()*1000.0,"outcomes":outcomes})
    );
    for watcher in active {
        watcher.stop();
    }
}
