use common::*;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use ferriboot_core::{
    BootConfig, BootLoader, ConfigError, FirmwareUpdater, ImageSlots, Slot, UpgradeMode,
};
mod common;

#[derive(Clone)]
struct LargeReadFlash(SimFlash);

impl ErrorType for LargeReadFlash {
    type Error = SimError;
}

impl ReadNorFlash for LargeReadFlash {
    const READ_SIZE: usize = 128;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SimError> {
        let off = offset as usize;
        if !off.is_multiple_of(Self::READ_SIZE) || !bytes.len().is_multiple_of(Self::READ_SIZE) {
            return Err(SimError::NotAligned);
        }
        let snapshot = self.0.snapshot();
        if off + bytes.len() > snapshot.len() {
            return Err(SimError::OutOfBounds);
        }
        bytes.copy_from_slice(&snapshot[off..off + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl NorFlash for LargeReadFlash {
    const WRITE_SIZE: usize = SimFlash::WRITE_SIZE;
    const ERASE_SIZE: usize = SimFlash::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SimError> {
        self.0.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), SimError> {
        self.0.erase(from, to)
    }
}

#[test]
fn constructors_reject_unsupported_read_geometry() {
    let rig = Rig::new(BootConfig::default());
    let primary = LargeReadFlash(rig.primary.clone());
    let secondary = LargeReadFlash(rig.secondary.clone());
    let scratch = LargeReadFlash(rig.scratch.clone());
    assert!(matches!(
        BootLoader::new(
            primary.clone(),
            secondary.clone(),
            Some(scratch),
            &rig.keys,
            rig.config,
        ),
        Err(ConfigError::UnsupportedReadSize)
    ));
    assert!(matches!(
        FirmwareUpdater::new_with_mode(primary, secondary, rig.config.mode,),
        Err(ConfigError::UnsupportedReadSize)
    ));
}

#[test]
fn low_level_updater_does_not_require_signing_keys_or_policy() {
    let rig = Rig::new(BootConfig::default());
    let mut updater = FirmwareUpdater::new_with_mode(
        rig.primary.clone(),
        rig.secondary.clone(),
        UpgradeMode::SwapScratch,
    )
    .unwrap();
    assert_eq!(updater.slot_size(), SLOT_SIZE as u32);
    assert!(updater.inspect_update().is_err());
}

#[test]
fn xip_version_ties_always_select_physical_primary() {
    for revert in [false, true] {
        let rig = Rig::new(BootConfig {
            mode: UpgradeMode::DirectXip { revert },
            ..BootConfig::default()
        });
        let bytes = build_image(&Spec::new((1, 0, 0, 0), 1000, 1));
        rig.primary.program(0, &bytes);
        rig.secondary.program(0, &bytes);
        rig.updater().confirm().unwrap();
        rig.updater_for(&rig.secondary, &rig.primary)
            .confirm()
            .unwrap();
        for active_slot in [Slot::Primary, Slot::Secondary] {
            let (active, inactive) = if active_slot == Slot::Primary {
                (&rig.primary, &rig.secondary)
            } else {
                (&rig.secondary, &rig.primary)
            };
            let mut updater = rig.updater_for(active, inactive);
            let images = ImageSlots::new(
                updater.inspect_active().unwrap(),
                Some(updater.inspect_update().unwrap()),
                active_slot,
            );
            let state = updater.image_state(&images).unwrap();
            assert_eq!(state.pending, active_slot == Slot::Secondary);
            assert_eq!(state.in_use, state.pending);
            assert!(state.active_confirmed);
            assert!(state.inactive_confirmed);
        }
    }
}

#[test]
fn consumed_xip_trial_and_swap_rollback_have_different_protection() {
    for mode in [
        UpgradeMode::SwapScratch,
        UpgradeMode::DirectXip { revert: true },
    ] {
        let rig = Rig::new(BootConfig {
            mode,
            ..BootConfig::default()
        });
        rig.primary
            .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
        rig.updater().confirm().unwrap();
        rig.stage_update(&build_image(&Spec::new((2, 0, 0, 0), 1000, 2)), false);
        let decision = rig.boot().unwrap();
        let (running, inactive) = if decision.slot == Slot::Primary {
            (&rig.primary, &rig.secondary)
        } else {
            (&rig.secondary, &rig.primary)
        };
        let mut updater = rig.updater_for(running, inactive);
        let images = ImageSlots::new(
            *decision.image.header(),
            Some(updater.inspect_update().unwrap()),
            decision.slot,
        );
        let state = updater.image_state(&images).unwrap();
        assert!(!state.active_confirmed);
        assert!(
            state.in_use,
            "preserve the pending fallback or swap rollback"
        );
        updater.confirm().unwrap();
        assert!(!updater.image_state(&images).unwrap().in_use);
    }
}
