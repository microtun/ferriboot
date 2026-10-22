//! MCUboot signing with upstream `imgtool` for the RP2350 slots (see
//! [`crate::sign`]). Both physical slots execute through the same ATRANS1
//! runtime alias, so images default to `--rom-fixed APP_XIP_BASE`; tests can
//! pass their own `--rom-fixed` for deliberately misplaced images.

pub use crate::sign::{
    Key, SignOptions, corrupt_hash_tlv, corrupt_signature, flip_payload_bit, unprotected_tlvs,
};

use super::layout;

pub const HEADER_SIZE: u32 = layout::HEADER_SIZE;
pub const SLOT_SIZE: u32 = layout::SLOT_SIZE;
pub const SECTOR: u32 = layout::ERASE_SIZE;

/// `imgtool sign` an Arm payload for the 4 MiB layout's slots.
pub fn sign(payload: &[u8], opts: &SignOptions) -> crate::Result<Vec<u8>> {
    crate::sign::sign(&super::BOARD.signing, payload, opts)
}
