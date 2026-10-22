//! Application side of an upgrade (`bootutil_public.c`).
//!
//! Typical OTA flow for the swap and overwrite modes (`active` = primary,
//! `update` = secondary):
//!
//! ```ignore
//! let mut updater = FirmwareUpdater::new(primary, secondary)?;
//! let mut w = updater.begin_update()?;          // drops any stale request
//! while let Some(chunk) = transport.next()? {
//!     w.write(chunk)?;                          // erases sectors lazily
//! }
//! w.finish()?;
//! updater.inspect_update()?;                   // optional, non-cryptographic format check
//! updater.mark_for_test()?;                    // bootloader authenticates on reboot
//! reset();
//! // ... in the new image, once it is known to work:
//! updater.confirm()?;
//! ```
//!
//! In direct-XIP mode `active` is whichever slot the app runs from and
//! `update` is the other one. Use [`FirmwareUpdater::new_with_mode`] to select
//! the matching trailer transitions; `new` preserves scratch-swap semantics.
//!
//! No signing keys are needed in an application. Header inspection is not
//! authentication: the bootloader is solely responsible for signature and
//! rollback-policy enforcement before installing or booting a candidate.

use embedded_storage::nor_flash::NorFlash;

use crate::{
    ConfigError, Error, ImageError, ImageHeader, Slot, UpgradeMode,
    flash::{self, Aligned, ERASED, align_up},
    image::{TLV_INFO_SIZE, TlvIter},
    trailer::{Flag, MagicState, SwapState, SwapType, Trailer},
};

/// Untrusted image-header metadata and physical slot identity used for
/// advisory update-state queries. These headers do NOT prove authenticity.
#[derive(Clone, Copy, Debug)]
pub struct ImageSlots {
    pub active: ImageHeader,
    pub inactive: Option<ImageHeader>,
    pub active_slot: Slot,
}

impl ImageSlots {
    pub const fn new(
        active: ImageHeader,
        inactive: Option<ImageHeader>,
        active_slot: Slot,
    ) -> Self {
        Self {
            active,
            inactive,
            active_slot,
        }
    }
}

/// Application-visible trailer state, derived by the upgrade strategy once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateState {
    pub active_confirmed: bool,
    pub inactive_confirmed: bool,
    pub pending: bool,
    pub in_use: bool,
    pub permanent: bool,
}

impl UpgradeMode {
    /// Whether the update slot holds the fallback of an unconfirmed trial,
    /// judged from the running image's trailer alone (no headers needed).
    ///
    /// In swap mode the previous image waits in the update slot for a revert.
    /// In direct-XIP with revert the bootloader erases an unconfirmed trial on
    /// the next boot and runs the other slot. Replacing that slot loses the
    /// rollback (swap) or leaves nothing bootable (direct-XIP).
    fn fallback_in_use(self, active: SwapState) -> bool {
        let unconfirmed_trial = active.magic == MagicState::Good
            && active.copy_done == Flag::Set
            && active.image_ok != Flag::Set;
        match self {
            Self::SwapScratch | Self::DirectXip { revert: true } => unconfirmed_trial,
            Self::Overwrite | Self::DirectXip { revert: false } => false,
        }
    }

    fn requires_confirmation(self) -> bool {
        !matches!(self, Self::DirectXip { revert: false })
    }

    fn inactive_is_confirmed(self, state: SwapState) -> bool {
        match self {
            Self::DirectXip { revert: true } => {
                state.magic == MagicState::Good && state.image_ok == Flag::Set
            }
            Self::DirectXip { revert: false } => true,
            Self::SwapScratch | Self::Overwrite => false,
        }
    }

    fn image_state(
        self,
        active: SwapState,
        inactive: SwapState,
        images: &ImageSlots,
    ) -> UpdateState {
        let (pending, permanent_mode) = match self {
            Self::SwapScratch => (inactive.magic == MagicState::Good, false),
            Self::Overwrite => (inactive.magic == MagicState::Good, true),
            Self::DirectXip { revert } => {
                let pending = images.inactive.is_some_and(|image| {
                    if revert && !inactive.xip_eligible() {
                        return false;
                    }
                    if revert && !active.xip_eligible() {
                        return true;
                    }
                    let cmp = image.version.semver_cmp(&images.active.version);
                    cmp.is_gt() || (cmp.is_eq() && images.active_slot == Slot::Secondary)
                });
                (pending, !revert)
            }
        };
        let rollback = self.fallback_in_use(active);
        UpdateState {
            active_confirmed: !(self.requires_confirmation()
                && active.magic == MagicState::Good
                && active.image_ok == Flag::Unset),
            inactive_confirmed: self.inactive_is_confirmed(inactive),
            pending,
            in_use: pending || rollback,
            permanent: pending
                && (permanent_mode
                    || (inactive.magic == MagicState::Good
                        && (inactive.image_ok == Flag::Set
                            || inactive.swap_type == SwapType::Perm))),
        }
    }
}

#[derive(Clone, Copy)]
enum FlagRequest {
    Confirm,
    Pending { permanent: bool },
}

pub struct FirmwareUpdater<F: NorFlash> {
    active: F,
    update: F,
    trailer: Trailer,
    mode: UpgradeMode,
}

impl<F: NorFlash> FirmwareUpdater<F> {
    /// Scratch-swap constructor. Other modes must use [`Self::new_with_mode`].
    pub fn new(active: F, update: F) -> Result<Self, ConfigError> {
        Self::new_with_mode(active, update, UpgradeMode::SwapScratch)
    }

    /// Construct an application-side updater without any trusted signing keys.
    pub fn new_with_mode(active: F, update: F, mode: UpgradeMode) -> Result<Self, ConfigError> {
        flash::check_geometry::<F>()?;
        let size = active.capacity() as u32;
        if update.capacity() as u32 != size {
            return Err(ConfigError::SlotSizeMismatch);
        }
        Ok(Self {
            active,
            update,
            trailer: Trailer::for_slot::<F>(size)?,
            mode,
        })
    }

    /// Largest signed image (header + body + TLVs) that can be installed.
    pub fn max_image_size(&self) -> u32 {
        self.trailer.max_image_size()
    }

    pub fn slot_size(&self) -> u32 {
        self.active.capacity() as u32
    }

    /// Read a running image header and check its structure and TLV bounds.
    /// This does NOT authenticate the image or enforce rollback policy.
    pub fn inspect_active(&mut self) -> Result<ImageHeader, Error<F::Error>> {
        let limit = self.max_image_size();
        Self::inspect(&mut self.active, limit)
    }

    /// Read the update-slot header and check basic format and TLV bounds.
    /// The bootloader must authenticate this image before execution.
    pub fn inspect_update(&mut self) -> Result<ImageHeader, Error<F::Error>> {
        let limit = self.max_image_size();
        Self::inspect(&mut self.update, limit)
    }

    /// Reject an upload whose declared image/TLV bounds extend past the
    /// received bytes. Trailing upload padding is permitted.
    pub fn inspect_update_received(
        &mut self,
        received: u32,
    ) -> Result<ImageHeader, Error<F::Error>> {
        let limit = received.min(self.max_image_size());
        Self::inspect(&mut self.update, limit)
    }

    fn inspect(flash: &mut F, limit: u32) -> Result<ImageHeader, Error<F::Error>> {
        let header = ImageHeader::read(flash)?.ok_or(ImageError::NoImage)?;
        header.check()?;
        if header
            .hashed_len()
            .and_then(|n| n.checked_add(TLV_INFO_SIZE))
            .is_none_or(|n| n > limit)
        {
            return Err(ImageError::ImageTooLarge.into());
        }
        let mut tlvs = TlvIter::begin(flash, &header, limit)?;
        while tlvs.next(flash)?.is_some() {}
        Ok(header)
    }

    /// Trailer state of the running image's slot.
    pub fn active_state(&mut self) -> Result<SwapState, Error<F::Error>> {
        self.trailer.read_state(&mut self.active)
    }

    /// Trailer state of the update slot.
    pub fn update_state(&mut self) -> Result<SwapState, Error<F::Error>> {
        self.trailer.read_state(&mut self.update)
    }

    /// Whether the running trailer still needs `image_ok`. Test images revert
    /// in scratch/direct-XIP-revert unless confirmed; overwrite confirmation
    /// is informational and direct-XIP without revert needs no confirmation.
    pub fn needs_confirmation(&mut self) -> Result<bool, Error<F::Error>> {
        if !self.mode.requires_confirmation() {
            return Ok(false);
        }
        let s = self.active_state()?;
        Ok(s.magic == MagicState::Good && s.image_ok == Flag::Unset)
    }

    /// Mark the running image as good (`boot_set_confirmed`).
    pub fn confirm(&mut self) -> Result<(), Error<F::Error>> {
        let s = self.active_state()?;
        Self::apply_flags(
            self.trailer,
            &mut self.active,
            s,
            self.mode,
            FlagRequest::Confirm,
        )
    }

    /// Start writing a new image into the update slot. Any previous upgrade
    /// request is removed first.
    ///
    /// Fails with [`Error::UpdateSlotInUse`], without touching flash, while
    /// the running image is an unconfirmed trial whose fallback image is in
    /// the update slot. Call [`Self::confirm`] first.
    pub fn begin_update(&mut self) -> Result<UpdateWriter<'_, F>, Error<F::Error>> {
        let progress = self.begin_progress()?;
        Ok(UpdateWriter {
            flash: &mut self.update,
            progress,
        })
    }

    pub(crate) fn begin_progress(&mut self) -> Result<UpdateProgress, Error<F::Error>> {
        self.ensure_update_slot_free()?;
        self.trailer.scramble(&mut self.update)?;
        Ok(UpdateProgress {
            max: self.max_image_size(),
            pos: 0,
            erased_to: 0,
            buf: Aligned([ERASED; WRITE_BUF]),
            fill: 0,
        })
    }

    pub(crate) fn write_progress(
        &mut self,
        progress: &mut UpdateProgress,
        data: &[u8],
    ) -> Result<(), Error<F::Error>> {
        progress.write(&mut self.update, data)
    }

    pub(crate) fn finish_progress(
        &mut self,
        progress: UpdateProgress,
    ) -> Result<u32, Error<F::Error>> {
        progress.finish(&mut self.update)
    }

    /// Erase the whole update slot. Fails with [`Error::UpdateSlotInUse`]
    /// under the same condition as [`Self::begin_update`].
    pub fn erase_update(&mut self) -> Result<(), Error<F::Error>> {
        self.ensure_update_slot_free()?;
        self.erase_update_unchecked()
    }

    /// Erase the update slot without the trial check. Only for removing data
    /// this updater wrote itself (an aborted upload), which passed the check
    /// when writing began.
    pub(crate) fn erase_update_unchecked(&mut self) -> Result<(), Error<F::Error>> {
        self.trailer.scramble(&mut self.update)?;
        flash::erase(&mut self.update, 0, self.trailer.first_sector_off())
    }

    /// Refuse to modify the update slot while it holds the fallback image of
    /// an unconfirmed trial. Reads only the running image's trailer.
    pub(crate) fn ensure_update_slot_free(&mut self) -> Result<(), Error<F::Error>> {
        let active = self.active_state()?;
        if self.mode.fallback_in_use(active) {
            warn!("update slot holds the fallback of an unconfirmed trial; confirm first");
            return Err(Error::UpdateSlotInUse);
        }
        Ok(())
    }

    /// `boot_set_pending`: request a test boot without verifying a signature.
    /// The bootloader always authenticates the candidate on the next boot.
    pub fn mark_for_test(&mut self) -> Result<(), Error<F::Error>> {
        self.set_pending(false)
    }

    /// Request a permanent upgrade. Still authenticated by the bootloader.
    pub fn mark_permanent(&mut self) -> Result<(), Error<F::Error>> {
        self.set_pending(true)
    }

    fn set_pending(&mut self, permanent: bool) -> Result<(), Error<F::Error>> {
        let t = self.trailer;
        let state = t.read_state(&mut self.update)?;
        Self::apply_flags(
            t,
            &mut self.update,
            state,
            self.mode,
            FlagRequest::Pending { permanent },
        )
    }

    // All mode-dependent writes use one transition table. Confirmation targets
    // the active area; pending requests target the inactive area.
    fn apply_flags(
        t: Trailer,
        flash: &mut F,
        state: SwapState,
        mode: UpgradeMode,
        request: FlagRequest,
    ) -> Result<(), Error<F::Error>> {
        match (mode, request) {
            (UpgradeMode::DirectXip { .. }, request) => {
                let permanent = match request {
                    FlagRequest::Confirm => true,
                    FlagRequest::Pending { permanent } => permanent,
                };
                Self::set_xip_flags(t, flash, state, permanent)
            }
            (_, FlagRequest::Confirm) => match state.magic {
                MagicState::Unset => Ok(()),
                MagicState::Bad => Err(Error::BadTrailer),
                MagicState::Good if state.image_ok != Flag::Unset => Ok(()),
                MagicState::Good => {
                    info!("confirming running image");
                    t.write_image_ok(flash)
                }
            },
            (_, FlagRequest::Pending { permanent }) => match state.magic {
                MagicState::Good => Ok(()),
                MagicState::Bad => Err(Error::BadTrailer),
                MagicState::Unset => {
                    t.write_magic(flash)?;
                    if permanent {
                        t.write_image_ok(flash)?;
                    }
                    let swap_type = if permanent {
                        SwapType::Perm
                    } else {
                        SwapType::Test
                    };
                    t.write_swap_info(flash, swap_type, 0)?;
                    info!("upgrade scheduled ({:?})", swap_type);
                    Ok(())
                }
            },
        }
    }

    fn set_xip_flags(
        t: Trailer,
        flash: &mut F,
        state: SwapState,
        permanent: bool,
    ) -> Result<(), Error<F::Error>> {
        if state.magic == MagicState::Bad {
            return Err(Error::BadTrailer);
        }
        if state.magic == MagicState::Unset {
            t.write_magic(flash)?;
        }
        if permanent {
            if state.copy_done == Flag::Unset {
                t.write_copy_done(flash)?;
            }
            if state.image_ok == Flag::Unset {
                t.write_image_ok(flash)?;
            }
        }
        Ok(())
    }

    /// Read the slot trailers once and derive pending/confirmation/protection
    /// using the configured strategy. Physical primary wins XIP version ties.
    pub fn image_state(&mut self, images: &ImageSlots) -> Result<UpdateState, Error<F::Error>> {
        let active = self.active_state()?;
        let inactive = self.update_state()?;
        Ok(self.mode.image_state(active, inactive, images))
    }

    pub fn update_is_pending(&mut self, images: &ImageSlots) -> Result<bool, Error<F::Error>> {
        Ok(self.image_state(images)?.pending)
    }

    /// Confirmation of the inactive image, using the same strategy rules as
    /// the aggregate state query.
    pub fn update_is_confirmed(&mut self) -> Result<bool, Error<F::Error>> {
        let inactive = self.update_state()?;
        Ok(self.mode.inactive_is_confirmed(inactive))
    }

    /// Includes a swap trial's rollback image after the pending request has
    /// been consumed. Call before erasing or replacing the inactive slot.
    pub fn update_is_in_use(&mut self, images: &ImageSlots) -> Result<bool, Error<F::Error>> {
        Ok(self.image_state(images)?.in_use)
    }

    pub fn release(self) -> (F, F) {
        (self.active, self.update)
    }
}

const WRITE_BUF: usize = 256;

/// Sequential writer for the update slot. Accepts arbitrarily sized chunks,
/// erases sectors just before they are first written and pads the tail.
pub struct UpdateWriter<'u, F: NorFlash> {
    flash: &'u mut F,
    progress: UpdateProgress,
}

impl<F: NorFlash> UpdateWriter<'_, F> {
    pub fn len(&self) -> u32 {
        self.progress.len()
    }
    pub fn is_empty(&self) -> bool {
        self.progress.is_empty()
    }
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error<F::Error>> {
        self.progress.write(self.flash, data)
    }
    pub fn finish(self) -> Result<u32, Error<F::Error>> {
        self.progress.finish(self.flash)
    }
}

// Owned writer state lets management return to normal dispatch between chunks.
// The flash borrow exists only for the duration of each write.
pub(crate) struct UpdateProgress {
    max: u32,
    pos: u32,
    erased_to: u32,
    buf: Aligned<WRITE_BUF>,
    fill: usize,
}

impl UpdateProgress {
    /// Number of bytes accepted so far.
    pub fn len(&self) -> u32 {
        self.pos + self.fill as u32
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append bytes to the image.
    fn write<F: NorFlash>(
        &mut self,
        flash: &mut F,
        mut data: &[u8],
    ) -> Result<(), Error<F::Error>> {
        if (self.len() as u64) + data.len() as u64 > u64::from(self.max) {
            return Err(ImageError::ImageTooLarge.into());
        }
        while !data.is_empty() {
            let n = (WRITE_BUF - self.fill).min(data.len());
            self.buf.0[self.fill..self.fill + n].copy_from_slice(&data[..n]);
            self.fill += n;
            data = &data[n..];
            if self.fill == WRITE_BUF {
                self.flush(flash, WRITE_BUF)?;
            }
        }
        Ok(())
    }

    fn flush<F: NorFlash>(&mut self, flash: &mut F, len: usize) -> Result<(), Error<F::Error>> {
        let end = self.pos + len as u32;
        if end > self.erased_to {
            let to = align_up(end, F::ERASE_SIZE as u32);
            flash::erase(flash, self.erased_to, to)?;
            self.erased_to = to;
        }
        flash
            .write(self.pos, &self.buf.0[..len])
            .map_err(Error::Flash)?;
        self.pos = end;
        self.fill = 0;
        self.buf.0.fill(ERASED);
        Ok(())
    }

    /// Write the buffered tail (padded with `0xff`) and return the image length.
    fn finish<F: NorFlash>(mut self, flash: &mut F) -> Result<u32, Error<F::Error>> {
        let total = self.len();
        if self.fill > 0 {
            let ws = F::WRITE_SIZE.max(1);
            let len = self.fill.div_ceil(ws) * ws;
            self.flush(flash, len)?;
        }
        Ok(total)
    }
}
