//! Shared fixtures for the runtime scenarios on the QEMU virtual board.

pub use ferriboot_core::Slot;
pub use ferriboot_hil::qemu::{
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

/// Erased slots, `variant`'s bootloader, the NOR fixture layout and a hook
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
use ferriboot_hil::qemu::layout::{APP_XIP_BASE, BOOT_INFO_ADDRESS, HEADER_SIZE, RAM_BASE};

/// An Arm payload the bootloader's vector-table check must refuse.
pub struct Malformed {
    pub name: &'static str,
    pub image: ArmImage,
}
/// Every case is re-signed: these isolate platform rejection from authentication.
pub fn malformed(img: &ArmImage) -> Vec<Malformed> {
    let vector = APP_XIP_BASE + HEADER_SIZE;
    vec![
        Malformed {
            name: "stack-over-boot-info",
            image: img.clone().set_stack(BOOT_INFO_ADDRESS + 8),
        },
        Malformed {
            name: "stack-below-ram",
            image: img.clone().set_stack(RAM_BASE - 8),
        },
        Malformed {
            name: "stack-at-ram-base",
            image: img.clone().set_stack(RAM_BASE),
        },
        Malformed {
            name: "stack-unaligned",
            image: img.clone().set_stack(img.stack() - 1),
        },
        Malformed {
            name: "reset-not-thumb",
            image: img.clone().set_reset(img.reset() & !1),
        },
        Malformed {
            name: "reset-in-header",
            image: img.clone().set_reset(APP_XIP_BASE | 1),
        },
        Malformed {
            name: "reset-in-vectors",
            image: img.clone().set_reset(vector | 1),
        },
        Malformed {
            name: "reset-past-payload",
            image: img
                .clone()
                .set_reset((vector + img.bytes().len() as u32) | 1),
        },
        Malformed {
            name: "reset-in-sram",
            image: img.clone().set_reset(RAM_BASE | 1),
        },
        Malformed {
            name: "truncated-vectors",
            image: img.clone().truncate(4),
        },
    ]
}

// Console differences belong here; all scenario bodies are shared.
pub const PLATFORM_REJECTED: &str = "PlatformRejected";
pub const NO_EXECUTABLE: &str = "no executable application";
pub const MAX_IMAGE_SIZE: &str = "1892352";
pub const STARTING_IMAGE: &str = "starting image";
pub const SECONDARY_START: &str = "from Secondary";
pub const NORMAL_FIRMWARE: &[&str] = &[
    "running image 1.0.0+0 in Primary, mode SwapScratch",
    "application healthy; image confirmed; virtual console ready",
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
