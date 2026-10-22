//! Shared fixtures for the runtime scenarios on a real ESP32-C3.

pub use ferriboot_core::Slot;
pub use ferriboot_hil::esp32c3::{
    artifacts::{self, Variant},
    dut::Dut,
    esp_image::EspImage,
    flash::{self, FlashImage, Request},
    hooks::{Behaviour, HookConfig},
    sign::{self, Key, SignOptions},
};
pub use ferriboot_hil::scenario::{BOOT, QUIET, behave, expect_running, field, version};

/// The HIL fixture (always with test hooks) as an ESP image.
pub fn payload() -> EspImage {
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

/// Erased slots, both signed boot stages, the board partition table and a hook
/// record in which every image confirms itself and idles.
pub fn blank(variant: Variant) -> FlashImage {
    let flash = flash::compose(artifacts::bootloader(variant)).unwrap();
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

// ----- malformed ESP payloads ---------------------------------------------------

use ferriboot_hil::esp32c3::layout;

// ESP32-C3 address ranges used to construct malformed test images. The
// on-device loader remains responsible for validating every candidate.
const IRAM: (u32, u32) = (0x4038_0000, 0x403E_0000);
const IRAM_DRAM_OFFSET: u32 = layout::IRAM_DRAM_OFFSET;
// Start of the protected loader from the board layout crate, as used by the
// bootloader's linker script and loader.
const PROTECTED: u32 = layout::PROTECTED_START;

/// An ESP image the bootloader's platform validator must refuse.
pub struct Malformed {
    pub name: &'static str,
    pub image: EspImage,
}

/// One smoke mutation targeting the ESP protected loader. Re-signing isolates
/// the executable-format check from signature validation.
pub fn smoke_malformed(img: &EspImage) -> Malformed {
    let iram = img.segment_in(IRAM);
    Malformed {
        name: "iram-over-protected-loader",
        image: img
            .clone()
            .set_segment_vaddr(iram, PROTECTED + IRAM_DRAM_OFFSET),
    }
}

// Console differences belong here; the scenarios are shared with RP2350.
pub const PLATFORM_REJECTED: &str = "rejecting application executable";
pub const NO_EXECUTABLE: &str = "unable to find a bootable image";
pub fn max_image_size(flash: &FlashImage) -> String {
    flash
        .layout
        .max_image_size::<ferriboot_hil::flash::Partition>()
        .unwrap()
        .to_string()
}
pub const STARTING_IMAGE: &str = "jumping to";
pub fn secondary_start(flash: &FlashImage) -> String {
    format!(
        "ESP image at {:#x}",
        flash.layout.secondary.offset + sign::HEADER_SIZE
    )
}
pub const NORMAL_FIRMWARE: &[&str] = &[
    "running image 1.0.0+0 in Primary, mode SwapScratch",
    "application healthy; image confirmed; USB-Serial-JTAG log console ready",
];
pub fn wrong_rom_fixed(flash: &FlashImage) -> u32 {
    flash.layout.primary.offset
}

pub fn assert_watchdog_reset(dut: &Dut, from: usize) {
    // esp-hal's ESP32-C3 reset reasons distinguish digital-core and full
    // system RTC watchdog resets. Neither software nor probe resets qualify.
    let (_, hello) = dut.expect_from(from, "@fbt hello", BOOT).unwrap();
    assert!(
        matches!(
            field(&hello, "reset"),
            Some("Some(CoreRtcWdt)" | "Some(SysRtcWdt)")
        ),
        "expected RTC watchdog reset: {hello}"
    );
}
