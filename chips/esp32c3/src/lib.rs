#![no_std]
//! Board-independent ESP32-C3 hardware and Ferriboot ABI constants.

pub const CHIP: &str = "esp32c3";
pub const ERASE_SIZE: u32 = 0x00001000;
pub const HEADER_SIZE: u32 = 0x00001000;
pub const MMU_PAGE_SIZE: u32 = 0x00010000;
pub const PARTITION_TABLE_SIZE: u32 = 0x00001000;
pub const IRAM_DRAM_OFFSET: u32 = 0x00700000;
pub const BL_DRAM_BASE: u32 = 0x3fc80000;
pub const PROTECTED_START: u32 = 0x3fcce710;
pub const IRAM_HANDOFF_SIZE: u32 = 0x00001000;
pub const IDF_IRAM_LOADER_START: u32 = 0x403ce710;
pub const BOOT_INFO_ADDRESS: u32 = 0x3fcde610;
pub const BOOT_INFO_SIZE: u32 = 0x00000100;
pub const ROM_DATA_START: u32 = 0x3fcde710;
pub const RTC_FAST_BASE: u32 = 0x50000000;
pub const RTC_FAST_SIZE: u32 = 0x00002000;
pub const LOADER_PLAN_ADDRESS: u32 = 0x3fcd5000;
pub const LOADER_STACK_BOTTOM: u32 = 0x3fcd6000;
pub const LOADER_STACK_TOP: u32 = 0x3fcd8000;
