//! RP2350 fixture flash algorithm for probe-rs. See the README.
//!
//! probe-rs loads this image into SRAM and calls the four CMSIS entry points
//! below directly, with its own stack. There is no vector table or runtime.
#![no_std]
#![no_main]

mod protocol;
mod rp2350;

use core::cell::RefCell;
use critical_section::Mutex;
use protocol::Algorithm;

/// Status codes returned through the CMSIS ABI; `0` is success.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Error {
    Format = 1,
    RomMissing = 2,
    /// Flash readback mismatch, or fast XIP reads disagreed with slow ones in `Init`.
    Verify = 3,
    Sequence = 4,
    Truncated = 5,
    /// Core1, PSM or QMI direct-mode timeout.
    Hardware = 6,
    RomAbi = 7,
    RcpNotReady = 8,
    ClockTimeout = 9,
    FlashUnsupported = 10,
}

fn status(result: Result<(), Error>) -> u32 {
    result.err().map_or(0, |error| error as u32)
}

static ALGORITHM: Mutex<RefCell<Algorithm>> = Mutex::new(RefCell::new(Algorithm::new()));

/// Run `f` on the single algorithm instance. probe-rs calls one entry point
/// at a time on core0, so the `RefCell` is never contended; a reentrant call
/// would panic (and report status 255) instead of aliasing.
///
/// Interrupts must already be masked: leaving the critical section restores
/// PRIMASK to its state on entry, which would otherwise undo the masking that
/// `Init` performs for the rest of the session.
fn with_algorithm<R>(f: impl FnOnce(&mut Algorithm) -> R) -> R {
    cortex_m::interrupt::disable();
    critical_section::with(|cs| f(&mut ALGORITHM.borrow_ref_mut(cs)))
}

#[unsafe(no_mangle)]
extern "C" fn Init(_address: u32, _clock: u32, _operation: u32) -> u32 {
    status(with_algorithm(Algorithm::init))
}

#[unsafe(no_mangle)]
extern "C" fn UnInit(operation: u32) -> u32 {
    status(with_algorithm(|algorithm| algorithm.uninit(operation)))
}

#[unsafe(no_mangle)]
extern "C" fn EraseSector(address: u32) -> u32 {
    status(with_algorithm(|algorithm| algorithm.erase_sector(address)))
}

/// # Safety
/// probe-rs passes `data` pointing to `size` readable bytes in its RAM
/// buffer (or null), valid for the duration of the call.
#[unsafe(no_mangle)]
unsafe extern "C" fn ProgramPage(address: u32, size: u32, data: *const u8) -> u32 {
    // SAFETY: the CMSIS flash-algorithm ABI contract above.
    let packet =
        (!data.is_null()).then(|| unsafe { core::slice::from_raw_parts(data, size as usize) });
    status(with_algorithm(|algorithm| {
        algorithm.program_page(address, packet)
    }))
}

/// Stop at a breakpoint with status 255 in r0, so probe-rs fails the call
/// immediately. (probe-rs does not catch HardFaults while flashing, so the
/// usual `udf`-based handlers such as `panic-probe` would hang until its
/// timeout.) Spin if a debugger resumes us.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // SAFETY: sets r0 and halts at a breakpoint; no memory access.
    unsafe { core::arch::asm!("movs r0, #255", "bkpt #0", out("r0") _, options(nomem, nostack)) };
    loop {
        cortex_m::asm::nop();
    }
}
