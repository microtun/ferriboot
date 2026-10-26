//! RP2350 host tooling: checked ELF export, PICOBIN parsing, ROM sealing,
//! SWD loading and the probe-rs fixture flash algorithm.
#![forbid(unsafe_code)]

pub mod algorithm;
pub mod elf;
pub mod picobin;
mod romseal;
pub mod seal;

use anyhow::{Result, ensure};
use ferriboot_build::tools::{
    keys::DevKey,
    mcuboot::{self, Geometry},
};
use ferriboot_layout_rp2350_pico2 as chip;
use std::{collections::BTreeMap, path::Path, process::Command};

/// MCUboot geometry of the compiled board layout.
pub fn geometry() -> Geometry {
    use ferriboot_layout_rp2350_pico2::*;
    Geometry {
        header: HEADER_SIZE,
        slot: SLOT_SIZE,
        erase: ERASE_SIZE,
        write: WRITE_SIZE,
        rom_fixed: Some(APP_XIP_BASE),
        class: "rp2350",
    }
}

/// Chip and board constants of the compiled layout (`layout/partitions.json`).
pub fn constants() -> BTreeMap<String, u32> {
    use ferriboot_layout_rp2350_pico2::*;
    ferriboot_build::named_constants![
        // Chip (chips/rp235x).
        XIP_BASE,
        APP_XIP_BASE,
        APP_XIP_WINDOW_SIZE,
        HEADER_SIZE,
        VECTOR_ALIGN,
        ERASE_SIZE,
        WRITE_SIZE,
        RAM_BASE,
        BOOT_INFO_ADDRESS,
        BOOT_INFO_SIZE,
        // Board.
        FLASH_SIZE,
        BOOTLOADER_SIZE,
        PRIMARY_OFFSET,
        SECONDARY_OFFSET,
        SLOT_SIZE,
        SCRATCH_OFFSET,
        SCRATCH_SIZE,
        STORAGE_OFFSET,
        STORAGE_SIZE,
        MAX_IMAGE_SIZE,
    ]
}

/// Development secp256k1 ROM-signing key.
pub const DEV_KEY: DevKey = DevKey {
    dir: "boards/rp2350-pico2/keys",
    stem: "dev-rp2350-rom-secp256k1",
    generate: seal::generate_pair,
};

/// Check a signed application against the RP2350 linking contract.
pub fn check_signed_app(data: &[u8]) -> Result<()> {
    let signed = mcuboot::inspect(data, geometry(), None)?;
    ensure!(
        signed.load_address == chip::APP_XIP_BASE
            && signed.header_size == chip::HEADER_SIZE as usize
            && signed.flags == 0x100,
        "signed header differs from RP2350 linking contract"
    );
    ferriboot_build::tools::elf::check_vectors(
        &data[signed.header_size..signed.header_size + signed.body_size],
        chip::APP_XIP_BASE + chip::HEADER_SIZE,
        chip::RAM_BASE,
        chip::BOOT_INFO_ADDRESS,
    )
}

/// Download a binary over SWD with probe-rs, then reset.
pub fn download(binary: &Path, address: u32) -> Result<()> {
    let mut options = vec![
        "--chip".to_owned(),
        ferriboot_build::tools::env("PROBE_RS_CHIP", "RP235x"),
        "--protocol".into(),
        "swd".into(),
    ];
    for (var, option) in [
        ("PROBE_RS_PROBE", "--probe"),
        ("PROBE_RS_SPEED_KHZ", "--speed"),
    ] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
        {
            options.extend([option.into(), value]);
        }
    }
    ferriboot_build::tools::run(
        Command::new("probe-rs")
            .arg("download")
            .args(&options)
            .args(["--binary-format", "bin", "--base-address"])
            .arg(format!("{address:#x}"))
            .arg(binary),
    )?;
    ferriboot_build::tools::run(Command::new("probe-rs").arg("reset").args(&options))
}
