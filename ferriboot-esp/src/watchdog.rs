//! RTC watchdog setup shared by the third-stage bootloader and OTA firmware.
//!
//! `esp_hal::init` disables the inherited RTC watchdog. Both stages must arm
//! their own watchdog after initialization; the bootloader's watchdog stays
//! active through the RAM handoff until the application initializes its HAL.

use esp_hal::{
    peripherals::RTC_TIMER,
    rtc_cntl::{Rtc, Rwdt, RwdtStage},
    time::Duration,
};

/// Start the ESP32-C3 RTC watchdog with a system reset at stage 0.
///
/// The timer runs independently of instruction fetch, so it continues through
/// the destructive RAM handoff. Call early from the application as well: its
/// own `esp_hal::init` disables the watchdog inherited from the bootloader.
pub fn start_rtc(timer: RTC_TIMER<'_>, timeout: Duration) -> Rwdt {
    let mut watchdog = Rtc::new(timer).rwdt;
    watchdog.set_timeout(RwdtStage::Stage0, timeout);
    // The HAL enables only stage 0, configured to reset the whole system.
    watchdog.enable();
    watchdog.feed();
    watchdog
}
