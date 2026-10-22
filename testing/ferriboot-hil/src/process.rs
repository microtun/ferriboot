//! Bounded subprocesses with live output, including silent-tool heartbeats.
//!
//! A temporary transcript avoids full stdout/stderr pipes blocking the child.
//! It is drained on the test thread so libtest still captures diagnostic output.
use std::{
    fs::File,
    io::Read,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(50);
const HEARTBEAT: Duration = Duration::from_secs(5);

/// Kill and reap on timeout, I/O failure, or unwinding.
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run without a shell, echoing both streams and enforcing a wall-clock limit.
pub fn run(cmd: &mut Command, label: &str, timeout: Duration) -> crate::Result<()> {
    run_with_poll(cmd, label, timeout, || Ok(()))
}

/// Poll a parent-side checkpoint while supervising the child. This lets a
/// worker hold the SWD session while the parent prepares capture for reset.
pub fn run_with_poll(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    mut poll: impl FnMut() -> crate::Result<()>,
) -> crate::Result<()> {
    eprintln!(
        "[{label}] starting {cmd:?} (timeout {:.1}s)",
        timeout.as_secs_f64()
    );
    let transcript = tempfile::NamedTempFile::new()?;
    let writer = transcript.reopen()?;
    // Clones share one write cursor, merging stdout/stderr without overwrites.
    cmd.stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    let mut reader = transcript.reopen()?;
    let started = Instant::now();
    let mut child = Running(
        cmd.spawn()
            .map_err(|e| format!("cannot start {cmd:?}: {e}"))?,
    );
    let mut pending = Vec::new();
    let mut heartbeat = started;
    loop {
        poll()?;
        drain(&mut reader, &mut pending, label)?;
        if let Some(status) = child.0.try_wait()? {
            // Drain the final writes after the process has exited.
            while drain(&mut reader, &mut pending, label)? {}
            flush(&mut pending, label);
            return finish(status, cmd, label, started.elapsed());
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            child.0.kill()?;
            child.0.wait()?;
            while drain(&mut reader, &mut pending, label)? {}
            flush(&mut pending, label);
            return Err(format!(
                "{label} timed out after {:.1}s: {cmd:?}; process killed",
                timeout.as_secs_f64()
            )
            .into());
        }
        if heartbeat.elapsed() >= HEARTBEAT {
            flush(&mut pending, label);
            eprintln!(
                "[{label}] still running ({:.1}s elapsed)",
                elapsed.as_secs_f64()
            );
            heartbeat = Instant::now();
        }
        thread::sleep(POLL.min(timeout.saturating_sub(elapsed)));
    }
}

// Limit each poll's I/O so even a child flooding its output cannot starve the
// timeout. Accept carriage-return progress records as well as ordinary lines.
fn drain(reader: &mut File, pending: &mut Vec<u8>, label: &str) -> std::io::Result<bool> {
    let mut buf = [0; 8192];
    let n = reader.read(&mut buf)?;
    for &byte in &buf[..n] {
        if matches!(byte, b'\n' | b'\r') {
            flush(pending, label);
        } else {
            pending.push(byte);
            if pending.len() >= buf.len() {
                flush(pending, label);
            }
        }
    }
    Ok(n != 0)
}
fn flush(pending: &mut Vec<u8>, label: &str) {
    if !pending.is_empty() {
        eprintln!("[{label}] {}", String::from_utf8_lossy(pending));
        pending.clear();
    }
}
fn finish(status: ExitStatus, cmd: &Command, label: &str, elapsed: Duration) -> crate::Result<()> {
    if !status.success() {
        return Err(format!("{label} failed ({status}): {cmd:?}").into());
    }
    eprintln!("[{label}] finished in {:.1}s", elapsed.as_secs_f64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The current test binary is the child: no Python, shell, or hardware is
    // needed. State is passed only in the child's environment, not set globally.
    fn helper(mode: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "process::tests::child_helper", "--nocapture"])
            .env("FERRIBOOT_TEST_CHILD_MODE", mode);
        cmd
    }
    #[test]
    fn child_helper() {
        match std::env::var("FERRIBOOT_TEST_CHILD_MODE").as_deref() {
            Ok("success") => {
                println!("stdout record");
                eprintln!("stderr record");
            }
            Ok("failure") => std::process::exit(7),
            Ok("hang") => loop {
                thread::park();
            },
            Ok("checkpoint") => {
                let dir =
                    std::path::PathBuf::from(std::env::var_os("FERRIBOOT_TEST_GATE").unwrap());
                std::fs::write(dir.join("ready"), b"").unwrap();
                while !dir.join("release").try_exists().unwrap() {
                    thread::sleep(Duration::from_millis(10));
                }
                println!("checkpoint released");
            }
            _ => {}
        }
    }
    #[test]
    fn successful_and_failed_processes_are_distinguished() {
        run(&mut helper("success"), "test", Duration::from_secs(5)).unwrap();
        let error = run(&mut helper("failure"), "test", Duration::from_secs(5)).unwrap_err();
        assert!(error.to_string().contains("failed"), "{error}");
    }
    #[test]
    fn a_silent_process_is_killed_on_timeout() {
        let started = Instant::now();
        let error = run(&mut helper("hang"), "test", Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn checkpoint_can_release_a_waiting_worker() {
        let gate = tempfile::tempdir().unwrap();
        let mut cmd = helper("checkpoint");
        cmd.env("FERRIBOOT_TEST_GATE", gate.path());
        let mut released = false;
        run_with_poll(&mut cmd, "checkpoint", Duration::from_secs(5), || {
            if !released && gate.path().join("ready").try_exists()? {
                std::fs::write(gate.path().join("release"), b"")?;
                released = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(released);
    }
    #[test]
    fn checkpoint_error_terminates_a_waiting_worker() {
        let started = Instant::now();
        let error = run_with_poll(
            &mut helper("hang"),
            "checkpoint",
            Duration::from_secs(5),
            || Err("capture setup failed".into()),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "capture setup failed");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn merged_stream_is_drained_with_carriage_return_records() {
        use std::io::Write;
        let transcript = tempfile::NamedTempFile::new().unwrap();
        let mut out = transcript.reopen().unwrap();
        let mut err = out.try_clone().unwrap();
        let mut reader = transcript.reopen().unwrap();
        out.write_all(b"out\r").unwrap();
        err.write_all(b"err\ntail").unwrap();
        let mut pending = Vec::new();
        assert!(drain(&mut reader, &mut pending, "test").unwrap());
        assert_eq!(pending, b"tail");
        assert!(!drain(&mut reader, &mut pending, "test").unwrap());
    }
}
