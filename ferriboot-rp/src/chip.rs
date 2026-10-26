//! RP2350 constants from `chips/rp235x`: the XIP windows, image header
//! policy, flash program/erase granularity and the SRAM handoff ABI.
//! Nothing here depends on the board; boards pass their flash map to
//! [`Layout::new`](crate::Layout::new).

pub use ferriboot_chip_rp235x::{
    APP_XIP_BASE, APP_XIP_WINDOW_SIZE, BOOT_INFO_ADDRESS, BOOT_INFO_SIZE, ERASE_SIZE, HEADER_SIZE,
    RAM_BASE, VECTOR_ALIGN, WRITE_SIZE, XIP_BASE,
};

const _: () = {
    assert!(ERASE_SIZE > 0);
    assert!(WRITE_SIZE > 0 && ERASE_SIZE.is_multiple_of(WRITE_SIZE));
    assert!(HEADER_SIZE > 0 && HEADER_SIZE.is_multiple_of(VECTOR_ALIGN));
    // QMI ATRANS1 maps the application's window directly after window 0.
    assert!(APP_XIP_BASE == XIP_BASE + APP_XIP_WINDOW_SIZE);
    assert!(APP_XIP_WINDOW_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(RAM_BASE < BOOT_INFO_ADDRESS);
    assert!(BOOT_INFO_ADDRESS.is_multiple_of(4));
    assert!(BOOT_INFO_SIZE as usize == ferriboot_core::shared_data::BOOT_INFO_SIZE);
};
