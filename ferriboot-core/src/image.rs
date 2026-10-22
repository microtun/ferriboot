//! MCUboot image format: header and TLV areas.
//!
//! ```text
//! +-------------------+  0
//! | image_header (32) |
//! | padding           |  (imgtool --pad-header)
//! +-------------------+  hdr_size
//! | image body        |
//! +-------------------+  hdr_size + img_size        <- tlv_offset()
//! | protected TLVs    |  info(0x6908, total) + entries   (protect_tlv_size)
//! +-------------------+
//! | TLVs              |  info(0x6907, total) + entries
//! +-------------------+                             <- total image size
//! ```
//!
//! The digest that is signed covers everything up to the end of the
//! protected TLV area.

use core::cmp::Ordering;

use embedded_storage::nor_flash::ReadNorFlash;

use crate::{Error, ImageError, flash};

pub const IMAGE_MAGIC: u32 = 0x96f3_b83d;
pub const IMAGE_HEADER_SIZE: usize = 32;
pub const TLV_INFO_MAGIC: u16 = 0x6907;
pub const TLV_PROT_INFO_MAGIC: u16 = 0x6908;
pub const TLV_INFO_SIZE: u32 = 4;
pub const TLV_HEADER_SIZE: u32 = 4;

/// Image header flags (`ih_flags`).
pub mod flags {
    pub const PIC: u32 = 0x0000_0001;
    pub const ENCRYPTED_AES128: u32 = 0x0000_0004;
    pub const ENCRYPTED_AES256: u32 = 0x0000_0008;
    pub const NON_BOOTABLE: u32 = 0x0000_0010;
    pub const RAM_LOAD: u32 = 0x0000_0020;
    pub const ROM_FIXED: u32 = 0x0000_0100;
    pub const COMPRESSED_LZMA1: u32 = 0x0000_0200;
    pub const COMPRESSED_LZMA2: u32 = 0x0000_0400;
    pub const COMPRESSED_ARM_THUMB: u32 = 0x0000_0800;

    /// Flags that make an image unusable here (`boot_check_header_valid`
    /// without `MCUBOOT_ENC_IMAGES` / `MCUBOOT_DECOMPRESS_IMAGES`). Like
    /// upstream, every other flag is ignored; in particular `RAM_LOAD`, which
    /// imgtool sets for `--load-addr`, has no effect outside RAM-load builds.
    pub const UNSUPPORTED: u32 = ENCRYPTED_AES128
        | ENCRYPTED_AES256
        | COMPRESSED_LZMA1
        | COMPRESSED_LZMA2
        | COMPRESSED_ARM_THUMB;
}

/// TLV types (`IMAGE_TLV_*`).
pub mod tlv {
    pub const KEYHASH: u16 = 0x01;
    pub const PUBKEY: u16 = 0x02;
    pub const SHA256: u16 = 0x10;
    pub const SHA384: u16 = 0x11;
    pub const SHA512: u16 = 0x12;
    pub const RSA2048_PSS: u16 = 0x20;
    pub const ECDSA224: u16 = 0x21;
    pub const ECDSA_SIG: u16 = 0x22;
    pub const RSA3072_PSS: u16 = 0x23;
    pub const ED25519: u16 = 0x24;
    pub const SIG_PURE: u16 = 0x25;
    pub const ENC_RSA2048: u16 = 0x30;
    pub const ENC_KW: u16 = 0x31;
    pub const ENC_EC256: u16 = 0x32;
    pub const ENC_X25519: u16 = 0x33;
    pub const ENC_X25519_SHA512: u16 = 0x34;
    pub const DEPENDENCY: u16 = 0x40;
    pub const SEC_CNT: u16 = 0x50;
    pub const BOOT_RECORD: u16 = 0x60;
    pub const DECOMP_SIZE: u16 = 0x70;
    pub const DECOMP_SHA: u16 = 0x71;
    pub const DECOMP_SIGNATURE: u16 = 0x72;
    pub const UUID_VID: u16 = 0x74;
    pub const UUID_CID: u16 = 0x75;

    /// TLVs that may legitimately live in the unprotected area (MCUboot's
    /// `allowed_unprot_tlvs`). Anything else found there is rejected, because
    /// it would not be covered by the signature.
    ///
    /// Upstream lists `SIG_PURE` only in `MCUBOOT_SIGN_PURE` builds (not
    /// supported here) and never allows the `DECOMP_*` TLVs outside the
    /// protected area. It lists exactly one of the two X25519 key-exchange
    /// TLVs depending on `MCUBOOT_HMAC_SHA512`; both are kept because
    /// encryption is unsupported and either one is ignored.
    pub const ALLOWED_UNPROTECTED: &[u16] = &[
        KEYHASH,
        PUBKEY,
        SHA256,
        SHA384,
        SHA512,
        RSA2048_PSS,
        ECDSA224,
        ECDSA_SIG,
        RSA3072_PSS,
        ED25519,
        ENC_RSA2048,
        ENC_KW,
        ENC_EC256,
        ENC_X25519,
        ENC_X25519_SHA512,
    ];
}

/// `ih_ver`. Ordering follows MCUboot's `boot_version_cmp`: only
/// `major.minor.revision` take part, the build number is ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageVersion {
    pub major: u8,
    pub minor: u8,
    pub revision: u16,
    pub build: u32,
}

impl ImageVersion {
    pub const fn new(major: u8, minor: u8, revision: u16, build: u32) -> Self {
        Self {
            major,
            minor,
            revision,
            build,
        }
    }

    /// Compare `major.minor.revision` only.
    pub fn semver_cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.revision).cmp(&(other.major, other.minor, other.revision))
    }
}

impl core::fmt::Display for ImageVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}.{}.{}+{}",
            self.major, self.minor, self.revision, self.build
        )
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for ImageVersion {
    fn format(&self, formatter: defmt::Formatter<'_>) {
        defmt::write!(
            formatter,
            "{}.{}.{}+{}",
            self.major,
            self.minor,
            self.revision,
            self.build
        );
    }
}

/// The 32-byte image header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageHeader {
    pub load_addr: u32,
    pub hdr_size: u16,
    pub protect_tlv_size: u16,
    pub img_size: u32,
    pub flags: u32,
    pub version: ImageVersion,
}

fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl ImageHeader {
    /// Parse a header. Returns `None` if the magic does not match, which
    /// MCUboot treats as "no image in this slot". Field sanity is checked
    /// separately by [`ImageHeader::check`].
    pub fn parse(bytes: &[u8; IMAGE_HEADER_SIZE]) -> Option<Self> {
        if le_u32(&bytes[0..4]) != IMAGE_MAGIC {
            return None;
        }
        Some(Self {
            load_addr: le_u32(&bytes[4..8]),
            hdr_size: le_u16(&bytes[8..10]),
            protect_tlv_size: le_u16(&bytes[10..12]),
            img_size: le_u32(&bytes[12..16]),
            flags: le_u32(&bytes[16..20]),
            version: ImageVersion {
                major: bytes[20],
                minor: bytes[21],
                revision: le_u16(&bytes[22..24]),
                build: le_u32(&bytes[24..28]),
            },
        })
    }

    /// Read the header at offset 0 of a slot.
    pub fn read<F: ReadNorFlash>(flash: &mut F) -> Result<Option<Self>, Error<F::Error>> {
        let mut buf = flash::Aligned([0u8; IMAGE_HEADER_SIZE]);
        flash::read(flash, 0, &mut buf.0)?;
        Ok(Self::parse(&buf.0))
    }

    /// Structural checks performed before an image is hashed.
    pub fn check(&self) -> Result<(), ImageError> {
        if usize::from(self.hdr_size) < IMAGE_HEADER_SIZE {
            return Err(ImageError::InvalidHeader);
        }
        if self.protect_tlv_size != 0 && u32::from(self.protect_tlv_size) < TLV_INFO_SIZE {
            return Err(ImageError::InvalidHeader);
        }
        if self.flags & flags::UNSUPPORTED != 0 {
            return Err(ImageError::UnsupportedFlags(
                self.flags & flags::UNSUPPORTED,
            ));
        }
        if !self.is_bootable() {
            return Err(ImageError::NonBootable);
        }
        self.hashed_len().ok_or(ImageError::InvalidHeader)?;
        Ok(())
    }

    pub fn is_bootable(&self) -> bool {
        self.flags & flags::NON_BOOTABLE == 0
    }

    /// Cheap candidate filtering against the physical area, before touching
    /// trial markers. Full validation separately excludes trailer sectors.
    pub fn check_capacity(&self, capacity: u32) -> Result<(), ImageError> {
        self.check()?;
        if self.hashed_len().ok_or(ImageError::InvalidHeader)? >= capacity {
            return Err(ImageError::ImageTooLarge);
        }
        Ok(())
    }

    /// Offset of the first TLV area (end of the image body).
    pub fn tlv_offset(&self) -> Option<u32> {
        u32::from(self.hdr_size).checked_add(self.img_size)
    }

    /// Number of bytes covered by the image digest.
    pub fn hashed_len(&self) -> Option<u32> {
        self.tlv_offset()?
            .checked_add(u32::from(self.protect_tlv_size))
    }
}

/// One TLV entry located in flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlvEntry {
    pub kind: u16,
    /// Offset of the value (just past the 4-byte TLV header) within the slot.
    pub value_offset: u32,
    pub len: u16,
    /// Whether the entry lives in the protected (signed) area.
    pub protected: bool,
}

/// Iterator over the protected and unprotected TLV areas of an image
/// (`bootutil_tlv_iter`), with strict bounds checking.
pub struct TlvIter {
    off: u32,
    area_end: u32,
    in_protected: bool,
    unprot_start: u32,
    end: u32,
}

impl TlvIter {
    /// Locate the TLV areas. `limit` is the largest valid end offset of the
    /// image (normally the start of the slot trailer).
    pub fn begin<F: ReadNorFlash>(
        flash: &mut F,
        hdr: &ImageHeader,
        limit: u32,
    ) -> Result<Self, Error<F::Error>> {
        let base = hdr.tlv_offset().ok_or(ImageError::InvalidHeader)?;
        let prot = u32::from(hdr.protect_tlv_size);
        if base.checked_add(TLV_INFO_SIZE).is_none_or(|e| e > limit) {
            return Err(ImageError::ImageTooLarge.into());
        }

        let mut info = [0u8; 4];
        flash::read(flash, base, &mut info)?;
        let unprot_start = if le_u16(&info[0..2]) == TLV_PROT_INFO_MAGIC {
            if u32::from(le_u16(&info[2..4])) != prot || prot < TLV_INFO_SIZE {
                return Err(ImageError::InvalidTlvInfo.into());
            }
            let start = base + prot;
            if start.checked_add(TLV_INFO_SIZE).is_none_or(|e| e > limit) {
                return Err(ImageError::ImageTooLarge.into());
            }
            flash::read(flash, start, &mut info)?;
            start
        } else {
            if prot != 0 {
                return Err(ImageError::InvalidTlvInfo.into());
            }
            base
        };

        if le_u16(&info[0..2]) != TLV_INFO_MAGIC {
            return Err(ImageError::InvalidTlvInfo.into());
        }
        let total = u32::from(le_u16(&info[2..4]));
        if total < TLV_INFO_SIZE {
            return Err(ImageError::InvalidTlvInfo.into());
        }
        let end = unprot_start
            .checked_add(total)
            .ok_or(ImageError::ImageTooLarge)?;
        if end > limit {
            return Err(ImageError::ImageTooLarge.into());
        }

        let (off, area_end, in_protected) = if prot > 0 {
            (base + TLV_INFO_SIZE, base + prot, true)
        } else {
            (unprot_start + TLV_INFO_SIZE, end, false)
        };

        Ok(Self {
            off,
            area_end,
            in_protected,
            unprot_start,
            end,
        })
    }

    /// Total size of the image: header, body and both TLV areas.
    pub fn total_size(&self) -> u32 {
        self.end
    }

    pub fn next<F: ReadNorFlash>(
        &mut self,
        flash: &mut F,
    ) -> Result<Option<TlvEntry>, Error<F::Error>> {
        loop {
            if self.off == self.area_end {
                if self.in_protected {
                    self.in_protected = false;
                    self.off = self.unprot_start + TLV_INFO_SIZE;
                    self.area_end = self.end;
                    continue;
                }
                return Ok(None);
            }
            if self.area_end - self.off < TLV_HEADER_SIZE {
                return Err(ImageError::TruncatedTlv.into());
            }

            let mut hdr = [0u8; 4];
            flash::read(flash, self.off, &mut hdr)?;
            let kind = le_u16(&hdr[0..2]);
            let len = le_u16(&hdr[2..4]);
            let value_offset = self.off + TLV_HEADER_SIZE;
            if u32::from(len) > self.area_end - value_offset {
                return Err(ImageError::TruncatedTlv.into());
            }
            self.off = value_offset + u32::from(len);
            return Ok(Some(TlvEntry {
                kind,
                value_offset,
                len,
                protected: self.in_protected,
            }));
        }
    }
}

/// Total size (header + body + TLVs) of the image in a slot, or `None` if the
/// slot holds no image.
pub fn image_size<F: ReadNorFlash>(
    flash: &mut F,
    limit: u32,
) -> Result<Option<u32>, Error<F::Error>> {
    match ImageHeader::read(flash)? {
        None => Ok(None),
        Some(hdr) => Ok(Some(TlvIter::begin(flash, &hdr, limit)?.total_size())),
    }
}
