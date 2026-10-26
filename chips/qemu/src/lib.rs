#![no_std]
//! Hardware and Ferriboot ABI constants for the patched QEMU MPS2-AN385.

pub const CHIP: &str = "mps2-an385";
pub const FLASH_SIZE: u32 = 0x00400000;
pub const ERASE_SIZE: u32 = 0x00001000;
pub const HEADER_SIZE: u32 = 0x00001000;
pub const SECURITY_COUNTER_CAPACITY: u32 = 0x00000600;
pub const APP_XIP_BASE: u32 = 0x000ff000;
pub const RAM_BASE: u32 = 0x20000000;
pub const BOOT_INFO_ADDRESS: u32 = 0x203ffe00;
pub const BOOT_INFO_SIZE: usize = 0x00000100;
pub const RESET_MARKER: u32 = 0x203fff00;
pub const SPI_BASE: u32 = 0x40020000;
pub const SPI_CS: u32 = 0x40030000;
pub const EFUSE_BASE: u32 = 0x40031000;
pub const TRNG_BASE: u32 = 0x40032000;
pub const UART_BASE: u32 = 0x40004000;
