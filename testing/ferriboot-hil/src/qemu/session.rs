//! Own patched QEMU processes and a virtual board's separate NOR/OTP files.
use crate::{Result, flash::FLASH_SIZE};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// One virtual board, independent of any QEMU process. Reusing this object
/// preserves its security counter across process exits and full flash rewrites.
/// Dropping the last handle deletes the private files, isolating test cases.
#[derive(Clone)]
pub struct VirtualBoard {
    state: Arc<BoardState>,
}

struct BoardState {
    directory: tempfile::TempDir,
    active: AtomicBool,
}

impl VirtualBoard {
    /// Provision a fresh board at this epoch exactly once, outside the guest.
    pub fn new(initial: u32) -> Result<Self> {
        if initial > super::layout::SECURITY_COUNTER_CAPACITY {
            return Err("initial QEMU security counter exceeds capacity".into());
        }
        let directory = tempfile::tempdir()?;
        let mut bytes = [0u8; super::layout::SECURITY_COUNTER_BYTES];
        for bit in 0..initial {
            bytes[(bit / 8) as usize] |= 1 << (bit % 8);
        }
        let mut file = std::fs::File::create_new(directory.path().join("efuses.bin"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(Self {
            state: Arc::new(BoardState {
                directory,
                active: AtomicBool::new(false),
            }),
        })
    }

    /// Read the current persisted counter from the virtual chip's eFuse file.
    /// Reject short or malformed fuse images instead of silently using zero.
    pub fn security_counter(&self) -> Result<u32> {
        let bytes = std::fs::read(self.state.directory.path().join("efuses.bin"))?;
        if bytes.len() != super::layout::SECURITY_COUNTER_BYTES {
            return Err("invalid QEMU eFuse image size".into());
        }
        let rows: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        ferriboot_core::security_counter::thermometer_rows_value(&rows, 32)
            .map_err(|error| format!("invalid QEMU eFuse counter: {error:?}").into())
    }

    /// Reflash and boot this board without reprovisioning its counter. Drop
    /// the previous Dut before booting again; concurrent guests are rejected.
    pub fn boot(&self, flash: &crate::flash::FlashImage) -> Result<crate::dut::Dut> {
        if !std::ptr::eq(flash.board(), &super::BOARD) {
            return Err("a QEMU virtual board requires a QEMU flash fixture".into());
        }
        crate::dut::Dut::boot_qemu(flash, self.clone())
    }

    fn acquire(&self) -> Result<Lease> {
        self.state
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "QEMU virtual board already has a running guest")?;
        Ok(Lease {
            board: self.clone(),
        })
    }
}

struct Lease {
    board: VirtualBoard,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.board.state.active.store(false, Ordering::Release);
    }
}

fn write_fixture(directory: &std::path::Path, flash: &[u8]) -> Result<()> {
    if flash.len() != FLASH_SIZE {
        return Err("expected complete 4 MiB flash image".into());
    }
    // Never touch efuses.bin here, even if it is missing or corrupt.
    std::fs::write(directory.join("flash.bin"), flash)?;
    std::fs::write(
        directory.join("bootloader.bin"),
        &flash[..super::layout::BOOTLOADER_SIZE as usize],
    )?;
    Ok(())
}

pub(crate) struct Session {
    child: Child,
    qmp: Option<BufReader<TcpStream>>,
    _lease: Lease,
}
impl Session {
    pub fn spawn(
        flash: &[u8],
        board: VirtualBoard,
    ) -> Result<(Self, std::io::PipeReader, TcpListener)> {
        let lease = board.acquire()?;
        let directory = lease.board.state.directory.path();
        write_fixture(directory, flash)?;
        // QEMU connects to the already-bound socket; no port reservation race.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let binary =
            std::env::var_os("FERRIBOOT_QEMU_BIN").unwrap_or_else(|| "qemu-system-arm".into());
        let (reader, writer) = std::io::pipe()?;
        let child = Command::new(binary)
            .current_dir(directory)
            .args([
                "-M",
                "mps2-an385",
                "-cpu",
                "cortex-m3",
                "-accel",
                "tcg",
                "-display",
                "none",
                "-monitor",
                "none",
                "-serial",
                "stdio",
                "-watchdog-action",
                "reset",
                // QEMU MPS2 patch attaches IF_MTD[0] to its W25Q32 on SSP0.
                "-drive",
                "if=mtd,format=raw,file=flash.bin",
                // Extra AN385 patch attaches IF_PFLASH[1] to Xilinx eFuse.
                "-drive",
                "if=pflash,index=1,format=raw,file=efuses.bin",
                "-kernel",
                "bootloader.bin",
                "-S",
                "-qmp",
            ])
            .arg(format!("tcp:{address}"))
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .spawn()
            .map_err(|e| {
                format!("cannot start patched QEMU: {e}; see boards/qemu/qemu/README.md and set FERRIBOOT_QEMU_BIN")
            })?;
        Ok((
            Self {
                child,
                qmp: None,
                _lease: lease,
            },
            reader,
            listener,
        ))
    }
    pub fn start(&mut self, listener: TcpListener) -> Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        let stream = loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(format!("QEMU exited before its QMP handshake: {status}").into());
            }
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("QEMU QMP connection failed: {e}").into()),
            }
        };
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        self.qmp = Some(BufReader::new(stream));
        if self.receive()?.get("QMP").is_none() {
            return Err("invalid QMP greeting".into());
        }
        self.execute("qmp_capabilities")?;
        self.execute("cont")
    }
    fn receive(&mut self) -> Result<Value> {
        let reader = self.qmp.as_mut().ok_or("QMP is not connected")?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err("QEMU closed QMP".into());
        }
        Ok(serde_json::from_str(&line).map_err(crate::Error::external)?)
    }
    fn execute(&mut self, command: &str) -> Result<()> {
        let reader = self.qmp.as_mut().ok_or("QMP is not connected")?;
        writeln!(
            reader.get_mut(),
            "{}",
            json!({"execute": command, "id": "ferriboot"})
        )?;
        // QMP events can precede a command reply. Bound the whole exchange,
        // including an event stream, rather than just each individual read.
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("QMP command timed out")?;
            self.qmp
                .as_ref()
                .unwrap()
                .get_ref()
                .set_read_timeout(Some(remaining))?;
            let reply = self.receive()?;
            if reply.get("id") != Some(&json!("ferriboot")) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                return Err(format!("QMP {command}: {error}").into());
            }
            if reply.get("return").is_none() {
                return Err("invalid QMP reply".into());
            }
            return Ok(());
        }
    }
    pub fn reset(&mut self) -> Result<()> {
        self.execute("system_reset")
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_boards_are_isolated_and_cannot_run_two_guests() {
        let board = VirtualBoard::new(9).unwrap();
        let other = VirtualBoard::new(0).unwrap();
        let path = board.state.directory.path().join("efuses.bin");
        let initial = std::fs::read(&path).unwrap();
        assert_eq!(&initial[..3], &[0xff, 1, 0]);
        assert_eq!(
            std::fs::read(other.state.directory.path().join("efuses.bin")).unwrap(),
            vec![0; super::super::layout::SECURITY_COUNTER_BYTES]
        );
        let lease = board.acquire().unwrap();
        assert!(board.clone().acquire().is_err());
        drop(lease);
        assert!(board.acquire().is_ok());
        assert!(VirtualBoard::new(super::super::layout::SECURITY_COUNTER_CAPACITY + 1).is_err());
    }

    #[test]
    fn security_counter_reads_the_actual_persisted_fuses() {
        let board = VirtualBoard::new(3).unwrap();
        assert_eq!(board.security_counter().unwrap(), 3);
        let path = board.state.directory.path().join("efuses.bin");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = 0xff;
        bytes[1] = 0x01;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(board.security_counter().unwrap(), 9);
        bytes[0] = 0x05; // Malformed thermometer: hole in the prefix.
        std::fs::write(&path, &bytes).unwrap();
        assert!(board.security_counter().is_err());
        std::fs::write(&path, &[0u8; 2]).unwrap();
        assert!(board.security_counter().is_err());
        let full = VirtualBoard::new(super::super::layout::SECURITY_COUNTER_CAPACITY).unwrap();
        assert_eq!(
            full.security_counter().unwrap(),
            super::super::layout::SECURITY_COUNTER_CAPACITY
        );
    }

    #[test]
    fn flash_restoration_never_resets_or_recreates_the_counter() {
        let board = VirtualBoard::new(0).unwrap();
        let directory = board.state.directory.path();
        let path = directory.join("efuses.bin");
        let mut burned = vec![0; super::super::layout::SECURITY_COUNTER_BYTES];
        burned[0] = 0x1f;
        std::fs::write(&path, &burned).unwrap();
        write_fixture(directory, &vec![0xff; FLASH_SIZE]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), burned);
        std::fs::remove_file(&path).unwrap();
        write_fixture(directory, &vec![0xff; FLASH_SIZE]).unwrap();
        assert!(!path.exists());
    }
}
