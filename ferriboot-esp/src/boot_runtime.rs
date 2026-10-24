//! `esp-hal` boot-time services for the ESP32-C3 third-stage bootloader.
//!
//! This module is intentionally not used by the standalone `bl-loader` RAM
//! payload: that payload overwrites the memory holding the HAL and must use
//! ROM entry points after the handoff has begun.

use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    peripherals::{Peripherals, RTC_TIMER, TIMG0},
    rtc_cntl::Rwdt,
    time::Duration,
};

/// Initialise the HAL for the factory Ferriboot application.
///
/// Clear the previous SRAM handoff *before* HAL startup, as a reset may leave
/// stale boot information behind. `esp_hal::init` configures clocks and turns
/// off the ROM's watchdogs; the C3-specific flash-boot mode bit additionally
/// needs to be cleared before a lengthy validation or flash swap.
///
/// Call exactly once, at the beginning of the third-stage bootloader (not in
/// an OTA application which has already initialised the HAL).
pub fn init() -> Peripherals {
    crate::BootInfo::invalidate();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    finish_flashboot_protection();
    peripherals
}

/// Clear only the C3 TIMG0 flash-boot protection bit. The HAL's standard WDT
/// driver can enable/disable the complete MWDT, but has no operation for this
/// ROM boot-mode bit. Leave the rest of the watchdog configuration unchanged.
fn finish_flashboot_protection() {
    let tg0 = TIMG0::regs();
    if tg0.wdtconfig0().read().wdt_flashboot_mod_en().bit_is_set() {
        // The PAC reset value unlocks WDT write protection on ESP32-C3.
        tg0.wdtwprotect().reset();
        tg0.wdtconfig0()
            .modify(|_, w| w.wdt_flashboot_mod_en().clear_bit());
        tg0.wdtconfig0()
            .modify(|_, w| w.wdt_conf_update_en().set_bit());
        tg0.wdtwprotect().write(|w| {
            // SAFETY: zero is the documented C3 watchdog lock value.
            unsafe { w.wdt_wkey().bits(0) }
        });
    }
}

/// Deadline between two completed flash operations while the bootloader
/// validates or swaps images, and for application startup after the handoff.
pub const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(15);

/// Arm the RTC watchdog at the start of the third-stage bootloader, right
/// after [`init`] (which disables the watchdog inherited from the ROM).
///
/// Feed it after every completed flash operation by wrapping the flash in
/// [`ferriboot_core::flash::ProgressFlash`]: validation and restartable swaps
/// of any slot size then stay within the deadline, while a stuck flash or
/// eFuse operation resets the device and the interrupted operation resumes
/// on the next boot. Feed it once more immediately before the handoff. The
/// driver need not be polled during the SRAM handoff; its hardware timer keeps
/// running while the loader overwrites the bootloader's DRAM. The application
/// must take responsibility for the watchdog after the jump.
pub fn start_watchdog(timer: RTC_TIMER<'_>) -> Rwdt {
    crate::watchdog::start_rtc(timer, WATCHDOG_TIMEOUT)
}

/// Give native USB-Serial-JTAG diagnostics time to drain, then reset.
///
/// Only for the ordinary HAL-backed bootloader, before the destructive handoff.
pub fn reset_after_delay() -> ! {
    crate::BootInfo::invalidate();
    Delay::new().delay_millis(2_000);
    esp_hal::system::software_reset()
}
