//! ESP32-C3 memory and MMIO constants (ESP-IDF / matching PAC) that only
//! Rust code needs. Values shared with linker scripts and host tools live in
//! `chips/esp32c3` (see the parent module).
pub const NAME: &str = "ESP32-C3";
pub const IMAGE_CHIP_ID: u16 = 5;
pub const IRAM: (u32, u32) = (0x4038_0000, 0x403E_0000);
pub const DRAM: (u32, u32) = (0x3FC8_0000, 0x3FCE_0000);
pub const RTC_FAST: (u32, u32) = (RTC_FAST_BASE, RTC_FAST_BASE + RTC_FAST_SIZE);

use super::{RTC_FAST_BASE, RTC_FAST_SIZE};

/// ESP32-C3 flash MMU geometry.
pub mod mmu {
    pub const ENTRIES: u32 = 128;
    pub const PAGE_SIZE: u32 = super::super::MMU_PAGE_SIZE;
    pub const IROM_BASE: u32 = 0x4200_0000;
    pub const DROM_BASE: u32 = 0x3C00_0000;
    pub const BUS_SIZE: u32 = ENTRIES * PAGE_SIZE;
}

// The protected-RAM handoff ABI of the C3 reference layout.
const _: () = {
    use super::*;
    // The loader starts where the retired IDF second-stage loader was placed.
    // It is never a LOAD segment in Ferriboot's ESP image: installed at runtime.
    assert!(IDF_IRAM_LOADER_START == 0x403C_E710);
    assert!(PROTECTED_START + IRAM_DRAM_OFFSET == IDF_IRAM_LOADER_START);
    assert!(IRAM_HANDOFF_SIZE == 0x1000);
    assert!(MMU_PAGE_SIZE == 0x1_0000);
    assert!(RTC_FAST_BASE == 0x5000_0000 && RTC_FAST_SIZE == 0x2000);
    assert!(BL_DRAM_BASE == DRAM.0);
};
