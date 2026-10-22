//! The bootloader variants and the firmware, built once per test process by
//! [`crate::artifacts`] under `target/hil-rp2350/` and converted with the
//! board's checked ROM secure-boot signer ([`image`](super::image)).

use std::sync::OnceLock;

pub use crate::artifacts::{Bootloader, Variant, repo_root};

use super::{
    BOARD,
    image::{self, ArmImage},
};

/// The bootloader built with `variant`'s features and converted for flash.
pub fn bootloader(variant: Variant) -> &'static Bootloader {
    static CELLS: [OnceLock<Bootloader>; Variant::COUNT] =
        [const { OnceLock::new() }; Variant::COUNT];
    CELLS[variant as usize].get_or_init(|| {
        // Fail before compiling if the default development key or override is missing.
        image::rom_signing_key()
            .unwrap_or_else(|e| panic!("cannot select the RP2350 ROM signing key: {e}"));
        let elf = crate::artifacts::build_bootloader(&BOARD, variant);
        let image = image::seal_bootloader(&elf)
            .unwrap_or_else(|e| panic!("cannot ROM-sign the RP2350 HIL bootloader: {e}"));
        Bootloader { elf, image }
    })
}

/// The example firmware ELF with built-in test hooks.
pub fn firmware_elf() -> &'static [u8] {
    static ELF: OnceLock<Vec<u8>> = OnceLock::new();
    ELF.get_or_init(|| crate::artifacts::build_firmware(&BOARD))
}

/// The firmware as an Arm payload. Every application image in the suite is this
/// image, signed with different versions.
pub fn firmware_image() -> &'static ArmImage {
    static IMAGE: OnceLock<ArmImage> = OnceLock::new();
    IMAGE.get_or_init(|| {
        image::from_elf(firmware_elf(), false)
            .unwrap_or_else(|e| panic!("cannot convert the firmware: {e}"))
    })
}
