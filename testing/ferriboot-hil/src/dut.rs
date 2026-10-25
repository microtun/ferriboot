//! The device under test: a physical board or an isolated QEMU guest.
//!
//! ESP32-C3 writes its full image with the Rust espflash worker, then resets over the
//! USB CDC console. RP2350 uses a bounded `ferriboot-flash` SWD worker for
//! fixture transport and probe resets. The worker exits before assertions so
//! it cannot mask watchdog recovery.
//!
//! Console lines are kept for the assertions and echoed with `eprintln!`, so
//! libtest shows them only for failing tests (or live with `--nocapture`).

use std::{
    io::ErrorKind,
    process::Command,
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle, sleep},
    time::{Duration, Instant},
};

use serialport::SerialPort;
#[cfg(feature = "qemu")]
use std::io::{BufRead, BufReader};

use crate::{
    Board, Error, NativeSerialPort,
    flash::{FLASH_SIZE, FlashImage},
};

pub const BAUD: u32 = 115_200;
const READ_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_CONSOLE_LINE: usize = 16 * 1024;

/// A chip reset (panic, watchdog, ...) may re-enumerate a USB console; how
/// long to wait for it.
const REENUMERATE: Duration = Duration::from_secs(10);

/// There is one board: scenarios take turns.
static PHYSICAL_BOARD: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct ConsoleState {
    lines: Vec<String>,
    /// The reader has stopped; no more lines will arrive.
    closed: bool,
}

/// All console output and reader-lifecycle notifications share one lock.
#[derive(Default)]
struct CapturedConsole {
    state: Mutex<ConsoleState>,
    changed: Condvar,
}

type SharedConsole = Arc<CapturedConsole>;

impl CapturedConsole {
    fn mark(&self) -> usize {
        self.state.lock().unwrap().lines.len()
    }

    fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    #[cfg(feature = "esp32c3-supermini")]
    fn contains_from(&self, from: usize, pred: impl Fn(&str) -> bool) -> bool {
        self.state
            .lock()
            .unwrap()
            .lines
            .iter()
            .skip(from)
            .any(|line| pred(line))
    }

    fn push_line(&self, board: &Board, bytes: &[u8]) {
        let line = String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        eprintln!("[{}] {line}", board.name);
        self.state.lock().unwrap().lines.push(line);
        self.changed.notify_all();
    }

    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.changed.notify_all();
    }

    fn wait_from(
        &self,
        from: usize,
        timeout: Duration,
        pred: impl Fn(&str) -> bool,
    ) -> Option<(usize, String)> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        let mut next = from;
        loop {
            while next < state.lines.len() {
                if pred(&state.lines[next]) {
                    return Some((next, state.lines[next].clone()));
                }
                next += 1;
            }
            let now = Instant::now();
            if now >= deadline || state.closed {
                return None;
            }
            state = self.changed.wait_timeout(state, deadline - now).unwrap().0;
        }
    }
}

/// Requests to the reader thread, which owns the port.
enum Cmd {
    Stop,
    #[cfg(feature = "esp32c3-supermini")]
    Reset(mpsc::Sender<crate::Result<()>>),
}

pub struct Dut {
    board: &'static Board,
    console: SharedConsole,
    cmds: mpsc::Sender<Cmd>,
    reader: Option<JoinHandle<()>>,
    _board_lock: Option<crate::board_lock::BoardLock>,
    _local_guard: Option<MutexGuard<'static, ()>>,
    #[cfg(feature = "qemu")]
    virtual_board: Option<Mutex<crate::qemu::session::Session>>,
}

/// Open a console at [`BAUD`] with the reader's timeout. Boards whose console
/// needs more (modem lines, say) wrap this in their [`Console::open`](crate::Console).
pub fn open_serial(name: &str) -> serialport::Result<NativeSerialPort> {
    serialport::new(name, BAUD)
        .timeout(READ_TIMEOUT)
        .open_native()
}

impl Dut {
    /// Program the board with `flash`, start capturing the console and
    /// reset into it.
    pub fn boot(flash: &FlashImage) -> crate::Result<Self> {
        #[cfg(feature = "qemu")]
        if std::ptr::eq(flash.board(), &crate::qemu::BOARD) {
            let board = crate::qemu::VirtualBoard::new(0)?;
            return board.boot(flash);
        }
        Self::boot_physical(flash)
    }

    fn boot_physical(flash: &FlashImage) -> crate::Result<Self> {
        let board = flash.board();
        let guard = PHYSICAL_BOARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let var = board.env("PORT");
        let name =
            std::env::var(&var).map_err(|_| format!("set {var} to {}", board.console.port_hint))?;
        // The Rust flasher needs exclusive use of ESP32-C3's USB CDC port.
        // Open RP2350's independent UART before programming, but open the
        // ESP32-C3 console only after the Rust flasher has exited.
        #[cfg(feature = "esp32c3-supermini")]
        let serial_flash = std::ptr::eq(board, &*crate::esp32c3::BOARD);
        #[cfg(not(feature = "esp32c3-supermini"))]
        let serial_flash = false;
        let port = if serial_flash {
            None
        } else {
            eprintln!("[dut] opening console {name}");
            Some((board.console.open)(&name)?)
        };
        eprintln!("[dut] programming the flash fixture");
        // Serialize ownership of the physical board across HIL test binaries.
        let lock = crate::board_lock::BoardLock::acquire(board, &name)?;
        let (cmds, rx) = mpsc::channel();
        let mut dut = Self {
            board,
            console: SharedConsole::default(),
            cmds,
            reader: None,
            _board_lock: Some(lock),
            _local_guard: Some(guard),
            #[cfg(feature = "qemu")]
            virtual_board: None,
        };
        // The RP2350 worker pauses at a reset checkpoint; ESP32-C3 opens
        // CDC after its flasher exits. Discard stale output at that point.
        let mut capture = Some((port, rx));
        program(board, &name, &flash.bytes(), || {
            let (port, rx) = capture.take().ok_or("console capture already started")?;
            dut.start_physical_console(&name, port, rx)
        })?;
        Ok(dut)
    }

    /// Start the reader at the worker's reset checkpoint, before the device
    /// reboots. ESP32-C3 must reopen CDC after flashing; RP2350 already has
    /// its independent UART handle.
    fn start_physical_console(
        &mut self,
        name: &str,
        port: Option<NativeSerialPort>,
        rx: mpsc::Receiver<Cmd>,
    ) -> crate::Result<()> {
        let port = match port {
            Some(port) => port,
            None => {
                eprintln!("[dut] opening console {name} after the flasher exited");
                reopen(name, self.board.console.open)?
            }
        };
        port.clear(serialport::ClearBuffer::All)?;
        // Clone before moving the port into the reader. The reader must be
        // live before the CDC reset so the earliest boot messages are seen.
        #[cfg(feature = "esp32c3-supermini")]
        let mut boot_control = if std::ptr::eq(self.board, &*crate::esp32c3::BOARD) {
            Some(port.try_clone_native()?)
        } else {
            None
        };
        let board = self.board;
        let console = self.console.clone();
        let name = name.to_owned();
        self.reader = Some(thread::spawn(move || {
            read_console(board, &name, port, board.console.open, &console, &rx)
        }));
        // RP2350 is reset by the probe worker after this callback returns.
        #[cfg(feature = "esp32c3-supermini")]
        if let Some(ref mut control) = boot_control {
            eprintln!("[dut] exiting ESP32-C3 ROM download mode via USB-Serial/JTAG reset");
            crate::esp32c3::boot_from_download(control)?;
        }
        Ok(())
    }

    /// Number of console lines so far; pass to the `expect_*` methods to only
    /// consider later output.
    pub fn mark(&self) -> usize {
        self.console.mark()
    }

    /// Wait for a line at index `from` or later containing `needle`. Returns
    /// its index and text.
    pub fn expect_from(
        &self,
        from: usize,
        needle: &str,
        timeout: Duration,
    ) -> crate::Result<(usize, String)> {
        self.console
            .wait_from(from, timeout, |line| line.contains(needle))
            .ok_or_else(|| {
                #[cfg(feature = "esp32c3-supermini")]
                if std::ptr::eq(self.board, &*crate::esp32c3::BOARD)
                    && needle == "@fbt hello"
                    && self.console.contains_from(from, |line| {
                        line.contains("waiting for download") || line.contains("DOWNLOAD(USB/UART")
                    })
                {
                    return "ESP32-C3 is still in ROM download mode after flashing; \
                            release BOOT/GPIO9 (if held low), check reset control \
                            lines, and perform an EN/power-cycle reset if needed"
                        .into();
                }
                format!("timed out waiting for {needle:?}").into()
            })
    }

    /// Fail if a line matching `pred` appears at `from` or later within `window`.
    pub fn expect_quiet_from(
        &self,
        from: usize,
        window: Duration,
        what: &str,
        pred: impl Fn(&str) -> bool,
    ) -> crate::Result<()> {
        match self.console.wait_from(from, window, pred) {
            Some((_, line)) => Err(format!("unexpected {what}: {line}").into()),
            None if self.console.is_closed() => {
                Err("console reader stopped during quiet assertion".into())
            }
            None => Ok(()),
        }
    }

    /// Reset the device, keeping its flash contents and console capture.
    pub fn reset(&self) -> crate::Result<()> {
        #[cfg(feature = "qemu")]
        if let Some(session) = &self.virtual_board {
            return session.lock().unwrap().reset();
        }
        // Reset on the console reader thread: it owns the current USB CDC
        // handle, including after an autonomous reset re-enumerated the port.
        // No second serial open or worker process is needed.
        #[cfg(feature = "esp32c3-supermini")]
        if std::ptr::eq(self.board, &*crate::esp32c3::BOARD) {
            let (tx, rx) = mpsc::channel();
            self.cmds.send(Cmd::Reset(tx)).map_err(Error::external)?;
            rx.recv_timeout(Duration::from_secs(30))
                .map_err(Error::external)??;
            return Ok(());
        }
        let mut cmd = Command::new(flash_worker(self.board)?);
        cmd.arg(self.board.name).arg("--reset");
        crate::process::run(&mut cmd, "probe reset", probe_timeout(self.board, false)?)
            .map_err(|e| with_probe_hint(self.board, e))
    }

    #[cfg(feature = "qemu")]
    pub(crate) fn boot_qemu(
        flash: &FlashImage,
        board: crate::qemu::VirtualBoard,
    ) -> crate::Result<Self> {
        let (session, output, listener) =
            crate::qemu::session::Session::spawn(&flash.bytes(), board)?;
        let (cmds, _) = mpsc::channel();
        let mut dut = Self {
            board: flash.board(),
            console: SharedConsole::default(),
            cmds,
            reader: None,
            _board_lock: None,
            _local_guard: None,
            virtual_board: Some(Mutex::new(session)),
        };
        let console = dut.console.clone();
        let board = dut.board;
        dut.reader = Some(thread::spawn(move || {
            read_qemu_console(board, output, &console)
        }));
        dut.virtual_board
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .start(listener)?;
        Ok(dut)
    }
}

impl Drop for Dut {
    fn drop(&mut self) {
        // Kill and reap QEMU before joining its pipe reader (also on startup failure).
        #[cfg(feature = "qemu")]
        drop(self.virtual_board.take());
        // Release the port before the next scenario opens it.
        let _ = self.cmds.send(Cmd::Stop);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Cargo builds binaries alongside integration tests (including test=false
/// binaries). Resolve the worker beside the profile's `deps` directory, also
/// supporting a caller already in that directory's parent.
pub(crate) fn flash_worker(board: &Board) -> crate::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| format!("cannot locate {} flash worker", board.chip))?;
    let dir = if dir.file_name().is_some_and(|name| name == "deps") {
        dir.parent()
            .ok_or("cannot locate Cargo profile directory")?
    } else {
        dir
    };
    let bin = "ferriboot-flash";
    let worker = dir.join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    if !worker.is_file() {
        return Err(format!(
            "{} flash worker missing at {}; build it with \
             cargo build -p ferriboot-hil --features {} --bin {bin}",
            board.chip,
            worker.display(),
            board.name
        )
        .into());
    }
    Ok(worker)
}

fn with_probe_hint<E: std::fmt::Display>(board: &Board, error: E) -> Error {
    match &board.probe {
        Some(probe) => Error::new(format!("{error}; {}", probe.hint)),
        None => Error::external(error),
    }
}

fn probe_timeout(board: &Board, programming: bool) -> crate::Result<Duration> {
    let var = board.env("PROBE_TIMEOUT_SECS");
    Ok(match std::env::var(&var) {
        Ok(value) => {
            let secs: u64 = value
                .parse()
                .map_err(|_| format!("{var} must be a positive integer"))?;
            if secs == 0 {
                return Err(format!("{var} must be positive").into());
            }
            Duration::from_secs(secs)
        }
        Err(std::env::VarError::NotPresent) => {
            Duration::from_secs(if programming { 600 } else { 30 })
        }
        Err(error) => return Err(error.into()),
    })
}

/// Program one complete 4 MiB fixture. ESP32-C3 flashes with the Rust espflash worker,
/// which exits before the console is opened. RP2350 uses a gated SWD worker
/// to retain its probe session through reset.
fn program(
    board: &Board,
    _port: &str,
    bytes: &[u8],
    mut before_reset: impl FnMut() -> crate::Result<()>,
) -> crate::Result<()> {
    if bytes.len() != FLASH_SIZE {
        return Err("expected complete 4 MiB flash image".into());
    }
    let inputs = tempfile::tempdir()?;
    let fixture = inputs.path().join("fixture.bin");
    std::fs::write(&fixture, bytes)?;
    #[cfg(feature = "esp32c3-supermini")]
    if std::ptr::eq(board, &*crate::esp32c3::BOARD) {
        // Release the serial port before console capture and CDC reset.
        crate::esp32c3::flash::write_full_image(_port, &fixture)?;
        return before_reset();
    }
    let mut cmd = Command::new(flash_worker(board)?);
    cmd.arg(board.name)
        .arg(fixture)
        .env(board.env("RESET_GATE"), inputs.path());
    // RP2350: one bounded child owns the probe through restoration and reset.
    // A hung USB operation can still be killed by the parent timeout.
    let ready = inputs.path().join("ready");
    let release = inputs.path().join("release");
    let mut released = false;
    crate::process::run_with_poll(
        &mut cmd,
        "flash fixture",
        probe_timeout(board, true)?,
        || {
            if !released && ready.try_exists()? {
                before_reset()?;
                std::fs::write(&release, b"")?;
                released = true;
            }
            Ok(())
        },
    )
    .map_err(|e| with_probe_hint(board, e))
}

/// Accumulate serial chunks without exposing partial lines to assertions.
#[derive(Default)]
struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    /// Return false if an unterminated line has grown beyond the capture limit.
    fn receive(&mut self, board: &Board, console: &SharedConsole, bytes: &[u8]) -> bool {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            console.push_line(board, &line);
        }
        self.pending.len() <= MAX_CONSOLE_LINE
    }

    fn finish(self, board: &Board, console: &SharedConsole) {
        if !self.pending.is_empty() {
            console.push_line(board, &self.pending);
        }
    }
}

/// Reader thread: split the console into lines. On a read error, reopen the
/// port if the board's console can re-enumerate; otherwise stop.
fn read_console<P, F>(
    board: &Board,
    name: &str,
    mut port: P,
    open: F,
    console: &SharedConsole,
    cmds: &mpsc::Receiver<Cmd>,
) where
    P: SerialPort,
    F: Fn(&str) -> serialport::Result<P>,
{
    let mut lines = LineBuffer::default();
    let mut buf = [0u8; 512];
    loop {
        match cmds.try_recv() {
            Ok(Cmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => break,
            #[cfg(feature = "esp32c3-supermini")]
            Ok(Cmd::Reset(ack)) => {
                // The console remains open throughout this control-line reset.
                let result = crate::esp32c3::boot_from_download(&mut port).map_err(Error::external);
                let _ = ack.send(result);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let error = match port.read(&mut buf) {
            Ok(0) => std::io::Error::from(ErrorKind::UnexpectedEof),
            Ok(n) => {
                if !lines.receive(board, console, &buf[..n]) {
                    eprintln!("[dut] console line exceeded 16 KiB");
                    break;
                }
                continue;
            }
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => continue,
            Err(e) => e,
        };
        if !board.console.reenumerates {
            eprintln!("[dut] {name}: {error}; console reader stopped");
            break;
        }
        eprintln!("[dut] {name}: {error}; waiting for the device to re-enumerate");
        drop(port);
        match reopen(name, &open) {
            Ok(p) => port = p,
            Err(e) => {
                eprintln!("[dut] {name} did not come back: {e}");
                break;
            }
        }
    }
    lines.finish(board, console);
    console.close();
}

fn reopen<P, F>(name: &str, open: F) -> serialport::Result<P>
where
    P: SerialPort,
    F: Fn(&str) -> serialport::Result<P>,
{
    let deadline = Instant::now() + REENUMERATE;
    loop {
        sleep(READ_TIMEOUT);
        match open(name) {
            Err(_) if Instant::now() < deadline => {}
            result => return result,
        }
    }
}

#[cfg(feature = "qemu")]
fn read_qemu_console(board: &Board, output: impl std::io::Read, console: &SharedConsole) {
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    loop {
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) if line.len() <= MAX_CONSOLE_LINE => console.push_line(board, &line),
            _ => break,
        }
        line.clear();
    }
    console.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TEST_BOARD;

    fn console_dut() -> Dut {
        let (cmds, _) = mpsc::channel();
        Dut {
            board: &TEST_BOARD,
            console: SharedConsole::default(),
            cmds,
            reader: None,
            _board_lock: None,
            _local_guard: Some(PHYSICAL_BOARD.lock().unwrap_or_else(|p| p.into_inner())),
            #[cfg(feature = "qemu")]
            virtual_board: None,
        }
    }

    #[test]
    fn console_assertions_respect_marks_and_reader_failure() {
        let dut = console_dut();
        dut.console
            .push_line(&TEST_BOARD, b"@fbt hello version=1.0.0+0\r\n");
        let mark = dut.mark();
        dut.console
            .push_line(&TEST_BOARD, b"@fbt hello version=2.0.0+0\r\n");
        let (i, line) = dut.expect_from(mark, "@fbt hello", Duration::ZERO).unwrap();
        assert_eq!(i, 1);
        assert_eq!(line, "@fbt hello version=2.0.0+0");
        assert!(dut.expect_from(mark, "1.0.0", Duration::ZERO).is_err());
        assert!(
            dut.expect_quiet_from(mark, Duration::ZERO, "old version", |l| l.contains("1.0.0"))
                .is_ok()
        );
        assert!(
            dut.expect_quiet_from(mark, Duration::ZERO, "new version", |l| l.contains("2.0.0"))
                .is_err()
        );
        dut.console.close();
        assert!(
            dut.expect_quiet_from(dut.mark(), Duration::ZERO, "output", |_| true)
                .is_err()
        );
    }

    #[test]
    fn serial_chunks_form_complete_lines_and_flush_the_last_fragment() {
        let console = SharedConsole::default();
        let mut lines = LineBuffer::default();
        assert!(lines.receive(&TEST_BOARD, &console, b"first\r"));
        assert_eq!(console.mark(), 0); // No partial line is visible yet.
        assert!(lines.receive(&TEST_BOARD, &console, b"\nsecond\nthird"));
        assert_eq!(console.mark(), 2);
        assert_eq!(
            console.wait_from(0, Duration::ZERO, |line| line == "first"),
            Some((0, "first".into()))
        );
        assert_eq!(
            console.wait_from(0, Duration::ZERO, |line| line == "second"),
            Some((1, "second".into()))
        );
        lines.finish(&TEST_BOARD, &console);
        assert_eq!(
            console.wait_from(2, Duration::ZERO, |line| line == "third"),
            Some((2, "third".into()))
        );
    }

    #[test]
    fn unterminated_console_lines_have_a_size_limit() {
        let console = SharedConsole::default();
        let mut lines = LineBuffer::default();
        assert!(lines.receive(&TEST_BOARD, &console, &vec![b'x'; MAX_CONSOLE_LINE]));
        assert!(!lines.receive(&TEST_BOARD, &console, b"y"));
        assert_eq!(console.mark(), 0);
    }

    #[cfg(feature = "qemu")]
    #[test]
    fn qemu_console_reader_closes_after_eof() {
        let console = SharedConsole::default();
        read_qemu_console(
            &TEST_BOARD,
            std::io::Cursor::new(b"hello\nworld\n"),
            &console,
        );
        assert!(console.is_closed());
        assert_eq!(
            console.wait_from(0, Duration::ZERO, |line| line == "world"),
            Some((1, "world".into()))
        );
    }

    #[cfg(feature = "esp32c3-supermini")]
    #[test]
    fn esp_reset_dispatches_to_console_reader_without_a_worker() {
        let (cmds, rx) = mpsc::channel();
        let dut = Dut {
            board: &crate::esp32c3::BOARD,
            console: SharedConsole::default(),
            cmds,
            reader: None,
            _board_lock: None,
            _local_guard: None,
            #[cfg(feature = "qemu")]
            virtual_board: None,
        };
        let reader = std::thread::spawn(move || match rx.recv().unwrap() {
            Cmd::Reset(ack) => ack.send(Ok(())).unwrap(),
            Cmd::Stop => panic!("unexpected stop before reset"),
        });
        dut.reset().unwrap();
        reader.join().unwrap();
    }

    #[test]
    fn environment_names_follow_the_board_prefix() {
        assert_eq!(TEST_BOARD.env("PORT"), "FERRIBOOT_HOST_PORT");
        assert_eq!(TEST_BOARD.env("RESET_GATE"), "FERRIBOOT_HOST_RESET_GATE");
        // Nothing sets FERRIBOOT_HOST_PROBE_TIMEOUT_SECS: the defaults apply.
        assert_eq!(
            probe_timeout(&TEST_BOARD, true).unwrap(),
            Duration::from_secs(600)
        );
        assert_eq!(
            probe_timeout(&TEST_BOARD, false).unwrap(),
            Duration::from_secs(30)
        );
    }
}
