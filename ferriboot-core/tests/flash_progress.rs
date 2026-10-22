mod common;

use std::cell::Cell;

use common::{Power, SECTOR, SimError, SimFlash};
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use ferriboot_core::flash::{ProgressFlash, ProgressFlashError};

#[test]
fn full_slot_erase_reports_each_completed_sector() {
    let power = Power::default();
    let raw = SimFlash::new(4 * SECTOR, &power);
    raw.program(0, &vec![0; 4 * SECTOR]);
    let pets = Cell::new(0);
    let mut flash = ProgressFlash::new(raw.clone(), || pets.set(pets.get() + 1));
    flash.erase(0, (4 * SECTOR) as u32).unwrap();
    assert_eq!(pets.get(), 4);
    assert_eq!(power.count(), 4);
    assert!(raw.snapshot().iter().all(|b| *b == 0xff));
}

#[test]
fn invalid_erase_never_mutates_flash_or_pets_watchdog() {
    let power = Power::default();
    let raw = SimFlash::new(4 * SECTOR, &power);
    raw.program(0, &vec![0; 4 * SECTOR]);
    let before = raw.snapshot();
    let pets = Cell::new(0);
    let mut flash = ProgressFlash::new(raw.clone(), || pets.set(pets.get() + 1));
    for (from, to, error) in [
        (0, (5 * SECTOR) as u32, ProgressFlashError::OutOfBounds),
        (SECTOR as u32, 0, ProgressFlashError::OutOfBounds),
        (0, (3 * SECTOR + 1) as u32, ProgressFlashError::NotAligned),
        (1, SECTOR as u32, ProgressFlashError::NotAligned),
    ] {
        assert_eq!(flash.erase(from, to), Err(error));
    }
    flash.erase(0, 0).unwrap();
    assert_eq!(pets.get(), 0);
    assert_eq!(power.count(), 0);
    assert_eq!(raw.snapshot(), before);
}

#[test]
fn failed_sector_does_not_report_progress_or_erase_later_sectors() {
    let power = Power::default();
    let raw = SimFlash::new(4 * SECTOR, &power);
    raw.program(0, &vec![0; 4 * SECTOR]);
    power.fail_after(2);
    let pets = Cell::new(0);
    let mut flash = ProgressFlash::new(raw.clone(), || pets.set(pets.get() + 1));
    assert_eq!(
        flash.erase(0, (4 * SECTOR) as u32),
        Err(ProgressFlashError::Driver(SimError::PowerLoss))
    );
    assert_eq!(pets.get(), 2);
    assert_eq!(power.count(), 3);
    assert!(raw.get(0..2 * SECTOR).iter().all(|b| *b == 0xff));
    assert!(raw.get(3 * SECTOR..4 * SECTOR).iter().all(|b| *b == 0));
}

#[test]
fn read_and_write_report_only_successful_io() {
    let power = Power::default();
    let pets = Cell::new(0);
    let mut flash = ProgressFlash::new(SimFlash::new(SECTOR, &power), || pets.set(pets.get() + 1));
    flash.write(0, &[0; 4]).unwrap();
    let mut bytes = [0xff; 4];
    flash.read(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 4]);
    assert_eq!(pets.get(), 2);
    assert!(flash.read(SECTOR as u32, &mut bytes).is_err());
    power.fail_after(0);
    assert!(flash.write(4, &[0; 4]).is_err());
    assert_eq!(pets.get(), 2);
}
