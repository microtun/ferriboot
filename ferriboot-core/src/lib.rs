#![no_std]
#![forbid(unsafe_code)]
//! Platform-independent core of Ferriboot, an unofficial Rust implementation of [MCUboot](https://github.com/mcu-tools/mcuboot).
//!
//! Everything in this crate is generic over [`embedded_storage::nor_flash::NorFlash`]
//! and contains no target-specific code, so it is unit tested on the host with a
//! simulated flash that injects power loss after every single erase/write.
//!
//! What is implemented, mirroring MCUboot's `bootutil`:
//!
//! * [`image`]: the MCUboot image header and TLV areas (protected and unprotected).
//! * [`validate`]: `bootutil_img_validate` for ECDSA P-256 images — SHA-256 over
//!   `header || body || protected TLVs`, key selection through `KEYHASH`/`PUBKEY`
//!   TLVs from a key ring, ECDSA P-256 prehash verification, a whitelist of TLVs that
//!   may appear outside the signed area, and policy checks (minimum version,
//!   security counter, imgtool `--vid/--cid` UUIDs).
//! * [`trailer`]: the image trailer (magic, `swap_info`, `copy_done`, `image_ok`,
//!   `swap_size`, swap status) with MCUboot's on-flash layout (`BOOT_MAX_ALIGN` = 8),
//!   so `imgtool sign --pad --confirm` images and the app-side API interoperate.
//! * [`BootLoader`]: the boot decision for three upgrade strategies
//!   ([`UpgradeMode`]): swap using scratch with test/permanent upgrades,
//!   revert and power-fail-safe resume; overwrite-only; and direct-XIP with
//!   optional revert. Downgrade prevention and bootstrapping are configurable.
//! * [`updater::FirmwareUpdater`]: the application side (`boot_set_pending`,
//!   `boot_set_confirmed`, lazily-erasing slot writer and optional non-cryptographic image-structure inspection).
//! * [`shared_data`]: upstream-compatible shared RAM boot-info TLVs with a
//!   bounded writer and checked decoder; platforms own the RAM reservation.
//! * [`layout`]: slot geometry and boot-handoff checks, and construction of
//!   the bootloader and updater over platform partitions.
//! * [`fih`]: fault-injection hardening. Irreversible steps (the jump to the
//!   application, security-counter burns) re-authenticate the image and
//!   consume the result through glitch-resistant checks.
//!
//! The image envelope parsing and policy types are adapted from
//! `microtun-mcuboot`.

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
