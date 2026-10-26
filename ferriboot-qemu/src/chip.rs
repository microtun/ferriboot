//! Facts about the emulated machine, from `chips/qemu`:
//! the W25Q32 SPI NOR on the patched AN385, the SSRAM execution window, the
//! SRAM handoff ABI and the test peripherals' MMIO addresses. Nothing here
//! depends on the board; it passes its flash map to
//! [`Layout::new`](crate::Layout::new).

pub use ferriboot_chip_qemu::{
    APP_XIP_BASE, BOOT_INFO_ADDRESS, BOOT_INFO_SIZE, EFUSE_BASE, ERASE_SIZE, FLASH_SIZE,
    HEADER_SIZE, RAM_BASE, RESET_MARKER, SECURITY_COUNTER_CAPACITY, SPI_BASE, SPI_CS, TRNG_BASE,
    UART_BASE,
};

/// The application's vector table, after the MCUboot header in SSRAM1.
pub const APP_VECTOR: u32 = APP_XIP_BASE + HEADER_SIZE;
/// AN385 SSRAM1, from which the copied image executes, ends here.
pub const EXEC_END: u32 = 0x40_0000;
pub const SECURITY_COUNTER_BYTES: usize = (SECURITY_COUNTER_CAPACITY / 8) as usize;

const _: () = {
    assert!(ERASE_SIZE > 0 && FLASH_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(HEADER_SIZE > 0);
    assert!(APP_VECTOR < EXEC_END);
    assert!(SECURITY_COUNTER_CAPACITY > 0 && SECURITY_COUNTER_CAPACITY.is_multiple_of(8));
    assert!(RAM_BASE < BOOT_INFO_ADDRESS);
    assert!(BOOT_INFO_SIZE == ferriboot_core::shared_data::BOOT_INFO_SIZE);
    assert!(TRNG_BASE.is_multiple_of(4));
    assert!(TRNG_BASE >= EFUSE_BASE + 0x108);
    assert!(BOOT_INFO_ADDRESS + BOOT_INFO_SIZE as u32 <= RESET_MARKER);
};
