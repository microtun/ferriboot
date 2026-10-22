//! The bootloader variants and the firmware, built once per test process by
//! [`crate::artifacts`] under `target/hil-esp32c3/`. The ESP-IDF second
//! stage and Ferriboot factory app are both RSA-3072 signed, as on hardware.

use std::{path::PathBuf, process::Command, sync::OnceLock};

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
        // Preserve the board's boot-chain, signing, and layout checks while
        // replacing esptool ELF conversion with the HIL Rust implementation.
        let temp = tempfile::tempdir().expect("temp dir for Ferriboot signed image");
        let elf_path: PathBuf = temp.path().join("ferriboot.elf");
        std::fs::write(&elf_path, &elf).expect("write Ferriboot ELF for signer");
        // Convert and secure-pad with Rust rather than invoking esptool's
        // `elf2image`. The production Python runner still checks SRAM regions,
        // signs the IDF second stage and both images, and enforces sizes.
        let esp_image = esp_image::from_elf(&elf)
            .and_then(EspImage::secure_pad_v2)
            .unwrap_or_else(|e| panic!("cannot create SBv2-padded ESP image: {e}"));
        let image_path = temp.path().join("ferriboot.esp.bin");
        std::fs::write(&image_path, esp_image.bytes()).expect("write padded ESP image");
        let runner = crate::artifacts::board_dir(&BOARD).join("tools/firmware.py");
        let mut cmd = Command::new("python3");
        cmd.arg(&runner)
            .arg("flash-ferriboot")
            .arg(&elf_path)
            .env("NO_FLASH", "1")
            .env("FERRIBOOT_ESP_IMAGE", &image_path)
            .env(
                "ESP_SBV2_KEY",
                crate::artifacts::board_dir(&BOARD).join("keys/dev-esp32c3-sbv2-rsa3072.pem"),
            );
        let status = crate::artifacts::run_captured(cmd, "sign")
            .unwrap_or_else(|error| panic!("cannot run {}: {error}", runner.display()));
        assert!(status.success(), "signing ESP32-C3 boot chain failed");
        let image = std::fs::read(temp.path().join("ferriboot.sbv2.bin"))
            .expect("missing signed factory Ferriboot image");
        let idf_image = std::fs::read(temp.path().join("bootloader.idf.sbv2.bin"))
            .expect("missing signed ESP-IDF bootloader image");
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
