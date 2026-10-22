//! Every MMIO register (and reserved RAM word) this crate touches, declared
//! once. This and [`crate::mmio`] are the crate's only `unsafe` code; the
//! drivers use these constants through safe reads and writes.
use crate::chip::{
    EFUSE_BASE, RESET_MARKER, SECURITY_COUNTER_BYTES, SPI_BASE, SPI_CS, TRNG_BASE, UART_BASE,
};
use crate::mmio::{Reg, RegArray};

/// CMSDK APB UART0 (CMSDK TRM).
pub mod uart {
    use super::*;
    const BASE: usize = UART_BASE as usize;
    // SAFETY: AN385 provides CMSDK APB UART0 at this base.
    pub const DATA: Reg = unsafe { Reg::new(BASE) };
    // SAFETY: as above.
    pub const STATE: Reg = unsafe { Reg::new(BASE + 0x04) };
    // SAFETY: as above.
    pub const CTRL: Reg = unsafe { Reg::new(BASE + 0x08) };
    // SAFETY: as above.
    pub const BAUDDIV: Reg = unsafe { Reg::new(BASE + 0x10) };
}

/// PL022 SSP0 and the board patch's W25Q32 chip select (PL022 TRM).
pub mod spi {
    use super::*;
    const BASE: usize = SPI_BASE as usize;
    // SAFETY: SSP0 MMIO, present on the QEMU mps2-an385 platform.
    pub const CR0: Reg = unsafe { Reg::new(BASE) };
    // SAFETY: as above.
    pub const CR1: Reg = unsafe { Reg::new(BASE + 0x04) };
    // SAFETY: as above.
    pub const DR: Reg = unsafe { Reg::new(BASE + 0x08) };
    // SAFETY: as above.
    pub const SR: Reg = unsafe { Reg::new(BASE + 0x0c) };
    // SAFETY: as above.
    pub const CPSR: Reg = unsafe { Reg::new(BASE + 0x10) };
    // SAFETY: this test-only register of the board patch drives the W25Q32 CS#.
    pub const CS: Reg = unsafe { Reg::new(SPI_CS as usize) };
}

/// The board patch's simulated TRNG. An unpatched QEMU returns zero from its
/// reserved/unimplemented area, which `Trng::try_new` detects.
pub mod trng {
    use super::*;
    const BASE: usize = TRNG_BASE as usize;
    // SAFETY: the patched AN385 machine maps these 32-bit MMIO registers.
    pub const DEVICE_ID: Reg = unsafe { Reg::new(BASE) };
    // SAFETY: as above.
    pub const STATUS: Reg = unsafe { Reg::new(BASE + 4) };
    // SAFETY: as above.
    pub const DATA: Reg = unsafe { Reg::new(BASE + 8) };
}

/// The board patch's MMIO frontend to QEMU's Xilinx eFUSE model.
pub mod efuse {
    use super::*;
    const BASE: usize = EFUSE_BASE as usize;
    // SAFETY: the board patch maps the 48 eFuse data words here.
    pub const WORDS: RegArray = unsafe { RegArray::new(BASE, SECURITY_COUNTER_BYTES / 4) };
    // SAFETY: the board patch interprets writes as an eFuse bit index and
    // only ever sets that bit.
    pub const PROGRAM_BIT: Reg = unsafe { Reg::new(BASE + 0x100) };
    // SAFETY: the board patch's 32-bit identification register.
    pub const DEVICE_ID: Reg = unsafe { Reg::new(BASE + 0x104) };
}

/// CMSDK APB watchdog (CMSDK TRM).
pub mod watchdog {
    use super::*;
    const BASE: usize = 0x4000_8000;
    // SAFETY: AN385 provides the CMSDK APB watchdog at this base.
    pub const LOAD: Reg = unsafe { Reg::new(BASE) };
    // SAFETY: as above.
    pub const CONTROL: Reg = unsafe { Reg::new(BASE + 0x08) };
    // SAFETY: as above. Any write clears the interrupt and reloads the counter.
    pub const INTCLR: Reg = unsafe { Reg::new(BASE + 0x0c) };
    // SAFETY: as above.
    pub const LOCK: Reg = unsafe { Reg::new(BASE + 0xc00) };
}

/// Two private retention words above the boot handoff, outside both linker
/// RAM ranges (checked in `chip`), which survive a watchdog reset.
pub mod retention {
    use super::*;
    // SAFETY: reserved RAM, excluded from both linkers' RAM regions; no Rust
    // reference ever covers it.
    pub const MARKER: Reg = unsafe { Reg::new(RESET_MARKER as usize) };
    // SAFETY: as above.
    pub const REASON: Reg = unsafe { Reg::new(RESET_MARKER as usize + 4) };
}
