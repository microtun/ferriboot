//! Opt-in anti-rollback check. Physical boards consume one OTP/eFuse epoch.
#![cfg(all(
    feature = "security-counter",
    any(
        feature = "esp32c3-supermini",
        feature = "rp2350-pico2",
        feature = "qemu"
    )
))]

mod common;

use common::*;

#[cfg(feature = "esp32c3-supermini")]
const CAPACITY: u32 = 16;
#[cfg(feature = "rp2350-pico2")]
const CAPACITY: u32 = 504;
#[cfg(feature = "qemu")]
const CAPACITY: u32 = ferriboot_hil::qemu::layout::SECURITY_COUNTER_CAPACITY;

/// Compute a single irreversible step without wrapping or touching hardware.
fn next_epoch(initial: u32, capacity: u32) -> ferriboot_hil::Result<u32> {
    initial
        .checked_add(1)
        .filter(|next| *next <= capacity)
        .ok_or_else(|| {
            format!(
                "security-counter fuses exhausted: DUT floor {initial} has reached capacity {capacity}; no unused fuse values remain"
            )
            .into()
        })
}

#[test]
fn exhausted_fuses_are_rejected_before_any_increment() {
    assert_eq!(next_epoch(0, CAPACITY).unwrap(), 1);
    assert_eq!(next_epoch(CAPACITY - 1, CAPACITY).unwrap(), CAPACITY);
    assert!(next_epoch(CAPACITY, CAPACITY).is_err());
    assert!(next_epoch(u32::MAX, CAPACITY).is_err());
}

fn image(version: &str, counter: u32) -> Vec<u8> {
    sign::sign(
        payload().bytes(),
        &SignOptions::new(version).security_counter(counter),
    )
    .unwrap()
}

fn expect_idle(dut: &Dut, from: usize, version: &str) {
    let i = expect_running(dut, from, version);
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
}

/// Reads the existing hardware floor before any upgrade and consumes one
/// fresh OTP/eFuse bit. Abort before signing or upgrading if fuses are full.
#[test]
fn confirmed_epoch_survives_flash_rewrite_and_rejects_rollback() {
    #[cfg(any(feature = "esp32c3-supermini", feature = "rp2350-pico2"))]
    let _board_test = common::lock_physical_board();
    // One explicit virtual board retains its counter between QEMU processes.
    // Ordinary Dut::boot calls deliberately create isolated virtual boards.
    #[cfg(feature = "qemu")]
    let board = ferriboot_hil::qemu::VirtualBoard::new(0).unwrap();
    #[cfg(feature = "qemu")]
    let initial = board.security_counter().unwrap();
    #[cfg(not(feature = "qemu"))]
    let initial = ferriboot_hil::sign::initial_security_counter().unwrap();
    // Fail before signing an upgrade or attempting to burn another fuse.
    let next = next_epoch(initial, CAPACITY).unwrap();
    eprintln!("[security-counter] floor will advance from {initial} to {next}");
    #[cfg(feature = "qemu")]
    let boot = |flash: &FlashImage| board.boot(flash);
    #[cfg(not(feature = "qemu"))]
    let boot = Dut::boot;

    // Build and sign every upgrade fixture before the first increment.
    // The direct fuse read did not modify flash or bootloader state.
    // All fixtures use the same bootloader; only authenticated epochs,
    // versions and trailers differ.
    let original = image("1.0.0", initial);
    let upgrade = image("2.0.0", next);
    // A larger semantic version must not override a lower security epoch.
    let rollback = image("9.0.0", initial);
    let trial = blank(Variant::SwapScratch);
    trial
        .install(Slot::Primary, &original)
        .confirm_raw(Slot::Primary);
    trial
        .stage(
            Variant::SwapScratch.mode(),
            Slot::Secondary,
            &upgrade,
            Request::Test,
        )
        .unwrap();
    behave(&trial, &[("2.0.0", Behaviour::Idle)]);

    let confirmed = blank(Variant::SwapScratch);
    confirmed
        .install(Slot::Primary, &original)
        .confirm_raw(Slot::Primary);
    confirmed
        .stage(
            Variant::SwapScratch.mode(),
            Slot::Secondary,
            &upgrade,
            Request::Test,
        )
        .unwrap();

    let stale = blank(Variant::SwapScratch);
    stale
        .install(Slot::Primary, &rollback)
        .confirm_raw(Slot::Primary);
    let recovery = blank(Variant::SwapScratch);
    recovery
        .install(Slot::Primary, &upgrade)
        .confirm_raw(Slot::Primary);

    // An unconfirmed higher-epoch trial must still revert to the old epoch.
    // Returning to 1.0.0 also proves the initial floor was not already higher.
    {
        let dut = boot(&trial).unwrap();
        expect_idle(&dut, 0, "2.0.0");
        let m = dut.mark();
        dut.reset().unwrap();
        expect_idle(&dut, m, "1.0.0");
    }

    // Confirmation is persisted by the app; the following boot commits OTP.
    {
        let dut = boot(&confirmed).unwrap();
        expect_idle(&dut, 0, "2.0.0");
        dut.expect_from(0, "@fbt confirmed", BOOT).unwrap();
        let m = dut.mark();
        dut.reset().unwrap();
        expect_idle(&dut, m, "2.0.0");
    }

    // Rewrite all flash, erasing the newer image and its confirmation. OTP
    // must still refuse a correctly signed, pre-confirmed older epoch.
    let rejected = (|| -> ferriboot_hil::Result<()> {
        let dut = boot(&stale)?;
        dut.expect_from(0, "SecurityCounterTooLow", BOOT)?;
        dut.expect_from(0, NO_EXECUTABLE, BOOT)?;
        dut.expect_quiet_from(0, QUIET, "rollback handoff", |line| {
            line.contains("@fbt hello") || line.contains(STARTING_IMAGE)
        })
    })();

    // Restore a usable fixture even if a rollback assertion failed. Seeing it
    // boot also bounds the hardware floor to exactly the one epoch consumed.
    let dut = boot(&recovery).unwrap();
    expect_idle(&dut, 0, "2.0.0");
    eprintln!("[security-counter] DUT floor is now {next}; future runs read it automatically");
    rejected.unwrap();
}
