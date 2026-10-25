//! ESP32-C3 board configuration; chip constants live in chips/esp32c3.

pub use ferriboot_chip_esp32c3::*;

/// Capacity of the flash installed on this board (not a chip property).
pub const FLASH_SIZE: u32 = 0x0040_0000;
