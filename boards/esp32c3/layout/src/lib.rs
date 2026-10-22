#![no_std]
//! ESP32-C3 chip constants and this board's flash size.
//! Target code reads partitions from flash; host tools read ESP-IDF CSV.

mod board;
pub use board::*;

const _: () = {
    assert!(ERASE_SIZE > 0 && FLASH_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(FLASH_SIZE.is_multiple_of(MMU_PAGE_SIZE));
};
