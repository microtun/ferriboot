//! Flash layout of the image slots, checked the same way on every platform.
//!
//! A platform crate only decides *where* the slots are (a fixed table, a
//! partition table read from flash, ...) and how to turn a [`Region`] into a
//! [`NorFlash`] partition. Geometry checks, handoff checks and the
//! construction of the [`BootLoader`] and [`FirmwareUpdater`] live here, so
//! every board enforces the same rules.

use embedded_storage::nor_flash::NorFlash;

use crate::{
    BootConfig, BootLoader, ConfigError, FirmwareUpdater, PublicKey, Slot, UpgradeMode,
    shared_data::BootInfo, trailer::Trailer,
};

/// A flash area in absolute device offsets.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub offset: u32,
    pub size: u32,
}

impl Region {
    pub const fn new(offset: u32, size: u32) -> Self {
        Self { offset, size }
    }

    /// Whether the absolute flash offset lies inside this region.
    pub fn contains(&self, offset: u32) -> bool {
        offset >= self.offset && offset - self.offset < self.size
    }

    /// Whether the two regions share at least one byte.
    pub fn overlaps(&self, other: &Region) -> bool {
        let end = |r: &Region| u64::from(r.offset) + u64::from(r.size);
        u64::from(self.offset) < end(other) && u64::from(other.offset) < end(self)
    }

    /// Check that the region is non-empty, aligned to `F`'s erase sectors and
    /// inside a device of `capacity` bytes.
    pub fn check<F: NorFlash>(&self, capacity: usize) -> Result<(), LayoutError> {
        let erase = F::ERASE_SIZE as u32;
        if erase == 0
            || self.size == 0
            || !self.offset.is_multiple_of(erase)
            || !self.size.is_multiple_of(erase)
        {
            return Err(LayoutError::Misaligned);
        }
        if self
            .offset
            .checked_add(self.size)
            .is_none_or(|end| end as usize > capacity)
        {
            return Err(LayoutError::OutOfBounds);
        }
        Ok(())
    }
}

/// Physical limits a [`SlotLayout`] must respect.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashBounds {
    /// Size of the whole flash device in bytes.
    pub capacity: usize,
    /// Bytes at the start of flash that slots must not overlap: the
    /// bootloader itself and anything fixed in front of the slots (such as a
    /// partition table).
    pub reserved: u32,
}

/// Why a layout or boot handoff was rejected.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// An area is empty or not aligned to erase sectors.
    Misaligned,
    /// The region exceeds storage capacity or its end overflows.
    OutOfBounds,
    /// A region overlaps the reserved area or another flash area.
    Overlap,
    /// The two image slots have different capacities.
    SlotSizeMismatch,
    /// Scratch swap was selected without a scratch region.
    MissingScratch,
    /// Scratch cannot hold both a data sector and its status sector.
    ScratchTooSmall,
    /// Boot mode, physical slot, verifier or usable capacity does not match.
    InvalidHandoff,
    /// The bootloader or updater rejected the slot geometry.
    Config(ConfigError),
}

impl From<ConfigError> for LayoutError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl core::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Misaligned => f.write_str("flash region is empty or not erase-aligned"),
            Self::OutOfBounds => f.write_str("flash region exceeds device capacity"),
            Self::Overlap => f.write_str("flash regions overlap"),
            Self::SlotSizeMismatch => f.write_str("image slots have different sizes"),
            Self::MissingScratch => f.write_str("scratch region is required"),
            Self::ScratchTooSmall => f.write_str("scratch region is too small"),
            Self::InvalidHandoff => f.write_str("invalid boot handoff"),
            Self::Config(error) => write!(f, "invalid boot configuration: {error:?}"),
        }
    }
}

/// Location of the two image slots and the optional scratch area.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotLayout {
    pub primary: Region,
    pub secondary: Region,
    pub scratch: Option<Region>,
}

/// A board's fixed flash map: the device bounds and where the slots are.
///
/// Boards whose slots are fixed at build time (rather than read from a
/// partition table in flash) publish one of these from their layout crate.
/// It holds board decisions only; the platform crate checks it against its
/// chip's rules when wrapping it in its own validated layout type, and
/// [`SlotLayout::validate`] applies the generic rules when a bootloader or
/// updater is constructed.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashMap {
    pub bounds: FlashBounds,
    pub slots: SlotLayout,
}

impl SlotLayout {
    pub const fn region(&self, slot: Slot) -> Region {
        match slot {
            Slot::Primary => self.primary,
            Slot::Secondary => self.secondary,
        }
    }

    /// The slot containing an absolute flash offset.
    pub fn slot_at(&self, offset: u32) -> Option<Slot> {
        if self.primary.contains(offset) {
            Some(Slot::Primary)
        } else if self.secondary.contains(offset) {
            Some(Slot::Secondary)
        } else {
            None
        }
    }

    /// Check the geometry before anything reads or writes the slots: equal
    /// slot sizes, erase alignment, device bounds, no overlap with the
    /// reserved area or each other, a usable scratch area for scratch swap,
    /// and room for a trailer.
    pub fn validate<F: NorFlash>(
        &self,
        bounds: FlashBounds,
        mode: UpgradeMode,
    ) -> Result<(), LayoutError> {
        if self.primary.size != self.secondary.size {
            return Err(LayoutError::SlotSizeMismatch);
        }
        let regions = [Some(self.primary), Some(self.secondary), self.scratch];
        for (i, region) in regions.iter().enumerate() {
            let Some(r) = region else { continue };
            r.check::<F>(bounds.capacity)?;
            if r.offset < bounds.reserved {
                return Err(LayoutError::Overlap);
            }
            if regions[..i].iter().flatten().any(|s| r.overlaps(s)) {
                return Err(LayoutError::Overlap);
            }
        }
        if mode == UpgradeMode::SwapScratch {
            let scratch = self.scratch.ok_or(LayoutError::MissingScratch)?;
            if (scratch.size as usize) < F::ERASE_SIZE.saturating_mul(2) {
                return Err(LayoutError::ScratchTooSmall);
            }
        }
        self.max_image_size::<F>()?;
        Ok(())
    }

    /// Usable signed-image size, including the MCUboot header and TLVs.
    pub fn max_image_size<F: NorFlash>(&self) -> Result<u32, LayoutError> {
        if self.primary.size != self.secondary.size {
            return Err(LayoutError::SlotSizeMismatch);
        }
        Ok(Trailer::for_slot::<F>(self.primary.size)?.max_image_size())
    }

    /// The physical slot the running application was started from, after
    /// checking that the bootloader's handoff matches this layout: the usable
    /// image size (when present), ECDSA P-256 verification, and (according
    /// to the bootloader-published mode) a valid running slot.
    pub fn active_slot<F: NorFlash>(&self, info: &BootInfo) -> Result<Slot, LayoutError> {
        let max = self.max_image_size::<F>()?;
        let matches = info.supports_layout(max) && info.is_supported();
        matches
            .then_some(info.active_slot())
            .ok_or(LayoutError::InvalidHandoff)
    }

    /// Validate the layout against `bounds` and construct the boot state
    /// machine over partitions created by `partition`.
    pub fn bootloader<'k, P: NorFlash>(
        &self,
        bounds: FlashBounds,
        keys: &'k [PublicKey],
        config: BootConfig,
        mut partition: impl FnMut(Region) -> Result<P, LayoutError>,
    ) -> Result<BootLoader<'k, P>, LayoutError> {
        self.validate::<P>(bounds, config.mode)?;
        let scratch = match (config.mode, self.scratch) {
            (UpgradeMode::SwapScratch, Some(r)) => Some(partition(r)?),
            (UpgradeMode::SwapScratch, None) => return Err(LayoutError::MissingScratch),
            _ => None,
        };
        Ok(BootLoader::new(
            partition(self.primary)?,
            partition(self.secondary)?,
            scratch,
            keys,
            config,
        )?)
    }

    /// Construct the application updater from validated geometry and BootInfo.
    /// Signing keys and signature policy remain solely in the bootloader.
    pub fn updater<P: NorFlash>(
        &self,
        bounds: FlashBounds,
        info: &BootInfo,
        mut partition: impl FnMut(Region) -> Result<P, LayoutError>,
    ) -> Result<FirmwareUpdater<P>, LayoutError> {
        let mode = info.mode();
        self.validate::<P>(bounds, mode)?;
        let active = self.active_slot::<P>(info)?;
        Ok(FirmwareUpdater::new_with_mode(
            partition(self.region(active))?,
            partition(self.region(active.other()))?,
            mode,
        )?)
    }
}
