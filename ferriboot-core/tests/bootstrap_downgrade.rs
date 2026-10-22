//! Bootstrapping with downgrade prevention, and the confirmation of a primary
//! rejection before the primary image is replaced.
//!
//! After a confirmed swap upgrade the previous image stays in the secondary
//! slot. A broken primary image (bit rot, or one glitched validation) must not
//! reinstall it while downgrade prevention is enabled.

mod common;

use common::*;
use ferriboot_core::{
    BootConfig, BootLoader, Error, ImageError, Slot, SwapType, UpgradeMode, ValidatedImage,
};

fn image(major: u8, seed: u8) -> Vec<u8> {
    build_image(&Spec::new((major, 0, 0, 0), 20_000, seed))
}

fn bootstrap_rig(mode: UpgradeMode, downgrade_prevention: bool) -> Rig {
    Rig::new(BootConfig {
        mode,
        bootstrap: true,
        downgrade_prevention,
        ..BootConfig::default()
    })
}

/// v1 installed, v2 test-swapped in and confirmed: v1 remains in the secondary.
fn after_confirmed_upgrade(downgrade_prevention: bool) -> Rig {
    let rig = bootstrap_rig(UpgradeMode::SwapScratch, downgrade_prevention);
    rig.primary.program(0, &image(1, 1));
    rig.stage_update(&image(2, 2), false);
    assert_eq!(rig.boot().unwrap().performed, SwapType::Test);
    rig.updater().confirm().unwrap();
    assert_eq!(rig.version_in(&rig.primary), Some((2, 0, 0)));
    assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)));
    rig
}

/// Flip one byte in the primary image body, leaving its header readable.
fn corrupt_primary_body(rig: &Rig) {
    let off = 0x100;
    let byte = rig.primary.get(off..off + 1)[0];
    rig.primary.program(off, &[byte ^ 0xff]);
}

fn loader(rig: &Rig) -> BootLoader<'_, SimFlash> {
    BootLoader::new(
        rig.primary.clone(),
        rig.secondary.clone(),
        Some(rig.scratch.clone()),
        &rig.keys,
        rig.config,
    )
    .unwrap()
}

#[test]
fn broken_primary_does_not_reinstall_previous_version() {
    let rig = after_confirmed_upgrade(true);
    corrupt_primary_body(&rig);
    let primary = rig.primary.snapshot();

    for _ in 0..2 {
        assert_eq!(rig.boot(), Err(Error::NoBootableImage));
    }
    // Neither slot was touched: the broken v2 stays, and v1 is kept for
    // recovery instead of being erased.
    assert_eq!(rig.primary.snapshot(), primary);
    assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)));
}

#[test]
fn without_downgrade_prevention_broken_primary_is_still_recovered() {
    // The previous behaviour remains available as an explicit choice.
    let rig = after_confirmed_upgrade(false);
    corrupt_primary_body(&rig);
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (1, SwapType::Revert)
    );
}

#[test]
fn factory_and_same_version_bootstraps_still_work() {
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        // Empty primary: nothing to compare with.
        let rig = bootstrap_rig(mode, true);
        rig.secondary.program(0, &image(2, 2));
        rig.exhaustive(|rig, d| {
            assert_eq!(d.image.header().version.major, 2);
            assert_eq!(rig.version_in(&rig.primary), Some((2, 0, 0)));
        });

        // Broken primary, same version available in the secondary.
        let rig = bootstrap_rig(mode, true);
        rig.primary.program(0, &image(2, 1));
        corrupt_primary_body(&rig);
        rig.secondary.program(0, &image(2, 2));
        assert_eq!(
            rig.boot().unwrap().image.header().version.major,
            2,
            "{mode:?}"
        );
    }
}

#[test]
fn single_glitched_rejection_does_not_replace_primary() {
    // Downgrade prevention off, so only the confirmation stands between one
    // wrong rejection and the overwrite.
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        let rig = bootstrap_rig(mode, false);
        rig.primary.program(0, &image(2, 2));
        rig.updater().confirm().unwrap();
        rig.secondary.program(0, &image(1, 1));
        let snapshot = rig.snapshot();

        let mut primary_checks = 0;
        let d = loader(&rig)
            .boot_with_validator(&mut |_: &mut SimFlash,
                                       stored: Slot,
                                       _: Slot,
                                       _: &ValidatedImage| {
                if stored == Slot::Primary {
                    primary_checks += 1;
                    if primary_checks == 1 {
                        return Err(Error::Image(ImageError::PlatformRejected));
                    }
                }
                Ok(())
            })
            .unwrap();

        assert_eq!(
            (d.image.header().version.major, d.performed),
            (2, SwapType::None),
            "{mode:?}"
        );
        assert_eq!(rig.snapshot(), snapshot, "{mode:?}: flash was modified");
    }
}

#[test]
fn consistent_rejection_still_bootstraps() {
    // A primary image the platform always rejects is replaced as before.
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        let rig = bootstrap_rig(mode, false);
        rig.primary.program(0, &image(2, 2));
        rig.updater().confirm().unwrap();
        rig.secondary.program(0, &image(1, 1));

        let mut rejected_v2 = false;
        let d = loader(&rig)
            .boot_with_validator(
                &mut |_: &mut SimFlash, _: Slot, _: Slot, image: &ValidatedImage| {
                    if image.header().version.major == 2 {
                        rejected_v2 = true;
                        return Err(Error::Image(ImageError::PlatformRejected));
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(rejected_v2);
        assert_eq!(d.image.header().version.major, 1, "{mode:?}");
    }
}
