#![no_std]
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

#[macro_use]
mod fmt;

mod direct_xip;
mod error;
mod overwrite;
mod swap_scratch;

pub mod boot;
pub mod fih;
pub mod flash;
pub mod image;
pub mod layout;
pub mod security_counter;
pub use security_counter::{SecurityCounterError, SecurityCounterStore, counter_permits};
pub mod shared_data;
pub mod trailer;
pub mod updater;
pub mod upload;
pub mod validate;

pub use boot::{BootConfig, BootDecision, BootLoader, ImageValidator, Slot, UpgradeMode};
pub use error::{ConfigError, Error, ImageError};
pub use fih::Fih;
pub use image::{ImageHeader, ImageVersion};
pub use layout::{FlashBounds, FlashMap, LayoutError, Region, SlotLayout};
pub use trailer::{Flag, MagicState, SwapState, SwapType};
pub use updater::{FirmwareUpdater, ImageSlots, UpdateState};
pub use validate::{
    Policy, PublicKey, SecurityConfigFingerprint, ValidatedImage, reauthenticate,
    security_config_fingerprint,
};

/// The [`UpgradeMode`] selected by the *calling* crate's `overwrite-only`,
/// `direct-xip` and `direct-xip-revert` Cargo features (see
/// [`UpgradeMode::from_features`]). Board bootloader crates can define
/// their mode as
///
/// ```ignore
/// pub const MODE: ferriboot_core::UpgradeMode = ferriboot_core::upgrade_mode_from_features!();
/// ```
///
/// Applications learn the selected mode from the bootloader handoff instead of
/// compiling the same choice independently. Selecting more than one strategy
/// in a calling crate fails to compile.
#[macro_export]
macro_rules! upgrade_mode_from_features {
    () => {
        $crate::UpgradeMode::from_features(
            cfg!(feature = "overwrite-only"),
            cfg!(feature = "direct-xip"),
            cfg!(feature = "direct-xip-revert"),
        )
    };
}
