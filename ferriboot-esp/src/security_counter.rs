//! ESP32-C3 application epoch in BLOCK0.SECURE_VERSION (16 repeatable bits).
//! USER_DATA is RS-coded and cannot be incrementally burned. IDF factory-app
//! anti-rollback must remain disabled: Ferriboot owns this field exclusively.
use core::sync::atomic::{AtomicBool, Ordering};

use esp_hal::{
    efuse::{self, SECURE_VERSION, WR_DIS},
    peripherals::EFUSE,
    time::{Duration, Instant},
};
use ferriboot_core::{SecurityCounterError as E, SecurityCounterStore};

// SECURE_VERSION starts at BLOCK0 bit 142: bit 14 of programming word 4.
const VERSION_WORD_BIT: u32 = 14;
const VERSION_WRITE_DISABLE_BIT: u32 = 18;
// SECURE_VERSION is 16 bits wide; each burnt bit is one epoch.
const SECURE_VERSION_BITS: u32 = 16;
// Use the HAL monotonic timer, not a CPU-frequency-dependent spin count.
// This is deliberately generous relative to an eFuse read/program operation;
// a timeout aborts the current operation and the bootloader then resets with
// a delay. Do not reissue commands inside a timed-out eFuse transaction.
const EFUSE_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
enum Operation {
    Read,
    Program,
    Any,
}

/// Set by the first [`EfuseSecurityCounter::new`] of this boot.
///
/// The target has no compare-and-swap, so this is a plain load then store;
/// that is race-free under `new`'s single-core, interrupts-disabled contract.
static CREATED: AtomicBool = AtomicBool::new(false);

/// Bootloader-only eFuse access, before interrupts/app execution.
///
/// esp-hal does not hand out an `EFUSE` peripheral singleton: it reads the
/// eFuse read registers itself (chip revision, MAC address, ADC calibration)
/// and has no programming API. Ferriboot is therefore the only code that
/// drives the programming registers, and at most one value of this type
/// exists per boot (checked at runtime).
pub struct EfuseSecurityCounter {
    _not_send_or_sync: core::marker::PhantomData<*const ()>,
}

impl EfuseSecurityCounter {
    /// # Panics
    /// If called more than once in the same boot.
    ///
    /// # Safety
    /// Not a memory-safety contract: `unsafe` marks the irreversible fuse
    /// burns this value performs. The caller guarantees:
    ///
    /// * `esp_hal::init` has run: the operation timeouts use its monotonic
    ///   timer, and would otherwise never expire;
    /// * interrupts are disabled and nothing else programs eFuses for this
    ///   value's lifetime (reads by esp-hal between operations are harmless);
    /// * SECURE_VERSION is reserved for Ferriboot, and IDF APP_ANTI_ROLLBACK
    ///   is disabled, including in the second stage.
    pub unsafe fn new() -> Self {
        assert!(
            !CREATED.load(Ordering::Relaxed),
            "only one eFuse security counter may exist per boot"
        );
        CREATED.store(true, Ordering::Relaxed);
        Self {
            _not_send_or_sync: core::marker::PhantomData,
        }
    }

    fn clear_program_registers() {
        // Reset all data and RS staging registers to zero. No programming
        // command is issued here; only SECURE_VERSION is subsequently staged.
        let efuse = EFUSE::regs();
        efuse.pgm_data0().reset();
        efuse.pgm_data1().reset();
        efuse.pgm_data2().reset();
        efuse.pgm_data3().reset();
        efuse.pgm_data4().reset();
        efuse.pgm_data5().reset();
        efuse.pgm_data6().reset();
        efuse.pgm_data7().reset();
        efuse.pgm_check_value0().reset();
        efuse.pgm_check_value1().reset();
        efuse.pgm_check_value2().reset();
    }

    fn set_timing() {
        // Match ESP-IDF v5.5.5 components/hal/esp32c3/efuse_hal.c.
        // The C3 eFuse clock is fixed: these are not APB-frequency settings.
        // The ROM timing helper sets PWR_OFF_NUM to 0x40 instead of IDF's 0x190.
        let efuse = EFUSE::regs();
        // The PAC marks these fields `unsafe` only because the SVD carries no
        // value constraints; there is no safe setter for them.
        efuse.dac_conf().modify(|_, w| {
            // SAFETY: documented C3 timing value; exclusive controller ownership.
            let w = unsafe { w.dac_num().bits(0xff) };
            // SAFETY: as above.
            unsafe { w.dac_clk_div().bits(0x28) }
        });
        // SAFETY: documented C3 timing value (IDF efuse_hal.c).
        efuse
            .wr_tim_conf1()
            .modify(|_, w| unsafe { w.pwr_on_num().bits(0x3000) });
        // SAFETY: documented C3 timing value (IDF efuse_hal.c).
        efuse
            .wr_tim_conf2()
            .modify(|_, w| unsafe { w.pwr_off_num().bits(0x190) });
    }

    fn wait_idle(operation: Operation, error: E) -> Result<(), E> {
        let started = Instant::now();
        loop {
            let command = EFUSE::regs().cmd().read();
            let busy = match operation {
                Operation::Read => command.read_cmd().bit_is_set(),
                Operation::Program => command.pgm_cmd().bit_is_set(),
                Operation::Any => command.read_cmd().bit_is_set() || command.pgm_cmd().bit_is_set(),
            };
            if !busy {
                return Ok(());
            }
            if started.elapsed() >= EFUSE_OPERATION_TIMEOUT {
                #[cfg(feature = "log")]
                log::error!(
                    "eFuse: timeout {:?}, read={}, program={}",
                    operation,
                    command.read_cmd().bit_is_set(),
                    command.pgm_cmd().bit_is_set()
                );
                return Err(error);
            }
            core::hint::spin_loop();
        }
    }

    fn refresh() -> Result<(), E> {
        #[cfg(feature = "log")]
        log::trace!("eFuse: read begin");
        // Never change timing or issue a command while another operation is busy.
        Self::wait_idle(Operation::Any, E::Busy)?;
        Self::set_timing();
        let efuse = EFUSE::regs();
        // SAFETY: documented C3 read opcode; no programming command is issued.
        efuse
            .conf()
            .modify(|_, w| unsafe { w.op_code().bits(0x5aa5) });
        efuse.cmd().modify(|_, w| w.read_cmd().set_bit());
        Self::wait_idle(Operation::Read, E::Read)?;
        // C3 hardware workaround, also present in ROM: check READ_CMD again
        // to ensure the eFuse clock is normal before accessing read registers.
        Self::wait_idle(Operation::Read, E::Read)?;
        #[cfg(feature = "log")]
        log::trace!("eFuse: read complete");
        Ok(())
    }
}

impl SecurityCounterStore for EfuseSecurityCounter {
    fn capacity(&self) -> u32 {
        SECURE_VERSION_BITS
    }

    fn get(&mut self) -> Result<u32, E> {
        // Refresh physical fuses; do not trust app-writable shadow registers.
        Self::refresh()?;
        // Reject repeat-coding errors in the counter, rather than interpreting
        // a damaged fuse as a smaller floor. Error register 3 mirrors word 4.
        if EFUSE::regs()
            .rd_repeat_err3()
            .read()
            .secure_version_err()
            .bits()
            != 0
        {
            return Err(E::InvalidState);
        }
        let bits = efuse::read_field_le::<u16>(SECURE_VERSION);
        let value = bits.count_ones();
        #[cfg(feature = "log")]
        log::debug!("eFuse: floor={}", value);
        Ok(value)
    }

    fn advance(&mut self, value: u32) -> Result<(), E> {
        if value > self.capacity() {
            return Err(E::Exhausted);
        }
        let current = self.get()?;
        if current >= value {
            return Ok(());
        }
        if efuse::read_field_le::<u32>(WR_DIS) & (1 << VERSION_WRITE_DISABLE_BIT) != 0 {
            return Err(E::Locked);
        }
        // Count burnt bits (same encoding as IDF SECURE_VERSION). Burn exactly
        // one fresh bit at a time, preserving pre-existing non-prefix values.
        for _ in current..value {
            let old = efuse::read_field_le::<u16>(SECURE_VERSION);
            let bit = (!old).trailing_zeros();
            // `current < value <= 16` implies a clear bit below 16, but check
            // it here: a glitched or inconsistent read must never stage a
            // bit outside SECURE_VERSION (bit 16 would be word-4 bit 30).
            if bit >= SECURE_VERSION_BITS {
                return Err(E::InvalidState);
            }
            // Exclusive boot-time controller access, BLOCK0 only; clear ALL
            // staging registers before and after programming so no unrelated
            // secure-boot, key or write-disable fuse can be burned.
            Self::wait_idle(Operation::Any, E::Busy)?;
            Self::clear_program_registers();
            let efuse = EFUSE::regs();
            // SAFETY: `bit < SECURE_VERSION_BITS` was checked above, so only a
            // SECURE_VERSION bit (word-4 bits 14..=29) is staged.
            efuse
                .pgm_data4()
                .write(|w| unsafe { w.pgm_data_4().bits(1 << (VERSION_WORD_BIT + bit)) });
            Self::set_timing();
            // SAFETY: documented C3 programming opcode, with only BLOCK0 staged.
            efuse
                .conf()
                .modify(|_, w| unsafe { w.op_code().bits(0x5a5a) });
            #[cfg(feature = "log")]
            log::debug!("eFuse: program BLOCK0 bit={} begin", bit);
            // SAFETY: block 0 is the only target; write clears READ_CMD and
            // explicitly selects BLOCK0 before issuing the programming command.
            efuse
                .cmd()
                .write(|w| unsafe { w.blk_num().bits(0).pgm_cmd().set_bit() });
            // On timeout do not clear staging or issue another operation.
            Self::wait_idle(Operation::Program, E::Write)?;
            Self::clear_program_registers();
            #[cfg(feature = "log")]
            log::debug!("eFuse: program complete; verifying");
            if self.get()? != old.count_ones() + 1 {
                return Err(E::NotPersisted);
            }
        }
        Ok(())
    }
}
