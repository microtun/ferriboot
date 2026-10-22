//! QEMU MPS2 AN385 host tooling: checked ELF export and board geometry.
#![forbid(unsafe_code)]

pub mod elf;

use ferriboot_build::tools::mcuboot::Geometry;
use qemu_layout as chip;
use std::collections::BTreeMap;

/// MCUboot geometry of the compiled board layout.
pub fn geometry() -> Geometry {
    use qemu_layout::*;
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
    use qemu_layout::*;
    let mut values: BTreeMap<String, u32> = BTreeMap::new();
    values.extend(chip::VALUES.iter().map(|&(k, v)| (k.to_owned(), v)));
    values.extend(
        [
            ("BOOTLOADER_SIZE", BOOTLOADER_SIZE),
            ("PRIMARY_OFFSET", PRIMARY_OFFSET),
            ("SECONDARY_OFFSET", SECONDARY_OFFSET),
            ("SLOT_SIZE", SLOT_SIZE),
            ("SCRATCH_OFFSET", SCRATCH_OFFSET),
            ("SCRATCH_SIZE", SCRATCH_SIZE),
            ("STORAGE_OFFSET", STORAGE_OFFSET),
            ("STORAGE_SIZE", STORAGE_SIZE),
        ]
        .map(|(k, v)| (k.into(), v)),
    );
    values
}
