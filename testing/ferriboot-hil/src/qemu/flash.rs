//! QEMU's 4 MiB NOR fixture: bootloader, slots, scratch and hook storage.
//! Composition uses the shared [`crate::flash`] implementation.

use ferriboot_core::{Policy, SlotLayout};

pub use crate::flash::{FLASH_SIZE, FlashImage, Request, SECTOR, trusted_keys};

use super::layout;

const _: () = assert!(layout::FLASH_SIZE as usize == FLASH_SIZE);
const _: () = assert!(layout::ERASE_SIZE == SECTOR);

/// Match the guest's default image policy.
pub fn policy() -> Policy {
    Policy::default()
}

/// The virtual board layout shared with the guest.
pub fn slot_layout() -> SlotLayout {
    // The board layout crate's flash map, as the bootloader uses it.
    layout::FLASH_MAP.slots
}

/// Erased NOR fixture with the QEMU-loadable bootloader at zero.
pub fn compose(bootloader: &[u8]) -> crate::Result<FlashImage> {
    if bootloader.len() > layout::BOOTLOADER_SIZE as usize {
        return Err("bootloader exceeds its partition".into());
    }
    let flash = FlashImage::erased(
        &super::BOARD,
        slot_layout(),
        Some(layout::STORAGE),
        policy(),
    );
    flash.program(0, bootloader);
    Ok(flash)
}
