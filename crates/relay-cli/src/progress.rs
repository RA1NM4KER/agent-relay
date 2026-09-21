//! A small, honest progress indicator for slow provider checks (for example the structured Codex
//! usage read, which starts `codex app-server`).
//!
//! It exists only for an interactive human: nothing at all is written unless the process is *not*
//! in `--json` mode **and** stderr is a real terminal, so pipes, redirected output, scripts and
//! machine-readable output never see a message, a spinner frame or an escape sequence. The label is
//! printed immediately (a CLI that is about to wait must not look frozen), a spinner animates after
//! it, and the line is fully cleared when the check finishes — or when it is dropped on an error
//! path, so the error that follows starts on a clean line. Colour is never used.

use std::{
    io::{IsTerminal as _, Write as _},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const CLEAR_LINE: &str = "\r\u{1b}[2K";
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const UNICODE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_FRAMES: &[&str] = &["|", "/", "-", "\\"];

pub struct Progress {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Progress {
    /// Starts showing `label` (for example `Checking Codex availability…`) if, and only if, this is
    /// an interactive human session; otherwise returns an inert value.
    #[must_use]
    pub fn start(label: &str, json_mode: bool) -> Self {
        Self::start_if(label, !json_mode && std::io::stderr().is_terminal())
    }

    fn start_if(label: &str, enabled: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if !enabled {
            return Self { stop, worker: None };
        }
        let frames = if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            ASCII_FRAMES
        } else {
            UNICODE_FRAMES
        };
        // Visible at once, before the first frame is even scheduled.
        write_stderr(&format!("{label} "));
        let label = label.to_owned();
        let flag = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            let mut frame = 0;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(FRAME_INTERVAL);
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                write_stderr(&format!("\r{label} {}", frames[frame % frames.len()]));
                frame += 1;
            }
        });
        Self {
            stop,
            worker: Some(worker),
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
        if let Some(worker) = self.worker.take() {
            self.stop.store(true, Ordering::Relaxed);
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
    fn a_disabled_indicator_is_inert_and_writes_nothing() {
        let progress = Progress::start_if("Checking…", false);
        assert!(progress.worker.is_none());
        progress.finish();
    }

    #[test]
    fn json_mode_never_enables_it_even_on_a_terminal() {
        assert!(Progress::start("Checking…", true).worker.is_none());
    }

    #[test]
    fn an_enabled_indicator_stops_and_joins_cleanly_on_finish_and_on_drop() {
        let progress = Progress::start_if("Checking…", true);
        assert!(progress.worker.is_some());
        progress.finish();
        let dropped = Progress::start_if("Checking…", true);
        drop(dropped);
    }
}
