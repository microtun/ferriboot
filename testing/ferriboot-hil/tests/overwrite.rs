//! Detailed QEMU scenarios (see `common` for the virtual-board fixtures).
#![cfg(all(feature = "regression", feature = "qemu"))]

mod common;

use common::*;

const V: Variant = Variant::Overwrite;

#[test]
fn unconfirmed_upgrade_is_not_reverted() {
    let flash = factory(V);
    behave(&flash, &[("2.0.0", Behaviour::ResetNoConfirm)]);
    flash
        .stage(V.mode(), Slot::Secondary, &signed("2.0.0"), Request::Test)
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let (_, info) = dut.expect_from(0, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "mode"), Some("Overwrite"));
    let i = expect_running(&dut, 0, "2.0.0");
    // The image resets itself without confirming; overwrite keeps it anyway.
    let (i, _) = dut.expect_from(i, "@fbt resetting", BOOT).unwrap();
    expect_running(&dut, i, "2.0.0");
}

#[test]
fn downgrade_is_rejected() {
    let flash = factory(V);
    flash.stage_raw(Slot::Secondary, &signed("0.9.0"), Request::Permanent);
    let dut = Dut::boot(&flash).unwrap();
    let (i, _) = dut.expect_from(0, "rejecting downgrade", BOOT).unwrap();
    let (i, _) = dut.expect_from(i, "erasing Secondary slot", BOOT).unwrap();
    expect_running(&dut, i, "1.0.0");
}

#[test]
fn confirmed_upgrade_runs_from_primary_and_persists() {
    common::checks::confirmed_upgrade_runs_from_primary_and_persists();
}

/// A trusted signature cannot make an unsafe executable eligible for copying.
#[test]
fn malformed_executables_are_rejected_before_overwrite() {
    for case in malformed(&payload()) {
        let image = sign::sign(case.image.bytes(), &SignOptions::new("2.0.0")).unwrap();
        let flash = factory(V);
        flash
            .stage(V.mode(), Slot::Secondary, &image, Request::Permanent)
            .unwrap();
        let dut = Dut::boot(&flash).unwrap();
        let (i, _) = dut
            .expect_from(0, PLATFORM_REJECTED, BOOT)
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let (i, _) = dut.expect_from(i, "erasing Secondary slot", BOOT).unwrap();
        let i = expect_running(&dut, i, "1.0.0");
        dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    }
}
