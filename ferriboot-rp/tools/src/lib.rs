//! RP2350 host tooling: checked ELF export, PICOBIN parsing, ROM sealing,
//! SWD loading and the probe-rs fixture flash algorithm.
#![forbid(unsafe_code)]

pub mod algorithm;
pub mod elf;
pub mod picobin;
mod romseal;
pub mod seal;

use anyhow::{Context, Result, ensure};
use ferriboot_build::tools::{
    keys::DevKey,
    mcuboot::{self, Geometry},
};
use ferriboot_layout_rp2350_pico2 as chip;
use probe_rs::{
    Permissions, Session,
    flashing::{self, DownloadOptions, FlashProgress},
    probe::{DebugProbeSelector, WireProtocol, list::Lister},
};
use std::{collections::BTreeMap, fs, path::Path};

/// MCUboot geometry of the compiled board layout.
pub fn geometry() -> Geometry {
    use ferriboot_layout_rp2350_pico2::*;
    Geometry {
        header: HEADER_SIZE,
        slot: SLOT_SIZE,
        erase: ERASE_SIZE,
        write: WRITE_SIZE,
        rom_fixed: Some(APP_XIP_BASE),
        class: "rp2350",
    }
}

/// Chip and board constants of the compiled layout (`layout/partitions.json`).
pub fn constants() -> BTreeMap<String, u32> {
    use ferriboot_layout_rp2350_pico2::*;
    ferriboot_build::named_constants![
        // Chip (chips/rp235x).
        XIP_BASE,
        APP_XIP_BASE,
        APP_XIP_WINDOW_SIZE,
        HEADER_SIZE,
        VECTOR_ALIGN,
        ERASE_SIZE,
        WRITE_SIZE,
        RAM_BASE,
        BOOT_INFO_ADDRESS,
        BOOT_INFO_SIZE,
        // Board.
        FLASH_SIZE,
        BOOTLOADER_SIZE,
        PRIMARY_OFFSET,
        SECONDARY_OFFSET,
        SLOT_SIZE,
        SCRATCH_OFFSET,
        SCRATCH_SIZE,
        STORAGE_OFFSET,
        STORAGE_SIZE,
        MAX_IMAGE_SIZE,
    ]
}

/// Development secp256k1 ROM-signing key.
pub const DEV_KEY: DevKey = DevKey {
    dir: "boards/rp2350-pico2/keys",
    stem: "dev-rp2350-rom-secp256k1",
    generate: seal::generate_pair,
};

/// Check a signed application against the RP2350 linking contract.
pub fn check_signed_app(data: &[u8]) -> Result<()> {
    let signed = mcuboot::inspect(data, geometry(), None)?;
    ensure!(
        signed.load_address == chip::APP_XIP_BASE
            && signed.header_size == chip::HEADER_SIZE as usize
            && signed.flags == 0x100,
        "signed header differs from RP2350 linking contract"
    );
    ferriboot_build::tools::elf::check_vectors(
        &data[signed.header_size..signed.header_size + signed.body_size],
        chip::APP_XIP_BASE + chip::HEADER_SIZE,
        chip::RAM_BASE,
        chip::BOOT_INFO_ADDRESS,
    )
}

/// Open one SWD session, preserving the CLI's probe, speed and target settings.
fn attach() -> Result<Session> {
    let lister = Lister::new();
    let mut probe = match std::env::var("PROBE_RS_PROBE") {
        Ok(spec) => lister.open(
            spec.parse::<DebugProbeSelector>()
                .context("invalid PROBE_RS_PROBE selector")?,
        )?,
        Err(std::env::VarError::NotPresent) => {
            let probes = lister.list_all();
            ensure!(
                probes.len() == 1,
                "expected exactly one SWD probe; set PROBE_RS_PROBE"
            );
            probes[0].open()?
        }
        Err(error) => return Err(error.into()),
    };
    probe.select_protocol(WireProtocol::Swd)?;
    match std::env::var("PROBE_RS_SPEED_KHZ") {
        Ok(speed) => {
            probe.set_speed(speed.parse::<u32>().context("invalid PROBE_RS_SPEED_KHZ")?)?;
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => return Err(error.into()),
    }
    let target = ferriboot_build::tools::env("PROBE_RS_CHIP", "RP235x");
    Ok(probe.attach(target.as_str(), Permissions::default())?)
}

fn program(session: &mut Session, image: &[u8], address: u32, skip_erase: bool) -> Result<()> {
    ensure!(!image.is_empty(), "cannot flash an empty image");
    let mut loader = session.target().flash_loader();
    loader.add_data(u64::from(address), image)?;
    let mut options = DownloadOptions::default();
    // The application path has already erased its whole slot, so it must not
    // erase a second time. The bootloader path retains normal sector erasure.
    options.skip_erase = skip_erase;
    loader.commit(session, options)?;
    Ok(())
}

fn reset(session: &mut Session) -> Result<()> {
    // Reset, rather than reset_and_halt, so the freshly flashed image boots.
    session.core(0)?.reset()?;
    Ok(())
}

/// Download a ROM-sealed bootloader over SWD, then reset. Unlike application
/// flashing this only erases the sectors touched by the bootloader image.
pub fn download(binary: &Path, address: u32) -> Result<()> {
    let image = fs::read(binary)?;
    let mut session = attach()?;
    program(&mut session, &image, address, false)?;
    reset(&mut session)
}

/// Calculate the *exact* slot erase range, allowing only the board's two
/// application slots. Validate before connecting to or modifying a device.
fn app_slot_range(address: u32, image_len: usize) -> Result<(u64, u64)> {
    ensure!(
        address == chip::XIP_BASE + chip::PRIMARY_OFFSET
            || address == chip::XIP_BASE + chip::SECONDARY_OFFSET,
        "RP2350 flash-app address is not an application slot"
    );
    ensure!(
        image_len > 0 && image_len <= chip::SLOT_SIZE as usize,
        "signed application exceeds slot or is empty"
    );
    ensure!(
        chip::ERASE_SIZE != 0
            && address.is_multiple_of(chip::ERASE_SIZE)
            && chip::SLOT_SIZE.is_multiple_of(chip::ERASE_SIZE),
        "RP2350 application slot must be sector aligned"
    );
    let end = address
        .checked_add(chip::SLOT_SIZE)
        .context("slot end overflow")?;
    ensure!(
        end <= chip::XIP_BASE + chip::FLASH_SIZE,
        "application slot exceeds flash"
    );
    Ok((u64::from(address), u64::from(end)))
}

/// Erase the entire selected application slot, program only the signed image
/// bytes, then reset. Both operations share one SWD session. No synthetic HEX
/// markers or whole-chip erase are used.
pub fn flash_app(image: &[u8], address: u32) -> Result<()> {
    let signed = mcuboot::inspect(image, geometry(), None)?;
    // The TLV end defines the real signed image length. Any padding or old
    // trailer bytes supplied by a caller must not be programmed into the slot.
    let payload = &image[..signed.size];
    let (start, end) = app_slot_range(address, payload.len())?;
    let mut session = attach()?;
    flashing::erase(
        &mut session,
        &mut FlashProgress::default(),
        start,
        end,
        false,
    )?;
    program(&mut session, payload, address, true)?;
    reset(&mut session)
}

#[cfg(test)]
mod flash_tests {
    use super::*;

    #[test]
    fn only_the_two_application_slots_are_valid_erase_ranges() {
        let primary = chip::XIP_BASE + chip::PRIMARY_OFFSET;
        let secondary = chip::XIP_BASE + chip::SECONDARY_OFFSET;
        assert_eq!(
            app_slot_range(primary, 123).unwrap(),
            (u64::from(primary), u64::from(primary + chip::SLOT_SIZE))
        );
        assert_eq!(
            app_slot_range(secondary, 123).unwrap(),
            (u64::from(secondary), u64::from(secondary + chip::SLOT_SIZE))
        );
        assert!(app_slot_range(primary + 1, 123).is_err());
        assert!(app_slot_range(chip::XIP_BASE, 123).is_err());
        assert!(app_slot_range(primary, 0).is_err());
        assert!(app_slot_range(primary, chip::SLOT_SIZE as usize + 1).is_err());
    }
}
