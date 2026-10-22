//! Swap using a scratch area (`swap_scratch.c`).
//!
//! The image area is swapped from the last sector towards the first, in
//! steps of up to `scratch_sectors - 1` sectors. Each step has three states,
//! and each completed state is recorded in the primary slot's status area:
//!
//! 1. erase scratch, copy secondary -> scratch
//! 2. erase secondary chunk, copy primary -> secondary
//! 3. erase primary chunk, copy scratch -> primary
//!
//! Every state only reads from areas that are not modified until a later
//! state has been recorded, so after a power loss the interrupted state is
//! simply executed again.
//!
//! Differences from MCUboot: slots must have equal size and uniform sectors,
//! and images may not extend into the sectors that hold the trailer (the
//! "use_scratch" special case for the trailer sector is therefore never
//! needed). The last scratch sector is reserved for the scratch trailer, so
//! image data copied into scratch can never be mistaken for a trailer magic.

use embedded_storage::nor_flash::NorFlash;

use crate::{
    Error, ImageError,
    boot::{BootDecision, BootLoader, CandidateKind, ImageValidator, Slot},
    flash,
    image::image_size,
    trailer::{Flag, MagicState, SwapState, SwapType},
};

const IDX_0: u32 = 1;
// Numeric values remain MCUboot's on-flash status encoding.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum SwapPhase {
    SaveSecondary = 1,
    MovePrimary = 2,
    RestoreSecondary = 3,
}

impl SwapPhase {
    fn from_number(value: u8) -> Self {
        match value {
            1 => Self::SaveSecondary,
            2 => Self::MovePrimary,
            3 => Self::RestoreSecondary,
            _ => unreachable!("Trailer::read_status returns phases 1..=3"),
        }
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusSource {
    None,
    Primary,
    Scratch,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BootStatus {
    /// 1-based index of the swap step to execute next.
    idx: u32,
    /// Next copy phase within that step.
    phase: SwapPhase,
    swap_type: SwapType,
    swap_size: u32,
    source: StatusSource,
}

impl BootStatus {
    fn reset() -> Self {
        Self {
            idx: IDX_0,
            phase: SwapPhase::SaveSecondary,
            swap_type: SwapType::None,
            swap_size: 0,
            source: StatusSource::None,
        }
    }

    fn is_reset(&self) -> bool {
        self.idx == IDX_0 && self.phase == SwapPhase::SaveSecondary
    }
}

/// `boot_status_tables`: where does the status of an interrupted swap live?
fn status_source(primary: &SwapState, scratch: &SwapState) -> StatusSource {
    let scratch_good = scratch.magic == MagicState::Good;
    match primary.magic {
        MagicState::Good if !scratch_good && primary.copy_done == Flag::Set => StatusSource::None,
        MagicState::Good if !scratch_good && primary.copy_done == Flag::Unset => {
            StatusSource::Primary
        }
        _ if scratch_good => StatusSource::Scratch,
        MagicState::Unset if primary.copy_done == Flag::Unset => StatusSource::Primary,
        _ => StatusSource::None,
    }
}

#[derive(Clone, Copy)]
enum Area {
    Primary,
    Scratch,
}

impl<F, C, R> BootLoader<'_, F, C, R>
where
    F: NorFlash,
    C: crate::boot::CounterHandle,
    R: crate::boot::EntropyHandle,
{
    pub(crate) fn boot_swap(
        &mut self,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<BootDecision, Error<F::Error>> {
        let mut bs = self.read_swap_status()?;
        let mut performed = SwapType::None;

        if !bs.is_reset() {
            info!(
                "resuming interrupted {:?} swap at step {} phase {:?}",
                bs.swap_type, bs.idx, bs.phase
            );
            self.swap_image(&mut bs)?;
            self.finish_swap(bs.swap_type)?;
            performed = bs.swap_type;
        } else {
            let mut swap_type = if bs.swap_type == SwapType::None {
                self.validated_swap_type(validator)?
            } else if self
                .check_upgrade_candidate(CandidateKind::for_swap(bs.swap_type), validator)?
                .is_some()
            {
                // A swap was set up but no step had completed yet.
                bs.swap_type
            } else {
                SwapType::Fail
            };

            // `MCUBOOT_BOOTSTRAP`: the primary slot is checked once, both to
            // install the secondary image into an empty or broken primary
            // slot and to decide below whether to overwrite instead of swap.
            // Both outcomes destroy the primary image, so the rejection is
            // confirmed independently.
            let primary_invalid = self.config.bootstrap
                && swap_type != SwapType::Fail
                && self.primary_rejection_confirmed(validator)?;
            if primary_invalid
                && swap_type == SwapType::None
                && self.bootstrap_candidate(validator)?.is_some()
            {
                // Upstream uses REVERT so that image_ok is set afterwards.
                swap_type = SwapType::Revert;
            }
            info!("swap type: {:?}", swap_type);

            match swap_type {
                SwapType::Test | SwapType::Perm | SwapType::Revert => {
                    if primary_invalid {
                        // `boot_perform_update`: with bootstrapping enabled,
                        // an invalid primary is overwritten, not swapped.
                        warn!("primary slot is not valid; copying secondary slot over it");
                        let max = self.slot_trailer.max_image_size();
                        let size = self.slot_image_size(Slot::Secondary)?.unwrap_or(max);
                        self.overwrite_primary(size)?;
                    } else {
                        bs.swap_type = swap_type;
                        self.swap_image(&mut bs)?;
                    }
                    self.finish_swap(swap_type)?;
                    performed = swap_type;
                }
                SwapType::Fail => {
                    // Pretend we reverted so the bad image is not retried.
                    self.set_primary_image_ok()?;
                }
                _ => {}
            }
        }

        self.validate_primary_for_boot(performed, validator)
    }

    /// `boot_validated_swap_type`.
    fn validated_swap_type(
        &mut self,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<SwapType, Error<F::Error>> {
        let primary = self.state(Slot::Primary)?;
        let secondary = self.state(Slot::Secondary)?;
        debug!("primary trailer: {:?}", primary);
        debug!("secondary trailer: {:?}", secondary);
        let swap_type = crate::trailer::swap_type_from_states(&primary, &secondary);
        if swap_type.is_upgrade()
            && self
                .check_upgrade_candidate(CandidateKind::for_swap(swap_type), validator)?
                .is_none()
        {
            return Ok(SwapType::None);
        }
        Ok(swap_type)
    }

    /// After a completed swap: confirm reverts and permanent upgrades, and
    /// mark the copy as done.
    fn finish_swap(&mut self, swap_type: SwapType) -> Result<(), Error<F::Error>> {
        if matches!(swap_type, SwapType::Revert | SwapType::Perm) {
            self.set_primary_image_ok()?;
        }
        if swap_type.is_upgrade() {
            let t = self.slot_trailer;
            if t.read_state(&mut self.primary)?.copy_done == Flag::Unset {
                t.write_copy_done(&mut self.primary)?;
            }
        }
        Ok(())
    }

    fn set_primary_image_ok(&mut self) -> Result<(), Error<F::Error>> {
        let t = self.slot_trailer;
        if t.read_state(&mut self.primary)?.image_ok == Flag::Unset {
            t.write_image_ok(&mut self.primary)?;
        }
        Ok(())
    }

    fn scratch_parts(&mut self) -> (&mut F, crate::trailer::Trailer) {
        (
            self.scratch
                .as_mut()
                .expect("scratch checked in BootLoader::new"),
            self.scratch_trailer
                .expect("scratch checked in BootLoader::new"),
        )
    }

    fn area(&mut self, area: Area) -> (&mut F, crate::trailer::Trailer) {
        match area {
            Area::Primary => (&mut self.primary, self.slot_trailer),
            Area::Scratch => self.scratch_parts(),
        }
    }

    /// `swap_read_status`.
    fn read_swap_status(&mut self) -> Result<BootStatus, Error<F::Error>> {
        let primary = self.state(Slot::Primary)?;
        let (scratch, st) = self.scratch_parts();
        let scratch_state = st.read_state(scratch)?;

        let mut bs = BootStatus::reset();
        bs.source = status_source(&primary, &scratch_state);
        let (area, state) = match bs.source {
            StatusSource::None => return Ok(bs),
            StatusSource::Primary => (Area::Primary, primary),
            StatusSource::Scratch => (Area::Scratch, scratch_state),
        };
        debug!("swap status source: {:?}", bs.source);

        let (f, t) = self.area(area);
        if let Some((idx, state)) = t.read_status(f)? {
            bs.idx = idx;
            bs.phase = SwapPhase::from_number(state);
        }
        bs.swap_type = state.swap_type;
        Ok(bs)
    }

    fn slot_image_size(&mut self, slot: Slot) -> Result<Option<u32>, Error<F::Error>> {
        let max = self.slot_trailer.max_image_size();
        match image_size(self.slot(slot), max) {
            Ok(size) => Ok(size),
            // A header with broken TLVs: swap the whole usable area.
            Err(Error::Image(_)) => Ok(Some(max)),
            Err(e) => Err(e),
        }
    }

    /// `boot_swap_image`.
    fn swap_image(&mut self, bs: &mut BootStatus) -> Result<(), Error<F::Error>> {
        let max = self.slot_trailer.max_image_size();
        if bs.is_reset() {
            let a = self.slot_image_size(Slot::Primary)?.unwrap_or(0);
            let b = self.slot_image_size(Slot::Secondary)?.unwrap_or(0);
            bs.swap_size = a.max(b);
        } else {
            let area = match bs.source {
                StatusSource::Scratch => Area::Scratch,
                _ => Area::Primary,
            };
            let (f, t) = self.area(area);
            let size = t.read_swap_size(f)?;
            bs.swap_size = if size == 0 || size > max {
                warn!("implausible swap size {:#x}; swapping whole slot", size);
                max
            } else {
                size
            };
        }
        info!(
            "swapping {:#x} bytes ({:?}, step {} phase {:?})",
            bs.swap_size, bs.swap_type, bs.idx, bs.phase
        );
        self.swap_run(bs)
    }

    fn chunk_sectors(&self) -> u32 {
        let es = F::ERASE_SIZE as u32;
        // Construction guarantees at least two sectors: metadata never
        // overlaps signed image bytes, even at the minimum geometry.
        self.scratch_trailer.expect("scratch mode").area_size() / es - 1
    }

    /// `swap_run`: iterate the steps from the end of the image to its start,
    /// skipping steps that already completed.
    fn swap_run(&mut self, bs: &mut BootStatus) -> Result<(), Error<F::Error>> {
        if bs.swap_size == 0 {
            return Ok(());
        }
        let es = F::ERASE_SIZE as u32;
        if bs.swap_size > self.slot_trailer.max_image_size() {
            return Err(ImageError::ImageTooLarge.into());
        }
        let chunk = self.chunk_sectors();
        let mut last = bs.swap_size.div_ceil(es) - 1;
        let mut step = IDX_0;
        loop {
            let first = (last + 1).saturating_sub(chunk);
            if step >= bs.idx {
                self.swap_sectors(first, (last - first + 1) * es, bs)?;
            }
            if first == 0 {
                return Ok(());
            }
            last = first - 1;
            step += 1;
        }
    }

    /// `swap_status_init`.
    fn status_init(&mut self, area: Area, bs: &BootStatus) -> Result<(), Error<F::Error>> {
        let secondary = self.state(Slot::Secondary)?;
        let (f, t) = self.area(area);
        if bs.swap_type != SwapType::None {
            t.write_swap_info(f, bs.swap_type, 0)?;
        }
        if secondary.image_ok == Flag::Set {
            t.write_image_ok(f)?;
        }
        t.write_swap_size(f, bs.swap_size)?;
        t.write_magic(f)
    }

    fn erase_scratch(&mut self) -> Result<(), Error<F::Error>> {
        let (f, t) = self.scratch_parts();
        flash::erase(f, 0, t.area_size())
    }

    /// `boot_swap_sectors`.
    fn swap_sectors(
        &mut self,
        first_sector: u32,
        sz: u32,
        bs: &mut BootStatus,
    ) -> Result<(), Error<F::Error>> {
        let off = first_sector * F::ERASE_SIZE as u32;
        let t = self.slot_trailer;
        debug!("swap step {}: offset {:#x}, {:#x} bytes", bs.idx, off, sz);

        if bs.phase == SwapPhase::SaveSecondary {
            self.erase_scratch()?;
            if bs.idx == IDX_0 {
                // Park the swap type in scratch while the primary trailer is
                // being re-initialised.
                self.status_init(Area::Scratch, bs)?;
                t.scramble(&mut self.primary)?;
                self.status_init(Area::Primary, bs)?;
                self.erase_scratch()?;
            }
            let scratch = self.scratch.as_mut().expect("scratch");
            flash::copy(&mut self.secondary, off, scratch, 0, sz)?;
            t.write_status(&mut self.primary, bs.idx, bs.phase as u8)?;
            bs.phase = SwapPhase::MovePrimary;
        }

        if bs.phase == SwapPhase::MovePrimary {
            if bs.idx == IDX_0 {
                // Only the primary slot may carry the status from now on.
                t.scramble(&mut self.secondary)?;
            }
            flash::erase(&mut self.secondary, off, off + sz)?;
            flash::copy(&mut self.primary, off, &mut self.secondary, off, sz)?;
            t.write_status(&mut self.primary, bs.idx, bs.phase as u8)?;
            bs.phase = SwapPhase::RestoreSecondary;
        }

        if bs.phase == SwapPhase::RestoreSecondary {
            flash::erase(&mut self.primary, off, off + sz)?;
            let scratch = self.scratch.as_mut().expect("scratch");
            flash::copy(scratch, 0, &mut self.primary, off, sz)?;
            t.write_status(&mut self.primary, bs.idx, bs.phase as u8)?;
            bs.idx += 1;
            bs.phase = SwapPhase::SaveSecondary;
        }
        Ok(())
    }
}
