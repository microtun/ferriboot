//! `seal`: add the final metadata block (load map, hash, signature, version, ...) to an image.
//! Port of picotool's `seal_command` / `sign_guts_*` / `place_new_block` / `hash_andor_sign`.

use super::crypto::{self, RawPublicKey};
use super::error::{Error, Result, bail, err};
use super::memory::MemoryView;
use super::model::{self, Chip, FLASH_START, Model};
use super::picobin::{self, Block, BlockStore, Item, LoadMapEntry, image_type as it};
use k256::SecretKey;

/// Default OTP rows for rollback versions (OTP_DATA_DEFAULT_BOOT_VERSION0/1_ROW).
pub const DEFAULT_ROLLBACK_ROWS: [u16; 2] = [0x4e, 0x51];

/// Options for [`seal`]; the fields correspond to picotool's command line flags.
#[derive(Debug, Clone, Default)]
pub struct SealOptions {
    /// `--hash`: add a SHA-256 HASH_VALUE item.
    pub hash: bool,
    /// `--sign`: add a secp256k1 SIGNATURE item.
    pub sign: bool,
    /// `--clear`: clear all of main SRAM on load.
    pub clear_sram: bool,
    /// `--pin-xip-sram`: pin XIP SRAM on load.
    pub pin_xip_sram: bool,
    /// `--external-sign`: hash and add an all-zero SIGNATURE placeholder for `reseal`.
    /// Implies `hash` and `sign`; no private key is needed.
    pub external_sign: bool,
    pub major: u16,
    pub minor: u16,
    pub rollback: u16,
    /// Rollback OTP rows (defaults to [`DEFAULT_ROLLBACK_ROWS`] when `rollback` is set).
    pub rollback_rows: Vec<u16>,
    /// Load address of a BIN file (default 0x10000000).
    pub bin_offset: Option<u32>,
}

/// Result of sealing an image.
#[derive(Debug, Clone)]
pub struct SealOutput {
    /// The sealed image (same format as the input).
    pub data: Vec<u8>,
    pub model: Model,
    /// The newly added block.
    pub block: Block,
    /// The SHA-256 that was signed / is to be signed externally.
    pub hash: Option<[u8; 32]>,
    /// The public key in the signature (all-zero placeholder for external signing).
    pub public_key: Option<RawPublicKey>,
}

/// The image being sealed: a flat BIN.
trait Storage: BlockStore {
    fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<()>;
    fn byte(&self, addr: u32) -> Option<u8>;
    /// (physical address, size) of each region of the image.
    fn regions(&self) -> Vec<(u32, u32)>;
}

struct Bin {
    start: u32,
    data: Vec<u8>,
}

impl Bin {
    fn range(&self, addr: u32, len: usize) -> Result<std::ops::Range<usize>> {
        let off = addr.wrapping_sub(self.start) as usize;
        if addr < self.start || off + len > self.data.len() {
            bail!(
                NotPossible,
                "The BIN file does not contain the address {:x}",
                addr
            );
        }
        Ok(off..off + len)
    }
}

impl BlockStore for Bin {
    fn words_from(&self, addr: u32) -> Result<Vec<u32>> {
        let r = self.range(addr, 0)?;
        Ok(picobin::lsb_bytes_to_words(&self.data[r.start..]))
    }
}

impl Storage for Bin {
    fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<()> {
        let r = self.range(addr, bytes.len())?;
        self.data[r].copy_from_slice(bytes);
        Ok(())
    }
    fn byte(&self, addr: u32) -> Option<u8> {
        self.range(addr, 1).ok().map(|r| self.data[r.start])
    }
    fn regions(&self) -> Vec<(u32, u32)> {
        vec![(self.start, self.data.len() as u32)]
    }
}

/// Mark a block as IGNORED (unless it's a partition table) - RP2350-E13 workaround.
fn set_block_ignored(s: &mut impl Storage, block_addr: u32) -> Result<()> {
    match s.byte(block_addr + 4) {
        Some(b) if b & 0x7f == picobin::ITEM_PARTITION_TABLE => Ok(()),
        Some(_) => s.write(block_addr + 4, &[0x7e]),
        None => bail!(
            NotPossible,
            "The image does not contain the block address {:x}",
            block_addr
        ),
    }
}

fn set_next_block(s: &mut impl Storage, blk: &mut Block, target: u32) -> Result<()> {
    blk.next_block_rel = target.wrapping_sub(blk.physical_addr);
    s.write(
        blk.physical_addr + blk.next_block_rel_index * 4,
        &blk.next_block_rel.to_le_bytes(),
    )
}

/// Link the existing block (loop) to a new block at the end of the image, and return the new
/// block, pre-populated with the items of the IMAGE_DEF it replaces.
fn place_new_block(
    s: &mut impl Storage,
    first: &mut Block,
    model: &Model,
    ignore_others: bool,
) -> Result<Block> {
    let (mut ram_end, mut flash_end) = (0u32, 0u32);
    for (addr, size) in s.regions().into_iter().filter(|r| r.1 != 0) {
        let end = addr.wrapping_add(size);
        if (model.sram_start()..model.sram_striped_end()).contains(&addr)
            || (model.xip_sram_start()..model.xip_sram_end()).contains(&addr)
        {
            ram_end = ram_end.max(end);
        } else if (model.flash_start()..model.flash_end()).contains(&addr) {
            flash_end = flash_end.max(end);
        }
    }
    let new_addr = if flash_end != 0 { flash_end } else { ram_end };
    if new_addr == 0 {
        bail!(NotPossible, "The image contains no data in flash or RAM");
    }

    if ignore_others {
        set_block_ignored(s, first.physical_addr)?;
    }
    let mut items = first.items.clone();
    if first.next_block_rel == 0 {
        set_next_block(s, first, new_addr)?;
    } else {
        let mut all = picobin::get_all_blocks(&*s, first)?;
        if ignore_others {
            for b in &all {
                set_block_ignored(s, b.physical_addr)?;
            }
        }
        let mut last = all.pop().expect("loop has a last block");
        set_next_block(s, &mut last, new_addr)?;
        if last.get(picobin::ITEM_1BS_IMAGE_TYPE).is_some() {
            items = last.items;
        }
    }

    let mut block = Block::new(new_addr, first.physical_addr.wrapping_sub(new_addr));
    block.items = items;
    // the hash / signature will be replaced
    for ty in [
        picobin::ITEM_SIGNATURE,
        picobin::ITEM_HASH_VALUE,
        picobin::ITEM_1BS_HASH_DEF,
    ] {
        block.remove(ty);
    }
    Ok(block)
}

fn apply_version(block: &mut Block, opts: &SealOptions) -> Result<()> {
    if opts.major == 0 && opts.minor == 0 && opts.rollback == 0 {
        return Ok(());
    }
    let (mut major, mut minor) = (opts.major, opts.minor);
    if let Some(Item::Version {
        major: m, minor: n, ..
    }) = block.remove(picobin::ITEM_1BS_VERSION)
    {
        major = if major == 0 { m } else { major };
        minor = if minor == 0 { n } else { minor };
    }
    if opts.rollback != 0 && !opts.sign {
        bail!(
            Incompatible,
            "You must sign the binary if adding a rollback version"
        );
    }
    let otp_rows = if opts.rollback != 0 {
        opts.rollback_rows.clone()
    } else {
        Vec::new()
    };
    block.items.push(Item::Version {
        major,
        minor,
        rollback: opts.rollback,
        otp_rows,
    });
    Ok(())
}

/// Signed Arm executables get EXTRA_SECURITY and an explicit ENTRY_POINT (+ VECTOR_TABLE).
/// `read_vtor(addr)` returns the initial (sp, pc) words of the vector table at `addr`.
fn apply_entry_point(
    block: &mut Block,
    default_vtor: u32,
    read_vtor: impl Fn(u32) -> Result<(u32, u32)>,
) -> Result<()> {
    let Some(Item::ImageType { flags }) = block.get_mut(picobin::ITEM_1BS_IMAGE_TYPE) else {
        return Ok(());
    };
    if it::image_type(*flags) != it::EXE || it::cpu(*flags) != it::CPU_ARM {
        return Ok(());
    }
    *flags |= it::EXTRA_SECURITY_BITS;
    if block.get(picobin::ITEM_1BS_ENTRY_POINT).is_some() {
        return Ok(());
    }
    let vtor = match block.get(picobin::ITEM_1BS_VECTOR_TABLE) {
        Some(Item::VectorTable { addr }) => *addr,
        _ => {
            block.items.push(Item::VectorTable { addr: default_vtor });
            default_vtor
        }
    };
    let (sp, ep) = read_vtor(vtor)?;
    block.items.push(Item::EntryPoint {
        ep,
        sp,
        splim: None,
    });
    Ok(())
}

/// Load map + the data it covers. `segments` are the (storage, runtime, data) regions used to
/// build a new load map; `read` fetches storage for an existing one.
fn load_map_hash_data(
    block: &mut Block,
    model: &Model,
    opts: &SealOptions,
    segments: Vec<(u32, u32, &[u8])>,
    read: impl Fn(u32, u32) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut pin_xip_sram = opts.pin_xip_sram;
    // The SDK's generic "pin XIP SRAM" load map (or any load map with --pin-xip-sram) is
    // replaced by a full one
    if let Some(lm) = block.load_map() {
        let generic = matches!(lm, [e] if e.storage_address == 0 && e.runtime_address == model.xip_sram_start() && e.size == 0);
        if generic || pin_xip_sram {
            pin_xip_sram = true;
            block.remove(picobin::ITEM_LOAD_MAP);
        }
    }

    let mut to_hash = Vec::new();
    if let Some(entries) = block.load_map() {
        for e in entries {
            match e.storage_address {
                0 => to_hash.extend(e.size.to_le_bytes()),
                s => to_hash.extend(read(s, e.size)?),
            }
        }
        return Ok(to_hash);
    }
    let mut entries = Vec::new();
    let mut clear = |runtime_address, size: u32| {
        entries.push(LoadMapEntry {
            storage_address: 0,
            runtime_address,
            size,
        });
        to_hash.extend(size.to_le_bytes());
    };
    if opts.clear_sram {
        clear(model.sram_start(), model.sram_end() - model.sram_start());
    }
    if pin_xip_sram {
        clear(model.xip_sram_start(), 0);
    }
    for (storage_address, runtime_address, data) in segments {
        entries.push(LoadMapEntry {
            storage_address,
            runtime_address,
            size: data.len() as u32,
        });
        to_hash.extend_from_slice(data);
    }
    block.items.push(Item::LoadMap {
        absolute: false,
        entries,
    });
    Ok(to_hash)
}

/// The block words covered by the hash (up to `words` words, with the TBYB flag cleared).
pub(crate) fn block_hash_bytes(block: &Block, words: usize) -> Vec<u8> {
    let mut w = block.to_words();
    w.truncate(words);
    if let (Some(Item::ImageType { flags }), Some(w1)) = (block.items.first(), w.get_mut(1))
        && flags & it::TBYB_BITS != 0
    {
        *w1 &= !0x8000_0000;
    }
    picobin::words_to_lsb_bytes(&w)
}

/// Add HASH_DEF and, as requested, SIGNATURE and HASH_VALUE items.
fn hash_and_sign(
    block: &mut Block,
    key: Option<&SecretKey>,
    opts: &SealOptions,
    mut to_hash: Vec<u8>,
) -> Result<(Option<[u8; 32]>, Option<RawPublicKey>)> {
    if !opts.hash && !opts.sign {
        return Ok((None, None));
    }
    block.items.push(Item::HashDef {
        hash_type: picobin::HASH_SHA256,
        block_words_to_hash: 0,
    });
    to_hash.extend(block_hash_bytes(block, block.to_words().len() - 3));
    let digest = crypto::sha256(&to_hash);

    let mut public_key = None;
    if opts.sign {
        let (pk, sig) = match key {
            Some(k) => (
                crypto::raw_public_key(&k.public_key()),
                crypto::sign_digest(k, &digest)?,
            ),
            None => ([0; 64], [0; 64]), // external signing placeholder
        };
        if key.is_some() && !crypto::verify_digest(&pk, &sig, &digest) {
            bail!(VerificationFailed, "Signature verification failed");
        }
        block.items.push(Item::Signature {
            sig_type: picobin::SIGNATURE_SECP256K1,
            signature: sig.to_vec(),
            public_key: pk.to_vec(),
        });
        public_key = Some(pk);
    }
    if opts.hash {
        block.items.push(Item::HashValue {
            hash: digest.to_vec(),
        });
    }
    Ok((Some(digest), public_key))
}

fn validate(opts: &mut SealOptions, key: Option<&SecretKey>) -> Result<()> {
    if opts.external_sign {
        opts.sign = true;
        opts.hash = true;
    }
    if opts.sign && !opts.external_sign && key.is_none() {
        bail!(Args, "missing key file for signing");
    }
    if opts.rollback != 0 {
        let defaulted = opts.rollback_rows.is_empty();
        if defaulted {
            opts.rollback_rows = DEFAULT_ROLLBACK_ROWS.to_vec();
        }
        let (have, need) = (opts.rollback_rows.len(), opts.rollback as usize / 24 + 1);
        if have < need {
            let how = if defaulted {
                "set by default"
            } else {
                "specified"
            };
            bail!(
                Args,
                "Rollback version {} requires {} rows - only {} {}",
                opts.rollback,
                need,
                have,
                how
            );
        }
        opts.rollback_rows.sort_unstable();
        if let Some(w) = opts
            .rollback_rows
            .windows(2)
            .find(|w| (w[1] as u32) < w[0] as u32 + 3)
        {
            bail!(
                Args,
                "Rollback rows are RBIT3, so must be three rows apart - {:x} and {:x} are too close",
                w[0],
                w[1]
            );
        }
    }
    Ok(())
}

fn no_block_error(model: &Model) -> Error {
    match model.chip {
        Chip::Rp2040 => err!(
            Format,
            "No metadata block found when sealing RP2040 binary - either use RP2350, or set PICO_CRT0_INCLUDE_PICOBIN_BLOCK=1"
        ),
        _ => err!(Format, "No metadata block found"),
    }
}

/// Seal an image. `key` is required when signing (unless `external_sign` is set).
pub fn seal(input: &[u8], opts: &SealOptions, key: Option<&SecretKey>) -> Result<SealOutput> {
    let mut opts = opts.clone();
    validate(&mut opts, key)?;
    let key = key.filter(|_| opts.sign && !opts.external_sign);
    seal_bin(input, &opts, key)
}

fn seal_bin(input: &[u8], opts: &SealOptions, key: Option<&SecretKey>) -> Result<SealOutput> {
    if !input.len().is_multiple_of(4) {
        bail!(Incompatible, "BIN file size must be a multiple of 4 bytes");
    }
    let start = opts.bin_offset.unwrap_or(FLASH_START);
    let model = model::detect_model(&MemoryView::bin(input, Some(start))?)?;
    let mut bin = Bin {
        start,
        data: input.to_vec(),
    };

    let mut first = picobin::find_first_block(&picobin::lsb_bytes_to_words(&bin.data), start)?
        .ok_or_else(|| no_block_error(&model))?;
    let mut block = place_new_block(&mut bin, &mut first, &model, opts.rollback != 0)?;
    apply_version(&mut block, opts)?;
    if opts.sign {
        apply_entry_point(&mut block, start, |vtor| {
            let r = bin.range(vtor, 8)?;
            let w = &bin.data[r];
            Ok((
                u32::from_le_bytes(w[0..4].try_into().unwrap()),
                u32::from_le_bytes(w[4..8].try_into().unwrap()),
            ))
        })?;
    }
    let to_hash = load_map_hash_data(
        &mut block,
        &model,
        opts,
        vec![(start, start, &bin.data)],
        |addr, size| Ok(bin.data[bin.range(addr, size as usize)?].to_vec()),
    )?;
    let (hash, public_key) = hash_and_sign(&mut block, key, opts, to_hash)?;
    let mut data = bin.data;
    data.extend(block.to_bytes());
    Ok(SealOutput {
        data,
        model,
        block,
        hash,
        public_key,
    })
}
