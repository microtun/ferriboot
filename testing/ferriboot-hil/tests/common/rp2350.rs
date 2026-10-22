//! Shared fixtures for the runtime scenarios on a real RP2350.

pub use ferriboot_core::Slot;
pub use ferriboot_hil::rp2350::{
    artifacts::{self, Variant},
    dut::Dut,
    flash::{self, FlashImage, Request},
    hooks::{Behaviour, HookConfig},
    image::ArmImage,
    sign::{self, Key, SignOptions},
};
pub use ferriboot_hil::scenario::{BOOT, QUIET, behave, expect_running, field, version};

/// The HIL fixture (always with test hooks) as an Arm payload.
pub fn payload() -> ArmImage {
    artifacts::firmware_image().clone()
}

/// The firmware signed with the trusted key as `version` (e.g. "2.0.0").
pub fn signed(version: &str) -> Vec<u8> {
    sign::sign(
        artifacts::firmware_image().bytes(),
        &SignOptions::new(version),
    )
    .unwrap()
}

/// Erased slots, `variant`'s bootloader, the physical flash layout and a hook
/// record in which every image confirms itself and idles.
pub fn blank(variant: Variant) -> FlashImage {
    let flash = flash::compose(&artifacts::bootloader(variant).image).unwrap();
    flash.configure(&HookConfig::default());
    flash
}

/// A device with a good, confirmed 1.0.0 image in the primary slot.
pub fn factory(variant: Variant) -> FlashImage {
    let flash = blank(variant);
    flash
        .install(Slot::Primary, &signed("1.0.0"))
        .confirm_raw(Slot::Primary);
    flash
}

// ----- malformed Arm payloads -----------------------------------------------
use ferriboot_hil::rp2350::layout::{APP_XIP_BASE, BOOT_INFO_ADDRESS};

/// An Arm payload the bootloader's vector-table check must refuse.
pub struct Malformed {
    pub name: &'static str,
    pub image: ArmImage,
}
/// One smoke mutation targeting the RP2350 reserved handoff block. Re-signing
/// isolates vector rejection from signature validation.
pub fn smoke_malformed(img: &ArmImage) -> Malformed {
    Malformed {
        name: "stack-over-boot-info",
        image: img.clone().set_stack(BOOT_INFO_ADDRESS + 8),
    }
}

// Console differences belong here; the scenarios are shared with ESP32-C3.
pub const PLATFORM_REJECTED: &str = "PlatformRejected";
pub const NO_EXECUTABLE: &str = "no executable application";
pub const MAX_IMAGE_SIZE: &str = "1957888";
pub const STARTING_IMAGE: &str = "starting image";
pub const SECONDARY_START: &str = "from Secondary";
pub const NORMAL_FIRMWARE: &[&str] = &[
    "running image 1.0.0+0 in Primary, mode SwapScratch",
    "application healthy; image confirmed; UART log console ready",
];
pub fn wrong_rom_fixed(_: &FlashImage) -> u32 {
    APP_XIP_BASE + 0x1000
}

pub fn assert_watchdog_reset(dut: &Dut, from: usize) {
    let (_, hello) = dut.expect_from(from, "@fbt hello", BOOT).unwrap();
    assert_eq!(field(&hello, "reset"), Some("Some(TimedOut)"));
}

// Shared smoke assertions use the fixture's geometry on ESP32-C3.
pub fn max_image_size(_flash: &FlashImage) -> String {
    MAX_IMAGE_SIZE.to_owned()
}

pub fn secondary_start(_flash: &FlashImage) -> String {
    SECONDARY_START.to_owned()
}
