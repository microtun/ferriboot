//! Terminal status for host-side operations. A percentage is displayed only
//! when a backend reports a total and completed work. Operations without a
//! measurable total show elapsed time, never an animated or fabricated bar.
use anyhow::{Context, ensure};
use std::{
    io::{self, IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const WIDTH: usize = 24;
const TICK: Duration = Duration::from_millis(120);

struct State {
    label: String,
    total: AtomicU64, // 0 means progress cannot be measured
    completed: AtomicU64,
    stopped: AtomicBool,
    start: Instant,
}

/// An updating status display for blocking erase, probe and flash APIs.
/// A determinate bar represents only reported work; an indeterminate
/// operation displays "working" and elapsed time without a bar.
/// Output is on stderr so stdout remains suitable for scripts.
/// Non-terminal output uses newline status messages instead of ANSI escapes.
pub struct Progress {
    state: Arc<State>,
    worker: Option<JoinHandle<()>>,
    terminal: bool,
    last_logged_decile: u64,
    finished: bool,
}

impl Progress {
    pub fn new(label: impl Into<String>, total: Option<u64>) -> Self {
        let terminal = io::stderr().is_terminal();
        let state = Arc::new(State {
            label: label.into(),
            total: AtomicU64::new(total.unwrap_or(0)),
            completed: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            start: Instant::now(),
        });
        if !terminal {
            eprintln!("{}", status(&state, None));
        }
        let active = Arc::clone(&state);
        let worker = Some(thread::spawn(move || {
            let mut frame = 0usize;
            while !active.stopped.load(Ordering::Relaxed) {
                if terminal {
                    // Redraw measured bars promptly. For operations without a
                    // total, updating elapsed time about once a second is enough.
                    if active.total.load(Ordering::Relaxed) != 0 || frame.is_multiple_of(10) {
                        draw(&active, None);
                    }
                } else if frame > 0 && frame.is_multiple_of(50) {
                    // Emit an elapsed-time heartbeat about every six seconds,
                    // including for processes that cannot report byte totals.
                    eprintln!("{}", status(&active, None));
                }
                frame += 1;
                thread::park_timeout(TICK);
            }
        }));
        Self {
            state,
            worker,
            terminal,
            last_logged_decile: 0,
            finished: false,
        }
    }

    /// The backend may provide a total only after preparing a flash layout.
    pub fn set_total(&mut self, total: Option<u64>) {
        self.state
            .total
            .store(total.unwrap_or(0), Ordering::Relaxed);
        self.last_logged_decile = 0;
        self.log_milestone();
    }

    /// Add *reported* completed bytes, never estimated bytes.
    pub fn inc(&mut self, bytes: u64) {
        // Do not wrap the byte counter if a backend sends a bad delta.
        let _ = self
            .state
            .completed
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(bytes))
            });
        self.log_milestone();
    }

    fn log_milestone(&mut self) {
        if self.terminal {
            return;
        }
        let total = self.state.total.load(Ordering::Relaxed);
        if total == 0 {
            return;
        }
        let done = self.state.completed.load(Ordering::Relaxed).min(total);
        let decile = ((done as u128 * 10) / total as u128) as u64;
        if decile > self.last_logged_decile && decile < 10 {
            self.last_logged_decile = decile;
            eprintln!("{}: {}%", self.state.label, decile * 10);
        }
    }

    /// Mark the operation successful without altering the measured amount.
    /// In particular, finish() never upgrades incomplete work to 100%.
    pub fn finish(&mut self) {
        self.end(true);
    }

    fn end(&mut self, success: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.state.stopped.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        if self.terminal {
            draw(&self.state, Some(success));
            eprintln!();
        } else {
            eprintln!("{}", status(&self.state, Some(success)));
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        // An error must never leave a half-rendered bar or a success message.
        self.end(false);
    }
}

fn bar(completed: u64, total: u64) -> String {
    debug_assert!(total > 0);
    let done = completed.min(total) as u128;
    let total = total as u128;
    let filled = ((done * WIDTH as u128) / total) as usize;
    format!(
        "[{}{}] {:>3}%",
        "#".repeat(filled),
        "-".repeat(WIDTH - filled),
        (done * 100) / total
    )
}

fn format_status(
    label: &str,
    completed: u64,
    total: u64,
    elapsed: f64,
    outcome: Option<bool>,
) -> String {
    let description = if total != 0 {
        format!("{label}: {}", bar(completed, total))
    } else if outcome.is_none() {
        format!("{label}: working")
    } else {
        label.to_owned()
    };
    let completion = match outcome {
        Some(true) => " — done",
        Some(false) => " — failed",
        None => "",
    };
    format!("{description}{completion} ({elapsed:.1}s)")
}

fn status(state: &State, outcome: Option<bool>) -> String {
    format_status(
        &state.label,
        state.completed.load(Ordering::Relaxed),
        state.total.load(Ordering::Relaxed),
        state.start.elapsed().as_secs_f64(),
        outcome,
    )
}

fn draw(state: &State, outcome: Option<bool>) {
    let mut stderr = io::stderr().lock();
    let _ = write!(stderr, "\r\x1b[2K{}", status(state, outcome));
    let _ = stderr.flush();
}

/// Run a subprocess with stdout and stderr *live*, retaining its own native
/// progress reporting (Cargo, Docker, espflash). Avoid printing our own
/// updating status at the same time: that would overwrite the child's output.
/// Show an initial working message and an independent elapsed-time outcome.
pub fn run(command: &mut std::process::Command, label: impl Into<String>) -> anyhow::Result<()> {
    let label = label.into();
    let start = Instant::now();
    eprintln!("{label}: working (0.0s; streaming subprocess output)");
    // Command::status inherits stdout/stderr unless explicitly overridden by
    // the caller. Unlike output(), it does not buffer diagnostics until exit.
    let result = command.status();
    match result {
        Ok(exit) => {
            eprintln!(
                "{label}: {} ({:.1}s)",
                if exit.success() { "done" } else { "failed" },
                start.elapsed().as_secs_f64()
            );
            ensure!(exit.success(), "command failed ({exit}): {command:?}");
            Ok(())
        }
        Err(error) => {
            eprintln!("{label}: failed ({:.1}s)", start.elapsed().as_secs_f64());
            Err(error).with_context(|| format!("cannot start {command:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_are_only_reported_for_measured_work() {
        assert!(bar(25, 100).contains("25%"));
        assert!(bar(200, 100).contains("100%"));
        let unknown = format_status("erase", 99, 0, 1.5, None);
        assert!(unknown.contains("working (1.5s)"));
        assert!(!unknown.contains('['));
        assert!(!unknown.contains('%'));
    }

    #[test]
    fn success_is_separate_from_the_measured_percentage() {
        let partial = format_status("program", 25, 100, 1.0, Some(true));
        assert!(partial.contains("25%"));
        assert!(partial.contains("done"));
        assert!(!partial.contains("100%"));
        let unknown = format_status("reset", 0, 0, 2.0, Some(true));
        assert!(unknown.contains("done (2.0s)"));
        assert!(!unknown.contains('['));
        assert!(!unknown.contains('%'));
        let failed = format_status("program", 25, 100, 1.0, Some(false));
        assert!(failed.contains("25%"));
        assert!(failed.contains("failed"));
        assert!(!failed.contains("done"));
    }

    #[test]
    fn lifecycle_finishes_even_when_called_twice() {
        let mut p = Progress::new("test progress", Some(100));
        p.inc(20);
        p.finish();
        p.finish();
    }

    #[cfg(unix)]
    #[test]
    fn child_failure_propagates_without_a_success_bar() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "printf 'test error\n' >&2; exit 7"]);
        assert!(run(&mut cmd, "failing flash operation").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn child_success_can_stream_native_output() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "printf 'native progress 42%%\n' >&2"]);
        assert!(run(&mut cmd, "streaming tool").is_ok());
    }
}
