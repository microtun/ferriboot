//! Overwrite-only upgrades (`MCUBOOT_OVERWRITE_ONLY`, matching the
//! `MCUBOOT_OVERWRITE_ONLY_FAST` variant: only the sectors holding the image
//! and the trailer are erased, and the primary trailer magic is written after
//! the copy).
//!
//! Power-fail safety comes from the order of operations: the secondary
//! trailer that requests the upgrade is erased last, so an interrupted copy is
//! simply restarted on the next boot.

use embedded_storage::nor_flash::NorFlash;

use crate::{
    Error,
    boot::{BootDecision, BootLoader, CandidateKind, ImageValidator, Slot},
    flash::{self, align_up},
    trailer::{SwapType, swap_type_from_states},
};

impl<F, C, R> BootLoader<'_, F, C, R>
where
    F: NorFlash,
    C: crate::boot::CounterHandle,
    R: crate::boot::EntropyHandle,
{
    pub(crate) fn boot_overwrite(
        &mut self,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<BootDecision, Error<F::Error>> {
        let primary = self.state(Slot::Primary)?;
        let secondary = self.state(Slot::Secondary)?;
        let mut performed = SwapType::None;

        let mut swap_type = swap_type_from_states(&primary, &secondary);
        let mut image = None;
        if swap_type.is_upgrade() {
            image = self.check_upgrade_candidate(CandidateKind::Upgrade, validator)?;
        } else if self.config.bootstrap && self.primary_rejection_confirmed(validator)? {
            // `MCUBOOT_BOOTSTRAP`: install the secondary image into an empty
            // or broken primary slot, reported as REVERT like upstream.
            image = self.bootstrap_candidate(validator)?;
            if image.is_some() {
                swap_type = SwapType::Revert;
            }
        }

        if let Some(img) = image {
            self.overwrite_primary(img.size())?;
            performed = swap_type;
        }
        self.validate_primary_for_boot(performed, validator)
    }

    /// `boot_copy_image`. Also used by swap mode for bootstrapping.
    pub(crate) fn overwrite_primary(&mut self, size: u32) -> Result<(), Error<F::Error>> {
        let es = F::ERASE_SIZE as u32;
        let len = align_up(size, es);
        let t = self.slot_trailer;
        info!(
            "overwriting primary slot with {:#x} bytes from secondary",
            size
        );

        t.scramble(&mut self.primary)?;
        flash::erase(&mut self.primary, 0, len)?;
        flash::copy(&mut self.secondary, 0, &mut self.primary, 0, len)?;
        // Like `MCUBOOT_OVERWRITE_ONLY_FAST` (and the full-slot copy, which
        // carries the secondary trailer along), leave a good magic in the
        // primary trailer so `boot_set_confirmed` and image-state reporting
        // behave as upstream. A power loss before the secondary is erased
        // simply repeats the overwrite.
        t.write_magic(&mut self.primary)?;

        // Remove the header first: if power is lost before the trailer is
        // erased, the next boot sees a trailer without an image and cleans it.
        flash::erase(&mut self.secondary, 0, es)?;
        t.scramble(&mut self.secondary)
    }
}
