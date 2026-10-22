//! RP2350 OTP page 32: 504 triplicated raw-bit epochs in rows 0x800..0x83e.
//! Each set of three 24-bit rows stores a bitwise-majority-voted thermometer.
//! Row 0x83f is unused: this layout has no format marker or auto-detection.
//! Raw programming deliberately disables ECC: bits are updated over time.
use crate::otp::{self, Operation, OtpBuffer, OtpError};
use ferriboot_core::{
    SecurityCounterError as E, SecurityCounterStore,
    security_counter::majority_thermometer_rows_value,
};

pub const FIRST_ROW: u32 = 0x800;
pub const ROWS: usize = 64;
pub const COPIES: usize = 3;
pub const DATA_ROWS: usize = 63;
pub const CAPACITY: u32 = DATA_ROWS as u32 / COPIES as u32 * 24;

fn access(row: u32, words: &mut [u32], write: bool) -> Result<(), E> {
    let operation = if write {
        Operation::Write
    } else {
        Operation::Read
    };
    otp::access(row, operation, OtpBuffer::Raw(words)).map_err(|error| match error {
        OtpError::LockBusy => E::Busy,
        OtpError::Read(_) => E::Read,
        OtpError::Write(_) => E::Write,
    })
}

fn read_page() -> Result<[u32; ROWS], E> {
    let mut rows = [0u32; ROWS];
    access(FIRST_ROW, &mut rows, false)?;
    Ok(rows)
}

fn decode(rows: &[u32; ROWS]) -> Result<u32, E> {
    // A 64th row cannot form a triple and must remain untouched.
    if rows[DATA_ROWS] != 0 {
        return Err(E::InvalidState);
    }
    majority_thermometer_rows_value(&rows[..DATA_ROWS], 24)
}

// Program a target prefix from the *physical* rows, not the voted value.
// This also repairs replicas missing after an interrupted previous write.
fn program_prefix(rows: &mut [u32; ROWS], target: u32) -> Result<(), E> {
    for n in 0..target {
        let first = (n as usize / 24) * COPIES;
        let bit = 1u32 << (n % 24);
        for (index, item) in rows.iter_mut().enumerate().skip(first).take(COPIES) {
            if *item & bit != 0 {
                continue;
            }
            let mut programmed = *item | bit;
            access(
                FIRST_ROW + index as u32,
                core::slice::from_mut(&mut programmed),
                true,
            )?;
            let mut observed = 0u32;
            access(
                FIRST_ROW + index as u32,
                core::slice::from_mut(&mut observed),
                false,
            )?;
            if observed & programmed != programmed {
                return Err(E::NotPersisted);
            }
            *item = observed;
        }
    }
    Ok(())
}

pub struct OtpSecurityCounter {
    _private: (),
}

impl OtpSecurityCounter {
    /// # Safety
    /// Not a memory-safety contract: `unsafe` marks the irreversible OTP burns
    /// this value performs. Call only on Secure Arm core 0 with interrupts
    /// disabled, core 1 stopped, and all of OTP page 32 exclusively allocated
    /// to Ferriboot (raw, not ECC), using the triple-redundant counter layout.
    pub unsafe fn new() -> Self {
        Self { _private: () }
    }
}

impl SecurityCounterStore for OtpSecurityCounter {
    fn capacity(&self) -> u32 {
        CAPACITY
    }

    fn get(&mut self) -> Result<u32, E> {
        decode(&read_page()?)
    }

    fn advance(&mut self, value: u32) -> Result<(), E> {
        if value > CAPACITY {
            return Err(E::Exhausted);
        }
        let mut rows = read_page()?;
        let current = decode(&rows)?;
        let target = current.max(value);
        // Burn each bit in all three rows before moving to the next logical
        // bit. A cut after one copy preserves the old voted epoch; after two,
        // the new epoch is active. No format marker is written.
        program_prefix(&mut rows, target)?;

        // A fresh full-page read catches inconsistent majority results, and
        // prevents handoff unless the requested floor and all copies persist.
        if decode(&read_page()?)? != target {
            return Err(E::NotPersisted);
        }
        Ok(())
    }
}
