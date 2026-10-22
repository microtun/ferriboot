//! The ESP32-C3's 4 MiB flash: signed ESP-IDF second stage at 0x0,
//! partition table, signed factory Ferriboot, then the data slots. Everything is derived
//! from `boards/esp32c3/layout/partitions.csv` by `ferriboot-tools-esp layout`, as
//! `ferriboot-tools-esp flash-ferriboot` does. The composer itself is [`crate::flash`].

use std::{path::Path, process::Command, time::Duration};

use espflash::{
    connection::{Connection, ResetAfterOperation, ResetBeforeOperation},
    flasher::{FlashSize, Flasher},
    target::{Chip, DefaultProgressCallback},
};
use ferriboot_core::{Policy, SlotLayout};
use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

pub use crate::flash::{FLASH_SIZE, FlashImage, Request, SECTOR, trusted_keys};

use super::layout;

const _: () = assert!(layout::FLASH_SIZE as usize == FLASH_SIZE);
const _: () = assert!(layout::ERASE_SIZE == SECTOR);

/// The same slot layout the target discovers in the flashed table.
pub fn slot_layout() -> crate::Result<SlotLayout> {
    Ok(super::partitions::read()?.slots)
}

/// Erased flash with both signed boot stages and the table used for signing.
/// The ROM verifies `idf_image`; ESP-IDF verifies `image` in its factory slot.
pub fn compose(chain: &super::artifacts::Bootloader) -> crate::Result<FlashImage> {
    let table = super::partitions::read()?;
    if chain.idf_image.len() > (0xc000_usize).min(table.offset as usize) {
        return Err("signed IDF second stage exceeds 48 KiB/table offset".into());
    }
    if chain.image.len() > table.factory.size as usize {
        return Err("signed Ferriboot exceeds its factory partition".into());
    }
    let flash = FlashImage::erased(
        &super::BOARD,
        table.slots,
        Some(table.storage),
        Policy::default(),
    );
    flash
        .program(0, &chain.idf_image)
        .program(table.offset, &table.bytes)
        .program(table.factory.offset, &chain.image);
    Ok(flash)
}

fn validate_image_size(image: &Path) -> crate::Result<()> {
    let size = std::fs::metadata(image)?.len();
    if size != FLASH_SIZE as u64 {
        return Err(format!("expected complete 4 MiB flash image, got {size} bytes").into());
    }
    Ok(())
}

/// Resolve the USB descriptor even when `port` is a stable `/dev/serial/by-id`
/// symlink rather than the `/dev/ttyACM*` name returned by serialport.
fn usb_port_info(port: &str, available: &[SerialPortInfo]) -> crate::Result<UsbPortInfo> {
    let resolved = std::fs::canonicalize(port).ok();
    available
        .iter()
        .find(|item| {
            item.port_name == port
                || resolved.as_ref().is_some_and(|path| {
                    std::fs::canonicalize(&item.port_name).ok().as_ref() == Some(path)
                })
        })
        .and_then(|item| match &item.port_type {
            SerialPortType::UsbPort(info) => Some(info.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            format!(
                "{port} is not a discoverable USB serial port; set FERRIBOOT_ESP32C3_PORT \
                 to the ESP32-C3 USB-Serial/JTAG device"
            )
            .into()
        })
}

/// Flash the fixture from the Rust worker, using the existing `espflash`
/// library and the ESP32-C3's ROM loader (no stub, no external `esptool`).
/// The port is released on return; the parent then opens CDC for boot capture
/// and performs its existing USB-Serial/JTAG reset.
pub fn write_full_image_in_worker(port: &str, image: &Path) -> crate::Result<()> {
    validate_image_size(image)?;
    let info = usb_port_info(port, &serialport::available_ports()?)?;
    let serial = serialport::new(port, crate::dut::BAUD)
        .timeout(Duration::from_secs(3))
        .open_native()?;
    let connection = Connection::new(
        serial,
        info,
        ResetAfterOperation::NoReset,
        ResetBeforeOperation::UsbReset,
        crate::dut::BAUD,
    );
    // Match esptool's previous --no-stub / --after no-reset behavior. The
    // parent handles the reset only once the console reader is running.
    let mut flasher = Flasher::connect(connection, false, false, false, Some(Chip::Esp32c3), None)
        .map_err(crate::Error::external)?;
    flasher.set_flash_size(FlashSize::_4Mb);
    let data = std::fs::read(image)?;
    eprintln!("[fixture] ESP32-C3: writing complete 4 MiB via espflash ROM loader");
    flasher
        .write_bin_to_flash(0, &data, &mut DefaultProgressCallback)
        .map_err(crate::Error::external)?;
    Ok(())
}

/// The shared `ferriboot-flash` worker isolates USB I/O so a hung operation
/// can be killed before the next HIL test takes the board lock.
fn worker_command(executable: &Path, port: &str, image: &Path) -> Command {
    let mut command = Command::new(executable);
    command.arg("esp32c3").arg(port).arg(image);
    command
}

/// HIL fixture flashing, supervised to ensure USB errors cannot hang the test
/// indefinitely. No external Python executable, pre-read or differential scan.
/// The caller reopens CDC after the worker exits and resets into the image.
pub fn write_full_image(port: &str, image: &Path) -> crate::Result<()> {
    validate_image_size(image)?;
    let var = super::BOARD.env("FLASH_TIMEOUT_SECS");
    let seconds = match std::env::var(&var) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| format!("{var} must be a positive integer"))?,
        Err(std::env::VarError::NotPresent) => 600,
        Err(error) => return Err(error.into()),
    };
    if seconds == 0 {
        return Err(format!("{var} must be positive").into());
    }
    let mut command = worker_command(&crate::dut::flash_worker(&super::BOARD)?, port, image);
    crate::process::run(
        &mut command,
        "espflash fixture write",
        Duration::from_secs(seconds),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn fixture_invokes_bounded_rust_worker_with_full_image_and_port() {
        let image = Path::new("/tmp/flash fixture.bin");
        let command = worker_command(
            Path::new("/tmp/ferriboot-flash"),
            "/dev/serial/by-id/esp-board",
            image,
        );
        assert_eq!(command.get_program(), "/tmp/ferriboot-flash");
        let args: Vec<OsString> = command.get_args().map(OsString::from).collect();
        assert_eq!(
            args,
            [
                "esp32c3",
                "/dev/serial/by-id/esp-board",
                "/tmp/flash fixture.bin"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn rejects_incomplete_fixture_before_flashing() {
        let image = tempfile::NamedTempFile::new().unwrap();
        for flash in [write_full_image, write_full_image_in_worker] {
            let error = flash("unused", image.path()).unwrap_err();
            assert!(
                error.to_string().contains("expected complete 4 MiB"),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_unrecognized_non_usb_ports() {
        let ports = [SerialPortInfo {
            port_name: "not-usb".into(),
            port_type: SerialPortType::PciPort,
        }];
        assert!(usb_port_info("not-usb", &ports).is_err());
        assert!(usb_port_info("missing", &ports).is_err());
    }
}
