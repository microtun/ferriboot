//! Check the hash / signature of a sealed image.

use super::crypto;
use super::error::Result;
use super::memory::MemoryView;
use super::model::{self, Model};
use super::picobin::{self, Block, Item, image_type as it};
use super::seal::block_hash_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    /// Nothing to verify.
    None,
    Passed,
    Failed,
}

impl Verified {
    pub fn describe(self) -> &'static str {
        match self {
            Verified::None => "none",
            Verified::Passed => "verified",
            Verified::Failed => "incorrect",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub model: Model,
    /// The block that was checked: the last block of the block loop (the one `seal` adds).
    pub block: Option<Block>,
    pub hash: Verified,
    pub signature: Verified,
}

impl VerifyReport {
    pub fn image_type(&self) -> Option<String> {
        let f = self.block.as_ref()?.image_type_flags()?;
        Some(match (it::image_type(f), it::cpu(f), it::security(f)) {
            (it::EXE, it::CPU_ARM, it::SECURITY_S) => "ARM Secure".into(),
            (it::EXE, it::CPU_ARM, it::SECURITY_NS) => "ARM Non-Secure".into(),
            (it::EXE, it::CPU_ARM, _) => "ARM".into(),
            (it::EXE, it::CPU_RISCV, _) => "RISC-V".into(),
            (it::EXE, it::CPU_VARMULET, _) => "Varmulet".into(),
            (it::DATA, ..) => "data".into(),
            _ => "invalid".into(),
        })
    }
}

/// Verify a block's hash / signature against the image contents.
pub fn verify_block(view: &MemoryView, block: &Block) -> (Verified, Verified) {
    let Some(Item::HashDef {
        block_words_to_hash,
        ..
    }) = block.get(picobin::ITEM_1BS_HASH_DEF)
    else {
        return (Verified::None, Verified::None);
    };
    let Some(entries) = block.load_map() else {
        return (Verified::Failed, Verified::Failed);
    };
    let mut to_hash = Vec::new();
    for e in entries {
        match e.storage_address {
            0 => to_hash.extend(e.size.to_le_bytes()),
            s => to_hash.extend(view.read(s, e.size as usize)),
        }
    }
    to_hash.extend(block_hash_bytes(block, *block_words_to_hash as usize));
    let digest = crypto::sha256(&to_hash);

    let check = |ok: bool| {
        if ok {
            Verified::Passed
        } else {
            Verified::Failed
        }
    };
    let hash = block
        .hash_value()
        .map_or(Verified::None, |h| check(h == digest));
    let sig = match block.get(picobin::ITEM_SIGNATURE) {
        Some(Item::Signature {
            signature,
            public_key,
            ..
        }) => check(crypto::verify_digest(public_key, signature, &digest)),
        _ => Verified::None,
    };
    (hash, sig)
}

/// Verify a flat BIN image.
pub fn verify(data: &[u8], bin_offset: u32) -> Result<VerifyReport> {
    let view = MemoryView::bin(data, Some(bin_offset))?;
    let model = model::detect_model(&view)?;
    let block = model::block_loop(&view)?.pop();
    let (hash, signature) = block
        .as_ref()
        .map_or((Verified::None, Verified::None), |b| verify_block(&view, b));
    Ok(VerifyReport {
        model,
        block,
        hash,
        signature,
    })
}
