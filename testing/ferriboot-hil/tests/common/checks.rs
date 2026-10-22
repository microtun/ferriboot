//! Representative scenarios used by the physical smoke suite and QEMU regressions.

use super::*;

pub fn confirmed_test_upgrade_persists_across_reset() {
    let flash = factory(Variant::SwapScratch);
    flash
        .stage(
            Variant::SwapScratch.mode(),
            Slot::Secondary,
            &signed("2.0.0"),
            Request::Test,
        )
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "mode"), Some("SwapScratch"));
    assert_eq!(field(&info, "slot"), Some("Primary"));
    assert_eq!(field(&info, "max"), Some(max_image_size(&flash).as_str()));
    dut.expect_from(i, "@fbt confirmed", BOOT).unwrap();
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();

    let m = dut.mark();
    dut.reset().unwrap();
    let i = expect_running(&dut, m, "2.0.0");
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("false"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

pub fn hardware_watchdog_resets_a_hung_test_image() {
    let flash = factory(Variant::SwapScratch);
    let mut hooks = HookConfig::default();
    hooks.set("2.0.0", Behaviour::HangNoFeed, 3);
    flash.configure(&hooks);
    flash
        .stage(
            Variant::SwapScratch.mode(),
            Slot::Secondary,
            &signed("2.0.0"),
            Request::Test,
        )
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (i, _) = dut.expect_from(i, "@fbt hang", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    assert_watchdog_reset(&dut, i);
}

pub fn without_a_hook_record_the_firmware_runs_normally() {
    let flash = factory(Variant::SwapScratch);
    flash.clear_hooks();
    let dut = Dut::boot(&flash).unwrap();
    let (i, _) = dut.expect_from(0, STARTING_IMAGE, BOOT).unwrap();
    for marker in NORMAL_FIRMWARE {
        dut.expect_from(i, marker, BOOT).unwrap();
    }
    dut.expect_quiet_from(i, QUIET, "test-hook output", |l| l.contains("@fbt"))
        .unwrap();
}

pub fn confirmed_trial_runs_in_place_from_secondary() {
    let flash = factory(Variant::DirectXipRevert);
    flash
        .stage(
            Variant::DirectXipRevert.mode(),
            Slot::Secondary,
            &signed("2.0.0"),
            Request::Test,
        )
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    dut.expect_from(0, &secondary_start(&flash), BOOT).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "slot"), Some("Secondary"));
    dut.expect_from(i, "@fbt confirmed", BOOT).unwrap();

    let m = dut.mark();
    dut.reset().unwrap();
    let i = expect_running(&dut, m, "2.0.0");
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("false"));
}

pub fn watchdog_resets_trial_and_erases_secondary_without_host_reset() {
    let flash = factory(Variant::DirectXipRevert);
    let mut hooks = HookConfig::default();
    hooks.set("2.0.0", Behaviour::HangNoFeed, 3);
    flash.configure(&hooks);
    flash
        .stage(
            Variant::DirectXipRevert.mode(),
            Slot::Secondary,
            &signed("2.0.0"),
            Request::Test,
        )
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (i, _) = dut.expect_from(i, "@fbt hang", BOOT).unwrap();
    // A watchdog reset can re-enumerate the USB console, losing the early
    // bootloader erase report. Prove autonomous recovery from the application
    // report before issuing any host reset.
    let i = expect_running(&dut, i + 1, "1.0.0");
    assert_watchdog_reset(&dut, i);
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "slot"), Some("Primary"));
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("false"));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();

    // A host reset keeps console capture active. An unerased trial would still
    // be discovered as a candidate (and erased again) on this observed boot.
    let m = dut.mark();
    dut.reset().unwrap();
    let (i, _) = dut.expect_from(m, "candidate: Primary slot", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    dut.expect_quiet_from(m, QUIET, "retained secondary trial", |line| {
        line.contains("candidate: Secondary slot") || line.contains("erasing Secondary slot")
    })
    .unwrap();
}

pub fn confirmed_upgrade_runs_from_primary_and_persists() {
    let flash = factory(Variant::Overwrite);
    flash
        .stage(
            Variant::Overwrite.mode(),
            Slot::Secondary,
            &signed("2.0.0"),
            Request::Permanent,
        )
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let i = expect_running(&dut, 0, "2.0.0");
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "mode"), Some("Overwrite"));
    assert_eq!(field(&info, "slot"), Some("Primary"));
    dut.expect_from(i, "@fbt confirmed", BOOT).unwrap();
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    let m = dut.mark();
    dut.reset().unwrap();
    let i = expect_running(&dut, m, "2.0.0");
    let (_, image) = dut.expect_from(i, "@fbt image", BOOT).unwrap();
    assert_eq!(field(&image, "pending"), Some("false"));
}
