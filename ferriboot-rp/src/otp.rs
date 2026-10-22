//! The RP2350 bootrom OTP access API (`otp_access`), shared by the rollback
//! counter and the board's secure-boot enrollment.
//!
//! This is the crate's only call into the ROM's OTP routine. The ROM needs
//! an aligned buffer of exactly the requested length; the typed
//! [`OtpBuffer`] slices guarantee that, so [`access`] is a safe function.
//! RP2350 OTP: SDK pico/bootrom.h, datasheet §5.4 and §13.
use embassy_rp::{pac, rom_data};

/// BOOTROM_LOCK_OTP is BOOTRAM spinlock 2; the ROM OTP API must be
/// serialized even though Ferriboot only calls it from core 0.
const OTP_BOOTLOCK_INDEX: usize = 2;
const OTP_WRITE: u32 = 0x0001_0000;
const OTP_ECC: u32 = 0x0002_0000;

/// An OTP access failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtpError {
    /// Another agent holds the ROM's OTP lock.
    LockBusy,
    /// The ROM refused or failed a read, with its status code.
    Read(i32),
    /// The ROM refused or failed a write, with its status code.
    Write(i32),
}

/// The ROM distinguishes 16-bit ECC rows from 32-bit raw rows. Using typed
/// slices guarantees the buffer alignment required by the OTP API: 2 bytes
/// for ECC rows and 4 bytes for raw rows.
pub enum OtpBuffer<'a> {
    Ecc(&'a mut [u16]),
    Raw(&'a mut [u32]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
}

/// The OTP hardware lock is acquired by *reading* the BOOTRAM register and
/// released by writing it. Held across the synchronous ROM call, including
/// ROM error paths.
struct RomLock;

impl RomLock {
    fn acquire() -> Result<Self, OtpError> {
        if pac::BOOTRAM.bootlock(OTP_BOOTLOCK_INDEX).read() == 0 {
            return Err(OtpError::LockBusy);
        }
        Ok(Self)
    }
}

impl Drop for RomLock {
    fn drop(&mut self) {
        pac::BOOTRAM.bootlock(OTP_BOOTLOCK_INDEX).write_value(0);
    }
}

/// Read or write consecutive OTP rows starting at `row`, one row per buffer
/// element. Writes are raw (or ECC) programming: bits only go from 0 to 1.
///
/// Call on Secure Arm core 0 with interrupts disabled and core 1 stopped.
/// That is required for correct OTP behaviour, not for memory safety: the
/// ROM call is serialized by the BOOTRAM lock and only touches `buffer`.
pub fn access(row: u32, operation: Operation, buffer: OtpBuffer<'_>) -> Result<(), OtpError> {
    let (buf, len, ecc) = match buffer {
        OtpBuffer::Ecc(words) => (
            words.as_mut_ptr().cast::<u8>(),
            core::mem::size_of_val(words),
            true,
        ),
        OtpBuffer::Raw(words) => (
            words.as_mut_ptr().cast::<u8>(),
            core::mem::size_of_val(words),
            false,
        ),
    };
    let write = operation == Operation::Write;
    let cmd = row | (if write { OTP_WRITE } else { 0 }) | (if ecc { OTP_ECC } else { 0 });
    let _lock = RomLock::acquire()?;

    // SAFETY: `buf` points to a live, exclusively borrowed slice aligned to
    // 2 bytes for ECC or 4 bytes for raw rows, and `len` is its exact byte
    // length. The call is synchronous and serialized by BOOTRAM's ROM OTP
    // lock, so the ROM never accesses `buf` after it returns.
    let status = unsafe { rom_data::otp_access(buf, len, cmd) };
    match (status, write) {
        (0, _) => Ok(()),
        (status, true) => Err(OtpError::Write(status)),
        (status, false) => Err(OtpError::Read(status)),
    }
}
