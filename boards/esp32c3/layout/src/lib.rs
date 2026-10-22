#![no_std]
//! The ESP32-C3 board for target code, linker scripts and host tools: the
//! chip facts from `ferriboot-esp/chips/esp32c3.json` (via `board.json`) plus
//! the board's flash size. Partition offsets and sizes are deliberately
//! absent: target code reads the flashed ESP-IDF table and host tools read
//! `partitions.csv` or its binary table.

include!(concat!(env!("OUT_DIR"), "/layout.rs"));

const _: () = {
    assert!(ERASE_SIZE > 0 && FLASH_SIZE.is_multiple_of(ERASE_SIZE));
    assert!(FLASH_SIZE.is_multiple_of(MMU_PAGE_SIZE));
};
