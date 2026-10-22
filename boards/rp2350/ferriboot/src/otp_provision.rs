//! Irreversible RP2350 ROM secure-boot OTP enrollment (dev policy by default;
//! `production` selects the hardened policy).
//!
//! The ROM-sealed flash image contains its 64-byte secp256k1 public key in
//! the PICOBIN SIGNATURE item. Derive SHA-256(public_key) from that item
//! at runtime, so a *single* FerriBoot build can be signed with different keys.
//! The private signing key stays on the host; only the security profile is
//! selected at compilation.
//!
//! RP2350 OTP: SDK regs/otp_data.h, SDK pico/bootrom.h, datasheet §10.5.
//! Secure Arm core 0 only; all calls occur with interrupts disabled and core 1
//! unstarted, after a *separately authenticated* MCUboot app is found.

use embedded_storage::nor_flash::ReadNorFlash;
use ferriboot_rp::SharedFlash;
use ferriboot_rp::otp::{self, Operation as OtpOperation, OtpBuffer, OtpError};
use sha2::{Digest, Sha256};

const FLASH_LIMIT: usize = rp2350_layout::BOOTLOADER_SIZE as usize;

const KEY_ROW: u32 = 0x080; // BOOTKEY0_0 through BOOTKEY0_15 (ECC 16-bit)
const FLAGS_ROW: u32 = 0x04b; // BOOT_FLAGS1 RBIT-3 (three raw 24-bit copies)
const CRIT_ROW: u32 = 0x040; // CRIT1 RBIT-8 (eight raw 24-bit copies)
const KEY0_VALID: u32 = 0x0000_0001;
const OTHER_KEYS_INVALID: u32 = 0x0000_0e00; // Permanently block slots 1, 2, 3.
const DEBUG_DISABLE: u32 = 1 << 2; // Disable ALL SWD processor/system access.
const SECURE_DEBUG_DISABLE: u32 = 1 << 1; // Explicit secure debug restriction.
const DEBUG_MASK: u32 = DEBUG_DISABLE | SECURE_DEBUG_DISABLE;
// Arm the glitch detectors from the OTP power-on sequence, so they also cover
// the mask ROM and its signature check of Ferriboot (datasheet §10.9).
const GLITCH_DETECTOR_ENABLE: u32 = 1 << 4;
// Optional sensitivity increase. Ferriboot never writes it, but tolerates a
// value provisioned separately (for example with picotool).
const GLITCH_DETECTOR_SENS: u32 = 0b11 << 5;
const KNOWN_CRIT: u32 =
    SECURE_BOOT_ENABLE | DEBUG_MASK | GLITCH_DETECTOR_ENABLE | GLITCH_DETECTOR_SENS;
const ALL_VALID_MASK: u32 = 0x0000_000f;
const OTHER_KEYS_VALID: u32 = ALL_VALID_MASK & !KEY0_VALID;
const KEY0_INVALID: u32 = 0x0000_0100;
const SECURE_BOOT_ENABLE: u32 = 1;

/// Only the immutable *profile* is compiled into FerriBoot. Signing may be
/// performed later with any protected, production-specific secp256k1 key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    Dev,
    Production,
}

#[cfg(not(feature = "production"))]
const PROFILE: Profile = Profile::Dev;
#[cfg(feature = "production")]
const PROFILE: Profile = Profile::Production;

#[repr(C)]
struct EnrollmentMarker {
    tag: [u8; 16],
    profile: [u8; 4],
}

#[used]
#[unsafe(link_section = ".rodata.rp2350_enrollment")]
static ENROLLMENT: EnrollmentMarker = EnrollmentMarker {
    tag: *b"FERRIBOOT_OTP_V3",
    // Preserve the V3 host-tool marker for the hardened profile (REL), now
    // selected only by Cargo's `production` feature.
    profile: if cfg!(feature = "production") {
        *b"REL\0"
    } else {
        *b"DEV\0"
    },
};

// PICOBIN metadata encoding from RP2350 datasheet §5.9 and pico-sdk
// boot/picobin.h. The first block is at XIP_BASE, the last PICOBIN block
// contains an item with 64 raw public-key bytes, followed by 64 signature
// bytes. The SHA-256 fingerprint of those raw bytes is what picotool puts
// into OTP BOOTKEY0; the SIGNATURE item does not contain that digest.
const BLOCK_START: u32 = 0xffff_ded3;
const BLOCK_END: u32 = 0xab12_3579;
const SIGNATURE_ITEM: u8 = 0x09;
const IMAGE_TYPE_ITEM: u8 = 0x42;
const HASH_DEF_ITEM: u8 = 0x47;
const LAST_ITEM: u8 = 0xff;
const SIGNATURE_SECP256K1: u32 = 1;
const SIGNATURE_WORDS: usize = 33;
const IMAGE_TYPE_ARM_SECURE: u32 = 0x1021_0142;
// picotool seal --sign turns on PICOBIN_IMAGE_TYPE_EXE_EXTRA_SECURITY_BITS
// (bit 11 in the IMAGE_TYPE upper halfword) in the appended sealed block.
const IMAGE_TYPE_ARM_SECURE_SIGNED: u32 = IMAGE_TYPE_ARM_SECURE | (0x0800 << 16);
// IMAGE_DEF blocks have a 384-byte ROM-defined limit. Keep access restricted
// to the reserved bootloader partition, not application or unbounded flash.
const MAX_IMAGE_BLOCK_BYTES: usize = 0x180;

/// A word reader lets the PICOBIN parser operate on either the flash driver
/// or a synthetic image.
trait FlashWords {
    fn word(&self, offset: usize) -> Result<u32, Error>;
}

/// Reads FerriBoot's own signed *bootloader* partition through the flash
/// driver (an XIP read, without raw pointers). Flash is not being erased or
/// reprogrammed during enrollment.
struct BootloaderFlash<'a, F>(&'a SharedFlash<F>);

impl<F: ReadNorFlash> FlashWords for BootloaderFlash<'_, F> {
    /// Read only word-aligned words from the bootloader partition.
    fn word(&self, offset: usize) -> Result<u32, Error> {
        if !offset.is_multiple_of(4) || offset.checked_add(4).is_none_or(|n| n > FLASH_LIMIT) {
            return Err(Error::InvalidSignatureMetadata);
        }
        let offset = u32::try_from(offset).map_err(|_| Error::InvalidSignatureMetadata)?;
        let mut bytes = [0u8; 4];
        self.0
            .borrow_mut()
            .read(offset, &mut bytes)
            .map_err(|_| Error::FlashRead)?;
        Ok(u32::from_le_bytes(bytes))
    }
}

/// The fields read from a PICOBIN image-definition block.
struct ImageBlock {
    public_key: Option<[u8; 64]>,
    next_link: i32,
    is_arm_secure: bool,
    has_sha256: bool,
}

/// Decode a block linked from the RP2350 first image definition. The ROM
/// requires LAST, relative LINK and FOOTER to form a closed block loop.
/// No speculative scan for signature bytes is done.
fn inspect_block(
    flash: &impl FlashWords,
    offset: usize,
    signed: bool,
) -> Result<ImageBlock, Error> {
    if !offset.is_multiple_of(4) || flash.word(offset)? != BLOCK_START {
        return Err(Error::InvalidSignatureMetadata);
    }
    let block_end = offset
        .checked_add(MAX_IMAGE_BLOCK_BYTES)
        .ok_or(Error::InvalidSignatureMetadata)?
        .min(FLASH_LIMIT);
    let mut cursor = offset + 4;
    let mut public_key = None;
    let mut secure_image = false;
    let mut hash_defined = false;
    while cursor.checked_add(12).is_some_and(|n| n <= block_end) {
        let header = flash.word(cursor)?;
        let kind = header as u8;
        if kind == LAST_ITEM {
            let words = ((header >> 8) & 0xffff) as usize;
            if words == 0 || words != (cursor - offset - 4) / 4 || cursor + 12 > block_end {
                return Err(Error::InvalidSignatureMetadata);
            }
            let link = flash.word(cursor + 4)? as i32;
            if flash.word(cursor + 8)? != BLOCK_END {
                return Err(Error::InvalidSignatureMetadata);
            }
            return Ok(ImageBlock {
                public_key,
                next_link: link,
                is_arm_secure: secure_image,
                has_sha256: hash_defined,
            });
        }
        let words = if kind & 0x80 == 0 {
            ((header >> 8) & 0xff) as usize
        } else {
            ((header >> 8) & 0xffff) as usize
        };
        let bytes = words
            .checked_mul(4)
            .ok_or(Error::InvalidSignatureMetadata)?;
        if words == 0 || cursor.checked_add(bytes + 12).is_none_or(|n| n > block_end) {
            return Err(Error::InvalidSignatureMetadata);
        }
        match kind {
            IMAGE_TYPE_ITEM => {
                let expected = if signed {
                    IMAGE_TYPE_ARM_SECURE_SIGNED
                } else {
                    IMAGE_TYPE_ARM_SECURE
                };
                if header != expected {
                    return Err(Error::InvalidSignatureMetadata);
                }
                secure_image = true;
            }
            HASH_DEF_ITEM => {
                if words != 2 || (header >> 24) as u8 != 1 {
                    return Err(Error::InvalidSignatureMetadata);
                }
                hash_defined = true;
            }
            SIGNATURE_ITEM => {
                if words != SIGNATURE_WORDS
                    || (header >> 24) != SIGNATURE_SECP256K1
                    || public_key.is_some()
                {
                    return Err(Error::InvalidSignatureMetadata);
                }
                let mut raw = [0u8; 64];
                for i in 0..16 {
                    raw[4 * i..4 * i + 4]
                        .copy_from_slice(&flash.word(cursor + 4 + i * 4)?.to_le_bytes());
                }
                if raw.iter().all(|b| *b == 0) {
                    return Err(Error::InvalidSignatureMetadata);
                }
                public_key = Some(raw);
            }
            _ => {}
        }
        cursor += bytes;
    }
    Err(Error::InvalidSignatureMetadata)
}

/// Read the *specific* signed PICOBIN block linked from FerriBoot's image
/// definition. Only a two-block loop is accepted for this BIN sealing path.
/// If the metadata is unsigned, corrupted, changed or not the expected form,
/// OTP programming is refused. Host-side picotool verifies the actual ECDSA
/// signature; once secure boot is enabled the ROM enforces it as well.
fn signing_key_hash<F: ReadNorFlash>(flash: &SharedFlash<F>) -> Result<[u8; 32], Error> {
    let flash = BootloaderFlash(flash);
    // First block contains FerriBoot's linker-built Arm Secure IMAGE_DEF and
    // 512-byte header, which the ROM follows to picotool's signed metadata.
    if flash.word(0)? != BLOCK_START
        || flash.word(4)? != IMAGE_TYPE_ARM_SECURE
        || flash.word(44)? != 0x0000_0aff
        || flash.word(52)? != BLOCK_END
    {
        return Err(Error::InvalidSignatureMetadata);
    }
    let initial = inspect_block(&flash, 0, false)?;
    if initial.public_key.is_some() || !initial.is_arm_secure || initial.next_link <= 0 {
        return Err(Error::InvalidSignatureMetadata);
    }
    let final_offset =
        usize::try_from(initial.next_link).map_err(|_| Error::InvalidSignatureMetadata)?;
    if !(512..FLASH_LIMIT).contains(&final_offset) {
        return Err(Error::InvalidSignatureMetadata);
    }
    let sealed = inspect_block(&flash, final_offset, true)?;
    if !sealed.is_arm_secure || !sealed.has_sha256 || sealed.next_link != -initial.next_link {
        return Err(Error::InvalidSignatureMetadata);
    }
    let key = sealed.public_key.ok_or(Error::MissingSignature)?;
    let digest = Sha256::digest(key);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest);
    Ok(hash)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyEnabled,
    Provisioned,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum Error {
    LockBusy,
    RomRead(i32),
    RomWrite(i32),
    WrongOrDamagedBootKey,
    UnexpectedOtpConfiguration,
    KeyReadbackFailed,
    FlagsReadbackFailed,
    GlitchDetectorReadbackFailed,
    SecureBootReadbackFailed,
    ProfileConflict,
    InvalidSignatureMetadata,
    MissingSignature,
    /// The flash driver failed to read the bootloader's own signature metadata.
    FlashRead,
}

/// Enrollment's view of the shared ROM OTP wrapper in `ferriboot_rp::otp`.
fn access(row: u32, operation: OtpOperation, buffer: OtpBuffer<'_>) -> Result<(), Error> {
    otp::access(row, operation, buffer).map_err(|error| match error {
        OtpError::LockBusy => Error::LockBusy,
        OtpError::Read(status) => Error::RomRead(status),
        OtpError::Write(status) => Error::RomWrite(status),
    })
}

fn read_hash() -> Result<[u8; 32], Error> {
    let mut rows = [0u16; 16];
    access(KEY_ROW, OtpOperation::Read, OtpBuffer::Ecc(&mut rows))?;
    let mut hash = [0u8; 32];
    for (index, value) in rows.into_iter().enumerate() {
        hash[2 * index..2 * index + 2].copy_from_slice(&value.to_le_bytes());
    }
    Ok(hash)
}

fn program_hash(expected: &[u8; 32]) -> Result<(), Error> {
    let mut rows = [0u16; 16];
    for (index, pair) in expected.as_chunks::<2>().0.iter().enumerate() {
        rows[index] = u16::from_le_bytes([pair[0], pair[1]]);
    }
    access(KEY_ROW, OtpOperation::Write, OtpBuffer::Ecc(&mut rows))?;
    if read_hash()? != *expected {
        return Err(Error::KeyReadbackFailed);
    }
    Ok(())
}

fn read_raw<const N: usize>(row: u32) -> Result<[u32; N], Error> {
    let mut values = [0u32; N];
    access(row, OtpOperation::Read, OtpBuffer::Raw(&mut values))?;
    Ok(values)
}

fn write_raw_bits(row: u32, len: usize, bits: u32) -> Result<(), Error> {
    for offset in 0..len {
        // Preserve other OTP fields and never request a 1->0 transition.
        let old = read_raw::<1>(row + offset as u32)?[0];
        if old & bits == bits {
            continue;
        }
        let mut value = old | bits;
        access(
            row + offset as u32,
            OtpOperation::Write,
            OtpBuffer::Raw(core::slice::from_mut(&mut value)),
        )?;
        if read_raw::<1>(row + offset as u32)?[0] & bits != bits {
            return Err(if row == FLAGS_ROW {
                Error::FlagsReadbackFailed
            } else {
                Error::SecureBootReadbackFailed
            });
        }
    }
    Ok(())
}

/// Inspect the OTP state without side effects. Release may tighten a previously
/// enrolled dev board; the reverse is impossible and explicitly rejected.
/// Release refuses other already-enrolled boot keys; dev tolerates additional
/// consistently enrolled boot keys without revoking the spare slots.
fn check_state(
    hash: &[u8; 32],
    expected: &[u8; 32],
    flags: &[u32; 3],
    crit: &[u32; 8],
    profile: Profile,
) -> Result<bool, Error> {
    if hash != &[0; 32] && hash != expected {
        return Err(Error::WrongOrDamagedBootKey);
    }
    if flags.iter().any(|v| v & KEY0_INVALID != 0) {
        return Err(Error::UnexpectedOtpConfiguration);
    }
    let extra_enrolled = flags[0] & OTHER_KEYS_VALID;
    if (profile == Profile::Production && extra_enrolled != 0)
        || flags.iter().any(|v| v & OTHER_KEYS_VALID != extra_enrolled)
    {
        // Release must not silently revoke an existing independent signing key.
        // Dev may use the open slots, but a partially enrolled key is unsafe.
        return Err(Error::UnexpectedOtpConfiguration);
    }
    // This dedicated enrollment path accepts only boot-key validity/revocation
    // bits and secure-boot/debug/glitch-detector critical flags. Unknown OTP
    // configuration is not silently overwritten or interpreted as an
    // unprovisioned chip.
    if crit.iter().any(|v| v & !KNOWN_CRIT != 0) {
        return Err(Error::UnexpectedOtpConfiguration);
    }
    if flags
        .iter()
        .any(|v| v & !(ALL_VALID_MASK | OTHER_KEYS_INVALID) != 0)
    {
        return Err(Error::UnexpectedOtpConfiguration);
    }
    let enabled = crit.iter().any(|v| v & SECURE_BOOT_ENABLE != 0);
    let valid = flags.iter().any(|v| v & KEY0_VALID != 0);
    if (enabled || valid) && hash != expected {
        return Err(Error::UnexpectedOtpConfiguration);
    }
    if profile == Profile::Dev
        && (flags.iter().any(|v| v & OTHER_KEYS_INVALID != 0)
            || crit.iter().any(|v| v & DEBUG_MASK != 0))
    {
        // OTP bits can never be cleared. Do not misreport a hardened board as
        // dev/debuggable or try to downgrade production -> development.
        return Err(Error::ProfileConflict);
    }
    // All redundant copies must agree before we call a profile complete.
    let required_flags = match profile {
        Profile::Dev => KEY0_VALID,
        Profile::Production => KEY0_VALID | OTHER_KEYS_INVALID,
    };
    let required_crit = match profile {
        Profile::Dev => SECURE_BOOT_ENABLE | GLITCH_DETECTOR_ENABLE,
        Profile::Production => SECURE_BOOT_ENABLE | GLITCH_DETECTOR_ENABLE | DEBUG_MASK,
    };
    Ok(hash == expected
        && flags.iter().all(|v| v & required_flags == required_flags)
        && crit.iter().all(|v| v & required_crit == required_crit))
}

pub fn ensure_provisioned<F: ReadNorFlash>(flash: &SharedFlash<F>) -> Result<Outcome, Error> {
    // Derive the ROM boot-key fingerprint from the seal of the *running*
    // bootloader, not from a compile-time key or from application firmware.
    let expected = signing_key_hash(flash)?;
    let hash = read_hash()?;
    let flags = read_raw::<3>(FLAGS_ROW)?;
    let crit = read_raw::<8>(CRIT_ROW)?;
    if check_state(&hash, &expected, &flags, &crit, PROFILE)? {
        return Ok(Outcome::AlreadyEnabled);
    }

    // Key hash and validity always come first. Already-enrolled dev boards can
    // be upgraded to production using precisely the same ROM boot-signing key.
    if hash == [0; 32] {
        program_hash(&expected)?;
    }
    if read_hash()? != expected {
        return Err(Error::KeyReadbackFailed);
    }
    write_raw_bits(FLAGS_ROW, 3, KEY0_VALID)?;
    if !read_raw::<3>(FLAGS_ROW)?
        .iter()
        .all(|v| v & KEY0_VALID != 0)
    {
        return Err(Error::FlagsReadbackFailed);
    }
    if PROFILE == Profile::Production {
        // Close the three *unused* signing slots, preserving BOOTKEY0. A development
        // build intentionally never programs these irreversible invalid bits.
        write_raw_bits(FLAGS_ROW, 3, OTHER_KEYS_INVALID)?;
        if !read_raw::<3>(FLAGS_ROW)?
            .iter()
            .all(|v| v & OTHER_KEYS_INVALID == OTHER_KEYS_INVALID)
        {
            return Err(Error::FlagsReadbackFailed);
        }
        // Burn both debug disables before enabling secure boot. These take
        // effect at OTP reset; production will lose normal external SWD access.
        write_raw_bits(CRIT_ROW, 8, DEBUG_MASK)?;
        if !read_raw::<8>(CRIT_ROW)?
            .iter()
            .all(|v| v & DEBUG_MASK == DEBUG_MASK)
        {
            return Err(Error::SecureBootReadbackFailed);
        }
    }
    // Both profiles arm the glitch detectors from OTP. This takes effect at
    // the next OTP reset and then also covers the ROM's signature check of
    // Ferriboot. Boards enrolled by an older Ferriboot gain it on this boot.
    write_raw_bits(CRIT_ROW, 8, GLITCH_DETECTOR_ENABLE)?;
    if !read_raw::<8>(CRIT_ROW)?
        .iter()
        .all(|v| v & GLITCH_DETECTOR_ENABLE != 0)
    {
        return Err(Error::GlitchDetectorReadbackFailed);
    }
    // Only after key, key flags, glitch detectors and (for production) debug
    // policy have been verified should ROM signature enforcement be enabled.
    // In development->production upgrade this bit is already set and write_raw_bits() is
    // a no-op.
    write_raw_bits(CRIT_ROW, 8, SECURE_BOOT_ENABLE)?;
    if !read_raw::<8>(CRIT_ROW)?
        .iter()
        .all(|v| v & SECURE_BOOT_ENABLE != 0)
    {
        return Err(Error::SecureBootReadbackFailed);
    }
    Ok(Outcome::Provisioned)
}
