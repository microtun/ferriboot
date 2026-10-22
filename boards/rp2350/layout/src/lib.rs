#![no_std]
//! The Pico 2 / Pico 2 W board (4 MiB flash): its flash partitions plus the
//! RP2350 chip facts it builds on, shared by firmware, linker scripts and
//! host-side tools.
//!
//! * `partitions.json` (picotool format) is the flash-layout source of truth.
//! * `board.json` holds the board's other values and names the chip
//!   description, `ferriboot-rp/chips/rp2350.json`.
//!
//! This crate generates `layout.rs` from those files; [`FLASH_MAP`] is the
//! board's flash map for `ferriboot_rp::Layout::new`. `../tools/layout.py`
//! reads the same inputs for the host-side image tools.

use ferriboot_core::{FlashBounds, FlashMap, Region, SlotLayout};

include!(concat!(env!("OUT_DIR"), "/layout.rs"));

/// The board's flash map: device size, bootloader reservation and slots.
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

/// Application-owned storage, after the scratch area.
pub const STORAGE: Region = Region::new(STORAGE_OFFSET, STORAGE_SIZE);

const _: () = {
    assert!(ERASE_SIZE > 0 && FLASH_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(BOOTLOADER_SIZE > 0 && BOOTLOADER_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(BOOTLOADER_SIZE == PRIMARY_OFFSET);
    assert!(HEADER_SIZE > 0 && HEADER_SIZE < MAX_IMAGE_SIZE && MAX_IMAGE_SIZE <= SLOT_SIZE);
    assert!(SECONDARY_OFFSET == PRIMARY_OFFSET + SLOT_SIZE);
    assert!(SECONDARY_OFFSET + SLOT_SIZE <= SCRATCH_OFFSET);
    assert!(STORAGE_OFFSET == SCRATCH_OFFSET + SCRATCH_SIZE);
    assert!(STORAGE_SIZE >= 0x8000 && STORAGE_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(STORAGE_OFFSET + STORAGE_SIZE == FLASH_SIZE);
    assert!(SLOT_SIZE <= APP_XIP_WINDOW_SIZE);
};
