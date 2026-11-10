//! ESP32-C3 ROM-only flashing via `espflash` (no RAM stub).
//!
//! Keep Ferriboot's flash layout checks and USB port selection here; delegate
//! serial reset, SLIP, ROM commands and data transfers to `espflash`.
//! A full-slot erase uses the ROM `FLASH_BEGIN` command rather than the
//! `espflash::Flasher::erase_region` API (which requires a RAM stub).
use anyhow::{Context, Result, anyhow, ensure};
use espflash::{
    command::{Command, CommandType},
    connection::{Connection, ResetAfterOperation, ResetBeforeOperation},
    flasher::{FlashSize, Flasher, SpiAttachParams, SpiSetParams},
    target::Chip,
};
use ferriboot_build::tools::progress::Progress;
use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    thread::sleep,
    time::{Duration, Instant},
};

const FLASH_SIZE: u32 = ferriboot_layout_esp32c3_supermini::FLASH_SIZE;
const SECTOR_SIZE: u32 = 4096;
const ROM_BLOCK_SIZE: u32 = 0x400;
const BAUD: u32 = 115_200;

/// Validate before opening a port: erase operations must cover whole sectors.
fn check_range(offset: u32, size: u32) -> Result<()> {
    ensure!(
        size > 0
            && offset.is_multiple_of(SECTOR_SIZE)
            && size.is_multiple_of(SECTOR_SIZE)
            && offset
                .checked_add(size)
                .is_some_and(|end| end <= FLASH_SIZE),
        "invalid ESP32 flash region {offset:#x} + {size:#x} (4 MiB flash, 4 KiB sectors)"
    );
    Ok(())
}

/// The ROM erases every sector that a write touches, even for short images.
fn check_write(offset: u32, bytes: &[u8]) -> Result<()> {
    ensure!(
        offset.is_multiple_of(SECTOR_SIZE),
        "unaligned ESP flash offset {offset:#x}"
    );
    ensure!(!bytes.is_empty(), "empty ESP flash image");
    let rounded = u32::try_from(bytes.len())
        .ok()
        .and_then(|len| len.checked_next_multiple_of(SECTOR_SIZE));
    ensure!(
        rounded.is_some_and(|len| offset.checked_add(len).is_some_and(|end| end <= FLASH_SIZE)),
        "ESP flash image at {offset:#x} exceeds {FLASH_SIZE:#x}"
    );
    Ok(())
}

fn same_path(a: &str, b: &str) -> bool {
    a == b
        || fs::canonicalize(a)
            .ok()
            .zip(fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
}

/// Only known USB serial ports are accepted. `ESPPORT` may be a stable symlink.
fn select_port(
    explicit: Option<&str>,
    available: &[SerialPortInfo],
) -> Result<(String, UsbPortInfo)> {
    if let Some(port) = explicit {
        let usb = available.iter().find_map(|info| {
            if !same_path(port, &info.port_name) {
                return None;
            }
            match &info.port_type {
                SerialPortType::UsbPort(usb) => Some(usb.clone()),
                _ => None,
            }
        });
        let usb = usb.with_context(|| format!(
            "{port} is not a discoverable USB serial port; set ESPPORT to the ESP32 USB-Serial/JTAG port"
        ))?;
        return Ok((port.to_owned(), usb));
    }
    // Do not guess which device to flash on a multi-board host.
    let mut candidates = available.iter().filter_map(|info| match &info.port_type {
        SerialPortType::UsbPort(usb) if usb.vid == 0x303a && usb.pid == 0x1001 => {
            Some((info.port_name.clone(), usb.clone()))
        }
        _ => None,
    });
    let one = candidates
        .next()
        .context("no ESP32 USB-Serial/JTAG port detected; set ESPPORT=/dev/ttyACM0")?;
    ensure!(
        candidates.next().is_none(),
        "multiple ESP32 devices found; set ESPPORT"
    );
    Ok(one)
}

/// `espflash` owns resetting into download mode, ROM sync, and SPI setup.
/// Retry opening the port because native USB can re-enumerate during reset.
fn connect_rom() -> Result<(Flasher, String)> {
    let ports = serialport::available_ports().context("enumerating serial ports")?;
    let requested = std::env::var("ESPPORT").ok().filter(|p| !p.is_empty());
    let (path, usb) = select_port(requested.as_deref(), &ports)?;
    eprintln!("Connecting ESP32 ROM downloader on {path} (no stub)");
    let mut last_error = None;
    for attempt in 1..=5 {
        let result = (|| -> Result<Flasher> {
            let serial = serialport::new(&path, BAUD)
                .timeout(Duration::from_secs(3))
                .open_native()
                .with_context(|| format!("opening {path}"))?;
            let connection = Connection::new(
                serial,
                usb.clone(),
                ResetAfterOperation::HardReset,
                if usb.vid == 0x303a && usb.pid == 0x1001 {
                    ResetBeforeOperation::UsbReset
                } else {
                    ResetBeforeOperation::DefaultReset
                },
                BAUD,
            );
            let mut flasher = Flasher::connect(
                connection,
                false, // Never load a RAM stub (including Secure Download Mode).
                false, // No flash reads or verification in Secure Download Mode.
                false, // Always program even when bytes happen to match.
                Some(Chip::Esp32c3),
                None,
            )
            .context("ROM SYNC / flash connection (is download mode enabled?)")?;
            flasher.set_flash_size(FlashSize::_4Mb);
            // Reject a pre-existing RAM stub, even if a device reset silently
            // failed. The ROM returns a nonzero SYNC value; a stub returns 0.
            let signature: u32 = flasher
                .connection()
                .with_timeout(Duration::from_secs(3), |c| c.command(Command::Sync))
                .context("checking ROM-only SYNC")?
                .try_into()
                .context("invalid ROM SYNC response")?;
            ensure!(
                signature != 0,
                "SYNC reached a RAM flasher stub, not the ESP32 ROM"
            );
            // In Secure Download Mode flash autodetection (and its implicit
            // SPI_SET_PARAMS) is skipped by espflash, so configure it ourselves.
            attach_flash(&mut flasher)?;
            Ok(flasher)
        })();
        match result {
            Ok(flasher) => return Ok((flasher, path)),
            Err(err) => {
                eprintln!("  ROM connection attempt {attempt}/5: {err:#}");
                last_error = Some(err);
                sleep(Duration::from_millis(300));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("ROM connection failed")))
}

/// Set the board's flash geometry even when Secure Download Mode prevents
/// `espflash` from reading its JEDEC ID during connection setup.
fn attach_flash(flasher: &mut Flasher) -> Result<()> {
    let connection = flasher.connection();
    connection
        .with_timeout(CommandType::SpiAttach.timeout(), |c| {
            c.command(Command::SpiAttach {
                spi_params: SpiAttachParams::default(),
            })
        })
        .context("ROM SPI_ATTACH")?;
    connection
        .with_timeout(CommandType::SpiSetParams.timeout(), |c| {
            c.command(Command::SpiSetParams {
                spi_params: SpiSetParams::default(FLASH_SIZE),
            })
        })
        .context("ROM SPI_SET_PARAMS (4 MiB)")?;
    Ok(())
}

fn flash_begin(address: u32, erase_size: u32, blocks: u32) -> Command<'static> {
    Command::FlashBegin {
        size: erase_size,
        blocks,
        block_size: ROM_BLOCK_SIZE,
        offset: address,
        supports_encryption: true, // C3 ROM takes a fifth, zero encryption word.
    }
}

/// The ROM erases the entire sector-aligned size before acknowledging
/// FLASH_BEGIN; no FLASH_DATA is sent for an erase-only operation.
fn erase_begin(address: u32, size: u32) -> Command<'static> {
    flash_begin(address, size, size.div_ceil(ROM_BLOCK_SIZE))
}

/// An unencrypted 1 KiB ROM data packet. `espflash` encodes SLIP, checksum,
/// sequence and 0xff padding exactly as the original ROM writer did.
fn flash_data(block: &[u8], sequence: u32) -> Command<'_> {
    Command::FlashData {
        data: block,
        pad_to: ROM_BLOCK_SIZE as usize,
        pad_byte: 0xff,
        sequence,
    }
}

fn rom_erase(flasher: &mut Flasher, address: u32, size: u32) -> Result<()> {
    check_range(address, size)?;
    // FLASH_BEGIN completes the full ROM hardware erase before returning.
    // The ROM has no intermediate progress events, so show elapsed time only.
    let mut progress = Progress::new(
        format!("Erasing ESP32 flash {address:#x}..{:#x}", address + size),
        None,
    );
    flasher
        .connection()
        .with_timeout(Duration::from_secs(120), |c| {
            c.command(erase_begin(address, size))
        })
        .with_context(|| format!("ROM FLASH_BEGIN erase at {address:#x}, size {size:#x}"))?;
    progress.finish();
    Ok(())
}

/// Write raw ROM packets, without sending FLASH_END. `espflash`'s
/// high-level `write_bins_to_flash` finishes with FLASH_END, which fails on
/// some secured ESP32-C3 ROM downloaders even after all data blocks were
/// acknowledged. The original Ferriboot writer deliberately omitted it.
///
/// Every region is begun and written on the *same* ROM connection, in caller
/// order. In particular, the signed second-stage bootloader is written last.
fn write_segments(flasher: &mut Flasher, regions: &[(u32, &[u8])]) -> Result<()> {
    for (address, bytes) in regions.iter().copied() {
        check_write(address, bytes)?;
        let size = u32::try_from(bytes.len()).context("ROM image too large")?;
        let erase_size = size.next_multiple_of(SECTOR_SIZE);
        let blocks = size.div_ceil(ROM_BLOCK_SIZE);
        // FLASH_BEGIN erases the sectors covered by this image. It does not
        // report intermediate completion, so avoid claiming erase percentage.
        let mut erase = Progress::new(
            format!("Erasing ESP32 flash for image at {address:#x}"),
            None,
        );
        flasher
            .connection()
            .with_timeout(Duration::from_secs(120), |c| {
                c.command(flash_begin(address, erase_size, blocks))
            })
            .with_context(|| format!("ROM FLASH_BEGIN at {address:#x}, erase {erase_size:#x}"))?;
        erase.finish();

        // Count only image bytes acknowledged by FLASH_DATA, not padded bytes
        // or commands that have merely been sent. Failed writes leave a failed
        // (and potentially partial) bar via Progress::drop.
        let mut program = Progress::new(
            format!("Programming ESP32 flash at {address:#x}"),
            Some(u64::from(size)),
        );
        for (index, block) in bytes.chunks(ROM_BLOCK_SIZE as usize).enumerate() {
            flasher
                .connection()
                .with_timeout(Duration::from_secs(10), |c| {
                    c.command(flash_data(block, index as u32))
                })
                .with_context(|| format!("ROM FLASH_DATA at {address:#x}, block {index}"))?;
            program.inc(block.len() as u64);
        }
        program.finish();
    }
    Ok(())
}

/// No FLASH_END is sent: reset the ESP32 only after all ROM commands have
/// succeeded, then release the USB port before opening the optional monitor.
fn finish(flasher: Flasher, port: &str, monitor: bool) -> Result<()> {
    let mut connection = flasher.into_connection();
    connection
        .reset()
        .context("resetting ESP32 after ROM flashing")?;
    drop(connection); // Release native USB before opening a console.
    if !monitor {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut serial = loop {
        match serialport::new(port, BAUD)
            .timeout(Duration::from_millis(500))
            .open()
        {
            Ok(port) => break port,
            Err(e) if Instant::now() < deadline => {
                eprintln!("Waiting for USB console {port}: {e}");
                sleep(Duration::from_millis(400));
            }
            Err(e) => return Err(e).with_context(|| format!("reopening USB console {port}")),
        }
    };
    serial.set_timeout(Duration::from_millis(500))?;
    let mut output = std::io::stdout().lock();
    eprintln!("ESP32 serial monitor (Ctrl+C to exit)");
    let mut buf = [0u8; 1024];
    loop {
        match serial.read(&mut buf) {
            Ok(0) => continue,
            Ok(n) => {
                output.write_all(&buf[..n])?;
                output.flush()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e).context("USB console disconnected; reopen ESPPORT to monitor"),
        }
    }
}

/// Flash one or more regions in the supplied order, in one ROM session.
pub fn write_flash(regions: &[(u32, &Path)], monitor: bool) -> Result<()> {
    ensure!(!regions.is_empty(), "no flash regions");
    ensure!(
        !monitor || regions.len() == 1,
        "monitor requires a single flash region"
    );
    // Read and validate everything before touching the target flash.
    let images: Vec<(u32, Vec<u8>)> = regions
        .iter()
        .map(|&(address, path)| -> Result<_> {
            let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            check_write(address, &bytes)?;
            Ok((address, bytes))
        })
        .collect::<Result<_>>()?;
    let segments: Vec<_> = images
        .iter()
        .map(|(address, bytes)| (*address, bytes.as_slice()))
        .collect();
    let (mut flasher, port) = connect_rom()?;
    write_segments(&mut flasher, &segments)?;
    finish(flasher, &port, monitor)
}

/// Erase every sector in a region using ROM FLASH_BEGIN alone.
pub fn erase_region(address: u32, size: u32) -> Result<()> {
    check_range(address, size)?;
    let (mut flasher, port) = connect_rom()?;
    rom_erase(&mut flasher, address, size)?;
    finish(flasher, &port, false)
}

/// Erase the entire MCUboot slot, then write its signed image without an
/// intervening reset; the rest of the slot must remain erased.
pub fn replace_region(address: u32, slot_size: u32, signed: &[u8], monitor: bool) -> Result<()> {
    check_range(address, slot_size)?;
    check_write(address, signed)?;
    ensure!(
        signed.len() <= slot_size as usize,
        "signed image exceeds application slot"
    );
    let (mut flasher, port) = connect_rom()?;
    rom_erase(&mut flasher, address, slot_size)?;
    write_segments(&mut flasher, &[(address, signed)])?;
    finish(flasher, &port, monitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erase_rejects_partial_or_overflowing_regions_before_serial_io() {
        for (address, size) in [
            (0x1001, 0x2000),
            (0x1000, 0x1fff),
            (0x1000, 0),
            (0xffff_f000, 0x2000),
            (FLASH_SIZE, 0x1000),
            (0x22f000, 0x200000),
        ] {
            assert!(check_range(address, size).is_err());
        }
        assert!(check_range(0x22f000, 0x1b9000).is_ok());
    }

    #[test]
    fn write_rejects_sector_overshoot_and_empty_image() {
        assert!(check_write(FLASH_SIZE - SECTOR_SIZE, &[0x5a]).is_ok());
        assert!(
            check_write(
                FLASH_SIZE - SECTOR_SIZE,
                &vec![0x5a; SECTOR_SIZE as usize + 1]
            )
            .is_err()
        );
        assert!(check_write(0x1001, &[0x5a]).is_err());
        assert!(check_write(0x1000, &[]).is_err());
    }

    #[test]
    fn ambiguous_usb_port_selection_never_guesses() {
        let usb = UsbPortInfo {
            vid: 0x303a,
            pid: 0x1001,
            serial_number: None,
            manufacturer: None,
            product: None,
            interface: None,
        };
        let one = SerialPortInfo {
            port_name: "/dev/ttyACM0".to_owned(),
            port_type: SerialPortType::UsbPort(usb.clone()),
        };
        let another = SerialPortInfo {
            port_name: "/dev/ttyACM1".to_owned(),
            port_type: SerialPortType::UsbPort(usb),
        };
        assert_eq!(
            select_port(None, std::slice::from_ref(&one)).unwrap().0,
            "/dev/ttyACM0"
        );
        assert!(select_port(None, &[one.clone(), another.clone()]).is_err());
        assert_eq!(
            select_port(Some("/dev/ttyACM1"), &[one, another])
                .unwrap()
                .0,
            "/dev/ttyACM1"
        );
    }

    #[test]
    fn write_begin_erases_rounded_sectors_but_counts_only_data_packets() {
        let mut raw = Vec::new();
        flash_begin(0x6f000, 0x1000, 2).write(&mut raw).unwrap();
        assert_eq!(
            &raw[..8],
            &[0, CommandType::FlashBegin as u8, 20, 0, 0, 0, 0, 0]
        );
        let words: Vec<_> = raw[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        assert_eq!(words.as_slice(), &[0x1000, 2, ROM_BLOCK_SIZE, 0x6f000, 0]);
    }

    #[test]
    fn rom_flash_data_pads_to_one_kib_with_ff_and_matching_checksum() {
        let mut raw = Vec::new();
        flash_data(&[0xc0, 0xdb, 0x12], 7).write(&mut raw).unwrap();
        assert_eq!(raw[0], 0);
        assert_eq!(raw[1], CommandType::FlashData as u8);
        assert_eq!(u16::from_le_bytes([raw[2], raw[3]]), 16 + 1024);
        assert_eq!(
            u32::from_le_bytes(raw[4..8].try_into().unwrap()),
            (0xef ^ 0xc0 ^ 0xdb ^ 0x12 ^ 0xff) as u32
        );
        assert_eq!(
            u32::from_le_bytes(raw[8..12].try_into().unwrap()),
            ROM_BLOCK_SIZE
        );
        assert_eq!(u32::from_le_bytes(raw[12..16].try_into().unwrap()), 7);
        assert_eq!(&raw[24..27], &[0xc0, 0xdb, 0x12]);
        assert!(raw[27..].iter().all(|byte| *byte == 0xff));
    }

    #[test]
    fn erase_command_is_rom_only_and_preserves_unencrypted_fifth_word() {
        let command = erase_begin(0x22f000, 0x1b9000);
        let mut raw = Vec::new();
        command.write(&mut raw).unwrap();
        assert_eq!(
            &raw[..8],
            &[0, CommandType::FlashBegin as u8, 20, 0, 0, 0, 0, 0]
        );
        let words: Vec<_> = raw[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        assert_eq!(
            words.as_slice(),
            &[
                0x1b9000,
                0x1b9000 / ROM_BLOCK_SIZE,
                ROM_BLOCK_SIZE,
                0x22f000,
                0,
            ]
        );
    }
}
