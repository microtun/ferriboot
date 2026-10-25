//! Per-boot OTP write protection for the authenticated Secure application.
//!
//! Enrollment and rollback-counter advancement happen *before* this module is
//! called. The OTP SW_LOCK registers only tighten permissions until the next
//! OTP reset; no fuses are programmed here. The next boot through Ferriboot
//! restores the ability to advance the counter before locking it again.

use embassy_rp::pac;

// Each RP2350 OTP page has 64 rows (datasheet, OTP_SW_LOCKx):
//   page 1: rows 0x040..0x07f (CRIT1 and BOOT_FLAGS)
//   page 2: rows 0x080..0x0bf (BOOTKEY0..BOOTKEY3)
//   page 32: rows 0x800..0x83f (Ferriboot's application rollback counter)
const PROTECTED_PAGES: [usize; 3] = [1, 2, 32];

// SW_LOCKx.SEC[1:0] and .NSEC[3:2] are thermometer-coded:
// 00 = read/write, 01 = read-only, 11 = inaccessible.
// Writes OR bits into the existing state. Set the low bit of *both* fields;
// never write 11 intentionally, since the application may still read OTP.
const READ_ONLY_BOTH: u32 = 0b0101;
const BOOTDIS_NEXT: u32 = 1 << 1;
const POWMAN_PASSWORD: u32 = 0x5afe_0000;

#[derive(Debug)]
pub enum Error {
    PageStillWritable,
    OtpBootBypassEnabled,
    PowmanBootBypassEnabled,
}

/// Prevent application writes to Ferriboot's critical OTP pages until reset.
///
/// # Preconditions
/// Called in Secure privileged mode with interrupts masked and core 1 stopped,
/// after the OTP enrollment/verification and security-counter update, just
/// before the application handoff. Afterwards the bootloader only *reads*
/// OTP (the handoff confirmation re-reads the rollback floor); it never
/// writes it again.
pub fn protect_application() -> Result<(), Error> {
    for page in PROTECTED_PAGES {
        // SW_LOCK writes are OR'd into the current state, including locks
        // installed by ROM or earlier stages. Do not use read/modify/write:
        // an inaccessible (11) page must remain inaccessible.
        pac::OTP
            .sw_lock(page)
            .write_value(pac::otp::regs::SwLock(READ_ONLY_BOTH));
        let state = pac::OTP.sw_lock(page).read().0;
        if state & READ_ONLY_BOTH != READ_ONLY_BOTH {
            return Err(Error::PageStillWritable);
        }
    }

    // Locks reset when the OTP power-on state machine runs. Block a Secure
    // application from setting watchdog/POWMAN scratch boot vectors to skip
    // Ferriboot on that reset and run while the OTP pages are unlocked.
    // The ROM specifically recommends setting both BOOTDIS.NEXT latches.
    pac::OTP
        .bootdis()
        .write_value(pac::otp::regs::Bootdis(BOOTDIS_NEXT));
    if pac::OTP.bootdis().read().0 & BOOTDIS_NEXT == 0 {
        return Err(Error::OtpBootBypassEnabled);
    }

    // All POWMAN registers through 0xac require the 0x5afe write password.
    // Writing NEXT=1 (and NOW=0) cannot relax an existing latch.
    pac::POWMAN
        .bootdis()
        .write_value(pac::powman::regs::Bootdis(POWMAN_PASSWORD | BOOTDIS_NEXT));
    if pac::POWMAN.bootdis().read().0 & BOOTDIS_NEXT == 0 {
        return Err(Error::PowmanBootBypassEnabled);
    }

    Ok(())
}
