//! ESP32-C3 clock glitch detector reset.
//!
//! The C3's analog clock glitch detector resets the digital core and RTC
//! (reset reason `SysClkGlitch`) when it sees a glitch on the clock. ESP-IDF's
//! second stage already enables it on rev v0.3+ (`bootloader_ana_reset_config`);
//! this module re-asserts and verifies it, so Ferriboot does not silently depend
//! on that stage's configuration. The register sequence mirrors ESP-IDF's
//! `bootloader_ana_clock_glitch_reset_config(true)`.
//!
//! The C3 has no lock for this setting: the application can turn it off again.
//! It is not a power/voltage or EM glitch detector, and IDF documents no such
//! detector for this chip.

use esp_hal::{
    efuse::{self, ChipRevision},
    peripherals::LPWR,
    rtc_cntl::SocResetReason,
};

/// `RTC_CNTL_FIB_SEL.FIB_GLITCH_RST`: clear for software control of the
/// glitch reset enable, instead of the analog default.
const FIB_GLITCH_RST: u8 = 1 << 0;

/// ESP-IDF disables glitch reset on earlier revisions: it was fixed in ECO3.
const MIN_REVISION: ChipRevision = ChipRevision { major: 0, minor: 3 };

/// What this boot found when enabling the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// The last reset was caused by a clock or power glitch detector.
    pub reset_by_glitch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Glitch reset is unreliable on this silicon revision.
    UnsupportedRevision(ChipRevision),
    /// The enable did not read back as set.
    NotEnabled,
}

/// Enable and verify the clock glitch reset. Call once, early in the
/// third-stage bootloader, after `esp_hal::init`.
pub fn enable() -> Result<Report, Error> {
    let report = Report {
        reset_by_glitch: matches!(
            esp_hal::system::reset_reason(),
            Some(SocResetReason::SysClkGlitch | SocResetReason::CorePwrGlitch)
        ),
    };
    let revision = efuse::chip_revision();
    if revision < MIN_REVISION {
        return Err(Error::UnsupportedRevision(revision));
    }

    let rtc = LPWR::regs();
    rtc.fib_sel().modify(|r, w| {
        // SAFETY: only FIB_GLITCH_RST changes; the brownout and super-WDT
        // selections keep their current values.
        unsafe { w.fib_sel().bits(r.fib_sel().bits() & !FIB_GLITCH_RST) }
    });
    rtc.ana_conf().modify(|_, w| w.glitch_rst_en().set_bit());

    if rtc.fib_sel().read().fib_sel().bits() & FIB_GLITCH_RST != 0
        || rtc.ana_conf().read().glitch_rst_en().bit_is_clear()
    {
        return Err(Error::NotEnabled);
    }
    Ok(report)
}
