//! The bootloader variants and the firmware, built once per test process by
//! [`crate::artifacts`] under `target/hil-esp32c3/`. The ESP-IDF second
//! stage and Ferriboot factory app are both RSA-3072 signed, as on hardware.

use std::sync::OnceLock;

pub use crate::artifacts::{Variant, repo_root};

/// A complete verified boot chain, not just Ferriboot's ELF.
pub struct Bootloader {
    /// Signed Ferriboot app, placed in the table's `ferriboot` partition.
    pub image: Vec<u8>,
    /// Signed standard ESP-IDF second stage at flash offset 0.
    pub idf_image: Vec<u8>,
}

use super::{
    BOARD,
    esp_image::{self, EspImage},
};

/// The bootloader built with `variant`'s features and converted for flash.
pub fn bootloader(variant: Variant) -> &'static Bootloader {
    static CELLS: [OnceLock<Bootloader>; Variant::COUNT] =
        [const { OnceLock::new() }; Variant::COUNT];
    CELLS[variant as usize].get_or_init(|| {
        let elf = crate::artifacts::build_bootloader(&BOARD, variant);
        let temp = tempfile::tempdir().expect("boot-chain temp directory");
        let elf_path = temp.path().join("ferriboot.elf");
        std::fs::write(&elf_path, &elf).expect("write Ferriboot ELF");
        let padded = esp_image::from_elf(&elf)
            .and_then(EspImage::secure_pad_v2)
            .unwrap_or_else(|e| panic!("cannot create ESP image: {e}"));
        let image_path = temp.path().join("image.bin");
        std::fs::write(&image_path, padded.bytes()).expect("write padded ESP image");
        let mut settings = ferriboot_tools_esp::chain::Settings::from_env();
        settings.no_flash = true;
        settings.prebuilt_image = Some(image_path);
        settings.key =
            crate::artifacts::board_dir(&BOARD).join("keys/dev-esp32c3-sbv2-rsa3072.pem");
        let chain = ferriboot_tools_esp::chain::boot_chain(&elf_path, &settings)
            .unwrap_or_else(|e| panic!("cannot sign ESP boot chain: {e:#}"));
        let image = chain.ferriboot;
        let idf_image = chain.stage2;
        Bootloader { image, idf_image }
    })
}

/// The example firmware ELF with built-in test hooks.
pub fn firmware_elf() -> &'static [u8] {
    static ELF: OnceLock<Vec<u8>> = OnceLock::new();
    ELF.get_or_init(|| crate::artifacts::build_firmware(&BOARD))
}

/// The firmware as an ESP image. Every application image in the suite is this
/// image, signed with different versions.
pub fn firmware_image() -> &'static EspImage {
    static IMAGE: OnceLock<EspImage> = OnceLock::new();
    IMAGE.get_or_init(|| {
        esp_image::from_elf(firmware_elf())
            .unwrap_or_else(|e| panic!("espflash cannot convert the firmware: {e}"))
    })
}
