#![no_std]
//! Shared flash and memory layout for QEMU guests, build scripts and host tools.
//!
//! Machine constants live in chips/qemu; this board's partition sizes,
//! offsets and [`FLASH_MAP`] are defined here.

use ferriboot_core::{FlashBounds, FlashMap, Region, SlotLayout};

pub use ferriboot_chip_qemu::*;

pub const BOOTLOADER_SIZE: u32 = 0x40000;
pub const SLOT_SIZE: u32 = 0x1d0000;
pub const SCRATCH_SIZE: u32 = 0x10000;

// The flash fixture consists of a bootloader, two slots, scratch and storage.
pub const PRIMARY_OFFSET: u32 = BOOTLOADER_SIZE;
pub const SECONDARY_OFFSET: u32 = PRIMARY_OFFSET + SLOT_SIZE;
pub const SCRATCH_OFFSET: u32 = SECONDARY_OFFSET + SLOT_SIZE;
pub const STORAGE_OFFSET: u32 = SCRATCH_OFFSET + SCRATCH_SIZE;
pub const STORAGE_SIZE: u32 = FLASH_SIZE - STORAGE_OFFSET;
// AN385 SSRAM1 holds the executable image; the W25Q32 is accessed over SPI.
pub const APP_VECTOR: u32 = APP_XIP_BASE + HEADER_SIZE;
pub const SECURITY_COUNTER_BYTES: usize = (SECURITY_COUNTER_CAPACITY / 8) as usize;

/// The board's flash map: the whole SPI NOR, the bootloader and the slots.
pub const FLASH_MAP: FlashMap = FlashMap {
    bounds: FlashBounds {
        capacity: FLASH_SIZE as usize,
        reserved: BOOTLOADER_SIZE,
    },
    slots: SlotLayout {
        primary: Region::new(PRIMARY_OFFSET, SLOT_SIZE),
        secondary: Region::new(SECONDARY_OFFSET, SLOT_SIZE),
        scratch: Some(Region::new(SCRATCH_OFFSET, SCRATCH_SIZE)),
    },
};

/// Application-owned storage (test hook records), after the scratch area.
pub const STORAGE: Region = Region::new(STORAGE_OFFSET, STORAGE_SIZE);

const _: () = {
    assert!(BOOTLOADER_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(SLOT_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(SCRATCH_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(HEADER_SIZE < SLOT_SIZE);
    assert!(STORAGE_OFFSET < FLASH_SIZE && STORAGE_SIZE >= ERASE_SIZE);
};
