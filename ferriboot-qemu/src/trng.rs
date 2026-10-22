//! Simulated AN385 TRNG MMIO for the QEMU test platform only.
//!
//! This reads QEMU guest-visible random values (including the deterministic
//! stream when `-seed` or record/replay is used), **not** true physical entropy.
//! The device implements `rand::CryptoRng` for API compatibility with the
//! hardware TRNGs; this marker does not turn seeded QEMU output into physical
//! entropy or make it safe for production cryptographic key generation.
use crate::regs::trng::{DATA, DEVICE_ID, STATUS};
use rand::{CryptoRng, RngCore};

const MAGIC: u32 = 0x474e_5254; // "TRNG", in little-endian memory order.
const READY: u32 = 1;

/// Handle to the test-only, always-ready random-data register.
pub struct Trng;

impl Trng {
    /// Fail closed if the expected QEMU board patch is absent or mismatched.
    pub fn try_new() -> Option<Self> {
        let id = DEVICE_ID.read();
        let status = STATUS.read();
        (id == MAGIC && status & READY != 0).then_some(Self)
    }

    /// Read one fresh guest-visible random sample; does not cache or seed a PRNG.
    pub fn next_u32(&mut self) -> u32 {
        // try_new checked the peripheral before the first read.
        DATA.read()
    }
}

// QEMU's simulated device is deliberately accepted as a CryptoRng for testing.
// Its determinism under `-seed`/record-replay is not a security guarantee.
impl RngCore for Trng {
    fn next_u32(&mut self) -> u32 {
        Trng::next_u32(self)
    }

    fn next_u64(&mut self) -> u64 {
        u64::from(self.next_u32()) | (u64::from(self.next_u32()) << 32)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let bytes = self.next_u32().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

impl CryptoRng for Trng {}
