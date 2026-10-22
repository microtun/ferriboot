#![no_std]
// `unsafe` is confined to `mmio`, `regs` and the boot-handoff reader below,
// so a new unsafe block anywhere else fails the build and gets reviewed.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
//! Test-only Cortex-M3 platform for QEMU mps2-an385.
//! Flash uses a W25Q32 SPI NOR chip on the emulated PL022 controller.
//!
//! The machine's facts are in [`chip`]; the board passes its flash map
//! through [`Layout::new`].
use core::fmt::Write;
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use ferriboot_core::{ImageError, ImageHeader, Region, shared_data::BootInfo};

pub mod chip;
pub use chip::*;
mod efuse;
mod layout;
#[allow(unsafe_code)] // The register handle and its constructors.
mod mmio;
#[allow(unsafe_code)] // Every register address, declared once.
mod regs;
pub use layout::Layout;
pub mod policy;
pub mod security_counter;
mod spi_flash;
pub mod trng;
mod uart;

#[derive(Clone, Copy, Debug)]
pub enum FlashError {
    Bounds,
    Alignment,
    Io,
    Protocol,
}

impl NorFlashError for FlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Io | Self::Protocol => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Partition {
    region: Region,
}

impl Partition {
    pub fn new(region: Region) -> Self {
        assert!(
            region
                .offset
                .checked_add(region.size)
                .is_some_and(|end| end <= FLASH_SIZE)
        );
        Self { region }
    }

    fn checked_offset(&self, offset: u32, len: usize, align: u32) -> Result<u32, FlashError> {
        if !offset.is_multiple_of(align) || !len.is_multiple_of(align as usize) {
            return Err(FlashError::Alignment);
        }
        if len > self.region.size as usize || offset as usize > self.region.size as usize - len {
            return Err(FlashError::Bounds);
        }
        Ok(self.region.offset + offset)
    }
}

impl ErrorType for Partition {
    type Error = FlashError;
}

impl ReadNorFlash for Partition {
    const READ_SIZE: usize = 1;

    fn capacity(&self) -> usize {
        self.region.size as usize
    }

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), FlashError> {
        spi_flash::read(self.checked_offset(offset, bytes.len(), 1)?, bytes)
    }
}

impl NorFlash for Partition {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), FlashError> {
        spi_flash::program(self.checked_offset(offset, bytes.len(), 4)?, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), FlashError> {
        if from > to {
            return Err(FlashError::Bounds);
        }
        let len = (to - from) as usize;
        spi_flash::erase(self.checked_offset(from, len, ERASE_SIZE)?, len)
    }
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        uart::print(text);
        Ok(())
    }
}

struct Logger;

static LOGGER: Logger = Logger;

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let _ = writeln!(Console, "{}", record.args());
    }

    fn flush(&self) {}
}

pub fn init() {
    cortex_m::interrupt::disable();
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    uart::init();
    spi_flash::init().expect("QEMU PL022/W25Q32 SPI NOR missing or broken");
}

/// The boot acceptance contract matches the common execution window.
pub fn vectors(
    layout: &Layout,
    header: &ImageHeader,
    words: &[u8; 8],
) -> Result<(u32, u32), ImageError> {
    use ferriboot_core::image::flags;
    let sp = u32::from_le_bytes(words[..4].try_into().unwrap());
    let reset = u32::from_le_bytes(words[4..].try_into().unwrap());
    let end = APP_VECTOR
        .checked_add(header.img_size)
        .ok_or(ImageError::PlatformRejected)?;
    if u32::from(header.hdr_size) != HEADER_SIZE
        || header.img_size < 8
        || header.img_size > layout.max_body_size()
        || header.flags & !flags::ROM_FIXED != 0
        || (header.flags & flags::ROM_FIXED != 0 && header.load_addr != APP_XIP_BASE)
        || sp <= RAM_BASE
        || sp > BOOT_INFO_ADDRESS
        || !sp.is_multiple_of(8)
        || reset & 1 == 0
        || (reset & !1) < APP_VECTOR + 8
        || (reset & !1) >= end
    {
        return Err(ImageError::PlatformRejected);
    }
    Ok((sp, reset))
}

#[allow(unsafe_code)] // Linker-reserved handoff RAM, written by the bootloader.
pub fn boot_info() -> BootInfo {
    // SAFETY: bootloader publishes this reserved range immediately before handoff.
    let bytes =
        unsafe { core::slice::from_raw_parts(BOOT_INFO_ADDRESS as *const u8, BOOT_INFO_SIZE) };
    BootInfo::from_handoff(bytes).expect("invalid boot handoff")
}

pub fn halt() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}

pub fn reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}

/// Start QEMU's CMSDK APB watchdog. Two expirations produce a reset.
pub fn watchdog(seconds: u8) {
    use regs::{retention, watchdog};
    // AN385 WDOGCLK is 25 MHz. Two equal periods make the requested deadline.
    let ticks = u32::from(seconds.max(1)) * 25_000_000 / 2;
    retention::MARKER.write(WATCHDOG_MARKER);
    watchdog::LOCK.write(0x1acce551);
    watchdog::LOAD.write(ticks);
    watchdog::CONTROL.write(3);
}

/// Restart the full deadline of a watchdog armed by [`watchdog`]. A `fn()`,
/// so it can be passed to [`ferriboot_core::flash::ProgressFlash`] directly.
pub fn feed_watchdog() {
    regs::watchdog::INTCLR.write(1);
}

/// Disarm the watchdog and clear the reset-reason marker, so a later reset is
/// not attributed to it.
pub fn stop_watchdog() {
    use regs::{retention, watchdog};
    watchdog::CONTROL.write(0);
    retention::MARKER.write(0);
}

/// The value of [`regs::retention::MARKER`] while our watchdog is armed.
const WATCHDOG_MARKER: u32 = 0x57444731;

pub fn take_reset_reason() {
    use regs::retention;
    let reason = retention::MARKER.read();
    retention::REASON.write(reason);
    retention::MARKER.write(0);
}

pub fn watchdog_reset() -> bool {
    // Populated by the bootloader (`take_reset_reason`) on this boot.
    regs::retention::REASON.read() == WATCHDOG_MARKER
}
