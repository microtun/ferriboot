//! ESP32-C3 host tooling: ESP image checks and secure padding, Secure Boot V2
//! signing, ESP-IDF partition tables and the IDF boot chain.
#![forbid(unsafe_code)]

pub mod chain;
pub mod elf;
pub mod flash;
pub mod image;
pub mod layout;
pub mod sbv2;

use anyhow::Result;
use ferriboot_tools::{keys::DevKey, mcuboot::Geometry};

/// MCUboot geometry of the board's primary slot (`boards/esp32c3/layout/partitions.csv`).
pub fn geometry() -> Result<Geometry> {
    use esp32c3_layout::*;
    let table = layout::default_layout()?;
    Ok(Geometry {
        header: HEADER_SIZE,
        slot: table.region("image-0")?.1,
        erase: ERASE_SIZE,
        write: 4,
        rom_fixed: None,
        class: "esp32c3",
    })
}

/// Development RSA-3072 Secure Boot V2 key.
pub const DEV_KEY: DevKey = DevKey {
    dir: "boards/esp32c3/keys",
    stem: "dev-esp32c3-sbv2-rsa3072",
    generate: sbv2::generate_pair,
};
