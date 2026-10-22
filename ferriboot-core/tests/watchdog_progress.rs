//! The board bootloaders run the whole boot decision through
//! [`ProgressFlash`], feeding a hardware watchdog after every completed flash
//! operation. These tests pin down the two properties that relies on:
//!
//! * the wrapper is transparent to the upgrade state machines, including
//!   power-loss recovery (it splits multi-sector erases, which moves the
//!   points where power can be lost), and
//! * the work between two feeds is bounded by one erase sector or one small
//!   transfer, so a fixed watchdog deadline covers any slot size.

mod common;

use std::{cell::Cell, rc::Rc};

use common::*;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use ferriboot_core::{
    BootConfig, BootDecision, BootLoader, Error, SwapType, UpgradeMode,
    flash::{ProgressFlash, ProgressFlashError},
};

/// Largest single operation the bootloader may perform between two feeds,
/// other than a one-sector erase. Matches the core's bounded I/O chunk.
const MAX_TRANSFER: usize = 512;

#[derive(Default)]
struct Largest {
    erase: Cell<usize>,
    transfer: Cell<usize>,
}

/// Records the largest erase and transfer that reach the driver.
#[derive(Clone)]
struct Probe {
    inner: SimFlash,
    largest: Rc<Largest>,
}

impl Probe {
    fn new(inner: &SimFlash, largest: &Rc<Largest>) -> Self {
        Self {
            inner: inner.clone(),
            largest: largest.clone(),
        }
    }

    fn transfer(&self, len: usize) {
        self.largest
            .transfer
            .set(self.largest.transfer.get().max(len));
    }
}

impl ErrorType for Probe {
    type Error = SimError;
}

impl ReadNorFlash for Probe {
    const READ_SIZE: usize = SimFlash::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SimError> {
        self.transfer(bytes.len());
        self.inner.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}

impl NorFlash for Probe {
    const WRITE_SIZE: usize = SimFlash::WRITE_SIZE;
    const ERASE_SIZE: usize = SimFlash::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SimError> {
        self.transfer(bytes.len());
        self.inner.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), SimError> {
        let len = to.saturating_sub(from) as usize;
        self.largest.erase.set(self.largest.erase.get().max(len));
        self.inner.erase(from, to)
    }
}

type BootResult = Result<BootDecision, Error<ProgressFlashError<SimError>>>;

/// Boot `rig` the way the board bootloaders do: every area wrapped in a
/// [`ProgressFlash`] that feeds the (simulated) watchdog.
fn boot_fed(rig: &Rig, feeds: &Cell<u64>, largest: &Rc<Largest>) -> BootResult {
    let feed = || feeds.set(feeds.get() + 1);
    let wrap = |area: &SimFlash| ProgressFlash::new(Probe::new(area, largest), &feed);
    BootLoader::new(
        wrap(&rig.primary),
        wrap(&rig.secondary),
        Some(wrap(&rig.scratch)),
        &rig.keys,
        rig.config,
    )
    .unwrap()
    .boot()
}

fn v1() -> Vec<u8> {
    build_image(&Spec::new((1, 0, 0, 0), 40_000, 1))
}

fn v2() -> Vec<u8> {
    build_image(&Spec::new((2, 0, 0, 0), 61_234, 2))
}

#[test]
fn swap_through_progress_flash_survives_every_power_loss() {
    let rig = Rig::new(BootConfig::default());
    rig.primary.program(0, &v1());
    rig.stage_update(&v2(), false);
    let staged = rig.snapshot();

    let feeds = Cell::new(0);
    let largest = Rc::new(Largest::default());
    rig.power.unlimited();
    rig.power.reset_count();
    let d = boot_fed(&rig, &feeds, &largest).expect("uninterrupted boot");
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::Test)
    );
    let total = rig.power.count();
    let done = rig.snapshot();
    assert!(feeds.get() >= total, "every mutating operation must feed");

    for n in 0..total {
        rig.restore(&staged);
        rig.power.fail_after(n);
        assert_eq!(
            boot_fed(&rig, &feeds, &largest),
            Err(Error::Flash(ProgressFlashError::Driver(
                SimError::PowerLoss
            ))),
            "power loss at op {n}"
        );
        if rig.snapshot() == done {
            // The final flag write had landed: the next boot is a second
            // boot of an unconfirmed test image and correctly reverts.
            continue;
        }
        rig.power.unlimited();
        let d = boot_fed(&rig, &feeds, &largest)
            .unwrap_or_else(|e| panic!("recovery after power loss at op {n} failed: {e:?}"));
        assert_eq!(d.image.header().version.major, 2, "power loss at op {n}");
        assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)));
    }

    assert_eq!(largest.erase.get(), SECTOR, "erase between feeds");
    assert!(
        largest.transfer.get() <= MAX_TRANSFER,
        "transfer between feeds: {} bytes",
        largest.transfer.get()
    );
}

#[test]
fn every_mode_bounds_work_between_feeds() {
    for mode in [
        UpgradeMode::SwapScratch,
        UpgradeMode::Overwrite,
        UpgradeMode::DirectXip { revert: false },
        UpgradeMode::DirectXip { revert: true },
    ] {
        let rig = Rig::new(BootConfig {
            mode,
            ..BootConfig::default()
        });
        rig.primary.program(0, &v1());
        rig.stage_update(&v2(), true);

        let feeds = Cell::new(0);
        let largest = Rc::new(Largest::default());
        let d = boot_fed(&rig, &feeds, &largest)
            .unwrap_or_else(|e| panic!("{mode:?}: boot failed: {e:?}"));
        assert_eq!(d.image.header().version.major, 2, "{mode:?}");
        assert!(feeds.get() > 0, "{mode:?}: no feeds");
        assert!(
            largest.erase.get() <= SECTOR,
            "{mode:?}: erase between feeds"
        );
        assert!(
            largest.transfer.get() <= MAX_TRANSFER,
            "{mode:?}: transfer between feeds: {} bytes",
            largest.transfer.get()
        );
    }
}
