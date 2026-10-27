#![no_std]
#![no_main]
//! QEMU mps2-an385 Ferriboot example firmware.
//!
//! Reads the running image and confirms a pending trial. An unauthenticated
//! `FBTHOOK1` record can select destructive test-hook behaviour. Without a
//! matching hook the example confirms a trial and runs an Embassy heartbeat.

use embassy_executor::Spawner;
use embassy_time::Timer;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use ferriboot_core::{FirmwareUpdater, ImageVersion};
use ferriboot_qemu as platform;
use ferriboot_test_hooks::{Behaviour, Config, Hook, RECORD_LEN};
use log::info;

mod time_driver;

/// Compile-time checked flash layout shared with Ferriboot.
const LAYOUT: platform::Layout = platform::Layout::new(ferriboot_layout_qemu::FLASH_MAP);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    log::error!("firmware panic: {info}");
    platform::reset()
}

#[cortex_m_rt::exception]
fn SysTick() {
    time_driver::on_tick();
}

#[cortex_m_rt::exception]
unsafe fn HardFault(_: &cortex_m_rt::ExceptionFrame) -> ! {
    platform::reset()
}

fn load_hook_config() -> Option<Config> {
    let mut record = [0; RECORD_LEN];
    platform::Partition::new(ferriboot_layout_qemu::STORAGE)
        .read(0, &mut record)
        .ok()?;
    Config::from_bytes(record)
}

#[embassy_executor::task]
async fn heartbeat() -> ! {
    let mut ticks = 0u32;
    loop {
        Timer::after_millis(500).await;
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(20) {
            info!("heartbeat: {} seconds", ticks / 2);
        }
    }
}

async fn keep_alive() -> ! {
    loop {
        Timer::after_secs(1).await;
    }
}

fn idle() -> ! {
    info!("@fbt idle");
    platform::halt()
}

fn fail(what: &str) -> ! {
    info!("@fbt error {}", what);
    platform::reset()
}

fn run_hook(
    hook: Hook,
    boot: &ferriboot_core::shared_data::BootInfo,
    mut updater: FirmwareUpdater<platform::Partition>,
    version: &ImageVersion,
) -> ! {
    let reset = if platform::watchdog_reset() {
        "Some(TimedOut)"
    } else {
        "None"
    };
    info!(
        "@fbt hello version={} behaviour={} reset={}",
        version, hook.raw, reset
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
    match hook.behaviour.unwrap_or_else(|| fail("unknown-behaviour")) {
        Behaviour::Normal => unreachable!("normal hooks are filtered by Config::select"),
        Behaviour::Confirm => {
            if pending {
                updater.confirm().unwrap_or_else(|_| fail("confirm"));
                info!("@fbt confirmed");
            }
            idle()
        }
        Behaviour::Idle => idle(),
        Behaviour::ResetNoConfirm => {
            info!("@fbt resetting");
            platform::reset()
        }
        Behaviour::Panic => panic!("deliberate test panic"),
        Behaviour::Fault => {
            info!("@fbt fault");
            cortex_m::asm::udf()
        }
        Behaviour::HangNoFeed => {
            info!("@fbt hang");
            platform::watchdog(if hook.watchdog_secs == 0 {
                15
            } else {
                hook.watchdog_secs
            });
            loop {
                core::hint::spin_loop();
            }
        }
        Behaviour::EraseActive => {
            LAYOUT
                .partition(boot.active_slot())
                .write(0, &[0; 4])
                .unwrap_or_else(|_| fail("erase-active"));
            info!("@fbt erased-active");
            platform::reset()
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    platform::init();
    let boot = platform::boot_info();
    let slot = LAYOUT
        .slots()
        .active_slot::<platform::Partition>(&boot)
        .expect("boot handoff does not match the application layout");
    let mut updater = FirmwareUpdater::new_with_mode(
        LAYOUT.partition(slot),
        LAYOUT.partition(slot.other()),
        boot.mode(),
    )
    .expect("valid bootloader strategy/layout");
    let active = updater.inspect_active().expect("running image header");
    info!(
        "running image {} in {:?}, mode {:?}",
        active.version,
        boot.active_slot(),
        boot.mode()
    );
    if let Some(hook) = load_hook_config().and_then(|config| config.select(&active.version)) {
        run_hook(hook, &boot, updater, &active.version);
    }
    if updater.needs_confirmation().expect("read trailer") {
        updater.confirm().expect("confirm trial image");
    }
    // Ferriboot hands off with NVIC sources quiesced and PRIMASK=0.
    // Destructive hooks run before the application starts its timer.
    time_driver::init(
        cortex_m::Peripherals::take()
            .expect("core peripherals")
            .SYST,
    );
    Timer::after_millis(10).await;
    info!("application healthy; image confirmed; virtual console ready");
    spawner.spawn(heartbeat().expect("spawn heartbeat"));
    keep_alive().await
}
