//! Terminal progress for host-side operations. A determinate bar is shown only
//! when the backend reports completed work; otherwise animate an activity bar
//! without pretending that elapsed time is a completion percentage.
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
    total: AtomicU64, // 0 means progress is indeterminate
    completed: AtomicU64,
    stopped: AtomicBool,
    start: Instant,
}

/// An updating progress bar that remains responsive while a blocking erase,
/// subprocess, or flash API call is in flight. Output is on stderr so stdout
/// remains suitable for scripts. Pipe/CI output uses newline status messages.
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
            eprintln!(
                "{}: {} (started)",
                state.label,
                bar(0, total.unwrap_or(0), 0, false)
            );
        }
        let active = Arc::clone(&state);
        let worker = Some(thread::spawn(move || {
            let mut frame = 0;
            while !active.stopped.load(Ordering::Relaxed) {
                if terminal {
                    draw(&active, frame, false);
                } else if frame > 0 && frame % 50 == 0 {
                    eprintln!(
                        "{}: {} ({:.1}s)",
                        active.label,
                        bar(
                            active.completed.load(Ordering::Relaxed),
                            active.total.load(Ordering::Relaxed),
                            frame,
                            false
                        ),
                        active.start.elapsed().as_secs_f64()
                    );
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

    /// The backend may provide a total only after preparing the flash layout.
    pub fn set_total(&mut self, total: Option<u64>) {
        self.state
            .total
            .store(total.unwrap_or(0), Ordering::Relaxed);
        self.last_logged_decile = 0;
        self.log_milestone();
    }

    /// Add *reported* completed bytes, never estimated bytes.
    pub fn inc(&mut self, bytes: u64) {
        self.state.completed.fetch_add(bytes, Ordering::Relaxed);
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
            draw(&self.state, 0, success);
            eprintln!(" {}", if success { "done" } else { "failed" });
        } else {
            eprintln!(
                "{}: {} ({}, {:.1}s)",
                self.state.label,
                bar(
                    self.state.completed.load(Ordering::Relaxed),
                    self.state.total.load(Ordering::Relaxed),
                    0,
                    success
                ),
                if success { "done" } else { "failed" },
                self.state.start.elapsed().as_secs_f64()
            );
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        // Errors must never leave a half-rendered progress bar on the terminal.
        self.end(false);
    }
}

fn bar(completed: u64, total: u64, frame: usize, success: bool) -> String {
    if success {
        return format!("[{}] 100%", "#".repeat(WIDTH));
    }
    if total > 0 {
        let filled = ((completed.min(total) as u128 * WIDTH as u128) / total as u128) as usize;
        return format!(
            "[{}{}] {:>3}%",
            "#".repeat(filled),
            "-".repeat(WIDTH - filled),
            ((completed.min(total) as u128 * 100) / total as u128)
        );
    }
    // Moving segment = activity, not an invented percentage.
    let travel = 2 * (WIDTH - 4);
    let step = frame % travel;
    let offset = if step < WIDTH - 4 {
        step
    } else {
        travel - step
    };
    format!(
        "[{}{}{}] working",
        " ".repeat(offset),
        "====",
        " ".repeat(WIDTH - offset - 4)
    )
}

fn draw(state: &State, frame: usize, success: bool) {
    let total = state.total.load(Ordering::Relaxed);
    let completed = state.completed.load(Ordering::Relaxed);
    let status = bar(completed, total, frame, success);
    let mut stderr = io::stderr().lock();
    let _ = write!(
        stderr,
        "\r\x1b[2K{} {} ({:.1}s)",
        state.label,
        status,
        state.start.elapsed().as_secs_f64()
    );
    let _ = stderr.flush();
}

/// Run a tool that does not expose byte-level progress. Its diagnostics are
/// replayed unchanged after completion; activity remains visible meanwhile.
pub fn run(command: &mut std::process::Command, label: impl Into<String>) -> anyhow::Result<()> {
    let mut progress = Progress::new(label, None);
    let output = command
        .output()
        .with_context(|| format!("cannot start {command:?}"))?;
    // Stop rendering before replaying the child's diagnostics; otherwise the
    // spinner can overwrite important errors on a real terminal.
    if output.status.success() {
        progress.finish();
    } else {
        drop(progress);
    }
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    ensure!(
        output.status.success(),
        "command failed ({}): {command:?}",
        output.status
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_percentage_is_bounded_and_indeterminate_has_no_percentage() {
        assert!(bar(25, 100, 0, false).contains("25%"));
        assert!(bar(200, 100, 0, false).contains("100%"));
        assert!(!bar(99, 0, 0, false).contains('%'));
        assert!(bar(0, 0, 0, true).contains("100%"));
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
}
