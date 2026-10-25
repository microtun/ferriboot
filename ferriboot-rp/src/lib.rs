#![no_std]
// `unsafe` is confined to the modules marked `#[allow(unsafe_code)]` below,
// so a new unsafe block anywhere else fails the build and gets reviewed.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
//! RP2350 support shared by Ferriboot bootloaders and applications, in Arm
//! Secure mode. No network transport or async executor is required by this
//! crate.
//!
//! Nothing here is board-specific: the chip's facts are in [`chip`] (from
//! `chips/rp2350`), and a board passes its [`FlashMap`] through
//! [`Layout::new`], which checks it against the chip's rules.

use core::cell::RefCell;

use embedded_storage::nor_flash::NorFlash;
pub use ferriboot_core::layout::{FlashBounds, FlashMap, LayoutError, Region, SlotLayout};
use ferriboot_core::{BootConfig, BootLoader, FirmwareUpdater, PublicKey, Slot};

pub mod boot;
pub mod boot_info;
pub mod glitch;
pub use boot::{BootError, ConfirmedBoot, PreparedBoot, prepare_with_security_counter_store};
pub use boot_info::BootInfo;
#[allow(unsafe_code)] // The one call into the ROM's OTP routine.
pub mod otp;
mod policy;
#[allow(unsafe_code)] // `OtpSecurityCounter::new` marks irreversible OTP burns.
pub mod security_counter;
pub use policy::{Policy, PolicyExt};

pub mod chip;
pub use chip::*;
mod layout;
pub use layout::Layout;
mod flash;
pub use flash::{FlashError, Partition, WordFlash};
pub mod vector;
pub mod xip;

#[allow(unsafe_code)] // Handoff RAM, core registers and the final jump.
pub mod hardware;
pub use hardware::FlashStorage;
#[cfg(feature = "uart-log")]
pub mod uart_logger;

pub type SharedFlash<F> = RefCell<F>;

pub fn share<F>(flash: F) -> SharedFlash<F> {
    RefCell::new(flash)
}

/// The board's bounds, after checking that the flash driver has the size the
/// board's layout was checked for. Both image slots must (still) fit the
/// common ATRANS1 execution window.
fn checked_bounds<F: NorFlash>(
    layout: &Layout,
    flash: &SharedFlash<F>,
) -> Result<FlashBounds, LayoutError> {
    if flash.borrow().capacity() != layout.flash_size() {
        return Err(LayoutError::OutOfBounds);
    }
    let bounds = layout.bounds();
    for slot in [Slot::Primary, Slot::Secondary] {
        xip::atrans1(bounds, layout.region(slot))?;
    }
    Ok(bounds)
}

/// Construct the shared boot algorithm over checked, isolated partitions.
/// Both physical slots execute at `APP_XIP_BASE` for ROM_FIXED checking.
pub fn bootloader<'a, 'k, F: NorFlash>(
    layout: &Layout,
    flash: &'a SharedFlash<F>,
    keys: &'k [PublicKey],
    mut config: BootConfig,
) -> Result<BootLoader<'k, Partition<'a, F>>, LayoutError> {
    let bounds = checked_bounds(layout, flash)?;
    config.slot_addresses = Some([APP_XIP_BASE; 2]);
    layout
        .slots()
        .bootloader(bounds, keys, config, |r| Partition::new(flash, r))
}

/// Construct the application updater for this boot's handoff. The upgrade
/// mode comes from BootInfo; the bootloader enforces signature policy.
pub fn updater<'a, F: NorFlash>(
    layout: &Layout,
    info: &ferriboot_core::shared_data::BootInfo,
    flash: &'a SharedFlash<F>,
) -> Result<FirmwareUpdater<Partition<'a, F>>, LayoutError> {
    let bounds = checked_bounds(layout, flash)?;
    layout
        .slots()
        .updater(bounds, info, |r| Partition::new(flash, r))
}
