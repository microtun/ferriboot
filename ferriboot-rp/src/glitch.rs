//! RP2350 glitch detectors (datasheet §10.9, "Glitch detector").
//!
//! The four on-die detectors watch for abnormal power and clock events. Once
//! armed, any trigger restarts the switched core power domain's power-on
//! state machine and latches `POWMAN.CHIP_RESET.HAD_GLITCH_DETECT`.
//!
//! The bootloader's OTP enrollment also burns `CRIT1.GLITCH_DETECTOR_ENABLE`,
//! which arms the detectors from power-on so they cover the mask ROM. This
//! module is still required on every boot:
//!
//! * Before that bit has taken effect (the enrollment boot and anything
//!   earlier), the register write is the only thing arming them.
//! * OTP arming can be overridden at runtime through `DISARM`. Setting `ARM`
//!   makes `DISARM` ignored, and `LOCK` stops the application changing
//!   either until the next reset.
//!
//! Sensitivity stays at the OTP-provided default (`CRIT1.GLITCH_DETECTOR_SENS`).

use embassy_rp::pac::{
    self,
    glitch_detector::{regs, vals},
};

/// Any nonzero value disables writes to ARM, DISARM, SENSITIVITY and LOCK.
const LOCKED: u8 = 0xff;
/// TRIG_STATUS.DET0..DET3.
const ALL_DETECTORS: u32 = 0xf;

/// What the detectors recorded before this boot armed them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// The last chip reset was caused by an armed glitch detector.
    pub reset_by_glitch: bool,
    /// TRIG_STATUS bits (DET0..DET3) recorded while the detectors were
    /// unarmed or before the last reset. Cleared by [`arm_and_lock`].
    pub triggered: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// ARM did not read back as armed.
    NotArmed,
    /// SENSITIVITY does not select the OTP default.
    SensitivityOverridden,
    /// LOCK did not read back as set.
    NotLocked,
}

/// Arm the glitch detectors at their OTP-default sensitivity and lock the
/// configuration until the next reset, so the Secure application can neither
/// disarm nor desensitise them.
///
/// Call first thing in the bootloader: Secure core 0, before clock
/// configuration and before any secret-dependent decision. If LOCK is already
/// set (for example by an earlier boot without an intervening reset of the
/// block), the configuration writes are ignored and the read-back alone
/// decides the result.
pub fn arm_and_lock() -> Result<Report, Error> {
    let detector = pac::GLITCH_DETECTOR;
    let report = Report {
        reset_by_glitch: pac::POWMAN.chip_reset().read().had_glitch_detect(),
        triggered: (detector.trig_status().read().0 & ALL_DETECTORS) as u8,
    };

    // Clear stale trigger history (write-1-clear) before arming. A detector
    // that is still in a failed state may report high again immediately.
    detector
        .trig_status()
        .write_value(regs::TrigStatus(u32::from(report.triggered)));

    detector
        .sensitivity()
        .write(|w| w.set_default(vals::Default::YES));
    detector.arm().write(|w| w.set_arm(vals::Arm::YES));
    detector.lock().write(|w| w.set_lock(LOCKED));

    // Hardware treats any value other than the NO patterns as YES.
    if detector.arm().read().arm() == vals::Arm::NO {
        return Err(Error::NotArmed);
    }
    if detector.sensitivity().read().default() == vals::Default::NO {
        return Err(Error::SensitivityOverridden);
    }
    if detector.lock().read().lock() == 0 {
        return Err(Error::NotLocked);
    }
    Ok(report)
}
