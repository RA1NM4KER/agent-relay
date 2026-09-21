//! A small, honest progress indicator for slow provider work (starting `codex app-server`, auth
//! inspection, session-liveness confirmation, launching a provider …).
//!
//! It exists only for an interactive human: nothing at all is written unless the process is *not*
//! in `--json` mode **and** stderr is a real terminal, so pipes, redirected output, scripts and
//! machine-readable output never see a label, a spinner frame or an escape sequence.
//!
//! One indicator lives across *all* the slow phases of a command: the label changes
//! ([`Progress::set_label`]) and result lines are printed above it ([`Progress::say`]) while the
//! spinner keeps going, so there is never a blank gap between "the spinner vanished" and "the next
//! thing was printed". The line is fully cleared when it is finished — or dropped on an error path,
//! so the error that follows starts on a clean line. Colour is never used.

use std::{
    io::{IsTerminal as _, Write as _},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const CLEAR_LINE: &str = "\r\u{1b}[2K";
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const UNICODE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_FRAMES: &[&str] = &["|", "/", "-", "\\"];

struct Shared {
    label: Mutex<String>,
    stop: AtomicBool,
    /// Serialises every write to the terminal so a result line never interleaves with a frame.
    write: Mutex<()>,
}

pub struct Progress {
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
    /// `--json`: `say` prints nothing at all.
    quiet: bool,
}

impl Progress {
    /// Starts showing `label` (for example `Checking Codex availability…`) if, and only if, this is
    /// an interactive human session; otherwise returns an inert value.
    #[must_use]
    pub fn start(label: &str, json_mode: bool) -> Self {
        let mut progress = Self::start_if(label, !json_mode && std::io::stderr().is_terminal());
        progress.quiet = json_mode;
        progress
    }

    fn start_if(label: &str, enabled: bool) -> Self {
        if !enabled {
            return Self {
                shared: None,
                worker: None,
                quiet: false,
            };
        }
        let shared = Arc::new(Shared {
            label: Mutex::new(label.to_owned()),
            stop: AtomicBool::new(false),
            write: Mutex::new(()),
        });
        let frames = if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            ASCII_FRAMES
        } else {
            UNICODE_FRAMES
        };
        // Visible at once, before the first frame is even scheduled.
        write_stderr(&format!("{label} "));
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::spawn(move || {
            let mut frame = 0;
            while !worker_shared.stop.load(Ordering::Relaxed) {
                std::thread::sleep(FRAME_INTERVAL);
                let _guard = worker_shared
                    .write
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if worker_shared.stop.load(Ordering::Relaxed) {
                    break;
                }
                let label = worker_shared
                    .label
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                write_stderr(&format!(
                    "{CLEAR_LINE}{label} {}",
                    frames[frame % frames.len()]
                ));
                frame += 1;
            }
        });
        Self {
            shared: Some(shared),
            worker: Some(worker),
            quiet: false,
        }
    }

    /// Changes what the running indicator says (the next frame shows it).
    pub fn set_label(&self, label: &str) {
        if let Some(shared) = &self.shared {
            *shared.label.lock().unwrap_or_else(|e| e.into_inner()) = label.to_owned();
        }
    }

    /// Prints a result line above the indicator, which keeps running underneath it. In a
    /// non-interactive context (no indicator) this is an ordinary `println!` to stdout, so callers
    /// use one code path either way.
    pub fn say(&self, text: &str) {
        match &self.shared {
            Some(shared) => {
                let _guard = shared.write.lock().unwrap_or_else(|e| e.into_inner());
                write_stderr(&format!("{CLEAR_LINE}{text}\n"));
            }
            None if self.quiet => {}
            None => println!("{text}"),
        }
    }

    /// Stops the indicator and clears its line. Also runs on drop, so early returns and errors
    /// never leave a half-drawn line behind.
    pub fn finish(self) {
        drop(self);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let (Some(shared), Some(worker)) = (self.shared.take(), self.worker.take()) {
            shared.stop.store(true, Ordering::Relaxed);
            let _ignored = worker.join();
            write_stderr(CLEAR_LINE);
        }
    }
}

fn write_stderr(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ignored = stderr.write_all(text.as_bytes());
    let _ignored = stderr.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_indicator_is_inert() {
        let progress = Progress::start_if("Checking…", false);
        assert!(progress.worker.is_none());
        progress.set_label("still nothing");
        progress.finish();
    }

    #[test]
    fn json_mode_never_enables_it_even_on_a_terminal() {
        assert!(Progress::start("Checking…", true).worker.is_none());
    }

    #[test]
    fn an_enabled_indicator_changes_label_and_stops_cleanly_on_finish_and_on_drop() {
        let progress = Progress::start_if("Checking…", true);
        assert!(progress.worker.is_some());
        progress.set_label("Preparing…");
        progress.say("a result line");
        progress.finish();
        drop(Progress::start_if("Checking…", true));
    }
}
