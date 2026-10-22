//! Detailed QEMU scenarios (see `common` for the virtual-board fixtures).
#![cfg(all(feature = "regression", feature = "qemu"))]

mod common;

use common::*;

#[test]
fn unconfirmed_image_is_kept_in_secondary_across_reset() {
    let v = Variant::DirectXip;
    let flash = factory(v);
    behave(&flash, &[("2.0.0", Behaviour::Idle)]);
    flash
        .stage(v.mode(), Slot::Secondary, &signed("2.0.0"), Request::Test)
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "mode"), Some("DirectXip"));
    assert_eq!(field(&info, "slot"), Some("Secondary"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    expect_running(&dut, m, "2.0.0");
}

/// Without revert, a valid image needs no pending magic at all.
#[test]
fn unmarked_newer_image_is_selected_without_revert() {
    let v = Variant::DirectXip;
    let flash = factory(v);
    behave(&flash, &[("2.0.0", Behaviour::Idle)]);
    flash.install(Slot::Secondary, &signed("2.0.0"));
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "slot"), Some("Secondary"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    expect_running(&dut, m, "2.0.0");
}
