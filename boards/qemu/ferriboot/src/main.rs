#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
//! QEMU test boot manager: signed MCUboot image in SPI NOR -> verified SSRAM
//! application. The platform shim supplies the simulated peripherals.

use embedded_storage::nor_flash::ReadNorFlash;
use ferriboot_core::{
    BootLoader, Error, Fih, PublicKey, Region, SecurityCounterStore, Slot, ValidatedImage,
    counter_permits, flash::ProgressFlash, image::ImageHeader, reauthenticate,
};
use ferriboot_qemu::{self as platform, Partition, policy::Policy};
use log::{error, info};
use qemu_layout::*;

mod config;

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

/// The board's flash map, checked against the machine at compile time.
const LAYOUT: platform::Layout = platform::Layout::new(qemu_layout::FLASH_MAP);
/// Deadline between two completed flash operations, matching the hardware
/// boards. QEMU's CMSDK watchdog resets after two half-deadline periods.
const WATCHDOG_SECS: u8 = 15;

/// A flash area as the boot decision sees it: every completed operation
/// feeds the watchdog.
type BootFlash = ProgressFlash<Partition, fn()>;

fn boot_area(region: Region) -> BootFlash {
    ProgressFlash::new(Partition::new(region), platform::feed_watchdog as fn())
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    error!("bootloader panic: {info}");
    halt()
}

/// Stop with the watchdog disarmed, as before it covered the bootloader.
fn halt() -> ! {
    platform::stop_watchdog();
    platform::halt()
}

#[cortex_m_rt::entry]
fn main() -> ! {
    // Owned until the final handoff, which writes VTOR through it.
    let core_peripherals = cortex_m::Peripherals::take().expect("core peripherals already taken");
    platform::init();
    platform::take_reset_reason();
    // Cover validation and restartable swaps: a hung flash operation resets
    // the machine and the next boot resumes the interrupted operation.
    platform::watchdog(WATCHDOG_SECS);
    // SAFETY: reserved handoff region is not used by the linker.
    unsafe {
        core::ptr::write_bytes(BOOT_INFO_ADDRESS as *mut u8, 0, BOOT_INFO_SIZE);
    }
    info!(
        "Ferriboot {} for QEMU (AN385 test), mode {:?}",
        env!("CARGO_PKG_VERSION"),
        config::MODE
    );
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
        PUBLIC_KEYS.map(|raw| PublicKey::from_raw(&raw).expect("invalid built-in public key"));
    // The two SPI-NOR slots are copied into one executable SSRAM window.
    // Like the ESP bootloader's partition table, the board layout supplies
    // these runtime addresses rather than keeping them in the common config.
    let mut boot_config = config::boot_config();
    boot_config.slot_addresses = Some([APP_XIP_BASE; 2]);
    // The simulated device fails closed when running against stock QEMU.
    // Each DATA read asks the host for a new guest-visible random word.
    let mut trng = platform::trng::Trng::try_new().expect("QEMU AN385 test TRNG missing");
    let mut security_counter = platform::security_counter::EfuseSecurityCounter::new();
    let (decision, max_image_size) = {
        let mut loader = BootLoader::new(
            boot_area(LAYOUT.region(Slot::Primary)),
            boot_area(LAYOUT.region(Slot::Secondary)),
            LAYOUT.slots().scratch.map(boot_area),
            &keys,
            boot_config,
        )
        .expect("invalid slot configuration")
        .with_security_counter_store(&mut security_counter)
        .with_entropy(&mut trng);
        let decision = match loader.boot_with_validator(
            &mut |flash: &mut BootFlash, _: Slot, _: Slot, image: &ValidatedImage| {
                let mut words = [0; 8];
                if image.header().img_size < 8 {
                    return Err(Error::Image(ferriboot_core::ImageError::PlatformRejected));
                }
                flash
                    .read(u32::from(image.header().hdr_size), &mut words)
                    .map_err(Error::Flash)?;
                platform::vectors(&LAYOUT, image.header(), &words).map_err(Error::Image)?;
                Ok(())
            },
        ) {
            Ok(d) => d,
            Err(Error::Flash(e)) => {
                // A flash failure may be transient; the operation is restartable.
                error!("flash failure: {e:?}; resetting to resume the operation");
                platform::reset()
            }
            Err(e) => {
                error!("no executable application: {e:?}");
                halt()
            }
        };
        (decision, loader.max_image_size())
    };
    let rollback_floor = ferriboot_core::SecurityCounterStore::get(&mut security_counter)
        .expect("cannot read hardware rollback floor");
    let data = ferriboot_core::shared_data::boot_save_shared_data::<BOOT_INFO_SIZE>(
        config::MODE,
        decision.slot,
        max_image_size,
        Some(config::version()),
        Some(rollback_floor),
    )
    .unwrap();
    let h = decision.image.header();
    let region = LAYOUT.region(decision.slot);
    // Copy the authenticated payload out of SPI NOR into executable SSRAM.
    // Both slots run at the same linked execution address; SPI NOR is not XIP.
    let mut source = boot_area(region);
    let mut copied = 0usize;
    let mut buffer = [0u8; 256];
    while copied < h.img_size as usize {
        let n = buffer.len().min(h.img_size as usize - copied);
        if let Err(e) = source.read(u32::from(h.hdr_size) + copied as u32, &mut buffer[..n]) {
            // A mid-copy SPI failure is restartable; the reserved boot-info
            // remains invalid until the entire image has been copied.
            error!("flash failure while copying application: {e:?}; resetting");
            platform::reset()
        }
        // SAFETY: validated image size and execution window were checked.
        unsafe {
            core::ptr::copy_nonoverlapping(
                buffer.as_ptr(),
                (APP_VECTOR as usize + copied) as *mut u8,
                n,
            );
        }
        copied += n;
    }
    // SAFETY: shared handoff area is reserved from the RAM linker map.
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.bytes().as_ptr(),
            BOOT_INFO_ADDRESS as *mut u8,
            data.bytes().len(),
        );
    }
    info!(
        "starting image {} from {:?}, operation {:?}",
        h.version, decision.slot, decision.performed
    );
    // Point of no return: nothing is printed after this. Randomize the
    // confirmation timing only after the last externally observable log.
    confirm(
        &mut source,
        &keys,
        &boot_config.policy,
        max_image_size,
        &decision.image,
        &mut security_counter,
        &mut trng,
    );
    // The QEMU test firmware does not feed a watchdog it did not arm, so
    // disarm it at the handoff (the hardware boards hand theirs over).
    platform::stop_watchdog();
    // SAFETY: privileged thread mode, interrupts disabled; the vector table
    // at APP_VECTOR was copied and confirmed above.
    unsafe { core_peripherals.SCB.vtor.write(APP_VECTOR) };
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    // SAFETY: as above; bootload sets MSP and branches without returning to
    // this frame.
    unsafe { cortex_m::asm::bootload(APP_VECTOR as *const u32) }
}

/// Fault-hardened confirmation before the jump: authenticate the slot a
/// second time, require the SSRAM copy that will execute to equal the
/// authenticated flash body byte for byte, re-check the copied vectors and the
/// rollback floor. Diverges (panics) on any disagreement.
#[inline(never)]
fn confirm<S: SecurityCounterStore, R: rand::CryptoRng>(
    source: &mut BootFlash,
    keys: &[PublicKey],
    policy: &Policy,
    max_image_size: u32,
    image: &ValidatedImage,
    counter: &mut S,
    random: &mut R,
) {
    let h = image.header();
    let authentic = reauthenticate(source, keys, policy, max_image_size, image);
    let mut words = [0u8; 8];
    for (i, b) in words.iter_mut().enumerate() {
        // SAFETY: the first eight bytes of the copied image in SSRAM.
        *b = unsafe { core::ptr::read_volatile((APP_VECTOR as usize + i) as *const u8) };
    }
    let vectors = Fih::ok(&platform::vectors(&LAYOUT, h, &words));
    let verdict = authentic & copy_matches(source, h) & vectors & counter_permits(counter, image);
    // Place the randomized delay after flash/MMIO checks, directly before the
    // fault-hardened branch. Do not log or touch flash between these calls.
    ferriboot_core::fih::random_delay(random.next_u32());
    verdict.require();
}

/// Compare the executable SSRAM copy with the image body in flash, without
/// data-dependent branches. A loop cut short leaves `seen` short of the size.
fn copy_matches(source: &mut BootFlash, h: &ImageHeader) -> Fih {
    let len = h.img_size as usize;
    let mut diff = 0u32;
    let mut seen = 0u32;
    let mut buffer = [0u8; 256];
    let mut offset = 0usize;
    while offset < len {
        let n = buffer.len().min(len - offset);
        if source
            .read(u32::from(h.hdr_size) + offset as u32, &mut buffer[..n])
            .is_err()
        {
            return ferriboot_core::fih::FAIL;
        }
        for (i, expected) in buffer[..n].iter().enumerate() {
            // SAFETY: within the copied image in the SSRAM execution window.
            let actual = unsafe {
                core::ptr::read_volatile((APP_VECTOR as usize + offset + i) as *const u8)
            };
            diff |= u32::from(actual ^ expected);
            seen = core::hint::black_box(seen.wrapping_add(1));
        }
        offset += n;
    }
    Fih::from_diff(core::hint::black_box(diff | (seen ^ h.img_size)))
}
