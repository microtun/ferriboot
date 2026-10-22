mod common;

use common::*;
use ferriboot_core::{
    BootConfig, Error, Slot, SwapType, UpgradeMode,
    image::flags,
    trailer::{Flag, MagicState},
};

fn v1() -> Vec<u8> {
    build_image(&Spec::new((1, 0, 0, 0), 40_000, 1))
}
fn v2() -> Vec<u8> {
    build_image(&Spec::new((2, 0, 0, 0), 61_234, 2))
}

fn swap_rig() -> Rig {
    let rig = Rig::new(BootConfig::default());
    rig.primary.program(0, &v1());
    rig
}

#[test]
fn boots_factory_image() {
    let rig = swap_rig();
    let d = rig.boot().unwrap();
    assert_eq!(d.slot, Slot::Primary);
    assert_eq!(d.performed, SwapType::None);
    assert_eq!(d.image.header().version.major, 1);
}

#[test]
fn test_upgrade_revert_and_confirm_survive_power_loss() {
    let rig = swap_rig();
    rig.stage_update(&v2(), false);

    // Test swap: v2 runs, v1 is kept in the secondary slot for a revert.
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 2);
        assert_eq!(rig.version_in(&rig.primary), Some((2, 0, 0)));
        assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)));
        let s = rig.updater().active_state().unwrap();
        assert_eq!(
            (s.magic, s.copy_done, s.image_ok),
            (MagicState::Good, Flag::Set, Flag::Unset)
        );
    });
    assert!(rig.updater().needs_confirmation().unwrap());
    let after_test = rig.snapshot();

    // Not confirmed: the next boot reverts to v1 and makes it permanent.
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 1);
        assert_eq!(rig.version_in(&rig.secondary), Some((2, 0, 0)));
        assert!(!rig.updater().needs_confirmation().unwrap());
    });
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (1, SwapType::None)
    );

    // Confirmed: v2 stays.
    rig.restore(&after_test);
    rig.updater().confirm().unwrap();
    assert!(!rig.updater().needs_confirmation().unwrap());
    for _ in 0..3 {
        let d = rig.boot().unwrap();
        assert_eq!(
            (d.image.header().version.major, d.performed),
            (2, SwapType::None)
        );
    }
}

#[test]
fn revert_ignores_corrupt_secondary_trailer() {
    // Upstream's revert row matches any secondary magic. A damaged secondary
    // trailer (here: a bad magic, as left by an interrupted trailer erase)
    // must not keep an unconfirmed test image running.
    let rig = swap_rig();
    rig.stage_update(&v2(), false);
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::Test)
    );
    assert!(rig.updater().needs_confirmation().unwrap());

    rig.secondary.program(SLOT_SIZE - 16, &[0u8; 16]);
    assert_eq!(rig.updater().update_state().unwrap().magic, MagicState::Bad);

    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 1);
        assert_eq!(rig.version_in(&rig.secondary), Some((2, 0, 0)));
        assert!(!rig.updater().needs_confirmation().unwrap());
    });
}

#[test]
fn permanent_upgrade_survives_power_loss() {
    let rig = swap_rig();
    rig.stage_update(&v2(), true);
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 2);
        let s = rig.updater().active_state().unwrap();
        assert_eq!((s.copy_done, s.image_ok), (Flag::Set, Flag::Set));
    });
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::None)
    );
}

#[test]
fn smaller_update_and_minimum_two_sector_scratch() {
    let mut rig = swap_rig();
    rig.scratch = SimFlash::new(2 * SECTOR, &rig.power);
    rig.primary
        .program(0, &build_image(&Spec::new((1, 0, 0, 0), 90_000, 3)));
    rig.stage_update(&build_image(&Spec::new((1, 1, 0, 0), 7_000, 4)), false);
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.minor, 1);
        assert_eq!(rig.version_in(&rig.secondary), Some((1, 0, 0)));
    });
}

#[test]
fn invalid_update_is_erased() {
    let rig = swap_rig();
    let mut spec = Spec::new((2, 0, 0, 0), 30_000, 2);
    spec.key = other_signing_key(); // trusted key, but tamper afterwards
    let mut img = build_image(&spec);
    rig.stage_update(&img, false);
    img[0x40] ^= 0xff;
    // Corrupt the staged copy in flash directly.
    rig.secondary.restore(&{
        let mut s = rig.secondary.snapshot();
        s[0x40] ^= 0xff;
        s
    });
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (1, SwapType::None)
    );
    assert!(
        rig.secondary.get(0..SLOT_SIZE).iter().all(|b| *b == 0xff),
        "secondary erased"
    );
}

#[test]
fn downgrade_prevention() {
    let rig = Rig::new(BootConfig {
        downgrade_prevention: true,
        ..BootConfig::default()
    });
    rig.primary.program(0, &v2());
    rig.stage_update(&v1(), true);
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::None)
    );
    assert_eq!(rig.version_in(&rig.secondary), None);
}

#[test]
fn bootstrap_from_secondary() {
    let mut cfg = BootConfig::default();
    assert_eq!(Rig::new(cfg).boot(), Err(Error::NoBootableImage));

    // Upstream copies the secondary image over an empty or broken primary
    // (`boot_copy_image`) and reports it as a REVERT so image_ok is set.
    cfg.bootstrap = true;
    let rig = Rig::new(cfg);
    rig.secondary.program(0, &v2());
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 2);
        assert_eq!(rig.version_in(&rig.primary), Some((2, 0, 0)));
    });
    let s = rig.updater().active_state().unwrap();
    assert_eq!(
        (s.magic, s.image_ok, s.copy_done),
        (MagicState::Good, Flag::Set, Flag::Set)
    );
    // Copied, not swapped: nothing is left behind in the secondary slot.
    assert_eq!(rig.version_in(&rig.secondary), None);
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::None)
    );
}

#[test]
fn bootstrap_reports_revert() {
    let rig = Rig::new(BootConfig {
        bootstrap: true,
        ..BootConfig::default()
    });
    rig.secondary.program(0, &v2());
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::Revert)
    );
}

#[test]
fn bootstrap_overwrites_invalid_primary_instead_of_swapping() {
    // With MCUBOOT_BOOTSTRAP, `boot_perform_update` copies rather than swaps
    // when the primary image does not validate, even for a test upgrade.
    let rig = Rig::new(BootConfig {
        bootstrap: true,
        ..BootConfig::default()
    });
    let mut broken = v1();
    broken[0x100] ^= 0xff;
    rig.primary.program(0, &broken);
    rig.stage_update(&v2(), false);
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::Test)
    );
    // The broken image was not swapped into the secondary slot.
    assert_eq!(rig.version_in(&rig.secondary), None);
    assert!(rig.secondary.get(0..SECTOR).iter().all(|b| *b == 0xff));
}

#[test]
fn bootstrap_in_overwrite_mode_survives_power_loss() {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::Overwrite,
        bootstrap: true,
        ..BootConfig::default()
    });
    rig.secondary.program(0, &v2());
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 2);
        assert_eq!(rig.version_in(&rig.primary), Some((2, 0, 0)));
    });
}

#[test]
fn corrupt_secondary_header_erases_the_slot() {
    // A header that is neither erased nor valid fails
    // `boot_check_header_valid`, and upstream erases the whole slot.
    let rig = swap_rig();
    rig.stage_update(&v2(), false);
    rig.secondary.program(0, &[0u8; 4]);
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (1, SwapType::None)
    );
    assert!(rig.secondary.get(0..SLOT_SIZE).iter().all(|b| *b == 0xff));
}

#[test]
fn non_bootable_secondary_only_loses_its_trailer() {
    let rig = swap_rig();
    rig.stage_update(&v2(), false);
    rig.secondary.program(16, &0x10u32.to_le_bytes()); // IMAGE_F_NON_BOOTABLE
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (1, SwapType::None)
    );
    assert_eq!(
        rig.updater().update_state().unwrap().magic,
        MagicState::Unset
    );
    assert_ne!(rig.secondary.get(0..4), vec![0xff; 4], "image kept");
}

#[test]
fn set_pending_on_bad_trailer_does_not_erase() {
    let rig = swap_rig();
    let mut up = rig.updater();
    {
        let mut w = up.begin_update().unwrap();
        w.write(&v2()).unwrap();
        w.finish().unwrap();
    }
    rig.secondary.program(SLOT_SIZE - 16, &[0u8; 16]);
    assert_eq!(up.mark_for_test(), Err(Error::BadTrailer));
    assert_eq!(rig.version_in(&rig.secondary), Some((2, 0, 0)));
}

#[test]
fn pending_request_only_writes_trailer_bootloader_rejects_tampered_payload() {
    let rig = swap_rig();
    let mut up = rig.updater();
    {
        let mut writer = up.begin_update().unwrap();
        writer.write(&v2()).unwrap();
        writer.finish().unwrap();
    }
    // No signature verification when marking pending. A corrupted body
    // remains structurally valid and must fail bootloader authentication.
    rig.secondary.program(40, &[0]);
    up.mark_for_test().unwrap();
    assert_eq!(up.update_state().unwrap().magic, MagicState::Good);
    drop(up);
    assert_eq!(rig.boot().unwrap().image.header().version.major, 1);
}

#[test]
fn overwrite_survives_power_loss() {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::Overwrite,
        ..BootConfig::default()
    });
    rig.primary.program(0, &v1());
    rig.stage_update(&v2(), false);
    rig.exhaustive(|rig, d| {
        assert_eq!(d.image.header().version.major, 2);
        assert_eq!(
            rig.updater().update_state().unwrap().magic,
            MagicState::Unset
        );
        // As with upstream (OVERWRITE_ONLY_FAST writes the magic; the
        // full-slot copy carries the secondary trailer), the primary is left
        // with a good magic and an unset image_ok.
        let s = rig.updater().active_state().unwrap();
        assert_eq!(
            (s.magic, s.image_ok, s.copy_done),
            (MagicState::Good, Flag::Unset, Flag::Unset)
        );
    });
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::None)
    );
    // boot_set_confirmed then records image_ok, and nothing is reverted.
    assert!(rig.updater().needs_confirmation().unwrap());
    rig.updater().confirm().unwrap();
    assert!(!rig.updater().needs_confirmation().unwrap());
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.image.header().version.major, d.performed),
        (2, SwapType::None)
    );
}

#[test]
fn direct_xip_with_revert() {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::DirectXip { revert: true },
        ..BootConfig::default()
    });
    // Factory image must be confirmed (imgtool --pad --confirm).
    rig.primary.program(0, &v1());
    let mut up = rig.updater_for(&rig.secondary, &rig.primary);
    up.mark_permanent().unwrap();

    let d = rig.boot().unwrap();
    assert_eq!((d.slot, d.image.header().version.major), (Slot::Primary, 1));

    // Upload v2 to the inactive (secondary) slot as a test image.
    rig.stage(&rig.primary, &rig.secondary, &v2(), false);
    let after_stage = rig.snapshot();
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.slot, d.image.header().version.major),
        (Slot::Secondary, 2)
    );
    // Not confirmed: v2 is erased and v1 boots again.
    let d = rig.boot().unwrap();
    assert_eq!((d.slot, d.image.header().version.major), (Slot::Primary, 1));
    assert_eq!(rig.version_in(&rig.secondary), None);

    // Confirmed this time.
    rig.restore(&after_stage);
    let d = rig.boot().unwrap();
    assert_eq!(d.slot, Slot::Secondary);
    rig.updater_for(&rig.secondary, &rig.primary)
        .confirm()
        .unwrap();
    for _ in 0..2 {
        let d = rig.boot().unwrap();
        assert_eq!(
            (d.slot, d.image.header().version.major),
            (Slot::Secondary, 2)
        );
    }
}

#[test]
fn direct_xip_without_revert_picks_newest_valid() {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::DirectXip { revert: false },
        ..BootConfig::default()
    });
    rig.primary.program(0, &v1());
    let mut bad = Spec::new((3, 0, 0, 0), 1000, 5);
    bad.corrupt_signature = true;
    rig.secondary.program(0, &build_image(&bad));
    let d = rig.boot().unwrap();
    assert_eq!((d.slot, d.image.header().version.major), (Slot::Primary, 1));
    rig.secondary.program(0, &vec![0xff; SLOT_SIZE]);
    rig.secondary.program(0, &v2());
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.slot, d.image.header().version.major),
        (Slot::Secondary, 2)
    );
}

#[test]
fn direct_xip_honours_rom_fixed() {
    const PRIMARY: u32 = 0x1_0000;
    const SECONDARY: u32 = 0x4_0000;
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::DirectXip { revert: false },
        slot_addresses: Some([PRIMARY, SECONDARY]),
        ..BootConfig::default()
    });
    rig.primary.program(0, &v1());

    // Built with `--rom-fixed` for the primary slot, but stored in the
    // secondary: upstream skips it without erasing it.
    let mut wrong = Spec::new((2, 0, 0, 0), 2000, 7);
    wrong.flags = flags::ROM_FIXED;
    wrong.load_addr = PRIMARY;
    rig.secondary.program(0, &build_image(&wrong));
    let d = rig.boot().unwrap();
    assert_eq!((d.slot, d.image.header().version.major), (Slot::Primary, 1));
    assert_eq!(rig.version_in(&rig.secondary), Some((2, 0, 0)));

    let mut right = wrong.clone();
    right.load_addr = SECONDARY;
    rig.secondary.program(0, &vec![0xff; SLOT_SIZE]);
    rig.secondary.program(0, &build_image(&right));
    let d = rig.boot().unwrap();
    assert_eq!(
        (d.slot, d.image.header().version.major),
        (Slot::Secondary, 2)
    );

    // Without configured addresses the flag is not checked.
    let rig2 = Rig::new(BootConfig {
        mode: UpgradeMode::DirectXip { revert: false },
        ..BootConfig::default()
    });
    rig2.primary.program(0, &v1());
    rig2.secondary.program(0, &build_image(&wrong));
    let d = rig2.boot().unwrap();
    assert_eq!(d.slot, Slot::Secondary);
}

#[test]
fn strategy_features_map_to_one_mode() {
    assert_eq!(
        UpgradeMode::from_features(false, false, false),
        UpgradeMode::SwapScratch
    );
    assert_eq!(
        UpgradeMode::from_features(true, false, false),
        UpgradeMode::Overwrite
    );
    assert_eq!(
        UpgradeMode::from_features(false, true, false),
        UpgradeMode::DirectXip { revert: false }
    );
    assert_eq!(
        UpgradeMode::from_features(false, false, true),
        UpgradeMode::DirectXip { revert: true }
    );
    // Board crates evaluate this in a `const`, turning the panic into a
    // compile error.
    const _: UpgradeMode = UpgradeMode::from_features(false, true, false);
}

#[test]
#[should_panic(expected = "select at most one upgrade strategy feature")]
fn conflicting_strategy_features_are_rejected() {
    let _ = UpgradeMode::from_features(false, true, true);
}
