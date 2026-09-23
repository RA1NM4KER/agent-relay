//! A small, honest progress indicator for slow provider work (starting `codex app-server`, auth
//! inspection, session-liveness confirmation, launching a provider …).
//!
//! It exists only for an interactive human: nothing at all is written unless the process is *not*
//! in `--json` mode **and** stderr is a real terminal, so pipes, redirected output, scripts and
//! machine-readable output never see a label, a spinner frame or an escape sequence.
//!
//! The first frame is deliberately delayed ([`START_DELAY`]): an operation that finishes within
//! that window is never drawn at all, so a fast path never flickers a spinner on and immediately
//! off. Nothing is written to the terminal, ever, for a `Progress` that starts and finishes inside
//! the delay — not even the clear sequence, since there is nothing on the line to clear.
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
/// How long a fast operation gets to finish before anything is drawn at all — see the module doc.
const START_DELAY: Duration = Duration::from_millis(250);
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
/// How often the delay wait re-checks for cancellation — deliberately shorter than the delay
/// itself, so `Drop` (which joins the worker unconditionally) never blocks the caller for more
/// than about this long even when the operation finished almost immediately.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);
const UNICODE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_FRAMES: &[&str] = &["|", "/", "-", "\\"];

/// Where rendered text goes — the real terminal in production, a captured buffer in tests, so the
/// delay/flicker-avoidance logic is verifiable without a real terminal or timing-fragile sleeps.
type Sink = Arc<dyn Fn(&str) + Send + Sync>;

struct Shared {
    label: Mutex<String>,
    stop: AtomicBool,
    /// Set the moment a frame is first actually drawn — before that, `Drop` has nothing to clear.
    drawn: AtomicBool,
    /// Serialises every write to the sink so a result line never interleaves with a frame.
    write: Mutex<()>,
    sink: Sink,
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
        let mut progress = Self::start_if(
            label,
            !json_mode && std::io::stderr().is_terminal(),
            Arc::new(write_stderr),
            START_DELAY,
        );
        progress.quiet = json_mode;
        progress
    }

    fn start_if(label: &str, enabled: bool, sink: Sink, start_delay: Duration) -> Self {
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
            drawn: AtomicBool::new(false),
            write: Mutex::new(()),
            sink,
        });
        let frames = if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            ASCII_FRAMES
        } else {
            UNICODE_FRAMES
        };
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::spawn(move || {
            // Slept in small increments and re-checked, not one long sleep: `Drop` joins this
            // thread unconditionally, so an uninterruptible sleep here would make finishing
            // *fast* block the caller for however much of the delay was left — defeating the
            // entire point of the delay.
            let cancelled = wait_cancellable(start_delay, CANCEL_POLL_INTERVAL, || {
                worker_shared.stop.load(Ordering::Relaxed)
            });
            if cancelled {
                // Finished inside the delay window: never draw anything, so there is nothing to
                // clear either — a fast path leaves the terminal completely untouched.
                return;
            }
            let mut frame = 0;
            loop {
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
                worker_shared.drawn.store(true, Ordering::Relaxed);
                (worker_shared.sink)(&format!(
                    "{CLEAR_LINE}{label} {}",
                    frames[frame % frames.len()]
                ));
                frame += 1;
                drop(_guard);
                // Cancellable for the same reason the start delay is: `Drop` joins this thread
                // unconditionally, so it should never have to wait out a whole animation frame
                // just to notice the operation already finished.
                if wait_cancellable(FRAME_INTERVAL, CANCEL_POLL_INTERVAL, || {
                    worker_shared.stop.load(Ordering::Relaxed)
                }) {
                    break;
                }
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
                shared.drawn.store(true, Ordering::Relaxed);
                (shared.sink)(&format!("{CLEAR_LINE}{text}\n"));
            }
            None if self.quiet => {}
            None => println!("{text}"),
        }
    }

    /// Stops the indicator and clears its line (only if a frame was ever actually drawn). Also
    /// runs on drop, so early returns and errors never leave a half-drawn line behind.
    pub fn finish(self) {
        drop(self);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let (Some(shared), Some(worker)) = (self.shared.take(), self.worker.take()) {
            shared.stop.store(true, Ordering::Relaxed);
            let _ignored = worker.join();
            if shared.drawn.load(Ordering::Relaxed) {
                (shared.sink)(CLEAR_LINE);
            }
        }
    }
}

/// Sleeps for up to `total`, checking `cancelled` at least every `step` (and immediately, before
/// ever sleeping at all) — returns `true` the moment it becomes true, `false` once `total` has
/// fully elapsed without that happening. Never oversleeps past `total`.
fn wait_cancellable(total: Duration, step: Duration, cancelled: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + total;
    loop {
        if cancelled() {
            return true;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(step.min(deadline - now));
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
    use std::sync::mpsc;

    /// A sink that forwards every write over a channel, so a test can wait for the *next* write
    /// (or prove none arrives) without sleeping for a fixed guess at how long is "long enough".
    fn capturing_sink() -> (Sink, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let sink: Sink = Arc::new(move |text: &str| {
            let _ignored = tx.send(text.to_owned());
        });
        (sink, rx)
    }

    #[test]
    fn a_disabled_indicator_is_inert() {
        let (sink, rx) = capturing_sink();
        let progress = Progress::start_if("Checking…", false, sink, Duration::ZERO);
        assert!(progress.worker.is_none());
        progress.set_label("still nothing");
        progress.finish();
        assert!(
            rx.try_recv().is_err(),
            "a disabled indicator writes nothing"
        );
    }

    #[test]
    fn json_mode_never_enables_it_even_on_a_terminal() {
        assert!(Progress::start("Checking…", true).worker.is_none());
    }

    /// The core new behaviour: finishing well inside the start delay must never draw a frame, and
    /// therefore must never even emit the clear sequence on drop — the terminal is left completely
    /// untouched, so a fast operation cannot flicker a spinner on and off.
    #[test]
    fn an_operation_finishing_within_the_start_delay_draws_nothing_at_all() {
        let (sink, rx) = capturing_sink();
        let progress = Progress::start_if(
            "Checking…",
            true,
            sink,
            Duration::from_secs(60), // effectively "never" within this test's lifetime
        );
        progress.finish();
        assert!(
            rx.try_recv().is_err(),
            "nothing must be written before the start delay elapses"
        );
    }

    /// Past the start delay, a frame is drawn, `say` prints a result line above it, and `finish`
    /// clears the line — waiting on the channel rather than a fixed sleep, so this cannot flake
    /// under CI scheduling variance.
    #[test]
    fn an_indicator_past_its_start_delay_draws_reports_and_clears_on_finish() {
        let (sink, rx) = capturing_sink();
        let progress = Progress::start_if("Checking…", true, sink, Duration::ZERO);
        assert!(progress.worker.is_some());
        let first_frame = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a frame is drawn once the (zero) delay has elapsed");
        assert!(first_frame.contains("Checking…"));

        progress.set_label("Preparing…");
        progress.say("a result line");
        let said = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("say() writes immediately");
        assert!(said.contains("a result line"));

        progress.finish();
        // finish() joins the worker before returning, so the clear (if any) has already been
        // sent by the time control returns here — no need to wait on the channel for it.
        let mut cleared = false;
        while let Ok(text) = rx.try_recv() {
            cleared |= text == CLEAR_LINE;
        }
        assert!(
            cleared,
            "finish() must clear a line that was actually drawn on"
        );
    }
}
