//! Facts about the chip selected by this crate's chip feature.
//!
//! The values shared with linker scripts, board layout crates and host tools
//! come from `chips/esp32c3`; the Rust-only register addresses and memory
//! regions live in the chip's module. Nothing here depends on the board:
//! flash size and partition geometry come from the partition table.

pub use ferriboot_chip_esp32c3::{
    BL_DRAM_BASE, BOOT_INFO_ADDRESS, BOOT_INFO_SIZE, ERASE_SIZE, HEADER_SIZE,
    IDF_IRAM_LOADER_START, IRAM_DRAM_OFFSET, IRAM_HANDOFF_SIZE, LOADER_PLAN_ADDRESS,
    LOADER_STACK_BOTTOM, LOADER_STACK_TOP, MMU_PAGE_SIZE, PARTITION_TABLE_SIZE, PROTECTED_START,
    ROM_DATA_START, RTC_FAST_BASE, RTC_FAST_SIZE,
};

#[cfg(feature = "esp32c3")]
mod esp32c3;
#[cfg(feature = "esp32c3")]
pub use esp32c3::*;

// Chip-independent consistency of the shared values.
const _: () = {
    assert!(ERASE_SIZE > 0);
    assert!(MMU_PAGE_SIZE > 0 && MMU_PAGE_SIZE.is_multiple_of(ERASE_SIZE));
    // The ESP image after the MCUboot header must stay MMU-page aligned when
    // a slot starts on a page boundary minus the header.
    assert!(HEADER_SIZE > 0 && HEADER_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(HEADER_SIZE < MMU_PAGE_SIZE);
    assert!(PARTITION_TABLE_SIZE > 0 && PARTITION_TABLE_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(PROTECTED_START > BL_DRAM_BASE);
    // Ferriboot's regular stack ends below the protected handoff region; the
    // new image's plan sits above the handoff and below the loader's stack.
    assert!(PROTECTED_START + IRAM_HANDOFF_SIZE <= LOADER_PLAN_ADDRESS);
    assert!(LOADER_PLAN_ADDRESS.is_multiple_of(16));
    assert!(LOADER_PLAN_ADDRESS + 0x800 <= LOADER_STACK_BOTTOM);
    assert!(LOADER_STACK_BOTTOM.is_multiple_of(16));
    assert!(LOADER_STACK_TOP.is_multiple_of(16));
    assert!(LOADER_STACK_BOTTOM + 0x1000 <= LOADER_STACK_TOP);
    assert!(LOADER_STACK_TOP < BOOT_INFO_ADDRESS);
    assert!(BOOT_INFO_ADDRESS.is_multiple_of(4));
    assert!(BOOT_INFO_SIZE as usize == ferriboot_core::shared_data::BOOT_INFO_SIZE);
    assert!(BOOT_INFO_ADDRESS + BOOT_INFO_SIZE == ROM_DATA_START);
};
