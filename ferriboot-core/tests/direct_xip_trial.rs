mod common;

use common::*;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use ferriboot_core::{BootConfig, BootLoader, Error, Flag, Slot, UpgradeMode, trailer::Trailer};

// Emulate a reset on the first validation read after copy_done is programmed.
// Header selection and trailer reads still succeed before that write.
struct ValidationCut {
    flash: SimFlash,
    marker: u32,
    interrupt: bool,
    armed: bool,
    write_error: bool,
    silent_write: bool,
}

impl ErrorType for ValidationCut {
    type Error = SimError;
}
impl ReadNorFlash for ValidationCut {
    const READ_SIZE: usize = SimFlash::READ_SIZE;
    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SimError> {
        if self.armed {
            return Err(SimError::PowerLoss);
        }
        self.flash.read(offset, bytes)
    }
}
impl NorFlash for ValidationCut {
    const WRITE_SIZE: usize = SimFlash::WRITE_SIZE;
    const ERASE_SIZE: usize = SimFlash::ERASE_SIZE;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SimError> {
        if offset == self.marker && self.write_error {
            return Err(SimError::OutOfBounds);
        }
        if offset == self.marker && self.silent_write {
            return Ok(()); // Fault injection: driver acknowledges a lost write.
        }
        self.flash.write(offset, bytes)?;
        if offset == self.marker && self.interrupt {
            self.armed = true;
        }
        Ok(())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), SimError> {
        self.flash.erase(from, to)
    }
}

fn staged_trial() -> Rig {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::DirectXip { revert: true },
        ..Default::default()
    });
    rig.primary
        .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    t.write_magic(&mut rig.primary.clone()).unwrap();
    t.write_image_ok(&mut rig.primary.clone()).unwrap();
    rig.stage_update(&build_image(&Spec::new((2, 0, 0, 0), 1000, 2)), false);
    rig
}

fn boot_with_fault(
    rig: &Rig,
    interrupt: bool,
    write_error: bool,
) -> Result<ferriboot_core::BootDecision, Error<SimError>> {
    boot_with_fault_mode(rig, interrupt, write_error, false)
}

fn boot_with_fault_mode(
    rig: &Rig,
    interrupt: bool,
    write_error: bool,
    silent_write: bool,
) -> Result<ferriboot_core::BootDecision, Error<SimError>> {
    let marker = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32)
        .unwrap()
        .copy_done_off();
    BootLoader::new(
        ValidationCut {
            flash: rig.primary.clone(),
            marker,
            interrupt: false,
            armed: false,
            write_error: false,
            silent_write: false,
        },
        ValidationCut {
            flash: rig.secondary.clone(),
            marker,
            interrupt,
            armed: false,
            write_error,
            silent_write,
        },
        None,
        &rig.keys,
        rig.config,
    )
    .unwrap()
    .boot()
}

#[test]
fn reset_during_validation_consumes_trial_and_next_boot_reverts() {
    let rig = staged_trial();
    assert_eq!(
        boot_with_fault(&rig, true, false),
        Err(Error::Flash(SimError::PowerLoss))
    );
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    assert_eq!(
        t.read_state(&mut rig.secondary.clone()).unwrap().copy_done,
        Flag::Set
    );
    assert_eq!(rig.boot().unwrap().slot, Slot::Primary);
    assert!(rig.secondary.snapshot().iter().all(|&b| b == 0xff));
}

#[test]
fn marker_write_error_is_best_effort_like_upstream() {
    let mut rig = staged_trial();
    rig.config.direct_xip_fail_closed = false; // Deliberate compatibility opt-out.
    assert_eq!(
        boot_with_fault(&rig, false, true).unwrap().slot,
        Slot::Secondary
    );
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    assert_eq!(
        t.read_state(&mut rig.secondary.clone()).unwrap().copy_done,
        Flag::Unset
    );
}

#[test]
fn marker_write_error_can_fail_closed() {
    let mut rig = staged_trial();
    rig.config.direct_xip_fail_closed = true;
    assert_eq!(
        boot_with_fault(&rig, false, true),
        Err(Error::Flash(SimError::OutOfBounds))
    );
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    assert_eq!(
        t.read_state(&mut rig.secondary.clone()).unwrap().copy_done,
        Flag::Unset
    );
}

#[test]
fn marker_write_acknowledged_but_not_persisted_fails_closed() {
    let rig = staged_trial(); // Fail-closed is now the default.
    assert_eq!(
        boot_with_fault_mode(&rig, false, false, true),
        Err(Error::TrialMarkerNotPersisted)
    );
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    assert_eq!(
        t.read_state(&mut rig.secondary.clone()).unwrap().copy_done,
        Flag::Unset
    );
}

#[test]
fn invalid_trial_is_erased_after_marking_and_falls_back() {
    let rig = staged_trial();
    let mut invalid = Spec::new((3, 0, 0, 0), 1000, 3);
    invalid.corrupt_signature = true;
    rig.secondary.program(0, &build_image(&invalid));
    assert_eq!(rig.boot().unwrap().slot, Slot::Primary);
    assert!(rig.secondary.snapshot().iter().all(|&b| b == 0xff));
}
