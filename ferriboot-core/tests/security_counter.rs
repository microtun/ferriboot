use ferriboot_core::{
    BootConfig, BootDecision, BootLoader, Error, SecurityCounterError as CounterError,
    SecurityCounterStore, Slot, SwapType, UpgradeMode, trailer::Trailer,
};
mod common;
use common::*;

#[derive(Default)]
struct Counter {
    value: u32,
    read_error: bool,
    write_error: bool,
    silent_write: bool,
    cut_after: Option<u32>,
    writes: u32,
}
impl Counter {
    fn at(value: u32) -> Self {
        Self {
            value,
            ..Self::default()
        }
    }
}
impl SecurityCounterStore for Counter {
    fn capacity(&self) -> u32 {
        16
    }
    fn get(&mut self) -> Result<u32, CounterError> {
        if self.read_error {
            Err(CounterError::Read)
        } else {
            Ok(self.value)
        }
    }
    fn advance(&mut self, value: u32) -> Result<(), CounterError> {
        self.writes += 1;
        if self.write_error {
            return Err(CounterError::Write);
        }
        if self.silent_write {
            return Ok(());
        }
        while self.value < value {
            self.value += 1;
            if let Some(n) = self.cut_after.as_mut() {
                if *n == 0 {
                    return Err(CounterError::Write);
                }
                *n -= 1;
            }
        }
        Ok(())
    }
}
fn image(version: u8, epoch: Option<u32>) -> Vec<u8> {
    let mut spec = Spec::new((version, 0, 0, 0), 6000, version);
    spec.sec_cnt = epoch;
    build_image(&spec)
}
fn loader<'a>(rig: &'a Rig) -> BootLoader<'a, SimFlash> {
    BootLoader::new(
        rig.primary.clone(),
        rig.secondary.clone(),
        Some(rig.scratch.clone()),
        &rig.keys,
        rig.config,
    )
    .unwrap()
}
fn boot(rig: &Rig, counter: &mut Counter) -> Result<BootDecision, Error<SimError>> {
    loader(rig).with_security_counter_store(counter).boot()
}
fn modes() -> [UpgradeMode; 4] {
    [
        UpgradeMode::SwapScratch,
        UpgradeMode::Overwrite,
        UpgradeMode::DirectXip { revert: false },
        UpgradeMode::DirectXip { revert: true },
    ]
}
fn rig(mode: UpgradeMode) -> Rig {
    Rig::new(BootConfig {
        mode,
        bootstrap: true,
        downgrade_prevention: true,
        ..BootConfig::default()
    })
}

#[test]
fn erased_primary_header_cannot_bypass_pending_or_bootstrap_floor() {
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        for pending in [false, true] {
            let rig = rig(mode);
            rig.primary.program(0, &image(9, Some(9)));
            if pending {
                rig.stage_update(&image(1, Some(1)), true);
            } else {
                rig.secondary.program(0, &image(1, Some(1)));
            }
            rig.primary.program(0, &[0xff; 4]);
            let mut counter = Counter::at(9);
            assert_eq!(boot(&rig, &mut counter), Err(Error::NoBootableImage));
            assert_eq!(counter.value, 9);
        }
    }
}

#[test]
fn forged_completed_swap_status_still_cannot_boot_old_signed_image() {
    let rig = rig(UpgradeMode::SwapScratch);
    let old = image(1, Some(1));
    rig.primary.program(0, &old);
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    let mut primary = rig.primary.clone();
    t.write_magic(&mut primary).unwrap();
    t.write_swap_info(&mut primary, SwapType::Perm, 0).unwrap();
    t.write_swap_size(&mut primary, old.len() as u32).unwrap();
    // Scratch holds three sectors of data. This image fits one completed step,
    // so resume skips copying and reaches the final validation directly.
    for state in 1..=3 {
        t.write_status(&mut primary, 1, state).unwrap();
    }
    let forged = rig.snapshot();
    assert_eq!(
        rig.boot().unwrap().image.security_counter(),
        Some(1),
        "fixture must exercise the unprotected resume bypass"
    );
    rig.restore(&forged);
    assert_eq!(boot(&rig, &mut Counter::at(9)), Err(Error::NoBootableImage));
}

#[test]
fn unselected_xip_slot_cannot_raise_counter_past_newer_version() {
    // Prior behavior authenticated both XIP slots and advanced the counter
    // for each *before* version selection. v1/SEC_CNT=2 made v2/SEC_CNT=1
    // unbootable even though v2 is the intended selected image.
    for revert in [false, true] {
        let rig = rig(UpgradeMode::DirectXip { revert });
        rig.primary.program(0, &image(2, Some(1)));
        rig.secondary.program(0, &image(1, Some(2)));
        // Confirm both slots explicitly so the regression covers XIP/revert
        // as well as the permanent no-revert strategy.
        let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
        t.write_magic(&mut rig.primary.clone()).unwrap();
        t.write_image_ok(&mut rig.primary.clone()).unwrap();
        t.write_magic(&mut rig.secondary.clone()).unwrap();
        t.write_image_ok(&mut rig.secondary.clone()).unwrap();
        let mut counter = Counter::at(0);
        let decision = boot(&rig, &mut counter).unwrap();
        assert_eq!(decision.slot, Slot::Primary);
        assert_eq!(decision.image.security_counter(), Some(1));
        assert_eq!((counter.value, counter.writes), (1, 1));
        // Subsequent boots must not silently consume the non-selected slot.
        assert_eq!(boot(&rig, &mut counter).unwrap().slot, Slot::Primary);
        assert_eq!((counter.value, counter.writes), (1, 1));
    }
}

#[test]
fn selected_secondary_xip_slot_advances_its_own_counter() {
    for revert in [false, true] {
        let rig = rig(UpgradeMode::DirectXip { revert });
        rig.primary.program(0, &image(1, Some(1)));
        rig.secondary.program(0, &image(2, Some(2)));
        let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
        t.write_magic(&mut rig.secondary.clone()).unwrap();
        t.write_image_ok(&mut rig.secondary.clone()).unwrap();
        let mut counter = Counter::at(0);
        assert_eq!(boot(&rig, &mut counter).unwrap().slot, Slot::Secondary);
        assert_eq!((counter.value, counter.writes), (2, 1));
    }
}

#[test]
fn erasing_newer_xip_slot_cannot_reactivate_old_slot() {
    for revert in [false, true] {
        let rig = rig(UpgradeMode::DirectXip { revert });
        rig.primary.program(0, &image(1, Some(1)));
        rig.updater().confirm().unwrap();
        rig.stage_update(&image(9, Some(9)), true);
        let mut counter = Counter::at(1);
        assert_eq!(boot(&rig, &mut counter).unwrap().slot, Slot::Secondary);
        assert_eq!(counter.value, 9);
        rig.secondary.restore(&vec![0xff; SLOT_SIZE]);
        assert_eq!(boot(&rig, &mut counter), Err(Error::NoBootableImage));
    }
}

#[test]
fn trial_never_advances_and_can_revert_but_confirmation_persists() {
    for mode in [
        UpgradeMode::SwapScratch,
        UpgradeMode::DirectXip { revert: true },
    ] {
        let rig = rig(mode);
        rig.primary.program(0, &image(1, Some(1)));
        rig.updater().confirm().unwrap();
        rig.stage_update(&image(2, Some(5)), false);
        let before = rig.snapshot();
        let mut counter = Counter::at(1);
        let trial = boot(&rig, &mut counter).unwrap();
        assert_eq!(trial.image.security_counter(), Some(5));
        assert_eq!((counter.value, counter.writes), (1, 0));
        assert_eq!(
            boot(&rig, &mut counter).unwrap().image.security_counter(),
            Some(1)
        );
        rig.restore(&before);
        let trial = boot(&rig, &mut counter).unwrap();
        match trial.slot {
            Slot::Primary => rig.updater().confirm().unwrap(),
            Slot::Secondary => rig
                .updater_for(&rig.secondary, &rig.primary)
                .confirm()
                .unwrap(),
        }
        assert_eq!(
            boot(&rig, &mut counter).unwrap().image.security_counter(),
            Some(5)
        );
        assert_eq!((counter.value, counter.writes), (5, 1));
        boot(&rig, &mut counter).unwrap();
        assert_eq!(counter.writes, 1, "unchanged floor must not wear/burn OTP");
    }
}

#[test]
fn observes_confirmation_before_installing_the_next_trial() {
    let rig = rig(UpgradeMode::SwapScratch);
    rig.primary.program(0, &image(1, Some(1)));
    rig.stage_update(&image(2, Some(5)), false);
    let mut counter = Counter::at(1);
    boot(&rig, &mut counter).unwrap();
    rig.updater().confirm().unwrap();
    rig.stage_update(&image(3, Some(8)), false);
    assert_eq!(
        boot(&rig, &mut counter).unwrap().image.security_counter(),
        Some(8)
    );
    assert_eq!(
        counter.value, 5,
        "consume old confirmation before swap moves its trailer"
    );
    assert_eq!(
        boot(&rig, &mut counter).unwrap().image.security_counter(),
        Some(5)
    );
}

#[test]
fn permanent_modes_and_permanent_swaps_advance() {
    for mode in modes() {
        let rig = rig(mode);
        rig.primary.program(0, &image(1, Some(1)));
        rig.stage_update(&image(2, Some(5)), true);
        let mut counter = Counter::at(1);
        assert_eq!(
            boot(&rig, &mut counter).unwrap().image.security_counter(),
            Some(5)
        );
        assert_eq!(counter.value, 5);
    }
}

#[test]
fn missing_or_unrepresentable_counter_is_rejected_even_at_zero_floor() {
    for mode in modes() {
        for epoch in [None, Some(17)] {
            let rig = rig(mode);
            rig.primary.program(0, &image(1, epoch));
            rig.updater().confirm().unwrap();
            let mut counter = Counter::at(0);
            assert_eq!(boot(&rig, &mut counter), Err(Error::NoBootableImage));
            assert_eq!(counter.writes, 0);
        }
    }
}

#[test]
fn invalid_signature_cannot_advance_fuses() {
    let rig = rig(UpgradeMode::SwapScratch);
    let mut spec = Spec::new((9, 0, 0, 0), 6000, 9);
    spec.sec_cnt = Some(9);
    spec.corrupt_signature = true;
    rig.primary.program(0, &build_image(&spec));
    rig.updater().confirm().unwrap();
    let mut counter = Counter::at(1);
    assert_eq!(boot(&rig, &mut counter), Err(Error::NoBootableImage));
    assert_eq!(counter.writes, 0);
}

#[test]
fn read_failure_stops_before_any_flash_mutation_in_every_mode() {
    for mode in modes() {
        let rig = rig(mode);
        rig.primary.program(0, &image(1, Some(1)));
        rig.stage_update(&image(2, Some(5)), true);
        let before = rig.snapshot();
        let mut counter = Counter {
            read_error: true,
            ..Counter::at(1)
        };
        assert_eq!(
            boot(&rig, &mut counter),
            Err(Error::SecurityCounter(CounterError::Read))
        );
        assert_eq!(rig.snapshot(), before);
    }
}

#[test]
fn failed_or_unpersisted_burn_never_returns_a_boot_decision() {
    for silent in [false, true] {
        let rig = rig(UpgradeMode::SwapScratch);
        rig.primary.program(0, &image(5, Some(5)));
        rig.updater().confirm().unwrap();
        let mut counter = Counter {
            write_error: !silent,
            silent_write: silent,
            ..Counter::at(1)
        };
        assert_eq!(
            boot(&rig, &mut counter),
            Err(Error::SecurityCounter(if silent {
                CounterError::NotPersisted
            } else {
                CounterError::Write
            }))
        );
    }
}

#[test]
fn interrupted_burns_resume_monotonically() {
    for n in 0..4 {
        let rig = rig(UpgradeMode::SwapScratch);
        rig.primary.program(0, &image(5, Some(5)));
        rig.updater().confirm().unwrap();
        let mut counter = Counter {
            cut_after: Some(n),
            ..Counter::at(1)
        };
        assert_eq!(
            boot(&rig, &mut counter),
            Err(Error::SecurityCounter(CounterError::Write))
        );
        assert!(counter.value > 1 && counter.value <= 5);
        counter.cut_after = None;
        assert_eq!(
            boot(&rig, &mut counter).unwrap().image.security_counter(),
            Some(5)
        );
        assert_eq!(counter.value, 5);
        // External flash restoration cannot undo even an interrupted burn.
        rig.primary.restore(&vec![0xff; SLOT_SIZE]);
        rig.primary.program(0, &image(1, Some(1)));
        assert_eq!(boot(&rig, &mut counter), Err(Error::NoBootableImage));
    }
}

#[test]
fn permanent_swap_power_cuts_still_enforce_floor_after_resume() {
    let rig = rig(UpgradeMode::SwapScratch);
    rig.primary.program(0, &image(1, Some(1)));
    rig.updater().confirm().unwrap();
    rig.stage_update(&image(5, Some(5)), true);
    let before = rig.snapshot();
    rig.power.reset_count();
    boot(&rig, &mut Counter::at(1)).unwrap();
    let operations = rig.power.count();
    for n in 0..operations {
        rig.restore(&before);
        let mut counter = Counter::at(1);
        rig.power.fail_after(n);
        assert_eq!(
            boot(&rig, &mut counter),
            Err(Error::Flash(SimError::PowerLoss))
        );
        rig.power.unlimited();
        let decision = boot(&rig, &mut counter).unwrap();
        assert_eq!(decision.image.security_counter(), Some(5), "cut {n}");
        assert_eq!(counter.value, 5);
    }
}

#[test]
fn hardware_floor_preserves_static_minimum_and_updater_fingerprint() {
    let rig = Rig::new(BootConfig {
        policy: ferriboot_core::Policy {
            min_security_counter: Some(7),
            ..Default::default()
        },
        ..BootConfig::default()
    });
    rig.primary.program(0, &image(5, Some(5)));
    let fingerprint = loader(&rig).security_config_fingerprint();
    let mut counter = Counter::at(1);
    let mut loader = loader(&rig).with_security_counter_store(&mut counter);
    assert_eq!(loader.boot(), Err(Error::NoBootableImage));
    assert_eq!(loader.security_config_fingerprint(), fingerprint);
}

#[test]
fn factory_epoch_is_committed_before_the_first_trial() {
    let rig = rig(UpgradeMode::SwapScratch);
    rig.primary.program(0, &image(1, Some(5)));
    // Factory's erased trailer is implicitly permanent; confirm() is a no-op.
    rig.stage_update(&image(2, Some(8)), false);
    let mut counter = Counter::at(0);
    assert_eq!(
        boot(&rig, &mut counter).unwrap().image.security_counter(),
        Some(8)
    );
    assert_eq!(counter.value, 5);
    assert_eq!(
        boot(&rig, &mut counter).unwrap().image.security_counter(),
        Some(5)
    );
}

#[test]
fn interrupted_trial_swaps_never_burn_the_trial_epoch() {
    let rig = rig(UpgradeMode::SwapScratch);
    rig.primary.program(0, &image(1, Some(1)));
    rig.stage_update(&image(5, Some(5)), false);
    let before = rig.snapshot();
    rig.power.reset_count();
    boot(&rig, &mut Counter::at(1)).unwrap();
    let operations = rig.power.count();
    for n in 0..operations {
        rig.restore(&before);
        let mut counter = Counter::at(1);
        rig.power.fail_after(n);
        assert_eq!(
            boot(&rig, &mut counter),
            Err(Error::Flash(SimError::PowerLoss))
        );
        assert_eq!(counter.value, 1, "cut {n}");
        rig.power.unlimited();
        let decision = boot(&rig, &mut counter).unwrap();
        assert!(matches!(decision.image.security_counter(), Some(1 | 5)));
        assert_eq!(counter.value, 1, "resume {n}");
        assert_eq!(counter.writes, 0);
    }
}
