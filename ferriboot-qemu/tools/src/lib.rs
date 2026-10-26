//! QEMU MPS2 AN385 host tooling: checked ELF export and board geometry.
#![forbid(unsafe_code)]

pub mod elf;

use ferriboot_build::tools::mcuboot::Geometry;
use std::collections::BTreeMap;

/// MCUboot geometry of the compiled board layout.
pub fn geometry() -> Geometry {
    use ferriboot_layout_qemu::*;
    Geometry {
        header: HEADER_SIZE,
        slot: SLOT_SIZE,
        erase: ERASE_SIZE,
        write: 4,
        rom_fixed: Some(APP_XIP_BASE),
        class: "qemu-an385",
    }
}

/// Machine and board constants of the compiled layout.
pub fn constants() -> BTreeMap<String, u32> {
    use ferriboot_layout_qemu::*;
    ferriboot_build::named_constants![
        // Machine (chips/qemu).
        FLASH_SIZE,
        ERASE_SIZE,
        HEADER_SIZE,
        SECURITY_COUNTER_CAPACITY,
        APP_XIP_BASE,
        RAM_BASE,
        BOOT_INFO_ADDRESS,
        BOOT_INFO_SIZE,
        RESET_MARKER,
        SPI_BASE,
        SPI_CS,
        EFUSE_BASE,
        TRNG_BASE,
        UART_BASE,
        // Board.
        BOOTLOADER_SIZE,
        PRIMARY_OFFSET,
        SECONDARY_OFFSET,
        SLOT_SIZE,
        SCRATCH_OFFSET,
        SCRATCH_SIZE,
        STORAGE_OFFSET,
        STORAGE_SIZE,
    ]
}
