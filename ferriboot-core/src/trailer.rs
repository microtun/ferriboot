//! The image trailer at the end of every slot, using MCUboot's on-flash layout
//! with `BOOT_MAX_ALIGN = 8`:
//!
//! ```text
//!   ~ swap status: status_entries * 3 * write_size     ~  <- status_off()
//!   | swap_size (u32 LE)  | 0xff padding to 8 bytes      |  <- size - 48
//!   | swap_info           | 0xff padding to 8 bytes      |  <- size - 40
//!   | copy_done           | 0xff padding to 8 bytes      |  <- size - 32
//!   | image_ok            | 0xff padding to 8 bytes      |  <- size - 24
//!   | MAGIC (16 bytes)                                   |  <- size - 16
//! ```
//!
//! The magic and flags sit at fixed offsets from the end of the slot, so
//! images padded with `imgtool sign --pad [--confirm]` are understood.

use embedded_storage::nor_flash::NorFlash;

use crate::{
    ConfigError, Error,
    flash::{self, ERASED, align_down},
};

/// `boot_img_magic` for `BOOT_MAX_ALIGN == 8`.
pub const BOOT_MAGIC: [u8; 16] = [
    0x77, 0xc2, 0x95, 0xf3, 0x60, 0xd2, 0xef, 0x7f, 0x35, 0x52, 0x50, 0x0f, 0x2c, 0xb6, 0x79, 0x80,
];
pub const MAX_ALIGN: u32 = 8;
pub const MAGIC_SIZE: u32 = 16;
/// Number of status entries per swap step (`BOOT_STATUS_STATE_COUNT`).
pub const STATUS_STATE_COUNT: u32 = 3;
/// Value written to a set flag.
pub const FLAG_SET: u8 = 0x01;
/// Size of magic + the four fixed fields.
pub const INFO_SIZE: u32 = 4 * MAX_ALIGN + MAGIC_SIZE;

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MagicState {
    Good,
    Bad,
    Unset,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    Set,
    Bad,
    Unset,
}

impl Flag {
    fn decode(b: u8) -> Self {
        match b {
            ERASED => Flag::Unset,
            FLAG_SET => Flag::Set,
            _ => Flag::Bad,
        }
    }
}

/// `BOOT_SWAP_TYPE_*`.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SwapType {
    /// No upgrade.
    None = 1,
    /// Swap to the secondary image; revert on the next boot unless confirmed.
    Test = 2,
    /// Swap to the secondary image permanently.
    Perm = 3,
    /// Swap back to the previous image (the test image was not confirmed).
    Revert = 4,
    /// The secondary image was invalid.
    Fail = 5,
}

impl SwapType {
    /// Decode the low nibble of `swap_info`. Unknown values read as `None`.
    pub fn from_raw(raw: u8) -> Self {
        match raw & 0x0f {
            2 => SwapType::Test,
            3 => SwapType::Perm,
            4 => SwapType::Revert,
            _ => SwapType::None,
        }
    }

    /// `BOOT_IS_UPGRADE`.
    pub fn is_upgrade(self) -> bool {
        matches!(self, SwapType::Test | SwapType::Perm | SwapType::Revert)
    }
}

/// Decoded trailer of one area (`struct boot_swap_state`).
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapState {
    pub magic: MagicState,
    pub swap_type: SwapType,
    pub image_num: u8,
    pub copy_done: Flag,
    pub image_ok: Flag,
}

impl SwapState {
    /// Eligible for direct-XIP with revert: pending or already confirmed,
    /// excluding a consumed, unconfirmed trial.
    pub fn xip_eligible(&self) -> bool {
        self.magic == MagicState::Good
            && (self.copy_done != Flag::Set || self.image_ok == Flag::Set)
    }
}

/// Determine the pending operation from the primary and secondary trailers
/// (`boot_swap_tables`).
///
/// Like upstream, the revert row matches any secondary magic: the test and
/// permanent rows are checked first, so a revert is only chosen when the
/// secondary trailer does not request an upgrade. A corrupt secondary
/// trailer therefore cannot keep an unconfirmed test image running.
pub fn swap_type_from_states(primary: &SwapState, secondary: &SwapState) -> SwapType {
    if secondary.magic == MagicState::Good {
        match secondary.image_ok {
            Flag::Unset => return SwapType::Test,
            Flag::Set => return SwapType::Perm,
            Flag::Bad => {}
        }
    }
    if primary.magic == MagicState::Good
        && primary.image_ok == Flag::Unset
        && primary.copy_done == Flag::Set
    {
        return SwapType::Revert;
    }
    SwapType::None
}

/// Trailer geometry of one flash area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trailer {
    size: u32,
    write_size: u32,
    erase_size: u32,
    status_entries: u32,
}

impl Trailer {
    /// Trailer of an image slot. Like MCUboot, the status area has room for
    /// one swap step per sector of the slot.
    pub fn for_slot<F: NorFlash>(size: u32) -> Result<Self, ConfigError> {
        let erase_size = F::ERASE_SIZE as u32;
        Self::new::<F>(size, size / erase_size.max(1))
    }

    /// Trailer of the scratch area (status for a single step).
    pub fn for_scratch<F: NorFlash>(size: u32) -> Result<Self, ConfigError> {
        Self::new::<F>(size, 1)
    }

    fn new<F: NorFlash>(size: u32, status_entries: u32) -> Result<Self, ConfigError> {
        let write_size = F::WRITE_SIZE.max(1) as u32;
        let erase_size = F::ERASE_SIZE as u32;
        if !write_size.is_power_of_two() || write_size > MAX_ALIGN {
            return Err(ConfigError::UnsupportedWriteSize);
        }
        if erase_size == 0 || size == 0 || !size.is_multiple_of(erase_size) {
            return Err(ConfigError::MisalignedArea);
        }
        let t = Self {
            size,
            write_size,
            erase_size,
            status_entries,
        };
        if t.total_size() >= size {
            return Err(ConfigError::TrailerDoesNotFit);
        }
        Ok(t)
    }

    pub fn area_size(&self) -> u32 {
        self.size
    }
    pub fn magic_off(&self) -> u32 {
        self.size - MAGIC_SIZE
    }
    pub fn image_ok_off(&self) -> u32 {
        align_down(self.magic_off() - MAX_ALIGN, MAX_ALIGN)
    }
    pub fn copy_done_off(&self) -> u32 {
        self.image_ok_off() - MAX_ALIGN
    }
    pub fn swap_info_off(&self) -> u32 {
        self.copy_done_off() - MAX_ALIGN
    }
    pub fn swap_size_off(&self) -> u32 {
        self.swap_info_off() - MAX_ALIGN
    }
    pub fn status_size(&self) -> u32 {
        self.status_entries * STATUS_STATE_COUNT * self.write_size
    }
    pub fn total_size(&self) -> u32 {
        self.status_size() + INFO_SIZE
    }
    pub fn status_off(&self) -> u32 {
        self.size - self.total_size()
    }
    pub fn status_entries(&self) -> u32 {
        self.status_entries
    }
    pub fn write_size(&self) -> u32 {
        self.write_size
    }

    /// Offset of the first sector that holds trailer data. Image data must end
    /// before this offset.
    pub fn first_sector_off(&self) -> u32 {
        align_down(self.status_off(), self.erase_size)
    }

    /// Largest image (header + body + TLVs) that fits in front of the trailer.
    pub fn max_image_size(&self) -> u32 {
        self.first_sector_off()
    }

    /// Read and decode the trailer (`boot_read_swap_state`).
    pub fn read_state<F: NorFlash>(&self, f: &mut F) -> Result<SwapState, Error<F::Error>> {
        let base = self.size - INFO_SIZE;
        let mut buf = flash::Aligned([0u8; INFO_SIZE as usize]);
        flash::read(f, base, &mut buf.0)?;
        let at = |off: u32| (off - base) as usize;

        let magic = &buf.0[at(self.magic_off())..];
        let magic = if flash::is_erased(magic) {
            MagicState::Unset
        } else if magic == BOOT_MAGIC {
            MagicState::Good
        } else {
            MagicState::Bad
        };

        let swap_info = buf.0[at(self.swap_info_off())];
        let (swap_type, image_num) = if swap_info == ERASED || (swap_info & 0x0f) > 4 {
            (SwapType::None, 0)
        } else {
            (SwapType::from_raw(swap_info), swap_info >> 4)
        };

        Ok(SwapState {
            magic,
            swap_type,
            image_num,
            copy_done: Flag::decode(buf.0[at(self.copy_done_off())]),
            image_ok: Flag::decode(buf.0[at(self.image_ok_off())]),
        })
    }

    pub fn write_magic<F: NorFlash>(&self, f: &mut F) -> Result<(), Error<F::Error>> {
        debug!("writing magic at {:#x}", self.magic_off());
        flash::write_padded(f, self.magic_off(), &BOOT_MAGIC)
    }

    pub fn write_image_ok<F: NorFlash>(&self, f: &mut F) -> Result<(), Error<F::Error>> {
        debug!("writing image_ok at {:#x}", self.image_ok_off());
        flash::write_padded(f, self.image_ok_off(), &[FLAG_SET])
    }

    pub fn write_copy_done<F: NorFlash>(&self, f: &mut F) -> Result<(), Error<F::Error>> {
        debug!("writing copy_done at {:#x}", self.copy_done_off());
        flash::write_padded(f, self.copy_done_off(), &[FLAG_SET])
    }

    pub fn write_swap_info<F: NorFlash>(
        &self,
        f: &mut F,
        swap_type: SwapType,
        image_num: u8,
    ) -> Result<(), Error<F::Error>> {
        let info = (image_num << 4) | (swap_type as u8 & 0x0f);
        debug!(
            "writing swap_info {:#x} at {:#x}",
            info,
            self.swap_info_off()
        );
        flash::write_padded(f, self.swap_info_off(), &[info])
    }

    pub fn write_swap_size<F: NorFlash>(
        &self,
        f: &mut F,
        size: u32,
    ) -> Result<(), Error<F::Error>> {
        flash::write_padded(f, self.swap_size_off(), &size.to_le_bytes())
    }

    pub fn read_swap_size<F: NorFlash>(&self, f: &mut F) -> Result<u32, Error<F::Error>> {
        let mut b = [0u8; 4];
        flash::read(f, self.swap_size_off(), &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Record completion of `state` (1..=3) of swap step `idx` (1-based).
    pub fn write_status<F: NorFlash>(
        &self,
        f: &mut F,
        idx: u32,
        state: u8,
    ) -> Result<(), Error<F::Error>> {
        let entry = (idx - 1) * STATUS_STATE_COUNT + u32::from(state - 1);
        let off = self.status_off() + entry * self.write_size;
        flash::write_padded(f, off, &[state])
    }

    /// Read the swap status (`swap_read_status_bytes`). Returns the
    /// `(idx, state)` of the next step to execute, or `None` if no status was
    /// recorded in this area.
    pub fn read_status<F: NorFlash>(
        &self,
        f: &mut F,
    ) -> Result<Option<(u32, u8)>, Error<F::Error>> {
        let entries = self.status_entries * STATUS_STATE_COUNT;
        let ws = self.write_size as usize;
        let mut buf = flash::Aligned([0u8; 256]);
        let per_chunk = (buf.0.len() / ws) as u32;

        let mut found = false;
        let mut found_idx: Option<u32> = None;
        let mut i = 0u32;
        'outer: while i < entries {
            let n = (entries - i).min(per_chunk);
            let bytes = &mut buf.0[..n as usize * ws];
            flash::read(f, self.status_off() + i * self.write_size, bytes)?;
            for j in 0..n {
                let erased = bytes[j as usize * ws] == ERASED;
                if erased {
                    if found && found_idx.is_none() {
                        found_idx = Some(i + j);
                    }
                } else if !found {
                    found = true;
                } else if found_idx.is_some() {
                    // A write after a gap: the status is inconsistent. MCUboot
                    // logs this and relies on primary slot validation.
                    error!("detected inconsistent swap status");
                    break 'outer;
                }
            }
            i += n;
        }

        if !found {
            return Ok(None);
        }
        let idx = found_idx.unwrap_or(entries);
        Ok(Some((
            idx / STATUS_STATE_COUNT + 1,
            (idx % STATUS_STATE_COUNT) as u8 + 1,
        )))
    }

    /// Erase every sector holding trailer data, back to front
    /// (`swap_scramble_trailer_sectors`).
    pub fn scramble<F: NorFlash>(&self, f: &mut F) -> Result<(), Error<F::Error>> {
        flash::erase_backwards(f, self.first_sector_off(), self.size)
    }
}
