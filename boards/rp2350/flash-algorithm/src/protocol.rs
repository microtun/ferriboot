//! Single-pass LZ4 sector restore protocol. See ../README.md for the wire format.
//!
//! Every physical sector has exactly one record, including erased sectors. The
//! host cannot skip records based on flash contents, so no scan or reply is needed.
//! One scratch SRAM bank holds a *compressed* sector, while `Algorithm::buffer`
//! holds the decoded sector. Neither the decoder nor the stream needs an allocator.

use crate::Error;
use crate::rp2350::Rp2350;
use bytemuck::{Pod, Zeroable};

pub const FLASH_SIZE: usize = 4 * 1024 * 1024;
pub const SECTOR: usize = 4096;
pub const PAGE: usize = 256;
pub const SECTORS: usize = FLASH_SIZE / SECTOR;
const WORDS_PER_PAGE: usize = PAGE / 4;

/// probe-rs sees a 5 MiB "flash" at this address; it is a command window, not NOR.
pub const WINDOW_START: u32 = 0x1000_0000;
pub const WINDOW_END: u32 = 0x1050_0000;

const PACKET: usize = 4096;
const HEADER: usize = 12;
const MAX_PAYLOAD: usize = PACKET - HEADER;
/// SRAM4 (scratch Y). Excluded from probe-rs's usable RAM; no algorithm code or
/// probe-rs stack/data is placed here. Only core0 accesses it during ProgramPage.
const COMPRESSED_SCRATCH: *mut [u8; SECTOR] = 0x2008_1000 as *mut [u8; SECTOR];

/// One flash sector as words, so flash is read with aligned word loads.
pub type Sector = [u32; SECTOR / 4];

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct PacketHeader {
    sequence: u32,
    length: u32,
    last: u32,
}

/// Exactly 1024 records of `[encoded_length: u16][bytes...]` in sector order.
/// 0 = erased, 4096 = raw, 1..4095 = an independent LZ4 block.
struct Restore {
    sector: usize,
    header: [u8; 2],
    header_filled: usize,
    body_len: usize,
    body_filled: usize,
}

impl Restore {
    const fn new() -> Self {
        Self {
            sector: 0,
            header: [0; 2],
            header_filled: 0,
            body_len: 0,
            body_filled: 0,
        }
    }

    fn complete_sector(
        &mut self,
        chip: &Rp2350,
        buffer: &mut Sector,
        input: &[u8; SECTOR],
    ) -> Result<(), Error> {
        match self.body_len {
            0 => buffer.fill(u32::MAX),
            SECTOR => bytemuck::bytes_of_mut(buffer).copy_from_slice(input),
            size => {
                let written = lz4_flex::block::decompress_into(
                    &input[..size],
                    bytemuck::bytes_of_mut(buffer),
                )
                .map_err(|_| Error::Format)?;
                if written != SECTOR {
                    return Err(Error::Format);
                }
            }
        }
        // Keep all preexisting flash semantics: skip unchanged, erase only when
        // required, program changed pages and read back the complete sector.
        write_sector(chip, buffer, self.sector as u16)?;
        self.sector += 1;
        self.header_filled = 0;
        self.body_len = 0;
        self.body_filled = 0;
        Ok(())
    }

    fn feed(
        &mut self,
        chip: &Rp2350,
        buffer: &mut Sector,
        input: &mut [u8; SECTOR],
        mut payload: &[u8],
    ) -> Result<(), Error> {
        while !payload.is_empty() {
            // Never accept trailing data or an extra 1025th record.
            if self.sector == SECTORS {
                return Err(Error::Format);
            }
            if self.header_filled != self.header.len() {
                if !fill(&mut self.header, &mut self.header_filled, &mut payload) {
                    break;
                }
                self.body_len = u16::from_le_bytes(self.header) as usize;
                if self.body_len > SECTOR {
                    return Err(Error::Format);
                }
                if self.body_len == 0 {
                    self.complete_sector(chip, buffer, input)?;
                    continue;
                }
            }
            if !fill(
                &mut input[..self.body_len],
                &mut self.body_filled,
                &mut payload,
            ) {
                break;
            }
            self.complete_sector(chip, buffer, input)?;
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), Error> {
        if self.sector == SECTORS && self.header_filled == 0 && self.body_filled == 0 {
            Ok(())
        } else {
            Err(Error::Truncated)
        }
    }
}

/// State for one Init..UnInit session.
struct Session {
    sequence: u32,
    finished: bool,
    /// Sticky until the next `Init`.
    error: Option<Error>,
    restore: Restore,
}

impl Session {
    const fn new() -> Self {
        Self {
            sequence: 0,
            finished: false,
            error: None,
            restore: Restore::new(),
        }
    }
}

pub struct Algorithm {
    chip: Rp2350,
    /// Decoded sector, also used as bootrom initialization scratch.
    buffer: Sector,
    /// The compressed-sector bank, claimed once by the first `init`.
    scratch: Option<&'static mut [u8; SECTOR]>,
    session: Session,
}

impl Algorithm {
    pub const fn new() -> Self {
        Self {
            chip: Rp2350::new(),
            buffer: [0; SECTOR / 4],
            scratch: None,
            session: Session::new(),
        }
    }

    pub fn init(&mut self) -> Result<(), Error> {
        if self.scratch.is_none() {
            // SAFETY: the scratch Y bank is reserved from the probe-rs RAM map
            // and nothing else in this image refers to it. There is a single
            // `Algorithm` (the static in main.rs), and this branch runs at most
            // once for it, so this is the only reference ever created.
            self.scratch = Some(unsafe { &mut *COMPRESSED_SCRATCH });
        }
        let result = self.chip.init(&mut self.buffer);
        self.session = Session::new();
        self.session.error = result.err();
        result
    }

    /// `operation` 1 ends probe-rs's erase phase; programming follows in a
    /// new session, so core1 stays parked and an empty stream is fine.
    pub fn uninit(&mut self, operation: u32) -> Result<(), Error> {
        const ERASE: u32 = 1;
        let session = &self.session;
        if operation != ERASE && (session.finished || session.error.is_some()) {
            self.chip.release_core1();
        }
        match session.error {
            Some(error) => Err(error),
            None if operation == ERASE || session.finished => Ok(()),
            None => Err(Error::Truncated),
        }
    }

    /// probe-rs insists on an erase phase; the restore stream decides what to erase.
    pub fn erase_sector(&self, address: u32) -> Result<(), Error> {
        let aligned = address.is_multiple_of(SECTOR as u32);
        if aligned && (WINDOW_START..WINDOW_END).contains(&address) {
            Ok(())
        } else {
            Err(Error::Format)
        }
    }

    /// Consume one 4096-byte command packet. `None` is a null data pointer.
    pub fn program_page(&mut self, address: u32, packet: Option<&[u8]>) -> Result<(), Error> {
        if let Some(error) = self.session.error {
            return Err(error);
        }
        let result = match packet {
            Some(packet) if packet.len() == PACKET => self.packet(address, packet),
            _ => Err(Error::Format),
        };
        self.session.error = result.err();
        result
    }

    fn packet(&mut self, address: u32, packet: &[u8]) -> Result<(), Error> {
        let Self {
            chip,
            buffer,
            scratch,
            session,
        } = self;
        let expected_address = WINDOW_START + session.sequence * PACKET as u32;
        if session.finished || address >= WINDOW_END || address != expected_address {
            return Err(Error::Sequence);
        }

        let header: PacketHeader = bytemuck::pod_read_unaligned(&packet[..HEADER]);
        let length = header.length as usize;
        let last = match header.last {
            0 if length == MAX_PAYLOAD => false,
            1 if length <= MAX_PAYLOAD => true,
            _ => return Err(Error::Format),
        };
        if header.sequence != session.sequence {
            return Err(Error::Sequence);
        }

        // `init` claimed the bank; a session never starts without it.
        let input = scratch.as_deref_mut().ok_or(Error::Sequence)?;
        session
            .restore
            .feed(chip, buffer, input, &packet[HEADER..HEADER + length])?;
        session.sequence += 1;
        if last {
            session.restore.finish()?;
            session.finished = true;
        }
        Ok(())
    }
}

/// Move bytes from the front of `input` into `buffer[*filled..]`.
fn fill(buffer: &mut [u8], filled: &mut usize, input: &mut &[u8]) -> bool {
    let count = (buffer.len() - *filled).min(input.len());
    let (head, tail) = input.split_at(count);
    buffer[*filled..*filled + count].copy_from_slice(head);
    *filled += count;
    *input = tail;
    *filled == buffer.len()
}

/// Make flash sector `sector` equal `desired`, touching as little as possible:
/// identical sectors are left alone, only changed pages are programmed, and the
/// sector is erased only if some bit has to go from 0 to 1.
fn write_sector(chip: &Rp2350, desired: &Sector, sector: u16) -> Result<(), Error> {
    let offset = usize::from(sector) * SECTOR;
    let (mut changed, mut nonblank, mut needs_erase) = (0u32, 0u32, false);
    for (i, &want) in desired.iter().enumerate() {
        let have = chip.read_word(offset + i * 4);
        let page = 1 << (i / WORDS_PER_PAGE);
        if have != want {
            changed |= page;
        }
        if want != u32::MAX {
            nonblank |= page;
        }
        needs_erase |= have & want != want;
    }
    if changed == 0 {
        return Ok(());
    }

    // After an erase every non-blank page must be rewritten, changed or not.
    let mut pages = if needs_erase { nonblank } else { changed };
    let bytes = bytemuck::bytes_of(desired);
    chip.begin_writes();
    if needs_erase {
        chip.erase_sector(offset);
    }
    while pages != 0 {
        // Program each run of consecutive pages with a single call.
        let first = pages.trailing_zeros() as usize;
        let count = (pages >> first).trailing_ones() as usize;
        pages &= !(((1 << count) - 1) << first);
        let range = first * PAGE..(first + count) * PAGE;
        chip.program(offset + range.start, &bytes[range]);
    }
    chip.end_writes();

    let verified = desired
        .iter()
        .enumerate()
        .all(|(i, &want)| chip.read_word(offset + i * 4) == want);
    if verified { Ok(()) } else { Err(Error::Verify) }
}
