#![no_std]
#![no_main]
//! RP2350 Ferriboot example firmware.
//!
//! Prints the running image version, confirms a trial upgrade, and uses an
//! Embassy heartbeat task to blink GPIO15. A record in the `storage` partition
//! can select unauthenticated, destructive test-hook behaviour by image version.
//! Do not deploy the example with test hooks in production.

use core::cell::RefCell;

use embassy_executor::Spawner;
use embassy_rp::{
    gpio::{Level, Output},
    uart::UartTx,
    watchdog::Watchdog,
};
use embassy_time::{Duration, Timer};
use embedded_storage::nor_flash::NorFlash;
use ferriboot_core::{FirmwareUpdater, ImageVersion, SlotLayout};
use ferriboot_layout_rp2350_pico2::STORAGE_OFFSET;
use ferriboot_rp::{BootInfo, Partition, SharedFlash, hardware, uart_logger};
use ferriboot_test_hooks::{Behaviour, Config, Hook, RECORD_LEN};
use log::{error, info};

// Below the RP2350's 24-bit watchdog limit and the same as the bootloader.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(15);
/// Compile-time checked flash layout shared with Ferriboot.
const LAYOUT: ferriboot_rp::Layout =
    ferriboot_rp::Layout::new(ferriboot_layout_rp2350_pico2::FLASH_MAP);

// Deliberate test faults must reboot through Ferriboot without confirming.
#[cortex_m_rt::exception]
unsafe fn HardFault(_frame: &cortex_m_rt::ExceptionFrame) -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    error!("firmware panic: {}", info);
    cortex_m::peripheral::SCB::sys_reset()
}

fn load_hook_config<F: NorFlash>(flash: &SharedFlash<F>) -> Option<Config> {
    let mut record = [0; RECORD_LEN];
    flash.borrow_mut().read(STORAGE_OFFSET, &mut record).ok()?;
    Config::from_bytes(record)
}

// Test hooks are handled before enabling interrupts and launching Embassy.
fn idle(watchdog: &RefCell<Watchdog>, timeout: Duration) -> ! {
    info!("@fbt idle");
    loop {
        watchdog.borrow_mut().feed(timeout);
        cortex_m::asm::delay(embassy_rp::clocks::clk_sys_freq() / 5);
    }
}

#[embassy_executor::task]
async fn heartbeat(mut led: Output<'static>) -> ! {
    let mut ticks = 0u32;
    loop {
        led.toggle();
        Timer::after_millis(500).await;
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(20) {
            info!("heartbeat: {} seconds", ticks / 2);
        }
    }
}

async fn keep_alive(watchdog: &RefCell<Watchdog>, timeout: Duration) -> ! {
    loop {
        watchdog.borrow_mut().feed(timeout);
        Timer::after_millis(1_000).await;
    }
}

fn fail(what: &str) -> ! {
    info!("@fbt error {}", what);
    // A failed test run must not appear healthy: allow the watchdog to reset.
    loop {
        core::hint::spin_loop();
    }
}

fn run_hook<F: NorFlash>(
    hook: Hook,
    boot: &BootInfo,
    layout: &SlotLayout,
    flash: &SharedFlash<F>,
    mut updater: FirmwareUpdater<Partition<'_, F>>,
    version: &ImageVersion,
    watchdog: &RefCell<Watchdog>,
) -> ! {
    let timeout = if hook.watchdog_secs == 0 {
        WATCHDOG_TIMEOUT
    } else if hook.watchdog_secs <= 15 {
        Duration::from_secs(u64::from(hook.watchdog_secs))
    } else {
        fail("watchdog-timeout")
    };
    // A connected probe must not prevent a watchdog test from expiring.
    watchdog.borrow_mut().pause_on_debug(false);
    watchdog.borrow_mut().feed(timeout);
    info!(
        "@fbt hello version={} behaviour={} reset={:?}",
        version,
        hook.raw,
        watchdog.borrow().reset_reason()
    );
    info!(
        "@fbt bootinfo mode={:?} slot={:?} max={} bootloader={:?}",
        boot.mode(),
        boot.active_slot(),
        boot.max_image_size().unwrap_or(0),
        boot.bootloader_version()
    );
    let pending = updater
        .needs_confirmation()
        .unwrap_or_else(|_| fail("trailer-unreadable"));
    info!("@fbt image version={} pending={}", version, pending);
    // ProgressFlash fed the default timeout during I/O; apply the test
    // override after the trailer read, before any intentional hang/fault.
    watchdog.borrow_mut().feed(timeout);
    match hook.behaviour.unwrap_or_else(|| fail("unknown-behaviour")) {
        Behaviour::Normal => unreachable!("normal hooks are filtered by Config::select"),
        Behaviour::Confirm => {
            if pending {
                updater.confirm().unwrap_or_else(|_| fail("confirm"));
                info!("@fbt confirmed");
            }
            idle(watchdog, timeout)
        }
        Behaviour::Idle => idle(watchdog, timeout),
        Behaviour::ResetNoConfirm => {
            info!("@fbt resetting");
            cortex_m::peripheral::SCB::sys_reset()
        }
        Behaviour::Panic => panic!("deliberate test panic"),
        Behaviour::Fault => {
            info!("@fbt fault");
            // UDF escalates to HardFault with the inherited PRIMASK set.
            cortex_m::asm::udf()
        }
        Behaviour::HangNoFeed => {
            info!("@fbt hang");
            loop {
                core::hint::spin_loop();
            }
        }
        Behaviour::EraseActive => {
            // The XIP payload shares its first sector with the header.
            // Invalidate only its magic without erasing currently executing code.
            let region = layout.region(boot.active_slot());
            flash
                .borrow_mut()
                .write(region.offset, &[0; 4])
                .unwrap_or_else(|_| fail("erase-active"));
            info!("@fbt erased-active");
            cortex_m::peripheral::SCB::sys_reset()
        }
    }
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    // Ferriboot hands off with interrupts masked. Hooks use polling before
    // enabling interrupts for Embassy below.
    let p = embassy_rp::init(Default::default());
    uart_logger::init(UartTx::new_blocking(
        p.UART0,
        p.PIN_0,
        uart_logger::config(),
    ));
    let watchdog = RefCell::new(Watchdog::new(p.WATCHDOG));
    let boot = BootInfo::read().expect("Ferriboot handoff required");
    let flash = ferriboot_rp::share(ferriboot_core::flash::ProgressFlash::new(
        hardware::flash::<{ LAYOUT.flash_size() }>(p.FLASH),
        || watchdog.borrow_mut().feed(WATCHDOG_TIMEOUT),
    ));
    let hooks = load_hook_config(&flash);
    let mut updater = ferriboot_rp::updater(&LAYOUT, &boot, &flash)
        .expect("valid bootloader strategy/verifier/layout");
    let active = updater.inspect_active().expect("running image header");
    info!(
        "running image {} in {:?}, mode {:?}",
        active.version,
        boot.active_slot(),
        boot.mode()
    );

    if let Some(hook) = hooks.and_then(|config| config.select(&active.version)) {
        run_hook(
            hook,
            &boot,
            LAYOUT.slots(),
            &flash,
            updater,
            &active.version,
            &watchdog,
        );
    }

    // The Embassy time driver requires interrupts after handoff.
    // SAFETY: not inside a critical section; the HAL and executor are
    // initialized and every bound interrupt handler is ready to run.
    unsafe { cortex_m::interrupt::enable() };
    spawner.spawn(heartbeat(Output::new(p.PIN_15, Level::Low)).expect("spawn heartbeat"));
    // Example health-check placeholder, as in the original application.
    Timer::after_millis(10).await;
    if updater.needs_confirmation().expect("read trailer") {
        updater.confirm().expect("confirm trial image");
    }
    info!("application healthy; image confirmed; UART log console ready");
    keep_alive(&watchdog, WATCHDOG_TIMEOUT).await
}
