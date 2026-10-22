//! MCUboot signing with upstream `imgtool` for the ESP32-C3 slots (see
//! [`crate::sign`]). Arguments follow `tools/mcuboot.py`, which the
//! board's `firmware.py sign` uses. The geometry comes from `partitions.csv`.

pub use crate::sign::{
    Key, SignOptions, corrupt_hash_tlv, corrupt_signature, flip_payload_bit, unprotected_tlvs,
};

use super::layout;

pub const HEADER_SIZE: u32 = layout::HEADER_SIZE;
pub const SECTOR: u32 = layout::ERASE_SIZE;

/// `imgtool sign` an ESP image for the 4 MiB layout's slots.
pub fn sign(payload: &[u8], opts: &SignOptions) -> crate::Result<Vec<u8>> {
    crate::sign::sign(&super::BOARD.signing, payload, opts)
}
