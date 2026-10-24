//! Small physical-board suite: one flash restoration per test, no case matrices.
//! Detailed policy and failure matrices live in the QEMU-only test binaries.
#![cfg(all(feature = "smoke", any(feature = "esp32c3", feature = "rp2350")))]

mod common;

use common::*;

/// A missing application must not wedge a field device or bypass validation.
/// Repeated diagnostics prove autonomous bootloader retries without a host reset.
#[test]
fn missing_application_reboots_without_handoff() {
    let _board_test = common::lock_physical_board();
    let dut = Dut::boot(&blank(Variant::SwapScratch)).unwrap();
    let (first, _) = dut.expect_from(0, NO_EXECUTABLE, BOOT).unwrap();
    dut.expect_from(first + 1, NO_EXECUTABLE, BOOT).unwrap();
    dut.expect_quiet_from(0, QUIET, "application handoff", |line| {
        line.contains("@fbt hello") || line.contains(STARTING_IMAGE)
    })
    .unwrap();
}

#[test]
fn confirmed_test_upgrade_persists_across_reset() {
    let _board_test = common::lock_physical_board();
    common::checks::confirmed_test_upgrade_persists_across_reset();
}

#[test]
fn hardware_watchdog_resets_a_hung_test_image() {
    let _board_test = common::lock_physical_board();
    common::checks::hardware_watchdog_resets_a_hung_test_image();
}

#[test]
fn without_a_hook_record_the_firmware_runs_normally() {
    let _board_test = common::lock_physical_board();
    common::checks::without_a_hook_record_the_firmware_runs_normally();
}

#[test]
fn confirmed_trial_runs_in_place_from_secondary() {
    let _board_test = common::lock_physical_board();
    common::checks::confirmed_trial_runs_in_place_from_secondary();
}

#[test]
fn watchdog_resets_trial_and_erases_secondary_without_host_reset() {
    let _board_test = common::lock_physical_board();
    common::checks::watchdog_resets_trial_and_erases_secondary_without_host_reset();
}

#[test]
fn confirmed_upgrade_runs_from_primary_and_persists() {
    let _board_test = common::lock_physical_board();
    common::checks::confirmed_upgrade_runs_from_primary_and_persists();
}

/// A trusted signature must not allow a payload to overwrite board-reserved RAM.
#[test]
fn platform_rejects_an_executable_overlapping_reserved_memory() {
    let _board_test = common::lock_physical_board();
    let case = smoke_malformed(&payload());
    let image = sign::sign(case.image.bytes(), &SignOptions::new("2.0.0")).unwrap();
    let flash = factory(Variant::SwapScratch);
    flash
        .stage(
            Variant::SwapScratch.mode(),
            Slot::Secondary,
            &image,
            Request::Test,
        )
        .unwrap();
    let dut = Dut::boot(&flash).unwrap();
    let (i, _) = dut
        .expect_from(0, PLATFORM_REJECTED, BOOT)
        .unwrap_or_else(|e| panic!("{}: {e}", case.name));
    let (i, _) = dut.expect_from(i, "erasing Secondary slot", BOOT).unwrap();
    let i = expect_running(&dut, i, "1.0.0");
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}
