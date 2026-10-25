#![no_std]
#![no_main]
//! ESP32-C3 Ferriboot example firmware.
//!
//! Prints the running image version and confirms a trial upgrade. A record
//! in the `storage` partition can select test-hook behaviour by image version.
//! These hooks are unauthenticated and unsafe for production deployment; with
//! no matching record, the example runs its normal Embassy heartbeat.

use core::cell::RefCell;

use embassy_executor::Spawner;
use embassy_time::Timer;
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use esp_hal::{
    clock::CpuClock,
    rtc_cntl::{Rwdt, RwdtStage},
    time::{Duration, Instant},
    timer::timg::TimerGroup,
};
use esp_println::println;
use ferriboot_core::{FirmwareUpdater, ImageVersion, SlotLayout};
use ferriboot_esp::{BootInfo, FlashStorage, SharedFlash, SharedStorage, StoragePartition};
use ferriboot_test_hooks::{Behaviour, Config, Hook, RECORD_LEN};
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    println!("firmware panic: {}", info);
    esp_hal::system::software_reset()
}

/// Read the shared test record before moving the flash into ProgressFlash.
fn load_hook_config(flash: &SharedFlash<'_>) -> Option<Config> {
    flash.lock(|f| {
        let mut f = f.borrow_mut();
        let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
        let offset = partitions::read_partition_table(&mut f, &mut table)
            .ok()?
            .iter()
            .find(|p| p.label_as_str() == "storage")?
            .offset();
        let mut record = [0u8; RECORD_LEN];
        f.read(offset, &mut record).ok()?;
        Config::from_bytes(record)
    })
}

// Hooks run before esp-rtos starts, so their idle loop uses polling.
fn idle(watchdog: &RefCell<Rwdt>) -> ! {
    println!("@fbt idle");
    loop {
        watchdog.borrow_mut().feed();
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(200) {}
    }
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

async fn keep_alive(watchdog: &RefCell<Rwdt>) -> ! {
    loop {
        watchdog.borrow_mut().feed();
        Timer::after_millis(1_000).await;
    }
}

fn fail(what: &str) -> ! {
    println!("@fbt error {}", what);
    // A failed test run must not appear healthy: allow the watchdog to reset.
    loop {
        core::hint::spin_loop();
    }
}

/// Report the shared `@fbt` protocol used by the board example firmware.
fn run_hook<F: NorFlash>(
    hook: Hook,
    boot: &BootInfo,
    layout: &SlotLayout,
    flash: &SharedStorage<F>,
    mut updater: FirmwareUpdater<StoragePartition<'_, F>>,
    version: &ImageVersion,
    watchdog: &RefCell<Rwdt>,
) -> ! {
    println!(
        "@fbt hello version={} behaviour={} reset={:?}",
        version,
        hook.raw,
        esp_hal::system::reset_reason()
    );
    println!(
        "@fbt bootinfo mode={:?} slot={:?} max={} bootloader={:?}",
        boot.mode(),
        boot.active_slot(),
        boot.max_image_size().unwrap_or(0),
        boot.bootloader_version()
    );
    let pending = updater
        .needs_confirmation()
        .unwrap_or_else(|_| fail("trailer-unreadable"));
    println!("@fbt image version={} pending={}", version, pending);

    // Flash operations use the inherited default deadline. Apply a shorter
    // test timeout only after the trailer has been read.
    if hook.watchdog_secs != 0 {
        let mut w = watchdog.borrow_mut();
        w.set_timeout(
            RwdtStage::Stage0,
            Duration::from_secs(u64::from(hook.watchdog_secs)),
        );
        w.feed();
    }
    match hook.behaviour.unwrap_or_else(|| fail("unknown-behaviour")) {
        Behaviour::Normal => unreachable!("normal hooks are filtered by Config::select"),
        Behaviour::Confirm => {
            if pending {
                updater.confirm().unwrap_or_else(|_| fail("confirm"));
                println!("@fbt confirmed");
            }
            idle(watchdog)
        }
        Behaviour::Idle => idle(watchdog),
        Behaviour::ResetNoConfirm => {
            println!("@fbt resetting");
            esp_hal::system::software_reset()
        }
        Behaviour::Panic => panic!("deliberate test panic"),
        Behaviour::Fault => {
            println!("@fbt fault");
            // SAFETY: deliberately raises an illegal-instruction exception.
            unsafe { core::arch::asm!("unimp") };
            fail("fault-returned")
        }
        Behaviour::HangNoFeed => {
            println!("@fbt hang");
            loop {
                core::hint::spin_loop();
            }
        }
        Behaviour::EraseActive => {
            let region = layout.region(boot.active_slot());
            let sector = FlashStorage::ERASE_SIZE as u32;
            let erased = flash.lock(|f| {
                f.borrow_mut()
                    .erase(region.offset, region.offset + sector)
                    .is_ok()
            });
            if !erased {
                fail("erase-active");
            }
            println!("@fbt erased-active");
            esp_hal::system::software_reset()
        }
    }
}

#[esp_hal::main]
async fn main(spawner: Spawner) {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // esp-hal disables the RTC watchdog during init; restart it immediately.
    let watchdog = RefCell::new(ferriboot_esp::watchdog::start_rtc(
        p.RTC_TIMER,
        Duration::from_secs(15),
    ));
    esp_println::logger::init_logger(log::LevelFilter::Info);
    let boot = BootInfo::read().expect("missing or invalid bootloader handoff");
    let storage = ferriboot_esp::open_flash(p.FLASH);
    let layout = ferriboot_esp::read_layout(&storage).expect("partition table");
    let hooks = load_hook_config(&storage);
    let flash = ferriboot_esp::share(ferriboot_core::flash::ProgressFlash::new(
        storage.into_inner().into_inner(),
        || watchdog.borrow_mut().feed(),
    ));
    let mut updater = ferriboot_esp::updater(&layout, &boot, &flash)
        .expect("bootloader handoff, verifier, or slot layout mismatch");
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
            &layout,
            &flash,
            updater,
            &active.version,
            &watchdog,
        );
    }

    // Initialize Embassy only on the normal example path. Hook actions above
    // intentionally run before esp-rtos starts (also under Espressif QEMU).
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);
    info!("ESP32-C3 Embassy runtime initialized");

    if updater.needs_confirmation().expect("read trailer") {
        // Example policy: a real application would check its health first.
        updater.confirm().expect("confirm trial image");
    }
    info!("application healthy; image confirmed; USB-Serial-JTAG log console ready");
    spawner.spawn(heartbeat().expect("spawn heartbeat"));
    keep_alive(&watchdog).await
}
