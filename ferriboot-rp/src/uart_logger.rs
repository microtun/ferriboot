//! Core-0 UART console shared by the RP2350 bootloader and example firmware
//! (feature `uart-log`).
//! TX uses the polling driver: no executor, DMA, or interrupts are needed.
use core::{
    cell::RefCell,
    fmt::{self, Write},
};
use cortex_m::interrupt::Mutex;
use embassy_rp::uart::{self, Blocking, UartTx};
use log::{Level, LevelFilter, Log, Metadata, Record};

pub const BAUDRATE: u32 = 115_200;
const RECORD_SIZE: usize = 256;

pub fn config() -> uart::Config {
    let mut config = uart::Config::default();
    config.baudrate = BAUDRATE;
    config
}

struct LogLine {
    bytes: [u8; RECORD_SIZE],
    len: usize,
    truncated: bool,
}

impl LogLine {
    fn format(level: Level, args: fmt::Arguments<'_>) -> Self {
        let mut line = Self {
            bytes: [0; RECORD_SIZE],
            len: 0,
            truncated: false,
        };
        let _ = write!(&mut line, "[{}] {}", level, args);
        if line.truncated {
            line.bytes[line.len..line.len + 3].copy_from_slice(b"...");
            line.len += 3;
        }
        line.bytes[line.len..line.len + 2].copy_from_slice(b"\r\n");
        line.len += 2;
        line
    }
}

impl Write for LogLine {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.truncated {
            return Ok(());
        }
        for ch in text.chars() {
            // Keep each log record on one line, even for control characters.
            let ch = if ch.is_control() { ' ' } else { ch };
            let mut buffer = [0; 4];
            let bytes = ch.encode_utf8(&mut buffer).as_bytes();
            // Reserve "...\r\n" and preserve complete UTF-8 characters.
            if self.len + bytes.len() > RECORD_SIZE - 5 {
                self.truncated = true;
                break;
            }
            self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
        }
        Ok(())
    }
}

struct UartLogger {
    tx: Mutex<RefCell<Option<UartTx<'static, Blocking>>>>,
}

static LOGGER: UartLogger = UartLogger {
    tx: Mutex::new(RefCell::new(None)),
};

#[derive(Debug)]
pub enum WriteError {
    /// UART is not initialized, or a nested writer already owns TX.
    Unavailable,
    Uart(uart::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("UART TX unavailable"),
            Self::Uart(error) => write!(f, "UART TX error: {:?}", error),
        }
    }
}

fn with_tx(
    send: impl FnOnce(&mut UartTx<'static, Blocking>) -> Result<(), uart::Error>,
) -> Result<(), WriteError> {
    // Take ownership for the entire log record, but only mask IRQs while
    // updating the slot. Nested log calls simply drop their record.
    let mut tx = cortex_m::interrupt::free(|cs| LOGGER.tx.borrow(cs).borrow_mut().take())
        .ok_or(WriteError::Unavailable)?;
    let result = send(&mut tx).map_err(WriteError::Uart);
    cortex_m::interrupt::free(|cs| *LOGGER.tx.borrow(cs).borrow_mut() = Some(tx));
    result
}

/// Send a complete console message and drain TX before returning. Flow control
/// is disabled, so this waits only for the UART baud clock, never for a host.
pub fn write(bytes: &[u8]) -> Result<(), WriteError> {
    with_tx(|tx| {
        tx.blocking_write(bytes)?;
        drain_tx(tx)
    })
}

fn drain_tx(tx: &mut UartTx<'static, Blocking>) -> Result<(), uart::Error> {
    tx.blocking_flush()?;
    // Embassy RP 0.10 only waits for the FIFO in blocking_flush. Also wait
    // for the last character's stop bit before reset or UART reinitialization.
    while tx.busy() {
        core::hint::spin_loop();
    }
    Ok(())
}

impl Log for UartLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let line = LogLine::format(record.level(), *record.args());
            let _ = write(&line.bytes[..line.len]);
        }
    }

    fn flush(&self) {
        let _ = with_tx(drain_tx);
    }
}

pub fn init(tx: UartTx<'static, Blocking>) {
    cortex_m::interrupt::free(|cs| *LOGGER.tx.borrow(cs).borrow_mut() = Some(tx));
    log::set_logger(&LOGGER).expect("UART log sink already installed");
    log::set_max_level(LevelFilter::Info);
}
