//! ESP32-C3 board support for the hardware scenarios.
//!
//! The suite builds the real signed IDF second stage, the signed Ferriboot
//! factory app and the example firmware, converts them with espflash, signs the boot stages with the
//! board's Secure Boot V2 tooling, signs firmware with upstream `imgtool`,
//! and composes complete 4 MiB flash images with ferriboot-core's own
//! updater/trailer code, including a test-hook record in the `storage`
//! partition that selects how each image version misbehaves. Each scenario
//! flashes a full 4 MiB image using the Rust espflash worker over native USB
//! serial, then opens USB CDC for boot capture and reset. Later resets reuse
//! the same CDC connection.
//!
//! This module describes the board ([`BOARD`]) and holds the ESP-specific
//! pieces; everything else is the crate's shared harness:
//! * [`layout`]: hardware/RAM ABI constants; partition geometry is read from
//!   the exact binary table that the fixture programs.
//! * [`artifacts`]: cached builds converted and SBv2-padded with espflash.
//! * [`esp_image`]: espflash conversion plus in-place mutations for malformed images.
//! * [`flash`]: partition table, flash image composition and Rust espflash flashing.
//! * [`sign`]: imgtool with the ESP slot geometry.

use crate::{Board, Console, Signing};
use serialport::SerialPort;
use std::sync::LazyLock;

pub mod artifacts;
pub mod esp_image;
pub mod flash;
mod partitions;
pub mod security_counter;
pub mod sign;

/// Hardware/RAM ABI shared with the target; no partition locations.
pub use ferriboot_layout_esp32c3_supermini as layout;

pub use crate::{Result, dut, hooks};

pub static BOARD: LazyLock<Board> = LazyLock::new(|| Board {
    name: "esp32c3-supermini",
    chip: "ESP32-C3",
    env_prefix: "FERRIBOOT_ESP32C3",
    target: "riscv32imc-unknown-none-elf",
    console: Console {
        port_hint: "the board's USB-Serial-JTAG port (preferably /dev/serial/by-id/...)",
        open: open_console,
        // The chip may re-enumerate its native USB device during resets.
        reenumerates: true,
    },
    probe: None,
    signing: Signing {
        header_size: sign::HEADER_SIZE,
        slot_size: partitions::read()
            .expect("ESP32-C3 partition table")
            .slots
            .primary
            .size,
        sector: sign::SECTOR,
        vid: "microtun.dev",
        cid: "esp32c3",
        rom_fixed: None,
    },
});

/// Open the console with both modem lines released. Deassert RTS before DTR
/// so the intermediate state cannot accidentally trigger a core reset.
fn open_console(name: &str) -> serialport::Result<crate::NativeSerialPort> {
    let mut port = crate::dut::open_serial(name)?;
    port.write_request_to_send(false)?;
    port.write_data_terminal_ready(false)?;
    Ok(port)
}

/// Control-line transitions for the ESP32-C3's native USB-Serial/JTAG CDC
/// interface, not the conventional external USB-to-UART modem pin wiring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsbControl {
    Dtr(bool),
    Rts(bool),
    PauseMs(u64),
}

fn flash_boot_sequence(
    mut apply: impl FnMut(UsbControl) -> serialport::Result<()>,
) -> serialport::Result<()> {
    // 0/0 clears the USB CDC download flag; 1/0 resets; 0/0 releases reset.
    // The short gaps let the peripheral and OS CDC driver settle between
    // control-line transitions. Never assert DTR during this sequence.
    for step in [
        UsbControl::Dtr(false),
        UsbControl::Rts(false),
        UsbControl::PauseMs(50),
        UsbControl::Rts(true),
        UsbControl::PauseMs(100),
        UsbControl::Rts(false),
    ] {
        apply(step)?;
    }
    Ok(())
}

/// Boot after the Rust flasher's no-reset operation. The ESP32-C3's
/// native USB-Serial/JTAG controller interprets CDC control lines specially:
/// DTR=false, RTS=false clears the download request; RTS=true with DTR=false
/// resets the core; RTS=false releases it to boot the signed SPI image.
///
/// In HIL this runs after the serial reader starts capturing output. It also
/// handles subsequent resets using the same port (without another process or
/// a target definition). A software reset can leave ROM download mode latched,
/// resulting in `DOWNLOAD(USB/UART0/1)` followed by `waiting for download`.
/// This cannot override a physically asserted BOOT/GPIO9 strap.
pub fn boot_from_download<P: serialport::SerialPort>(port: &mut P) -> serialport::Result<()> {
    flash_boot_sequence(|step| match step {
        UsbControl::Dtr(state) => port.write_data_terminal_ready(state),
        UsbControl::Rts(state) => port.write_request_to_send(state),
        UsbControl::PauseMs(ms) => {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        }
    })
}
