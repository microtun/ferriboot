//! The update slot holds an unconfirmed trial's fallback image. The updater
//! must refuse to replace or erase it until the running image is confirmed.
use common::*;
use ferriboot_core::{
    BootConfig, Error, ImageHeader, ImageSlots, Slot, SwapType, UpgradeMode,
    upload::{FirmwareUpload, UploadError, UploadRequest},
};
mod common;

fn v(major: u8, seed: u8) -> Vec<u8> {
    build_image(&Spec::new((major, 0, 0, 0), 9000, seed))
}

fn header(f: &SimFlash) -> ImageHeader {
    ImageHeader::read(&mut f.clone()).unwrap().unwrap()
}

/// Swap mode, running v2 as an unconfirmed test image with v1 parked in the
/// secondary slot for the revert.
fn swap_trial() -> Rig {
    let rig = Rig::new(BootConfig::default());
    rig.primary.program(0, &v(1, 1));
    rig.boot().unwrap();
    rig.stage_update(&v(2, 2), false);
    assert_eq!(rig.boot().unwrap().performed, SwapType::Test);
    assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)));
    rig
}

/// Direct-XIP with revert, running v2 from the secondary slot as an
/// unconfirmed trial with confirmed v1 in the primary slot.
fn xip_trial() -> Rig {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::DirectXip { revert: true },
        ..BootConfig::default()
    });
    rig.primary.program(0, &v(1, 1));
    rig.updater_for(&rig.primary, &rig.secondary)
        .confirm()
        .unwrap();
    assert_eq!(rig.boot().unwrap().slot, Slot::Primary);
    rig.stage(&rig.primary, &rig.secondary, &v(2, 2), false);
    assert_eq!(rig.boot().unwrap().slot, Slot::Secondary);
    rig
}

#[test]
fn swap_trial_refuses_begin_and_erase_and_still_reverts() {
    let rig = swap_trial();
    let before = rig.snapshot();
    let mut up = rig.updater();
    assert!(matches!(up.begin_update(), Err(Error::UpdateSlotInUse)));
    assert_eq!(up.erase_update(), Err(Error::UpdateSlotInUse));
    drop(up);
    assert_eq!(rig.snapshot(), before, "refusal must not touch flash");

    // The trial is never confirmed: the next boot reverts to v1.
    let d = rig.boot().unwrap();
    assert_eq!(d.performed, SwapType::Revert);
    assert_eq!(d.image.header().version.major, 1);
}

#[test]
fn swap_trial_allows_update_after_confirm() {
    let rig = swap_trial();
    let mut up = rig.updater();
    up.confirm().unwrap();
    {
        let mut w = up.begin_update().unwrap();
        w.write(&v(3, 3)).unwrap();
        w.finish().unwrap();
    }
    up.mark_for_test().unwrap();
    drop(up);
    let d = rig.boot().unwrap();
    assert_eq!(d.performed, SwapType::Test);
    assert_eq!(d.image.header().version.major, 3);
}

#[test]
fn xip_revert_trial_refuses_begin_and_falls_back() {
    let rig = xip_trial();
    let before = rig.snapshot();
    // The app runs from the secondary slot; its update slot is the primary.
    let mut up = rig.updater_for(&rig.secondary, &rig.primary);
    assert!(matches!(up.begin_update(), Err(Error::UpdateSlotInUse)));
    assert_eq!(up.erase_update(), Err(Error::UpdateSlotInUse));
    drop(up);
    assert_eq!(rig.snapshot(), before);

    // Previously this left nothing bootable. Now the unconfirmed trial is
    // erased and confirmed v1 runs again.
    let d = rig.boot().unwrap();
    assert_eq!((d.slot, d.image.header().version.major), (Slot::Primary, 1));
}

#[test]
fn xip_revert_trial_allows_update_after_confirm() {
    let rig = xip_trial();
    let mut up = rig.updater_for(&rig.secondary, &rig.primary);
    up.confirm().unwrap();
    {
        let mut w = up.begin_update().unwrap();
        w.write(&v(3, 3)).unwrap();
        w.finish().unwrap();
    }
    up.mark_for_test().unwrap();
    drop(up);
    let d = rig.boot().unwrap();
    assert_eq!((d.slot, d.image.header().version.major), (Slot::Primary, 3));
}

#[test]
fn modes_without_a_fallback_are_unaffected() {
    for mode in [
        UpgradeMode::Overwrite,
        UpgradeMode::DirectXip { revert: false },
    ] {
        let rig = Rig::new(BootConfig {
            mode,
            ..BootConfig::default()
        });
        rig.primary.program(0, &v(1, 1));
        rig.boot().unwrap();
        rig.stage_update(&v(2, 2), false);
        rig.boot().unwrap();
        // Overwrite leaves a good primary magic without image_ok; neither
        // mode has a fallback for the update slot to protect.
        let mut up = rig.updater();
        assert!(up.begin_update().is_ok(), "{mode:?}");
        assert!(up.erase_update().is_ok(), "{mode:?}");
    }
}

#[test]
fn upload_during_trial_is_pending_even_with_stale_cached_headers() {
    for (rig, active, update) in [
        {
            let r = swap_trial();
            let (a, u) = (r.primary.clone(), r.secondary.clone());
            (r, a, u)
        },
        {
            let r = xip_trial();
            let (a, u) = (r.secondary.clone(), r.primary.clone());
            (r, a, u)
        },
    ] {
        let before = rig.snapshot();
        let mut up = rig.updater_for(&active, &update);
        // A cache that forgot the inactive image must not unlock the slot.
        let images = ImageSlots::new(header(&active), None, Slot::Primary);
        let image = v(3, 3);
        let mut upload = FirmwareUpload::new(false);
        let result = upload.receive(
            &mut up,
            &images,
            UploadRequest {
                off: 0,
                len: Some(image.len() as u32),
                sha: None,
                upgrade: false,
                data: &image[..1000],
            },
            0,
        );
        assert_eq!(result, Err(UploadError::Pending));
        assert!(!upload.is_active());
        assert!(!upload.cleanup_required());
        drop(up);
        assert_eq!(rig.snapshot(), before, "fallback image must survive");
    }
}
