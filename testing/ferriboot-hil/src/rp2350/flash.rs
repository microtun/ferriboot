//! The RP2350's 4 MiB flash: the ROM-loadable SRAM bootloader at zero, then
//! the slots, scratch and storage of the board's `partitions.json`. The
//! composer itself is [`crate::flash`].

use ferriboot_core::{Policy, SlotLayout};

pub use crate::flash::{FLASH_SIZE, FlashImage, Request, SECTOR, trusted_keys};

use super::layout;

const _: () = assert!(layout::FLASH_SIZE as usize == FLASH_SIZE);
const _: () = assert!(layout::ERASE_SIZE == SECTOR);

/// `Policy::rp2350()` in `ferriboot-rp` is the default policy.
pub fn policy() -> Policy {
    Policy::default()
}

/// The fixed Pico 2 / Pico 2 W layout (`ferriboot_layout_rp2350::FLASH_MAP`).
pub fn slot_layout() -> SlotLayout {
    // The board layout crate's flash map, as the bootloader uses it.
    layout::FLASH_MAP.slots
}

/// Erased physical flash with the ROM-loadable SRAM bootloader at zero.
/// The RP geometry is generated from the board's partitions.json; unlike
/// ESP there is no binary partition table needed by the boot algorithm.
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
