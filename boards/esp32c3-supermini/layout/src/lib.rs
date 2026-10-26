#![no_std]
//! ESP32-C3 board configuration; chip constants live in chips/esp32c3.
//! Target code reads partitions from flash; host tools read ESP-IDF CSV.

pub use ferriboot_chip_esp32c3::*;

/// Capacity of the flash installed on this board (not a chip property).
pub const FLASH_SIZE: u32 = 0x0040_0000;

const _: () = {
    assert!(ERASE_SIZE > 0 && FLASH_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(FLASH_SIZE.is_multiple_of(MMU_PAGE_SIZE));
};
