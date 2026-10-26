#![no_std]
//! Board-independent RP235x family hardware and Ferriboot ABI constants.
//!
//! One die covers every supported package: RP2350A/B and RP2354A/B (the
//! RP2354 adds 2 MiB of in-package flash, which is a board layout's
//! `FLASH_SIZE`, not a chip fact).

pub const CHIP: &str = "rp2350";
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
