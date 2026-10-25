//! Pico 2/Pico 2 W board configuration; RP2350 constants live in chips/rp235x.

pub use ferriboot_chip_rp235x::*;

/// Capacity of the flash installed on this board (not a chip property).
pub const FLASH_SIZE: u32 = 0x0040_0000;
