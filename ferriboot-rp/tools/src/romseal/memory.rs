//! Address-space view of a flat BIN file: maps physical addresses to offsets.

use super::error::{Result, bail};
use super::model::FLASH_START;
use super::picobin::{self, BlockStore};

#[derive(Debug, Clone)]
pub struct MemoryView<'a> {
    data: &'a [u8],
    /// (start address, end address, file offset), sorted by address
    ranges: Vec<(u32, u32, usize)>,
    binary_start: u32,
}

impl<'a> MemoryView<'a> {
    /// A BIN file loaded at `offset` (default 0x10000000).
    pub fn bin(data: &'a [u8], offset: Option<u32>) -> Result<Self> {
        let start = offset.unwrap_or(FLASH_START);
        let Some(end) = start
            .checked_add(data.len() as u32)
            .filter(|_| data.len() <= u32::MAX as usize)
        else {
            bail!(
                Format,
                "BIN file is too large for load address {:08x}",
                start
            );
        };
        Ok(MemoryView {
            data,
            ranges: vec![(start, end, 0)],
            binary_start: start,
        })
    }

    /// The image's start address (flash start, or the lowest RAM address).
    pub fn binary_start(&self) -> u32 {
        self.binary_start
    }

    /// Read `size` bytes at `addr`, zero-filling unmapped addresses.
    pub fn read(&self, addr: u32, size: usize) -> Vec<u8> {
        let mut out = vec![0u8; size];
        let end = addr as u64 + size as u64;
        for &(from, to, off) in &self.ranges {
            let (lo, hi) = ((addr as u64).max(from as u64), end.min(to as u64));
            if lo < hi {
                let src = off + (lo - from as u64) as usize;
                let n = ((hi - lo) as usize).min(self.data.len().saturating_sub(src));
                let dst = (lo - addr as u64) as usize;
                out[dst..dst + n].copy_from_slice(&self.data[src..src + n]);
            }
        }
        out
    }

    /// File offset of an address range (which must lie within one mapped range).
    pub fn file_offset(&self, addr: u32, size: usize) -> Result<usize> {
        self.ranges
            .iter()
            .find(|r| addr >= r.0 && addr as u64 + size as u64 <= r.1 as u64)
            .map(|r| r.2 + (addr - r.0) as usize)
            .filter(|o| o + size <= self.data.len())
            .ok_or_else(|| {
                super::error::err!(
                    WriteFailed,
                    "Address range {:08x}+{:x} is not contained in the file",
                    addr,
                    size
                )
            })
    }
}

impl BlockStore for MemoryView<'_> {
    fn words_from(&self, addr: u32) -> Result<Vec<u32>> {
        Ok(picobin::lsb_bytes_to_words(
            &self.read(addr, picobin::MAX_BLOCK_SIZE as usize * 4),
        ))
    }
}
