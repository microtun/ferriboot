//! Detailed QEMU scenarios (see `common` for the virtual-board fixtures).
#![cfg(all(feature = "regression", feature = "qemu"))]

mod common;

use common::*;

const V: Variant = Variant::DirectXipRevert;

fn stage_trial(flash: &FlashImage, image: &[u8]) {
    flash
        .stage(V.mode(), Slot::Secondary, image, Request::Test)
        .unwrap();
}

#[test]
fn factory_image_runs_from_primary() {
    let dut = Dut::boot(&factory(V)).unwrap();
    let i = expect_running(&dut, 0, "1.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "mode"), Some("DirectXip"));
    assert_eq!(field(&info, "slot"), Some("Primary"));
}

/// The newer trial runs from the secondary slot, confirms itself, and keeps
/// running there after a reset.
#[test]
fn confirmed_trial_runs_in_place_from_secondary() {
    common::checks::confirmed_trial_runs_in_place_from_secondary();
}

/// An unconfirmed trial is consumed by its first boot; the next boot erases
/// it and falls back to the confirmed image.
#[test]
fn unconfirmed_trial_is_erased_and_primary_resumes() {
    for (behaviour, marker) in [
        (Behaviour::ResetNoConfirm, "@fbt resetting"),
        (Behaviour::Panic, "deliberate test panic"),
        (Behaviour::Fault, "@fbt fault"),
    ] {
        let flash = factory(V);
        behave(&flash, &[("2.0.0", behaviour)]);
        stage_trial(&flash, &signed("2.0.0"));
        let dut = Dut::boot(&flash).unwrap();
        let i = expect_running(&dut, 0, "2.0.0");
        let (i, _) = dut.expect_from(i, marker, BOOT).unwrap();
        let (i, _) = dut.expect_from(i, "erasing Secondary slot", BOOT).unwrap();
        let i = expect_running(&dut, i, "1.0.0");
        dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    }
}

#[test]
fn hung_trial_is_erased_after_reset() {
    let flash = factory(V);
    behave(&flash, &[("2.0.0", Behaviour::HangNoFeed)]);
    stage_trial(&flash, &signed("2.0.0"));
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    dut.expect_from(i, "@fbt hang", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap(); // sooner than the 15 s hardware watchdog
    let (m, _) = dut.expect_from(m, "erasing Secondary slot", BOOT).unwrap();
    expect_running(&dut, m, "1.0.0");
}

#[test]
fn malformed_trial_is_rejected_and_erased() {
    for case in malformed(&payload()) {
        let flash = factory(V);
        stage_trial(
            &flash,
            &sign::sign(case.image.bytes(), &SignOptions::new("2.0.0")).unwrap(),
        );
        let dut = Dut::boot(&flash).unwrap();
        let (i, _) = dut
            .expect_from(0, PLATFORM_REJECTED, BOOT)
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let (i, _) = dut.expect_from(i, "erasing Secondary slot", BOOT).unwrap();
        let i = expect_running(&dut, i, "1.0.0");
        dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    }
}

/// `imgtool --rom-fixed`: an image linked for a different runtime address is
/// skipped (not erased) when stored in the secondary slot.
#[test]
fn rom_fixed_image_in_the_wrong_slot_is_skipped() {
    let flash = factory(V);
    let wrong_base = wrong_rom_fixed(&flash);
    let image = sign::sign(
        payload().bytes(),
        &SignOptions::new("2.0.0")
            .arg("--rom-fixed")
            .arg(format!("{wrong_base:#x}")),
    )
    .unwrap();
    stage_trial(&flash, &image);
    let dut = Dut::boot(&flash).unwrap();
    dut.expect_from(0, "holds an image built for", BOOT)
        .unwrap();
    expect_running(&dut, 0, "1.0.0");
    // Prove the skipped image was retained without requiring a flash dump: on
    // the next boot it must be discovered and skipped again.
    let m = dut.mark();
    dut.reset().unwrap();
    let (m, _) = dut
        .expect_from(m, "holds an image built for", BOOT)
        .unwrap();
    expect_running(&dut, m, "1.0.0");
}

#[test]
fn watchdog_resets_trial_and_erases_secondary_without_host_reset() {
    common::checks::watchdog_resets_trial_and_erases_secondary_without_host_reset();
}

/// Slot selection and trailer transitions must work in both directions.
#[test]
fn unconfirmed_primary_trial_is_erased_and_secondary_resumes() {
    let flash = blank(V);
    flash
        .install(Slot::Secondary, &signed("1.0.0"))
        .confirm_raw(Slot::Secondary);
    behave(&flash, &[("2.0.0", Behaviour::Idle)]);
    flash
        .stage(V.mode(), Slot::Primary, &signed("2.0.0"), Request::Test)
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "slot"), Some("Primary"));
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("true"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    let (i, _) = dut.expect_from(m, "erasing Primary slot", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "slot"), Some("Secondary"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    expect_running(&dut, m, "1.0.0");
}

#[test]
fn permanent_image_in_either_slot_survives_without_confirmation() {
    for slot in [Slot::Primary, Slot::Secondary] {
        let flash = blank(V);
        flash
            .install(slot.other(), &signed("1.0.0"))
            .confirm_raw(slot.other());
        behave(&flash, &[("2.0.0", Behaviour::Idle)]);
        flash
            .stage(V.mode(), slot, &signed("2.0.0"), Request::Permanent)
            .unwrap();
        let dut = Dut::boot(&flash).unwrap();
        let i = expect_running(&dut, 0, "2.0.0");
        let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
        let expected_slot = format!("{slot:?}");
        assert_eq!(field(&info, "slot"), Some(expected_slot.as_str()));
        let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
        assert_eq!(field(&image, "pending"), Some("false"));
        dut.expect_from(i, "@fbt idle", BOOT).unwrap();
        let m = dut.mark();
        dut.reset().unwrap();
        expect_running(&dut, m, "2.0.0");
    }
}

#[test]
fn unmarked_secondary_is_erased_in_revert_mode() {
    let flash = factory(V);
    flash.install(Slot::Secondary, &signed("2.0.0"));
    let dut = Dut::boot(&flash).unwrap();
    let (i, _) = dut.expect_from(0, "erasing Secondary slot", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    expect_running(&dut, m, "1.0.0");
    dut.expect_quiet_from(m, QUIET, "repeated erase", |l| {
        l.contains("erasing Secondary slot")
    })
    .unwrap();
}
