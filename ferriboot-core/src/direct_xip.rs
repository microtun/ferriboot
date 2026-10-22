//! Direct execute-in-place (`MCUBOOT_DIRECT_XIP`, `MCUBOOT_DIRECT_XIP_REVERT`).
//!
//! Both slots are execution slots. The valid image with the highest version
//! is booted (ties go to the primary slot). Images flagged `ROM_FIXED` are
//! only booted from the slot at their `load_addr` when slot addresses are
//! configured. With revert enabled, a slot is
//! only eligible if its trailer magic is set, `copy_done` is set the first
//! time an image is selected, and an image that was selected before but never
//! confirmed (`image_ok`) is erased.

use embedded_storage::nor_flash::NorFlash;

use crate::{
    Error,
    boot::{BootDecision, BootLoader, ImageValidator, Slot},
    trailer::{Flag, SwapType},
};

impl<F, C, R> BootLoader<'_, F, C, R>
where
    F: NorFlash,
    C: crate::boot::CounterHandle,
    R: crate::boot::EntropyHandle,
{
    pub(crate) fn boot_direct_xip(
        &mut self,
        revert: bool,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<BootDecision, Error<F::Error>> {
        let slots = [Slot::Primary, Slot::Secondary];
        let mut tried = [false; 2];

        loop {
            // find_slot_with_highest_version
            let mut best: Option<(usize, crate::ImageVersion)> = None;
            for (i, slot) in slots.iter().enumerate() {
                if tried[i] {
                    continue;
                }
                // `boot_get_slot_usage`: only headers that pass the cheap
                // structural checks are candidates; others are left alone.
                if let Some(h) = self.header(*slot)?
                    && h.check_capacity(self.slot(*slot).capacity() as u32).is_ok()
                    && best.is_none_or(|(_, v)| h.version.semver_cmp(&v).is_gt())
                {
                    best = Some((i, h.version));
                }
            }
            let Some((i, version)) = best else {
                error!("no bootable image found");
                return Err(Error::NoBootableImage);
            };
            tried[i] = true;
            let slot = slots[i];
            info!("candidate: {:?} slot, version {}", slot, version);

            // `boot_rom_address_check`: an image built for a fixed address
            // is skipped (not erased) in any other slot.
            if let (Some(addrs), Some(h)) = (self.config.slot_addresses, self.header(slot)?)
                && h.flags & crate::image::flags::ROM_FIXED != 0
                && h.load_addr != addrs[i]
            {
                warn!(
                    "{:?} slot at {:#x} holds an image built for {:#x}; skipping",
                    slot, addrs[i], h.load_addr
                );
                continue;
            }

            let state = self.state(slot)?;
            if revert && !state.xip_eligible() {
                warn!(
                    "{:?} slot image was not confirmed (or is not pending)",
                    slot
                );
                self.scramble_slot(slot)?;
                continue;
            }

            // Mark an unconfirmed trial before validation, so resetting during
            // validation consumes the trial. Confirmed images need no trial
            // marker and must not require an extra flash write to boot.
            if revert && state.copy_done != Flag::Set && state.image_ok != Flag::Set {
                let t = self.slot_trailer;
                if let Err(error) = t.write_copy_done(self.slot(slot)) {
                    if self.config.direct_xip_fail_closed {
                        return Err(error);
                    }
                    warn!("failed to set copy_done in {:?} slot", slot);
                } else if self.config.direct_xip_fail_closed
                    && self.state(slot)?.copy_done != Flag::Set
                {
                    // Flash drivers can acknowledge writes they did not persist.
                    // Never execute an unconfirmed trial without observing the
                    // consumed marker on flash.
                    error!("copy_done did not persist in {:?} slot", slot);
                    return Err(Error::TrialMarkerNotPersisted);
                }
            }

            match self.validate_for_boot(slot, slot, validator) {
                Ok(image) => {
                    info!(
                        "booting {:?} slot, version {}",
                        slot,
                        image.header().version
                    );
                    return Ok(BootDecision {
                        slot,
                        image,
                        performed: SwapType::None,
                    });
                }
                Err(Error::Image(e)) => {
                    error!("image in {:?} slot is not valid: {:?}", slot, e);
                    self.scramble_slot(slot)?;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
