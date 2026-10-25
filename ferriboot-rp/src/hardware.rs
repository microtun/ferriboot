//! Arm Secure RP2350 hardware bindings. Applications reserve the top 256
//! bytes of SRAM and enable interrupts after their own initialization.
pub use crate::boot_info::BootInfo;
use crate::{
    ConfirmedBoot, WordFlash,
    boot_info::{BootData, WORDS, wire_words},
};

// Referencing this symbol makes a missing SRAM reservation a link error.
// It is `mut` because the bootloader rewrites this RAM: mutating an immutable
// static, extern or not, is undefined behaviour even through volatile stores.
// Its type spans the whole reservation, so word pointers stay in bounds.
unsafe extern "C" {
    static mut _ferriboot_boot_info_start: [u32; WORDS];
}

/// The first word of the boot-info reservation.
fn reservation() -> *mut u32 {
    (&raw mut _ferriboot_boot_info_start).cast()
}

/// The flash driver for a board with `SIZE` bytes of flash; use the board's
/// [`Layout::flash_size`](crate::Layout::flash_size).
pub type FlashStorage<'d, const SIZE: usize> = WordFlash<
    embassy_rp::flash::Flash<'d, embassy_rp::peripherals::FLASH, embassy_rp::flash::Blocking, SIZE>,
>;

pub fn flash<const SIZE: usize>(
    peripheral: embassy_rp::Peri<'_, embassy_rp::peripherals::FLASH>,
) -> FlashStorage<'_, SIZE> {
    WordFlash::new(embassy_rp::flash::Flash::new_blocking(peripheral))
}

impl BootInfo {
    pub fn read() -> Option<Self> {
        let area = reservation();
        let mut words = [0; WORDS];
        for (i, word) in words.iter_mut().enumerate() {
            // SAFETY: linker scripts exclude this aligned region from normal
            // RAM, and `i` stays within its `WORDS` words.
            let ptr = unsafe { area.add(i) };
            // SAFETY: the reserved word is readable by the bootloader.
            *word = unsafe { ptr.read_volatile() };
        }
        if let Ok(info) = Self::from_words(&words) {
            return Some(info);
        }
        // Firmware-first migration: an older bootloader only reserved/wrote
        // the final 80 bytes of SRAM. This range is within our reservation.
        const LEGACY_BYTES: usize = 80;
        const LEGACY_START: usize = (WORDS * 4 - LEGACY_BYTES) / 4;
        let mut legacy = [0u8; LEGACY_BYTES];
        for (chunk, word) in legacy
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&words[LEGACY_START..])
        {
            *chunk = word.to_le_bytes();
        }
        Self::from_bytes(&legacy).ok()
    }

    pub fn invalidate() {
        // SAFETY: the reserved header belongs to the boot handoff and no Rust
        // reference to the reservation exists.
        unsafe { reservation().write_volatile(0) };
    }

    pub fn publish(data: &BootData) {
        Self::invalidate();
        let area = reservation();
        let words = wire_words(data);
        // Commit the header last, after all entries have been written.
        for (i, word) in words.iter().enumerate().skip(1) {
            // SAFETY: the reservation holds WORDS entries in both linkers.
            let ptr = unsafe { area.add(i) };
            // SAFETY: the reserved word belongs to the boot handoff.
            unsafe { ptr.write_volatile(*word) };
        }
        cortex_m::asm::dmb();
        // SAFETY: commit the aligned header after all body stores are ordered.
        unsafe { area.write_volatile(words[0]) };
        cortex_m::asm::dsb();
    }
}

/// Publish a confirmed boot plan and enter its reset handler.
///
/// Only a [`ConfirmedBoot`] is accepted: the plan must have passed
/// [`PreparedBoot::confirm`](crate::PreparedBoot::confirm), the fault-hardened
/// second authentication. There is deliberately no other way into the
/// application.
///
/// `cp` must come from `cortex_m::Peripherals::take()`, called before
/// `embassy_rp::init` (whose stack-guard setup steals them as well).
///
/// # Safety
/// The authenticated flash must not have changed since `prepare`. Call from
/// privileged Secure core 0 thread mode, without live DMA or core 1 tasks.
/// The application must use this port's linker and interrupt contract.
/// The caller must run from SRAM or the identity-mapped ATRANS0 window;
/// it must not execute from the ATRANS1 application window.
pub unsafe fn execute(confirmed: &ConfirmedBoot, cp: cortex_m::Peripherals) -> ! {
    let plan = confirmed.plan();
    cortex_m::interrupt::disable();
    BootInfo::publish(plan.shared_data());
    // SAFETY: the caller runs from SRAM/ATRANS0 with interrupts masked and
    // no core 1 or DMA activity, as `map_application` requires.
    unsafe { map_application(plan.xip_translation()) };
    // SAFETY: preparation checked the vectors; the caller preserves flash and
    // guarantees exclusive ownership of core/peripheral state through handoff.
    unsafe {
        jump(
            cp,
            plan.vector_address(),
            plan.stack_pointer(),
            plan.reset_handler(),
        )
    }
}

/// Caller runs from SRAM or the identity-mapped ATRANS0 window, with
/// interrupts masked and no core 1 or DMA accesses. The value is checked
/// before handoff; no code may run from ATRANS1 while it is being changed.
unsafe fn map_application(translation: u32) {
    cortex_m::asm::dsb();
    // RP2350 datasheet 12.14.6: QMI ATRANS1 (offset 0x38). A safe register
    // write; the caller's contract is what makes changing it sound.
    embassy_rp::pac::QMI
        .atrans(1)
        .write_value(embassy_rp::pac::qmi::regs::Atrans(translation));
    cortex_m::asm::dsb();
    // XIP cache tags are virtual: discard entries from the previous mapping.
    // The board bootloader runs in SRAM; ATRANS0 keeps physical storage unchanged.
    // SAFETY: ROM routine; nothing is executing from or reading XIP flash
    // concurrently (caller contract), and it touches no Rust-visible memory.
    unsafe { embassy_rp::rom_data::flash_flush_cache() };
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// # Safety
/// Privileged Secure thread mode on core 0 with exclusive ownership of the
/// core peripherals. `vector`, `sp` and `reset` were validated against the
/// authenticated image: a vector table, an aligned stack top, and a Thumb
/// reset handler inside it.
unsafe fn jump(mut cp: cortex_m::Peripherals, vector: u32, sp: u32, reset: u32) -> ! {
    cortex_m::interrupt::disable();
    cp.SYST.disable_counter();
    cp.SYST.disable_interrupt();
    cp.SYST.clear_current();
    // RP2350 has 53 external interrupt vector entries (two NVIC banks):
    // disable and unpend all of them.
    for bank in 0..2 {
        // SAFETY: ICER/ICPR are write-one-to-clear; all ones only disables
        // and unpends interrupts, which cannot break memory safety.
        unsafe { cp.NVIC.icer[bank].write(u32::MAX) };
        // SAFETY: as above.
        unsafe { cp.NVIC.icpr[bank].write(u32::MAX) };
    }
    cortex_m::peripheral::SCB::clear_pendsv();
    cortex_m::peripheral::SCB::clear_pendst();
    // Clear bootloader stack guards and priority masks before changing MSP.
    // PRIMASK remains set until the application has initialized its HAL.
    // SAFETY: removes the stack-limit guard; no further Rust stack frames
    // are created before MSP is replaced below.
    unsafe { cortex_m::register::msplim::write(0) };
    // SAFETY: as above, for the (unused) process stack.
    unsafe { cortex_m::register::psplim::write(0) };
    // SAFETY: PRIMASK stays set, so lowering BASEPRI enables nothing yet.
    unsafe { cortex_m::register::basepri::write(0) };
    // cortex-m 0.7 can only read FAULTMASK, so clear it directly.
    // SAFETY: PRIMASK stays set; clearing FAULTMASK enables nothing yet.
    unsafe {
        core::arch::asm!("msr FAULTMASK, {zero}", zero = in(reg) 0u32, options(nostack, preserves_flags));
    }
    // SAFETY: `vector` is the validated, aligned vector table of the image.
    unsafe { cp.SCB.vtor.write(vector) };
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    // Reset CONTROL (MSP, privileged, no inherited FP context) and jump in a
    // single no-return assembly block, without stack access after replacing
    // MSP. `cortex_m::asm::bootstrap` only clears SPSEL, so it is not used.
    // SAFETY: validated Thumb reset address, aligned stack and vector table.
    unsafe {
        core::arch::asm!(
            "msr CONTROL, {zero}",
            "isb",
            "msr MSP, {sp}",
            "bx {reset}",
            zero = in(reg) 0u32,
            sp = in(reg) sp,
            reset = in(reg) reset,
            options(noreturn, nostack),
        )
    }
}
