//! Test-only monotonic eFuse counter independent of the SPI flash image.
//! QEMU's Xilinx eFUSE model persists the 1536 low-first OTP bits in a
//! separate host block backend. The MMIO words are little-endian bitmaps.
use super::efuse;
use ferriboot_core::{
    SecurityCounterError as E, SecurityCounterStore, security_counter::thermometer_value,
};
// A sibling module, like `efuse`, so the host tests can supply both.
use super::chip::SECURITY_COUNTER_CAPACITY;

/// Bootloader-only counter access. The harness prevents multiple guests from
/// sharing the same backing image simultaneously.
#[derive(Default)]
pub struct EfuseSecurityCounter;

impl EfuseSecurityCounter {
    pub fn new() -> Self {
        Self
    }
}

impl SecurityCounterStore for EfuseSecurityCounter {
    fn capacity(&self) -> u32 {
        SECURITY_COUNTER_CAPACITY
    }

    fn get(&mut self) -> Result<u32, E> {
        // Fail closed if a QEMU without the separate eFuse patch was used.
        if !efuse::present() {
            return Err(E::Read);
        }
        let mut value = 0;
        let mut ended = false;
        for word in 0..(SECURITY_COUNTER_CAPACITY as usize / 32) {
            let count = thermometer_value(efuse::read_word(word), 32)?;
            if ended && count != 0 {
                return Err(E::InvalidState);
            }
            ended |= count < 32;
            value += count;
        }
        Ok(value)
    }

    fn advance(&mut self, value: u32) -> Result<(), E> {
        if value > self.capacity() {
            return Err(E::Exhausted);
        }
        let mut current = self.get()?;
        while current < value {
            // Write a single new bit. Model persists that 32-bit fuse row;
            // read back before burning the next bit or handing off.
            efuse::program_bit(current);
            if self.get()? != current + 1 {
                return Err(E::NotPersisted);
            }
            current += 1;
        }
        Ok(())
    }
}
