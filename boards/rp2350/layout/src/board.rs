//! Pico 2/Pico 2 W board configuration; RP2350 constants live in chips/rp2350.

pub use ferriboot_chip_rp2350::*;

/// Capacity of the flash installed on this board (not a chip property).
pub const FLASH_SIZE: u32 = 0x0040_0000;
