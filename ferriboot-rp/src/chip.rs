//! RP2350 constants generated from `chips/rp235x`: the XIP windows, image
//! header policy, flash program/erase granularity and the SRAM handoff ABI.
//! Nothing here depends on the board; boards pass their flash map to
//! [`Layout::new`](crate::Layout::new).

include!(concat!(env!("OUT_DIR"), "/chip.rs"));

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
