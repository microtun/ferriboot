//! Bootloader-local upgrade configuration and package version.

use ferriboot_core::{BootConfig, ImageVersion, UpgradeMode};
use ferriboot_rp::{Policy, PolicyExt as _};

pub const MODE: UpgradeMode = ferriboot_core::upgrade_mode_from_features!();

/// Bootloader settings selected by this binary's Cargo features.
pub fn boot_config() -> BootConfig {
    BootConfig {
        mode: MODE,
        downgrade_prevention: cfg!(feature = "downgrade-prevention"),
        bootstrap: cfg!(feature = "bootstrap"),
        policy: Policy::rp2350(),
        direct_xip_fail_closed: true,
        // ferriboot-rp supplies the common runtime address for both slots.
        slot_addresses: None,
    }
}

/// Bootloader version included in the application handoff.
pub fn version() -> ImageVersion {
    ImageVersion::new(
        env!("CARGO_PKG_VERSION_MAJOR")
            .parse()
            .expect("major version"),
        env!("CARGO_PKG_VERSION_MINOR")
            .parse()
            .expect("minor version"),
        env!("CARGO_PKG_VERSION_PATCH")
            .parse()
            .expect("patch version"),
        0,
    )
}
