//! RP2350 "picobin" metadata blocks: parsing, serialisation and block-loop discovery
//! (the subset of picotool's `bintool/metadata.h` needed for sealing). Items that `seal`
//! doesn't need to inspect (partition tables, unknown items) are kept as raw words.

use super::error::{Result, bail};

pub const BLOCK_MARKER_START: u32 = 0xffff_ded3;
pub const BLOCK_MARKER_END: u32 = 0xab12_3579;
pub const MAX_BLOCK_SIZE: u32 = 0x280;

pub const ITEM_1BS_IMAGE_TYPE: u8 = 0x42;
pub const ITEM_1BS_VECTOR_TABLE: u8 = 0x03;
pub const ITEM_1BS_ENTRY_POINT: u8 = 0x44;
pub const ITEM_1BS_ROLLING_WINDOW_DELTA: u8 = 0x05;
pub const ITEM_LOAD_MAP: u8 = 0x06;
pub const ITEM_1BS_HASH_DEF: u8 = 0x47;
pub const ITEM_1BS_VERSION: u8 = 0x48;
pub const ITEM_SIGNATURE: u8 = 0x09;
pub const ITEM_PARTITION_TABLE: u8 = 0x0a;
pub const ITEM_HASH_VALUE: u8 = 0x4b;
pub const ITEM_2BS_LAST: u8 = 0xff;

pub const HASH_SHA256: u8 = 0x01;
pub const SIGNATURE_SECP256K1: u8 = 0x01;

/// IMAGE_DEF flag fields (`PICOBIN_IMAGE_TYPE_*`).
pub mod image_type {
    pub const EXE: u16 = 1;
    pub const DATA: u16 = 2;
    pub const SECURITY_NS: u16 = 1;
    pub const SECURITY_S: u16 = 2;
    pub const CPU_ARM: u16 = 0;
    pub const CPU_RISCV: u16 = 1;
    pub const CPU_VARMULET: u16 = 2;
    pub const CHIP_RP2040: u16 = 0;
    pub const CHIP_RP2350: u16 = 1;
    pub const EXTRA_SECURITY_BITS: u16 = 0x0800;
    pub const TBYB_BITS: u16 = 0x8000;

    pub fn image_type(f: u16) -> u16 {
        f & 0xf
    }
    pub fn security(f: u16) -> u16 {
        (f >> 4) & 3
    }
    pub fn cpu(f: u16) -> u16 {
        (f >> 8) & 7
    }
    pub fn chip(f: u16) -> u16 {
        (f >> 12) & 7
    }
}

/// Little-endian bytes to words (any trailing partial word is ignored).
pub fn lsb_bytes_to_words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

pub fn words_to_lsb_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Size (in words) from an item header; 2-byte size if bit 7 of the type is set.
pub fn decode_size(header: u32) -> u32 {
    (header >> 8) & if header & 0x80 != 0 { 0xffff } else { 0xff }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadMapEntry {
    /// Absolute storage address (0 for "clear"/"pin" entries).
    pub storage_address: u32,
    pub runtime_address: u32,
    pub size: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    ImageType {
        flags: u16,
    },
    VectorTable {
        addr: u32,
    },
    RollingWindowDelta {
        addr: i32,
    },
    EntryPoint {
        ep: u32,
        sp: u32,
        splim: Option<u32>,
    },
    LoadMap {
        absolute: bool,
        entries: Vec<LoadMapEntry>,
    },
    Version {
        major: u16,
        minor: u16,
        rollback: u16,
        otp_rows: Vec<u16>,
    },
    HashDef {
        hash_type: u8,
        block_words_to_hash: u16,
    },
    Signature {
        sig_type: u8,
        signature: Vec<u8>,
        public_key: Vec<u8>,
    },
    HashValue {
        hash: Vec<u8>,
    },
    /// Any other item, as its raw words (header included).
    Raw(Vec<u32>),
}

/// Item header for `ty` with the given size in words.
fn header(ty: u8, size: u32) -> u32 {
    let two_byte = ty & 0x80 != 0 || size >= 256;
    (size << 8) | if two_byte { 0x80 } else { 0 } | ty as u32
}

impl Item {
    pub fn item_type(&self) -> u8 {
        match self {
            Item::ImageType { .. } => ITEM_1BS_IMAGE_TYPE,
            Item::VectorTable { .. } => ITEM_1BS_VECTOR_TABLE,
            Item::RollingWindowDelta { .. } => ITEM_1BS_ROLLING_WINDOW_DELTA,
            Item::EntryPoint { .. } => ITEM_1BS_ENTRY_POINT,
            Item::LoadMap { .. } => ITEM_LOAD_MAP,
            Item::Version { .. } => ITEM_1BS_VERSION,
            Item::HashDef { .. } => ITEM_1BS_HASH_DEF,
            Item::Signature { .. } => ITEM_SIGNATURE,
            Item::HashValue { .. } => ITEM_HASH_VALUE,
            Item::Raw(w) => w[0] as u8,
        }
    }

    /// Parse an item from its words (header first). `item_addr` is the header's address.
    fn parse(words: &[u32], item_addr: u32) -> Result<Item> {
        let h = words[0];
        let body = &words[1..];
        let need = |n: usize| -> Result<()> {
            if body.len() < n {
                bail!(
                    Format,
                    "Metadata item at {:08x} (type {:02x}) is malformed",
                    item_addr,
                    h as u8
                );
            }
            Ok(())
        };
        Ok(match h as u8 {
            ITEM_1BS_IMAGE_TYPE => Item::ImageType {
                flags: (h >> 16) as u16,
            },
            ITEM_1BS_VECTOR_TABLE => {
                need(1)?;
                Item::VectorTable { addr: body[0] }
            }
            ITEM_1BS_ROLLING_WINDOW_DELTA => {
                need(1)?;
                Item::RollingWindowDelta {
                    addr: body[0] as i32,
                }
            }
            ITEM_1BS_ENTRY_POINT => {
                need(2)?;
                Item::EntryPoint {
                    ep: body[0],
                    sp: body[1],
                    splim: body.get(2).copied(),
                }
            }
            ITEM_LOAD_MAP => {
                let n = ((h >> 24) & 0x7f) as usize;
                let absolute = h & 0x8000_0000 != 0;
                need(3 * n)?;
                let entries = body
                    .chunks_exact(3)
                    .take(n)
                    .map(|e| match (e[0], absolute) {
                        (0, _) => LoadMapEntry {
                            storage_address: 0,
                            runtime_address: e[1],
                            size: e[2],
                        },
                        (s, true) => LoadMapEntry {
                            storage_address: s,
                            runtime_address: e[1],
                            size: e[2].wrapping_sub(e[1]),
                        },
                        (s, false) => LoadMapEntry {
                            storage_address: s.wrapping_add(item_addr),
                            runtime_address: e[1],
                            size: e[2],
                        },
                    })
                    .collect();
                Item::LoadMap { absolute, entries }
            }
            ITEM_1BS_VERSION => {
                need(1)?;
                let n = (h >> 24) as usize;
                let (mut rollback, mut otp_rows) = (0, Vec::new());
                if n > 0 {
                    // rollback (16 bits), then the rows packed two per word
                    need(1 + (n + 2) / 2)?;
                    let halves: Vec<u16> = body[1..]
                        .iter()
                        .flat_map(|w| [*w as u16, (*w >> 16) as u16])
                        .collect();
                    rollback = halves[0];
                    otp_rows = halves[1..=n].to_vec();
                }
                Item::Version {
                    major: (body[0] >> 16) as u16,
                    minor: body[0] as u16,
                    rollback,
                    otp_rows,
                }
            }
            ITEM_1BS_HASH_DEF => {
                need(1)?;
                Item::HashDef {
                    hash_type: (h >> 24) as u8,
                    block_words_to_hash: body[0] as u16,
                }
            }
            ITEM_HASH_VALUE => Item::HashValue {
                hash: words_to_lsb_bytes(body),
            },
            ITEM_SIGNATURE => {
                if body.len() != 32 {
                    bail!(
                        Format,
                        "Bad block size {} for signature item",
                        body.len() + 1
                    );
                }
                Item::Signature {
                    sig_type: (h >> 24) as u8,
                    public_key: words_to_lsb_bytes(&body[..16]),
                    signature: words_to_lsb_bytes(&body[16..]),
                }
            }
            _ => Item::Raw(words.to_vec()),
        })
    }

    /// Serialise. `item_addr` is the address the item's header will be written at.
    pub fn to_words(&self, item_addr: u32) -> Vec<u32> {
        let ty = self.item_type();
        let mut v: Vec<u32> = match self {
            Item::ImageType { flags } => return vec![header(ty, 1) | (*flags as u32) << 16],
            Item::VectorTable { addr } => vec![*addr],
            Item::RollingWindowDelta { addr } => vec![*addr as u32],
            Item::EntryPoint { ep, sp, splim } => [*ep, *sp].into_iter().chain(*splim).collect(),
            Item::LoadMap { absolute, entries } => {
                let mut v =
                    vec![(entries.len() as u32) << 24 | if *absolute { 1 << 31 } else { 0 }];
                for e in entries {
                    v.extend(match (e.storage_address, absolute) {
                        (0, _) => [0, e.runtime_address, e.size],
                        (s, true) => [s, e.runtime_address, e.runtime_address.wrapping_add(e.size)],
                        (s, false) => [s.wrapping_sub(item_addr), e.runtime_address, e.size],
                    });
                }
                let n = v.len() as u32;
                v[0] |= header(ty, n);
                return v;
            }
            Item::Version {
                major,
                minor,
                rollback,
                otp_rows,
            } => {
                let mut v = vec![(*major as u32) << 16 | *minor as u32];
                if !otp_rows.is_empty() {
                    let halves: Vec<u16> = std::iter::once(*rollback)
                        .chain(otp_rows.iter().copied())
                        .collect();
                    v.extend(
                        halves
                            .chunks(2)
                            .map(|c| c[0] as u32 | (*c.get(1).unwrap_or(&0) as u32) << 16),
                    );
                }
                let n = v.len() as u32 + 1;
                return [header(ty, n) | (otp_rows.len() as u32) << 24]
                    .into_iter()
                    .chain(v)
                    .collect();
            }
            Item::HashDef {
                hash_type,
                block_words_to_hash,
            } => {
                // 0 means "everything up to and including this item"
                let words = if *block_words_to_hash == 0 {
                    u32::MAX
                } else {
                    *block_words_to_hash as u32
                };
                return vec![header(ty, 2) | (*hash_type as u32) << 24, words];
            }
            Item::Signature {
                sig_type,
                signature,
                public_key,
            } => {
                let mut v = lsb_bytes_to_words(public_key);
                v.extend(lsb_bytes_to_words(signature));
                return [header(ty, v.len() as u32 + 1) | (*sig_type as u32) << 24]
                    .into_iter()
                    .chain(v)
                    .collect();
            }
            Item::HashValue { hash } => lsb_bytes_to_words(hash),
            Item::Raw(w) => return w.clone(),
        };
        v.insert(0, header(ty, v.len() as u32 + 1));
        v
    }
}

/// A metadata block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub physical_addr: u32,
    pub next_block_rel: u32,
    /// Word index (from the start marker) of the next-block-offset word.
    pub next_block_rel_index: u32,
    pub items: Vec<Item>,
}

impl Block {
    pub fn new(physical_addr: u32, next_block_rel: u32) -> Self {
        Block {
            physical_addr,
            next_block_rel,
            next_block_rel_index: 0,
            items: Vec::new(),
        }
    }

    pub fn to_words(&self) -> Vec<u32> {
        let mut words = vec![BLOCK_MARKER_START];
        for item in &self.items {
            let idx = words.len() as u32;
            let mut w = item.to_words(self.physical_addr + 4 * idx);
            if let Item::HashDef {
                block_words_to_hash: 0,
                ..
            } = item
            {
                w[1] = idx + 2;
            }
            words.extend(w);
        }
        let n = words.len() as u32 - 1;
        words.extend([
            ITEM_2BS_LAST as u32 | n << 8,
            self.next_block_rel,
            BLOCK_MARKER_END,
        ]);
        words
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        words_to_lsb_bytes(&self.to_words())
    }

    pub fn find(&self, ty: u8) -> Option<usize> {
        self.items.iter().position(|i| i.item_type() == ty)
    }

    pub fn get(&self, ty: u8) -> Option<&Item> {
        self.find(ty).map(|i| &self.items[i])
    }

    pub fn get_mut(&mut self, ty: u8) -> Option<&mut Item> {
        self.find(ty).map(move |i| &mut self.items[i])
    }

    pub fn remove(&mut self, ty: u8) -> Option<Item> {
        self.find(ty).map(|i| self.items.remove(i))
    }

    pub fn image_type_flags(&self) -> Option<u16> {
        match self.get(ITEM_1BS_IMAGE_TYPE)? {
            Item::ImageType { flags } => Some(*flags),
            _ => None,
        }
    }

    pub fn hash_value(&self) -> Option<&[u8]> {
        match self.get(ITEM_HASH_VALUE)? {
            Item::HashValue { hash } => Some(hash),
            _ => None,
        }
    }

    pub fn load_map(&self) -> Option<&[LoadMapEntry]> {
        match self.get(ITEM_LOAD_MAP)? {
            Item::LoadMap { entries, .. } => Some(entries),
            _ => None,
        }
    }
}

/// Parse a block whose start marker is `words[0]`, located at `addr`. Returns `Ok(None)` if
/// the words don't form a complete block.
fn parse_block(words: &[u32], addr: u32) -> Result<Option<Block>> {
    let mut i = 1;
    while i < words.len() {
        let size = decode_size(words[i]) as usize;
        if words[i] as u8 == ITEM_2BS_LAST {
            if size != i - 1 || words.get(i + 2) != Some(&BLOCK_MARKER_END) {
                return Ok(None);
            }
            let mut items = Vec::new();
            let mut j = 1;
            while j < i {
                let n = decode_size(words[j]) as usize;
                items.push(Item::parse(&words[j..j + n], addr + 4 * j as u32)?);
                j += n;
            }
            return Ok(Some(Block {
                physical_addr: addr,
                next_block_rel: words[i + 1],
                next_block_rel_index: i as u32 + 1,
                items,
            }));
        }
        if size == 0 {
            return Ok(None);
        }
        i += size;
    }
    Ok(None)
}

/// Find the first valid block in words located at `storage_addr`.
pub fn find_first_block(words: &[u32], storage_addr: u32) -> Result<Option<Block>> {
    for (i, _) in words
        .iter()
        .enumerate()
        .filter(|(_, w)| **w == BLOCK_MARKER_START)
    {
        if let Some(b) = parse_block(&words[i..], storage_addr + 4 * i as u32)? {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

/// Something block loops can be followed through.
pub trait BlockStore {
    /// The words from `addr` to the end of the contiguous region containing it.
    fn words_from(&self, addr: u32) -> Result<Vec<u32>>;
}

/// Follow the block loop from `first`, returning every subsequent block (the last one is the
/// block that links back to `first`).
pub fn get_all_blocks<S: BlockStore + ?Sized>(store: &S, first: &Block) -> Result<Vec<Block>> {
    let mut addr = first.physical_addr.wrapping_add(first.next_block_rel);
    let mut all: Vec<Block> = Vec::new();
    loop {
        let words = store.words_from(addr)?;
        if words.first() != Some(&BLOCK_MARKER_START) {
            bail!(
                Unknown,
                "Block loop is not valid - no block found at {:08x}",
                addr
            );
        }
        let Some(blk) = parse_block(&words, addr)? else {
            bail!(
                Unknown,
                "Block loop is not valid - incomplete block found at {:08x}",
                addr
            );
        };
        let next = blk.physical_addr.wrapping_add(blk.next_block_rel);
        all.push(blk);
        if next == first.physical_addr {
            return Ok(all);
        }
        if all.iter().any(|b| b.physical_addr == next) {
            bail!(
                Unknown,
                "Block loop is not valid - contains a loop to {:08x}, but first block at {:08x}",
                next,
                first.physical_addr
            );
        }
        addr = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_block() {
        let mut b = Block::new(0x1000_0138, 0);
        b.items.push(Item::ImageType { flags: 0x1021 });
        b.items.push(Item::LoadMap {
            absolute: false,
            entries: vec![LoadMapEntry {
                storage_address: 0x1000_0000,
                runtime_address: 0x1000_0000,
                size: 0x100,
            }],
        });
        b.items.push(Item::Version {
            major: 1,
            minor: 2,
            rollback: 3,
            otp_rows: vec![0x4e, 0x51, 0x54],
        });
        b.items.push(Item::Version {
            major: 1,
            minor: 2,
            rollback: 3,
            otp_rows: vec![0x4e, 0x51],
        });
        b.items.push(Item::HashDef {
            hash_type: 1,
            block_words_to_hash: 0,
        });
        b.items.push(Item::Raw(vec![0x0000_02fe, 0x1234]));
        let words = b.to_words();
        let mut parsed = find_first_block(&words, 0x1000_0138).unwrap().unwrap();
        // the HASH_DEF's word count is explicit once parsed
        parsed.items[4] = Item::HashDef {
            hash_type: 1,
            block_words_to_hash: 0,
        };
        assert_eq!(parsed.to_words(), words);
        assert_eq!(parsed.load_map().unwrap()[0].storage_address, 0x1000_0000);
    }
}
