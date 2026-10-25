#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
//! Ferriboot third-stage boot manager for the ESP32-C3.
//!
//! Boot chain: ROM -> ESP-IDF second stage (0x0) -> signed factory Ferriboot
//! (0x20000) -> the application image in `image-0` (or `image-1` in
//! direct-XIP mode), loaded by a RAM handoff loader.

use core::cell::RefCell;

use esp_hal::rng::{Trng, TrngSource};
use ferriboot_core::{Error, PublicKey, flash::ProgressFlash};
use ferriboot_esp::{boot_runtime, chip, glitch, loader};
use log::{error, info, warn};

mod config;

// Standard IDF app descriptor; with linkall.x, this should land at binary 0x20.
esp_bootloader_esp_idf::esp_app_desc!();

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

/// The flash as the boot decision sees it: every completed operation feeds
/// the watchdog.
type BootFlash<'d, 'w> = ProgressFlash<ferriboot_esp::FlashStorage<'d>, &'w dyn Fn()>;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    esp_println::println!("ferriboot panic: {}", info);
    boot_runtime::reset_after_delay()
}

#[esp_hal::main]
fn main() -> ! {
    // ESP-IDF installs the C3 platform map before HAL startup. The shared
    // boot runtime also invalidates stale boot info and clears ROM flash-boot
    // protection before long-running slot validation or swaps.
    let peripherals = boot_runtime::init();
    // Arm the watchdog before any long-running step. It is fed after every
    // completed flash operation and runs on, never stopped, into the handoff.
    let watchdog = RefCell::new(boot_runtime::start_watchdog(peripherals.RTC_TIMER));
    let feed_watchdog = || watchdog.borrow_mut().feed();
    esp_println::logger::init_logger(log::LevelFilter::Info);
    info!(
        "Ferriboot {} for {}, mode {:?}",
        env!("CARGO_PKG_VERSION"),
        chip::NAME,
        config::MODE
    );
    // Re-assert the clock glitch reset ESP-IDF's second stage should have
    // enabled, before slot validation, swaps or eFuse access.
    match glitch::enable() {
        Ok(report) => {
            if report.reset_by_glitch {
                warn!("last reset was caused by a glitch detector");
            }
            info!("clock glitch detector reset enabled");
        }
        Err(e) => {
            error!("glitch detector setup failed: {:?}", e);
            boot_runtime::reset_after_delay()
        }
    }

    // Entropy for the random delays in front of fault-hardened checks. The
    // C3 RNG only produces true randomness while an entropy source runs; the
    // SAR ADC source is reverted (when dropped) before the handoff.
    let trng_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let mut trng = Trng::try_new().expect("TRNG entropy source");

    let storage = ferriboot_esp::open_flash(peripherals.FLASH);
    let flash_size = storage.lock(|storage| storage.borrow().capacity()) as u32;
    info!("flash size: {} KiB", flash_size / 1024);
    let layout = ferriboot_esp::read_layout(&storage)
        .expect("failed to read slot layout from partition table");
    // From here on, all flash access feeds the watchdog.
    let flash = ferriboot_esp::share(BootFlash::new(
        storage.into_inner().into_inner(),
        &feed_watchdog,
    ));

    info!(
        "primary {:#x}+{:#x}, secondary {:#x}+{:#x}, scratch {:?}",
        layout.primary.offset,
        layout.primary.size,
        layout.secondary.offset,
        layout.secondary.size,
        layout.scratch
    );

    let keys = PUBLIC_KEYS.map(|k| PublicKey::from_raw(&k).expect("invalid built-in public key"));

    // Lets direct-XIP honour `imgtool --rom-fixed` like upstream.
    let mut boot_config = config::boot_config();
    boot_config.slot_addresses = Some([layout.primary.offset, layout.secondary.offset]);

    // Boot-time fuse access is exclusive; no peripheral IRQs are needed here.
    // The IDF second stage leaves APP_ANTI_ROLLBACK disabled; Ferriboot owns
    // SECURE_VERSION independently of the factory boot manager's app descriptor.
    riscv::interrupt::disable();
    // SAFETY: `esp_hal::init` ran in `boot_runtime::init`; single core with
    // IRQs disabled for the counter's lifetime, and nothing else in this
    // binary programs eFuses; SECURE_VERSION is reserved for Ferriboot and
    // IDF anti-rollback is off (see above).
    let mut security_counter =
        unsafe { ferriboot_esp::security_counter::EfuseSecurityCounter::new() };
    let (decision, max_image_size) = {
        let mut bootloader = ferriboot_esp::bootloader(&layout, &flash, &keys, boot_config)
            .expect("invalid slot configuration")
            .with_security_counter_store(&mut security_counter)
            .with_entropy(&mut trng);

        let decision = match bootloader.boot_with_validator(
            &mut |partition: &mut ferriboot_esp::StoragePartition<'_, BootFlash<'_, '_>>,
                  _: ferriboot_core::Slot,
                  target: ferriboot_core::Slot,
                  image: &ferriboot_core::ValidatedImage| {
                let header_size = u32::from(image.header().hdr_size);
                let destination = layout
                    .region(target)
                    .offset
                    .checked_add(header_size)
                    .ok_or(Error::Image(ferriboot_core::ImageError::PlatformRejected))?;
                match loader::validate(partition, header_size, destination, image.header().img_size)
                {
                    Ok(()) => Ok(()),
                    Err(loader::LoadError::Flash(error)) => Err(Error::Flash(error)),
                    Err(error) => {
                        error!("rejecting application executable: {:?}", error);
                        Err(Error::Image(ferriboot_core::ImageError::PlatformRejected))
                    }
                }
            },
        ) {
            Ok(d) => d,
            Err(Error::Flash(e)) => {
                // Possibly transient; everything is restartable.
                error!("flash error: {:?}; resetting", e);
                boot_runtime::reset_after_delay()
            }
            Err(e) => {
                error!("unable to find a bootable image: {:?}", e);
                boot_runtime::reset_after_delay()
            }
        };
        (decision, bootloader.max_image_size())
    };

    let region = layout.region(decision.slot);
    let image = region.offset + u32::from(decision.image.header().hdr_size);
    info!(
        "starting image {} from {:?} slot (ESP image at {:#x})",
        decision.image.header().version,
        decision.slot,
        image
    );

    let rollback_floor = match ferriboot_core::SecurityCounterStore::get(&mut security_counter) {
        Ok(floor) => floor,
        Err(e) => {
            error!("cannot read hardware rollback floor: {:?}; resetting", e);
            boot_runtime::reset_after_delay()
        }
    };
    let boot_info = match ferriboot_esp::boot_info::boot_save_shared_data(
        config::MODE,
        decision.slot,
        max_image_size,
        Some(config::version()),
        Some(rollback_floor),
    ) {
        Ok(info) => info,
        Err(e) => {
            error!("cannot build shared boot information: {:?}; resetting", e);
            boot_runtime::reset_after_delay()
        }
    };

    let plan = flash.lock(|storage| {
        loader::plan(
            &mut *storage.borrow_mut(),
            image,
            decision.image.header().img_size,
            boot_info,
        )
    });
    let plan = match plan {
        Ok(plan) => plan,
        Err(e) => {
            error!("cannot load the application image: {:?}", e);
            boot_runtime::reset_after_delay();
        }
    };
    info!("jumping to {:#010x}", plan.entry());
    // Point of no return: nothing is printed after this. Authenticate the
    // slot a second time, re-derive the plan and re-read the rollback floor,
    // then decide after a random delay. A disagreement panics (resetting).
    let plan = loader::confirm(
        plan,
        &flash,
        loader::Authenticated {
            region,
            image: &decision.image,
            keys: &keys,
            policy: &boot_config.policy,
            max_image_size,
        },
        &mut security_counter,
        &mut trng,
    );
    // Drop the TRNG handle before disabling its entropy source; the HAL
    // refuses to disable the source while a TRNG user is still alive.
    drop(trng);
    drop(trng_source);
    // Leave the hardware timer running through the destructive RAM handoff,
    // with a full deadline for the application's startup.
    feed_watchdog();
    loader::execute(plan)
}
