//! Pico 2 / Pico 2 W board constants: RP2350 addresses, image ABI and flash size.
//!
//! Shared by target code, host tools and `ferriboot-build` through this
//! layout crate. Also included by this crate's partition generator.

pub const CHIP: &str = "rp2350";
pub const FLASH_SIZE: u32 = 0x0040_0000;

pub const XIP_BASE: u32 = 0x10000000;
pub const APP_XIP_BASE: u32 = 0x10400000;
pub const APP_XIP_WINDOW_SIZE: u32 = 0x00400000;
pub const HEADER_SIZE: u32 = 0x00000200;
pub const VECTOR_ALIGN: u32 = 0x00000200;
pub const ERASE_SIZE: u32 = 0x00001000;
pub const WRITE_SIZE: u32 = 0x00000004;
pub const RAM_BASE: u32 = 0x20000000;
pub const BOOT_INFO_ADDRESS: u32 = 0x2007ff00;
pub const BOOT_INFO_SIZE: u32 = 0x00000100;

/// Named values used by linker-script generation.
pub const VALUES: &[(&str, u32)] = &[
    ("XIP_BASE", XIP_BASE),
    ("APP_XIP_BASE", APP_XIP_BASE),
    ("APP_XIP_WINDOW_SIZE", APP_XIP_WINDOW_SIZE),
    ("HEADER_SIZE", HEADER_SIZE),
    ("VECTOR_ALIGN", VECTOR_ALIGN),
    ("ERASE_SIZE", ERASE_SIZE),
    ("WRITE_SIZE", WRITE_SIZE),
    ("RAM_BASE", RAM_BASE),
    ("BOOT_INFO_ADDRESS", BOOT_INFO_ADDRESS),
    ("BOOT_INFO_SIZE", BOOT_INFO_SIZE),
];
