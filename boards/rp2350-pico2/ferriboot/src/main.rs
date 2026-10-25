#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
//! ROM loads Ferriboot into SRAM -> authenticated Arm Secure application in XIP flash.
use core::cell::RefCell;

use embassy_rp::{bind_interrupts, peripherals::TRNG, trng, uart::UartTx};
use ferriboot_core::{Error, PublicKey, flash::ProgressFlash};
use ferriboot_rp::{BootError, BootInfo, glitch, hardware};
use log::{error, info, warn};

// Every RP2350 build enrolls/verifies ROM secure boot. `production` selects
// irreversible debug/key-slot lockdown and implies the required boot policy.
// Without that feature, the OTP policy remains development-oriented.

mod config;
mod otp_lock;
mod otp_provision;
use ferriboot_rp::uart_logger;
// Below RP2350's 24-bit watchdog limit; match the other board crate. The
// same deadline covers bootloader work (fed after every flash operation) and
// application startup after the handoff.
const WATCHDOG_TIMEOUT: embassy_time::Duration = embassy_time::Duration::from_secs(15);

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

/// The board's flash map, checked against the RP2350's rules at compile time.
const LAYOUT: ferriboot_rp::Layout =
    ferriboot_rp::Layout::new(ferriboot_layout_rp2350_pico2::FLASH_MAP);

// The bootloader only uses the blocking TRNG API, which never unmasks this
// interrupt; the binding is required by the driver's constructor.
bind_interrupts!(struct Irqs {
    TRNG_IRQ => trng::InterruptHandler<TRNG>;
});

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    error!("bootloader panic: {}", info);
    reset_after_delay()
}

/// Never hand off after an error, even one that appears permanent. A later
/// boot may succeed if a read or check was affected by a transient fault.
fn reset_after_delay() -> ! {
    BootInfo::invalidate();
    // UART records have already drained synchronously. Back off before every
    // retry so a persistent fault does not cause a tight reboot loop.
    cortex_m::asm::delay(embassy_rp::clocks::clk_sys_freq().saturating_mul(2));
    cortex_m::peripheral::SCB::sys_reset()
}

#[cortex_m_rt::entry]
fn main() -> ! {
    cortex_m::interrupt::disable();
    // Take the core peripherals for the final handoff before embassy-rp's
    // stack-guard setup steals them (which would make `take` return None).
    let core_peripherals = cortex_m::Peripherals::take().expect("core peripherals already taken");
    BootInfo::invalidate();
    // Arm and lock the glitch detectors before clock setup, slot validation
    // or any OTP access. The result is reported once the UART is available.
    let glitch_setup = glitch::arm_and_lock();
    let p = embassy_rp::init(Default::default());
    // Restart the timer (a software reset may leave the application's one
    // running) with a full deadline. Every completed flash operation feeds
    // it, so validation and restartable swaps of any slot size are covered,
    // while a stuck flash, OTP or TRNG operation resets the device and the
    // interrupted operation is resumed on the next boot.
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    watchdog.pause_on_debug(true);
    watchdog.start(WATCHDOG_TIMEOUT);
    let watchdog = RefCell::new(watchdog);
    let feed_watchdog = || watchdog.borrow_mut().feed(WATCHDOG_TIMEOUT);
    uart_logger::init(UartTx::new_blocking(
        p.UART0,
        p.PIN_0,
        uart_logger::config(),
    ));
    embassy_rp::install_core0_stack_guard().expect("bootloader stack guard");
    info!(
        "Ferriboot {} for RP2350 (Arm Secure), mode {:?}",
        env!("CARGO_PKG_VERSION"),
        config::MODE
    );
    match glitch_setup {
        Ok(report) => {
            if report.reset_by_glitch {
                warn!("last reset was caused by a glitch detector");
            }
            if report.triggered != 0 {
                warn!(
                    "glitch detectors previously triggered: {:#06b}",
                    report.triggered
                );
            }
            info!("glitch detectors armed and locked");
        }
        Err(err) => {
            // Never run an application without the configured protection.
            error!("glitch detector setup failed: {:?}; resetting", err);
            reset_after_delay();
        }
    }
    // Entropy for the random delays in front of fault-hardened checks.
    let mut trng = trng::Trng::new(p.TRNG, Irqs, trng::Config::default());
    let flash = ferriboot_rp::share(ProgressFlash::new(
        hardware::flash::<{ LAYOUT.flash_size() }>(p.FLASH),
        &feed_watchdog,
    ));
    let slots = LAYOUT.slots();
    info!(
        "primary {:#x}+{:#x}, secondary {:#x}+{:#x}, scratch {:?}",
        slots.primary.offset,
        slots.primary.size,
        slots.secondary.offset,
        slots.secondary.size,
        slots.scratch
    );
    let keys =
        PUBLIC_KEYS.map(|raw| PublicKey::from_raw(&raw).expect("invalid built-in P-256 key"));
    // SAFETY: Secure core 0, IRQs disabled, core 1 off; page 32 is reserved
    // exclusively for the application rollback epoch (see board README).
    let mut security_counter = unsafe { ferriboot_rp::security_counter::OtpSecurityCounter::new() };
    let plan = match ferriboot_rp::prepare_with_security_counter_store(
        &LAYOUT,
        &flash,
        &keys,
        config::boot_config(),
        Some(config::version()),
        &mut security_counter,
        &mut trng,
    ) {
        Ok(plan) => plan,
        Err(BootError::Boot(Error::Flash(e))) => {
            error!("flash failure: {:?}; resetting to resume the operation", e);
            reset_after_delay()
        }
        Err(e) => {
            error!("no executable application: {:?}; resetting to retry", e);
            reset_after_delay()
        }
    };
    // Give enrollment a full deadline: its OTP accesses do not touch flash.
    // A reset part-way through is handled like a power loss.
    feed_watchdog();
    // A verified MCUboot application must exist before we commit any permanent
    // ROM security OTP. Always finish enrollment/verification and reboot
    // through ROM (now enforcing signatures) before executing the app.
    match otp_provision::ensure_provisioned(&flash) {
        Ok(otp_provision::Outcome::AlreadyEnabled) => {}
        Ok(otp_provision::Outcome::Provisioned) => {
            info!("RP2350 secure boot OTP verified; rebooting into authenticated ROM path");
            cortex_m::peripheral::SCB::sys_reset();
        }
        Err(err) => {
            error!(
                "RP2350 secure boot OTP provisioning refused/failed: {:?}; resetting",
                err
            );
            // A refusal or inconsistent readback still blocks handoff. Every
            // new boot rechecks OTP before attempting an irreversible write.
            reset_after_delay();
        }
    }
    info!(
        "starting image {} from {:?}, operation {:?}, VTOR {:#010x}, SP {:#010x}, reset {:#010x}",
        plan.decision().image.header().version,
        plan.decision().slot,
        plan.decision().performed,
        plan.vector_address(),
        plan.stack_pointer(),
        plan.reset_handler()
    );
    // Enrollment and any rollback-counter writes are complete. Do not pass
    // OTP write access to a Secure/privileged application: lock the ROM key,
    // critical flags, and counter pages for this boot before handoff.
    if let Err(err) = otp_lock::protect_application() {
        error!("RP2350 OTP handoff lock failed: {:?}; resetting", err);
        reset_after_delay();
    }
    // Point of no return. Authenticate the image a second time, re-read its
    // vectors and the rollback floor, then decide after a random delay. A
    // disagreement panics (resetting); there is no error path to skip. Keep
    // this after the last UART output so it cannot be timed from the log.
    let plan = plan.confirm(&flash, &keys, &mut security_counter, &mut trng);
    // Protect application startup before its reset handler executes: hand
    // over the running timer with a full deadline (never stopped since the
    // bootloader started). An attached debugger may pause it.
    feed_watchdog();
    // UART TX is fully drained. Nothing mutates flash after prepare.
    // The HAL time driver supplies the blocking UART configuration delay;
    // no executor or async timers are used. The bootloader never starts UART
    // IRQs, DMA, USB, or core 1. Publish the handoff only after all checks.
    // SAFETY: privileged Secure core 0 thread mode, quiescent peripherals and
    // unchanged authenticated image, with a checked linker/vector contract.
    unsafe { hardware::execute(&plan, core_peripherals) }
}
