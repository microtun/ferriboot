#![no_std]
// `unsafe` is confined to the modules marked `#[allow(unsafe_code)]` below,
// so a new unsafe block anywhere else fails the build and gets reviewed.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
//! ESP chip glue shared by the bootloader and by applications doing OTA.
//!
//! Built for exactly one chip, selected by a Cargo feature (`esp32c3`). The
//! chip's facts are in [`chip`]; nothing in this crate is board-specific.
//!
//! * Slots are found in the ESP-IDF partition table by label:
//!   [`PRIMARY_LABEL`], [`SECONDARY_LABEL`] and (for swap mode)
//!   [`SCRATCH_LABEL`]. Use `data, undefined` partitions so ESP-IDF tooling
//!   leaves them alone. [`read_layout`] finds them; [`bootloader`] and
//!   [`updater`] check them with `ferriboot-core`'s shared [`SlotLayout`]
//!   rules before touching flash.
//! * The flash is shared between the slots through an embassy-sync blocking
//!   mutex and embassy-embedded-hal [`BlockingPartition`]s, so no async
//!   executor is required.
//! * [`BootInfo`] passes the selected slot from the bootloader through reserved
//!   SRAM; applications do not inspect MMU registers to discover their slot.

use core::cell::RefCell;
use embedded_storage::nor_flash::NorFlash;

pub use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_sync::blocking_mutex::{Mutex, raw::NoopRawMutex};
use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
pub use esp_storage::FlashStorage;
pub use ferriboot_core::layout::{FlashBounds, LayoutError, Region, SlotLayout};
use ferriboot_core::{BootConfig, BootLoader, FirmwareUpdater, PublicKey};
#[allow(unsafe_code)] // Linker-reserved handoff RAM.
pub mod boot_info;
/// HAL-backed third-stage startup, watchdog and reset helpers.
#[cfg(feature = "boot-runtime")]
#[allow(unsafe_code)] // PAC field without a safe setter (WDT key).
pub mod boot_runtime;
pub mod chip;
pub use chip::mmu;
/// Clock glitch detector reset enable and verification.
#[allow(unsafe_code)] // PAC field without a safe setter (FIB_SEL).
pub mod glitch;
/// RAM handoff to the application: plan, fault-hardened confirmation, jump.
#[cfg(feature = "boot-runtime")]
#[allow(unsafe_code)] // Payload installation and the final jump.
pub mod loader;
pub mod loader_plan;
pub use boot_info::BootInfo;
mod policy;
#[allow(unsafe_code)] // eFuse programming registers.
pub mod security_counter;
pub mod watchdog;
pub use policy::{Policy, PolicyExt};

pub const PRIMARY_LABEL: &str = "image-0";
pub const SECONDARY_LABEL: &str = "image-1";
pub const SCRATCH_LABEL: &str = "image-scratch";

/// The flash, shareable between several partitions.
pub type SharedFlash<'d> = SharedStorage<FlashStorage<'d>>;

/// Shared storage, optionally wrapped to report flash progress to a watchdog.
pub type SharedStorage<F> = Mutex<NoopRawMutex, RefCell<F>>;

/// One slot (or the scratch area) as a `NorFlash`.
pub type Partition<'a, 'd> = StoragePartition<'a, FlashStorage<'d>>;

/// One partition over a generic storage driver.
pub type StoragePartition<'a, F> = BlockingPartition<'a, NoopRawMutex, F>;

/// Construct the ROM-backed flash driver from the HAL's unique FLASH
/// peripheral and make it shareable by the slot partitions.
///
/// Call once per boot. `esp-storage` owns the low-level flash operations;
/// `esp-hal` supplies the peripheral singleton and boot-time clock setup.
pub fn open_flash<'d>(flash: esp_hal::peripherals::FLASH<'d>) -> SharedFlash<'d> {
    share(FlashStorage::new(flash))
}

/// Wrap a [`FlashStorage`] so it can be split into partitions.
pub fn share<F: NorFlash>(flash: F) -> SharedStorage<F> {
    Mutex::new(RefCell::new(flash))
}

// Only the table locator is compiled in. All partition locations and sizes
// come from the flashed table, independently of the CSV used to build it.
include!(concat!(env!("OUT_DIR"), "/partition_table.rs"));

/// Reading the slot layout from the partition table failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionTableError {
    /// The partition table could not be read or is corrupt.
    Unreadable,
    /// A required partition is missing.
    MissingPartition(&'static str),
}

/// Read the slot locations from the partition table. The geometry is
/// checked by [`bootloader`] and [`updater`] before any slot is accessed.
pub fn read_layout(flash: &SharedFlash<'_>) -> Result<SlotLayout, PartitionTableError> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    flash.lock(|f| {
        let mut f = f.borrow_mut();
        let table = partitions::read_partition_table(&mut f, &mut buf)
            .map_err(|_| PartitionTableError::Unreadable)?;
        // Only raw accessors are used: `partition_type()` panics on
        // application-defined types.
        let find = |label: &str| {
            table
                .iter()
                .find(|p| p.label_as_str() == label)
                .map(|p| Region::new(p.offset(), p.len()))
        };
        Ok(SlotLayout {
            primary: find(PRIMARY_LABEL)
                .ok_or(PartitionTableError::MissingPartition(PRIMARY_LABEL))?,
            secondary: find(SECONDARY_LABEL)
                .ok_or(PartitionTableError::MissingPartition(SECONDARY_LABEL))?,
            scratch: find(SCRATCH_LABEL),
        })
    })
}

/// One region of the shared flash as a `NorFlash` partition.
pub fn partition<F: NorFlash>(flash: &SharedStorage<F>, region: Region) -> StoragePartition<'_, F> {
    BlockingPartition::new(flash, region.offset, region.size)
}

/// The device limits every layout is checked against.
fn bounds<F: NorFlash>(flash: &SharedStorage<F>) -> FlashBounds {
    FlashBounds {
        capacity: flash.lock(|f| f.borrow().capacity()),
        reserved: PARTITION_TABLE_END,
    }
}

/// Check the layout against the flash and construct the boot state machine.
pub fn bootloader<'a, 'k, F: NorFlash>(
    layout: &SlotLayout,
    flash: &'a SharedStorage<F>,
    keys: &'k [PublicKey],
    config: BootConfig,
) -> Result<BootLoader<'k, StoragePartition<'a, F>>, LayoutError> {
    layout.bootloader(bounds(flash), keys, config, |r| Ok(partition(flash, r)))
}

/// Create an updater using this boot's explicit handoff. Invalid layout
/// metadata or a secondary-slot handoff in swap/overwrite mode fails closed.
/// The upgrade mode comes from BootInfo; signature verification happens only
/// in the bootloader.
pub fn updater<'a, F: NorFlash>(
    layout: &SlotLayout,
    info: &BootInfo,
    flash: &'a SharedStorage<F>,
) -> Result<FirmwareUpdater<StoragePartition<'a, F>>, LayoutError> {
    layout.updater(bounds(flash), info, |r| Ok(partition(flash, r)))
}
