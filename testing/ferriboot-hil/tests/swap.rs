//! Detailed QEMU scenarios (see `common` for the virtual-board fixtures).
#![cfg(all(feature = "regression", feature = "qemu"))]

mod common;

use std::time::Duration;

use common::*;

const V: Variant = Variant::SwapScratch;

fn stage(flash: &FlashImage, image: &[u8], request: Request) {
    flash
        .stage(V.mode(), Slot::Secondary, image, request)
        .unwrap();
}

#[test]
fn boots_factory_image_with_a_valid_handoff() {
    let dut = Dut::boot(&factory(V)).unwrap();
    let i = expect_running(&dut, 0, "1.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "mode"), Some("SwapScratch"));
    assert_eq!(field(&info, "slot"), Some("Primary"));
    assert_eq!(field(&info, "max"), Some(MAX_IMAGE_SIZE));
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("false"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

#[test]
fn erased_device_halts_without_running_anything() {
    let dut = Dut::boot(&blank(V)).unwrap();
    dut.expect_from(0, NO_EXECUTABLE, BOOT).unwrap();
    dut.expect_quiet_from(0, QUIET, "application output", |l| l.contains("@fbt"))
        .unwrap();
}

#[test]
fn confirmed_test_upgrade_persists_across_reset() {
    common::checks::confirmed_test_upgrade_persists_across_reset();
}

/// A test image that resets itself before confirming is reverted by the
/// next boot without outside help.
fn reverts_after_self_reset(behaviour: Behaviour, marker: &str) {
    let flash = factory(V);
    behave(&flash, &[("2.0.0", behaviour)]);
    stage(&flash, &signed("2.0.0"), Request::Test);
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (i, _) = dut.expect_from(i, marker, BOOT).unwrap();
    let (i, _) = dut.expect_from(i, "swap type: Revert", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

#[test]
fn test_upgrade_that_resets_before_confirming_is_reverted() {
    reverts_after_self_reset(Behaviour::ResetNoConfirm, "@fbt resetting");
}

#[test]
fn test_upgrade_that_panics_is_reverted() {
    // The firmware's panic handler prints the panic and resets.
    reverts_after_self_reset(Behaviour::Panic, "deliberate test panic");
}

#[test]
fn test_upgrade_that_faults_is_reverted() {
    reverts_after_self_reset(Behaviour::Fault, "@fbt fault");
}

/// A test image that hangs is reverted by the next reset, here supplied before
/// the 15 s hardware watchdog fires. The watchdog scenario below covers the
/// autonomous reset.
#[test]
fn test_upgrade_that_hangs_is_reverted_on_reset() {
    let flash = factory(V);
    behave(&flash, &[("2.0.0", Behaviour::HangNoFeed)]);
    stage(&flash, &signed("2.0.0"), Request::Test);
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (i, _) = dut.expect_from(i, "@fbt hang", BOOT).unwrap();
    dut.expect_quiet_from(i + 1, Duration::from_secs(3), "further output", |_| true)
        .unwrap();

    let m = dut.mark();
    dut.reset().unwrap();
    let (i, _) = dut.expect_from(m, "swap type: Revert", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

#[test]
fn test_upgrade_that_never_confirms_is_reverted_on_reset() {
    let flash = factory(V);
    behave(&flash, &[("2.0.0", Behaviour::Idle)]);
    stage(&flash, &signed("2.0.0"), Request::Test);
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    let i = expect_running(&dut, m, "1.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

/// A hung test image is reset by the hardware watchdog (shortened to 3 s) and
/// reverted without outside help.
#[test]
fn hardware_watchdog_resets_a_hung_test_image() {
    common::checks::hardware_watchdog_resets_a_hung_test_image();
}

#[test]
fn permanent_upgrade_is_kept_without_confirmation() {
    let flash = factory(V);
    behave(&flash, &[("2.0.0", Behaviour::Idle)]);
    stage(&flash, &signed("2.0.0"), Request::Permanent);
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("false"));
    let m = dut.mark();
    dut.reset().unwrap();
    expect_running(&dut, m, "2.0.0");
}

/// Pending images that fail authentication or policy are erased and the
/// running image is kept.
#[test]
fn unauthentic_and_downgrade_candidates_are_erased() {
    let v2 = signed("2.0.0");
    let untrusted = sign::sign(
        payload().bytes(),
        &SignOptions::new("9.0.0").key(Key::Untrusted),
    )
    .unwrap();
    let invalid = "secondary slot is not valid";
    let cases: [(&str, Vec<u8>, &str); 5] = [
        (
            "bad signature",
            sign::corrupt_signature(v2.clone()),
            invalid,
        ),
        (
            "modified payload",
            sign::flip_payload_bit(v2.clone(), 0x200),
            invalid,
        ),
        ("modified hash", sign::corrupt_hash_tlv(v2.clone()), invalid),
        ("untrusted key", untrusted, invalid),
        ("downgrade", signed("0.9.0"), "rejecting downgrade"),
    ];
    for (name, image, reason) in cases {
        let flash = factory(V);
        flash
            .stage(V.mode(), Slot::Secondary, &image, Request::Test)
            .unwrap();
        let dut = Dut::boot(&flash).unwrap();
        let (i, _) = dut
            .expect_from(0, reason, BOOT)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let (i, _) = dut
            .expect_from(i, "erasing Secondary slot", BOOT)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let i = expect_running(&dut, i, "1.0.0");
        dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    }
}

/// Correctly signed images whose platform payload breaks the executable contract
/// are refused before any swap, and the candidate is erased.
#[test]
fn malformed_executables_are_rejected_before_installation() {
    for case in malformed(&payload()) {
        let image = sign::sign(case.image.bytes(), &SignOptions::new("2.0.0")).unwrap();
        let flash = factory(V);
        flash
            .stage(V.mode(), Slot::Secondary, &image, Request::Test)
            .unwrap();
        let dut = Dut::boot(&flash).unwrap();
        let name = case.name;
        let (i, _) = dut
            .expect_from(0, PLATFORM_REJECTED, BOOT)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let (i, _) = dut
            .expect_from(i, "erasing Secondary slot", BOOT)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let i = expect_running(&dut, i, "1.0.0");
        dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    }
}

/// A correctly signed primary image that violates the executable contract must
/// never run, even with nothing else to boot.
#[test]
fn malformed_primary_is_never_started() {
    for case in malformed(&payload()) {
        let flash = blank(V);
        let image = sign::sign(case.image.bytes(), &SignOptions::new("1.0.0")).unwrap();
        flash
            .install(Slot::Primary, &image)
            .confirm_raw(Slot::Primary);
        let dut = Dut::boot(&flash).unwrap();
        dut.expect_from(0, NO_EXECUTABLE, BOOT)
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        dut.expect_quiet_from(0, QUIET, "application output", |l| {
            l.contains("@fbt") || l.contains(STARTING_IMAGE)
        })
        .unwrap();
    }
}

#[test]
fn corrupted_primary_without_fallback_halts() {
    let flash = factory(V);
    let mut primary = flash.read(flash.layout.primary.offset, 0x3_0000);
    primary[sign::HEADER_SIZE as usize + 0x400] ^= 0x10;
    flash.install(Slot::Primary, &primary);
    let dut = Dut::boot(&flash).unwrap();
    dut.expect_from(0, "primary slot is not valid", BOOT)
        .unwrap();
    dut.expect_quiet_from(0, QUIET, "application output", |l| l.contains("@fbt"))
        .unwrap();
}

/// An application that destroys its own image is recovered by bootstrapping
/// the valid image still in the secondary slot.
#[test]
fn self_erasing_application_is_recovered_by_bootstrap() {
    let flash = blank(V);
    behave(&flash, &[("1.0.0", Behaviour::EraseActive)]);
    flash
        .install(Slot::Primary, &signed("1.0.0"))
        .confirm_raw(Slot::Primary)
        .install(Slot::Secondary, &signed("2.0.0"));
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "1.0.0");
    let (i, _) = dut.expect_from(i, "@fbt erased-active", BOOT).unwrap();
    let i = expect_running(&dut, i, "2.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

/// With erased storage the hooks stay out of the way: the firmware takes its
/// normal path and starts its ordinary application loop.
#[test]
fn without_a_hook_record_the_firmware_runs_normally() {
    common::checks::without_a_hook_record_the_firmware_runs_normally();
}
