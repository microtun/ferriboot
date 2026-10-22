use common::*;
use ferriboot_core::{
    BootConfig, BootLoader, Error, ImageError, Slot, UpgradeMode, ValidatedImage,
};
mod common;

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
fn rejected_executable_never_replaces_the_primary_in_any_mode() {
    for mode in [
        UpgradeMode::SwapScratch,
        UpgradeMode::Overwrite,
        UpgradeMode::DirectXip { revert: false },
        UpgradeMode::DirectXip { revert: true },
    ] {
        let rig = Rig::new(BootConfig {
            mode,
            bootstrap: true,
            ..BootConfig::default()
        });
        rig.primary
            .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
        rig.updater().confirm().unwrap();
        rig.stage_update(&build_image(&Spec::new((2, 0, 0, 0), 1000, 2)), false);
        let primary = rig.primary.snapshot();
        let mut checked_candidate = false;
        let decision = loader(&rig)
            .boot_with_validator(
                &mut |_: &mut SimFlash, stored: Slot, target: Slot, image: &ValidatedImage| {
                    if image.header().version.major == 2 {
                        checked_candidate = true;
                        assert_eq!(stored, Slot::Secondary);
                        assert_eq!(
                            target,
                            if matches!(mode, UpgradeMode::DirectXip { .. }) {
                                stored
                            } else {
                                Slot::Primary
                            }
                        );
                        return Err(Error::Image(ImageError::PlatformRejected));
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(checked_candidate);
        assert_eq!(decision.image.header().version.major, 1);
        assert_eq!(
            rig.primary.snapshot(),
            primary,
            "rejection must preserve the working slot"
        );
        assert!(rig.secondary.snapshot().iter().all(|b| *b == 0xff));
    }
}

#[test]
fn validation_reads_the_stored_candidate_but_checks_the_execution_destination() {
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        let rig = Rig::new(BootConfig {
            mode,
            ..BootConfig::default()
        });
        rig.primary
            .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
        let spec = Spec::new((2, 0, 0, 0), 1000, 9);
        rig.stage_update(&build_image(&spec), true);
        let mut checked = false;
        let decision = loader(&rig)
            .boot_with_validator(
                &mut |flash: &mut SimFlash, stored: Slot, target: Slot, image: &ValidatedImage| {
                    assert_eq!(target, Slot::Primary);
                    if stored == Slot::Secondary {
                        let mut bytes = [0; 8];
                        ferriboot_core::flash::read(
                            flash,
                            u32::from(image.header().hdr_size),
                            &mut bytes,
                        )?;
                        assert_eq!(&bytes[..], &spec.payload[..8]);
                        checked = true;
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(checked);
        assert_eq!(decision.image.header().version.major, 2);
    }
}

#[test]
fn transient_platform_read_failure_does_not_erase_or_install_the_candidate() {
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        let rig = Rig::new(BootConfig {
            mode,
            ..BootConfig::default()
        });
        rig.primary
            .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
        rig.stage_update(&build_image(&Spec::new((2, 0, 0, 0), 1000, 2)), false);
        let before = rig.snapshot();
        assert_eq!(
            loader(&rig).boot_with_validator(
                &mut |_: &mut SimFlash, stored: Slot, _: Slot, _: &ValidatedImage| {
                    if stored == Slot::Secondary {
                        Err(Error::Flash(SimError::PowerLoss))
                    } else {
                        Ok(())
                    }
                }
            ),
            Err(Error::Flash(SimError::PowerLoss))
        );
        assert_eq!(rig.snapshot(), before);
    }
}

#[test]
fn bootstrap_also_rejects_unexecutable_candidates_before_copying() {
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        let rig = Rig::new(BootConfig {
            mode,
            bootstrap: true,
            ..BootConfig::default()
        });
        rig.secondary
            .program(0, &build_image(&Spec::new((2, 0, 0, 0), 1000, 2)));
        let before = rig.primary.snapshot();
        assert_eq!(
            loader(&rig).boot_with_validator(
                &mut |_: &mut SimFlash, _: Slot, _: Slot, _: &ValidatedImage| {
                    Err(Error::Image(ImageError::PlatformRejected))
                }
            ),
            Err(Error::NoBootableImage)
        );
        assert_eq!(rig.primary.snapshot(), before);
    }
}

#[test]
fn bootstrap_and_rollback_have_explicit_downgrade_semantics() {
    for mode in [UpgradeMode::SwapScratch, UpgradeMode::Overwrite] {
        let rig = Rig::new(BootConfig {
            mode,
            bootstrap: true,
            downgrade_prevention: true,
            ..BootConfig::default()
        });
        let mut broken = Spec::new((2, 0, 0, 0), 1000, 1);
        broken.corrupt_signature = true;
        rig.primary.program(0, &build_image(&broken));
        rig.secondary
            .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 2)));
        // Neither mode reinstalls the older image over a broken newer one,
        // and neither erases what may be the last authentic image.
        assert_eq!(rig.boot(), Err(Error::NoBootableImage), "{mode:?}");
        assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)), "{mode:?}");
    }
    let rig = Rig::new(BootConfig {
        downgrade_prevention: true,
        ..BootConfig::default()
    });
    rig.primary
        .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
    rig.stage_update(&build_image(&Spec::new((2, 0, 0, 0), 1000, 2)), false);
    assert_eq!(rig.boot().unwrap().image.header().version.major, 2);
    assert_eq!(
        rig.boot().unwrap().image.header().version.major,
        1,
        "rollback may restore an older image"
    );
}
