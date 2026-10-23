//! `reseal`: replace the placeholder signature written by `seal --external-sign` with an
//! externally produced one (HSM, cloud KMS, `openssl pkeyutl`, ...).

use super::crypto::{self, RawPublicKey, RawSignature};
use super::error::{Result, bail};
use super::memory::MemoryView;
use super::model::{self, Model};
use super::picobin::{self, Block, Item};
use k256::PublicKey;

#[derive(Debug, Clone)]
pub struct ResealOutput {
    pub model: Model,
    /// The rewritten last block of the block loop.
    pub block: Block,
    pub public_key: RawPublicKey,
    pub signature: RawSignature,
}

/// Insert a DER signature of the image hash (as printed by `seal --external-sign`) and its
/// public key into the image, in place. The signature is verified first.
pub fn reseal(
    image: &mut [u8],
    bin_offset: u32,
    signature_der: &[u8],
    public_key: &PublicKey,
) -> Result<ResealOutput> {
    let pk = crypto::raw_public_key(public_key);
    let Some(sig) = crypto::der_to_raw(signature_der) else {
        bail!(Args, "Signature was not in DER format");
    };
    let view = MemoryView::bin(image, Some(bin_offset))?;
    let model = model::detect_model(&view)?;
    let Some(mut block) = model::block_loop(&view)?.pop() else {
        bail!(NotPossible, "Last block not found");
    };
    let Some(Item::Signature {
        signature,
        public_key,
        ..
    }) = block.get_mut(picobin::ITEM_SIGNATURE)
    else {
        bail!(NotPossible, "Last block does not contain a signature item");
    };
    *public_key = pk.to_vec();
    *signature = sig.to_vec();
    let Some(Ok(digest)) = block.hash_value().map(<[u8; 32]>::try_from) else {
        bail!(
            NotPossible,
            "Last block does not contain a SHA-256 hash value item"
        );
    };
    if !crypto::verify_digest(&pk, &sig, &digest) {
        bail!(VerificationFailed, "Signature verification failed");
    }
    let bytes = block.to_bytes();
    let offset = view.file_offset(block.physical_addr, bytes.len())?;
    image[offset..offset + bytes.len()].copy_from_slice(&bytes);
    Ok(ResealOutput {
        model,
        block,
        public_key: pk,
        signature: sig,
    })
}
