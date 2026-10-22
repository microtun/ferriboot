#![allow(dead_code, unused_imports)]
//! Fixtures shared by hardware smoke checks and detailed QEMU scenarios.
//! Exactly one board feature supplies payloads, console strings and layout.

#[cfg(any(
    all(feature = "esp32c3", feature = "rp2350"),
    all(feature = "esp32c3", feature = "qemu"),
    all(feature = "rp2350", feature = "qemu"),
))]
compile_error!(
    "the scenario tests run against one board: enable exactly one of `esp32c3`, `rp2350`, or `qemu`"
);

#[cfg(feature = "esp32c3")]
mod esp32c3;
#[cfg(feature = "esp32c3")]
pub use esp32c3::*;

#[cfg(feature = "rp2350")]
mod rp2350;
#[cfg(feature = "rp2350")]
pub use rp2350::*;

#[cfg(feature = "qemu")]
mod qemu;
#[cfg(feature = "qemu")]
pub use qemu::*;

// libtest runs tests in parallel by default. A physical fixture can only
// execute one scenario at a time, while QEMU remains fully parallel.
// This lock covers the entire scenario (including fixture preparation), rather
// than just the interval when a DUT connection is open.
#[cfg(any(feature = "esp32c3", feature = "rp2350"))]
pub fn lock_physical_board() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A failed test may poison the mutex, but subsequent tests can still
    // restore the board from their own freshly constructed flash fixture.
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// Keep representative checks identical between physical boards and QEMU.
pub mod checks;
