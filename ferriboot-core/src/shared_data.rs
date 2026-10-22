//! MCUboot's bootloader-to-runtime shared RAM TLVs.
//!
//! Standard wire constants and field encodings follow upstream `boot_status.h`
//! and `boot_record.c`: <https://github.com/mcu-tools/mcuboot/tree/main/boot/bootutil>.
//! Only standard BLINFO fields are emitted. Older Ferriboot private TLVs are
//! ignored as unknown extensions for backwards-compatible reads.

use crate::{ImageVersion, Slot, UpgradeMode};

pub const SHARED_DATA_TLV_INFO_MAGIC: u16 = 0x2016;
/// Reserved SRAM for upstream-compatible TLVs; not the encoded TLV length.
pub const BOOT_INFO_SIZE: usize = 256;
/// [`BOOT_INFO_SIZE`] in 32-bit words. Platforms read and publish the
/// reservation with aligned word accesses.
pub const BOOT_INFO_WORDS: usize = BOOT_INFO_SIZE / 4;
const _: () = assert!(BOOT_INFO_SIZE.is_multiple_of(4));
pub const TLV_MAJOR_BLINFO: u8 = 2;
pub const BLINFO_MODE: u16 = 0;
pub const BLINFO_SIGNATURE_TYPE: u16 = 1;
pub const BLINFO_RECOVERY: u16 = 2;
pub const BLINFO_RUNNING_SLOT: u16 = 3;
pub const BLINFO_BOOTLOADER_VERSION: u16 = 4;
pub const BLINFO_MAX_APPLICATION_SIZE: u16 = 5;
/// Upstream name for image 0 (legacy constant above is an alias).
pub const BLINFO_MAX_APPLICATION_SIZE_IMAGE_0: u16 = BLINFO_MAX_APPLICATION_SIZE;
/// Optional nonvolatile security counter for image 0, as in MCUboot.
pub const BLINFO_SECURITY_COUNTER_IMAGE_0: u16 = 0x10;
pub const SIGNATURE_ECDSA_P256: u8 = 2;
pub const RECOVERY_NONE: u8 = 0;
const HEADER_SIZE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedDataError {
    Overflow,
    Duplicate,
    Invalid,
    Missing,
    UnsupportedMode,
}

/// An owned, zero-filled shared area. Entries have no alignment padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedData<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> SharedData<N> {
    pub fn new() -> Result<Self, SharedDataError> {
        if N < HEADER_SIZE || N > u16::MAX as usize {
            return Err(SharedDataError::Overflow);
        }
        let mut data = Self { bytes: [0; N] };
        data.bytes[..2].copy_from_slice(&SHARED_DATA_TLV_INFO_MAGIC.to_le_bytes());
        data.bytes[2..4].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        Ok(data)
    }

    pub fn bytes(&self) -> &[u8; N] {
        &self.bytes
    }

    /// Append a unique major/minor entry, like `boot_add_data_to_shared_area`.
    /// Failure leaves the previous contents intact.
    pub fn add(&mut self, major: u8, minor: u16, value: &[u8]) -> Result<(), SharedDataError> {
        if major > 0xf || minor > 0xfff {
            return Err(SharedDataError::Invalid);
        }
        let kind = (u16::from(major) << 12) | minor;
        let end = u16_at(&self.bytes, 2) as usize;
        let mut offset = HEADER_SIZE;
        while offset < end {
            if u16_at(&self.bytes, offset) == kind {
                return Err(SharedDataError::Duplicate);
            }
            offset += HEADER_SIZE + u16_at(&self.bytes, offset + 2) as usize;
        }
        let new_end = end
            .checked_add(HEADER_SIZE)
            .and_then(|n| n.checked_add(value.len()))
            .filter(|&n| n <= N && n <= u16::MAX as usize)
            .ok_or(SharedDataError::Overflow)?;
        self.bytes[end..end + 2].copy_from_slice(&kind.to_le_bytes());
        self.bytes[end + 2..end + 4].copy_from_slice(&(value.len() as u16).to_le_bytes());
        self.bytes[end + 4..new_end].copy_from_slice(value);
        self.bytes[2..4].copy_from_slice(&(new_end as u16).to_le_bytes());
        Ok(())
    }
}

/// Build the boot-info entries for this port's single image and ECDSA P-256
/// verifier, with optional nonvolatile hardware rollback floor.
/// The platform publishes this buffer in reserved RAM after validating/selecting
/// the image, preserving it through application loading.
pub fn boot_save_shared_data<const N: usize>(
    mode: UpgradeMode,
    slot: Slot,
    max_image_size: u32,
    bootloader_version: Option<ImageVersion>,
    security_counter: Option<u32>,
) -> Result<SharedData<N>, SharedDataError> {
    if max_image_size == 0
        || (!matches!(mode, UpgradeMode::DirectXip { .. }) && slot != Slot::Primary)
    {
        return Err(SharedDataError::Invalid);
    }
    let mut data = SharedData::new()?;
    let mode = match mode {
        UpgradeMode::SwapScratch => 1,
        UpgradeMode::Overwrite => 2,
        UpgradeMode::DirectXip { revert: false } => 4,
        UpgradeMode::DirectXip { revert: true } => 5,
    };
    let slot = slot.number();
    data.add(TLV_MAJOR_BLINFO, BLINFO_MODE, &[mode])?;
    data.add(
        TLV_MAJOR_BLINFO,
        BLINFO_SIGNATURE_TYPE,
        &[SIGNATURE_ECDSA_P256],
    )?;
    data.add(TLV_MAJOR_BLINFO, BLINFO_RECOVERY, &[RECOVERY_NONE])?;
    data.add(TLV_MAJOR_BLINFO, BLINFO_RUNNING_SLOT, &[slot])?;
    if let Some(v) = bootloader_version {
        let mut bytes = [v.major, v.minor, 0, 0, 0, 0, 0, 0];
        bytes[2..4].copy_from_slice(&v.revision.to_le_bytes());
        bytes[4..8].copy_from_slice(&v.build.to_le_bytes());
        data.add(TLV_MAJOR_BLINFO, BLINFO_BOOTLOADER_VERSION, &bytes)?;
    }
    data.add(
        TLV_MAJOR_BLINFO,
        BLINFO_MAX_APPLICATION_SIZE,
        &max_image_size.to_le_bytes(),
    )?;
    if let Some(counter) = security_counter {
        data.add(
            TLV_MAJOR_BLINFO,
            BLINFO_SECURITY_COUNTER_IMAGE_0,
            &counter.to_le_bytes(),
        )?;
    }
    Ok(data)
}

/// The handoff as little-endian words for aligned stores into the platform's
/// reservation. `words[0]` is the header (magic and length): store every other
/// word first and the header last, so a reader never sees a valid header in
/// front of a partially written body.
pub fn wire_words(data: &SharedData<BOOT_INFO_SIZE>) -> [u32; BOOT_INFO_WORDS] {
    let mut words = [0; BOOT_INFO_WORDS];
    for (word, bytes) in words.iter_mut().zip(data.bytes().as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    words
}

/// A checked, owned copy of the entries used by this port. Unknown entries
/// are skipped so other shared-data consumers and future fields can coexist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootInfo {
    mode: UpgradeMode,
    slot: Slot,
    signature_type: u8,
    recovery: Option<u8>,
    max_image_size: Option<u32>,
    bootloader_version: Option<ImageVersion>,
    security_counter: Option<u32>,
}

impl BootInfo {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SharedDataError> {
        if bytes.len() < HEADER_SIZE || u16_at(bytes, 0) != SHARED_DATA_TLV_INFO_MAGIC {
            return Err(SharedDataError::Invalid);
        }
        let end = u16_at(bytes, 2) as usize;
        if end < HEADER_SIZE || end > bytes.len() {
            return Err(SharedDataError::Invalid);
        }
        let (mut mode, mut slot, mut signature_type, mut recovery) = (None, None, None, None);
        let (mut max_image_size, mut bootloader_version, mut security_counter) = (None, None, None);
        let mut offset = HEADER_SIZE;
        while offset < end {
            if end - offset < HEADER_SIZE {
                return Err(SharedDataError::Invalid);
            }
            let kind = u16_at(bytes, offset);
            let next = offset + HEADER_SIZE + u16_at(bytes, offset + 2) as usize;
            if next > end {
                return Err(SharedDataError::Invalid);
            }
            let mut previous = HEADER_SIZE;
            while previous < offset {
                if u16_at(bytes, previous) == kind {
                    return Err(SharedDataError::Duplicate);
                }
                previous += HEADER_SIZE + u16_at(bytes, previous + 2) as usize;
            }
            let value = &bytes[offset + HEADER_SIZE..next];
            match (kind >> 12, kind & 0xfff) {
                (major, BLINFO_MODE) if major == u16::from(TLV_MAJOR_BLINFO) => {
                    mode = Some(match one_byte(value)? {
                        1 => UpgradeMode::SwapScratch,
                        2 => UpgradeMode::Overwrite,
                        4 => UpgradeMode::DirectXip { revert: false },
                        5 => UpgradeMode::DirectXip { revert: true },
                        _ => return Err(SharedDataError::UnsupportedMode),
                    })
                }
                (major, BLINFO_SIGNATURE_TYPE) if major == u16::from(TLV_MAJOR_BLINFO) => {
                    signature_type = Some(one_byte(value)?)
                }
                (major, BLINFO_RECOVERY) if major == u16::from(TLV_MAJOR_BLINFO) => {
                    recovery = Some(one_byte(value)?)
                }
                (major, BLINFO_RUNNING_SLOT) if major == u16::from(TLV_MAJOR_BLINFO) => {
                    slot =
                        Some(Slot::from_number(one_byte(value)?).ok_or(SharedDataError::Invalid)?)
                }
                (major, BLINFO_BOOTLOADER_VERSION) if major == u16::from(TLV_MAJOR_BLINFO) => {
                    if value.len() != 8 {
                        return Err(SharedDataError::Invalid);
                    }
                    bootloader_version = Some(ImageVersion {
                        major: value[0],
                        minor: value[1],
                        revision: u16_at(value, 2),
                        build: u32::from_le_bytes(value[4..8].try_into().unwrap()),
                    });
                }
                (major, BLINFO_MAX_APPLICATION_SIZE) if major == u16::from(TLV_MAJOR_BLINFO) => {
                    let v =
                        u32::from_le_bytes(value.try_into().map_err(|_| SharedDataError::Invalid)?);
                    if v == 0 {
                        return Err(SharedDataError::Invalid);
                    }
                    max_image_size = Some(v);
                }
                (major, BLINFO_SECURITY_COUNTER_IMAGE_0)
                    if major == u16::from(TLV_MAJOR_BLINFO) =>
                {
                    security_counter = Some(u32::from_le_bytes(
                        value.try_into().map_err(|_| SharedDataError::Invalid)?,
                    ));
                }
                _ => {}
            }
            offset = next;
        }
        Ok(Self {
            mode: mode.ok_or(SharedDataError::Missing)?,
            slot: slot.ok_or(SharedDataError::Missing)?,
            signature_type: signature_type.ok_or(SharedDataError::Missing)?,
            recovery,
            max_image_size,
            bootloader_version,
            security_counter,
        })
    }

    /// Read a platform's reserved RAM handoff. Parse wire format only; the
    /// application must separately validate capabilities and slot geometry.
    pub fn from_handoff(bytes: &[u8]) -> Result<Self, SharedDataError> {
        Self::from_bytes(bytes)
    }

    /// [`from_handoff`](Self::from_handoff) for the reservation read with
    /// aligned word loads (the inverse of [`wire_words`]).
    pub fn from_handoff_words(words: &[u32; BOOT_INFO_WORDS]) -> Result<Self, SharedDataError> {
        let mut bytes = [0; BOOT_INFO_SIZE];
        for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *chunk = word.to_le_bytes();
        }
        Self::from_handoff(&bytes)
    }

    /// This application's image verifier accepts ECDSA P-256 only. Recovery
    /// describes the bootloader's capabilities, not the image verifier.
    pub fn is_supported(&self) -> bool {
        self.signature_type == SIGNATURE_ECDSA_P256
    }

    pub fn active_slot(&self) -> Slot {
        self.slot
    }
    pub fn mode(&self) -> UpgradeMode {
        self.mode
    }
    pub fn signature_type(&self) -> u8 {
        self.signature_type
    }
    pub fn recovery(&self) -> Option<u8> {
        self.recovery
    }
    pub fn max_image_size(&self) -> Option<u32> {
        self.max_image_size
    }
    pub fn bootloader_version(&self) -> Option<ImageVersion> {
        self.bootloader_version
    }
    /// The rollback floor published by the bootloader, if hardware backed.
    /// This is metadata, never authority to lower application verification.
    pub fn security_counter(&self) -> Option<u32> {
        self.security_counter
    }

    /// Whether this handoff is compatible with the application's physical
    /// slot capacity. Swap/overwrite execute from primary; direct-XIP may run
    /// from either slot. The bootloader-published mode is authoritative.
    pub fn supports_layout(&self, max_image_size: u32) -> bool {
        self.max_image_size
            .is_none_or(|reported| reported == max_image_size)
            && (matches!(self.mode, UpgradeMode::DirectXip { .. }) || self.slot == Slot::Primary)
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn one_byte(bytes: &[u8]) -> Result<u8, SharedDataError> {
    if bytes.len() == 1 {
        Ok(bytes[0])
    } else {
        Err(SharedDataError::Invalid)
    }
}
