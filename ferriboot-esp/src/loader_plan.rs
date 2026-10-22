//! ESP executable-layout validation shared by pre-install checks and handoff.
use embedded_storage::nor_flash::ReadNorFlash;
use ferriboot_core::Fih;

use crate::{chip::*, mmu};

const ESP_IMAGE_MAGIC: u8 = 0xE9;
const IMAGE_HEADER_LEN: u32 = 24;
const SEGMENT_HEADER_LEN: u32 = 8;
use bl_loader::MAX_SEGMENTS;
pub use bl_loader::{CopyOp, MapOp, Plan};

/// Fields are only read through `Debug` when logging.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum LoadError<E> {
    Flash(E),
    /// A header or segment extends beyond the authenticated MCUboot payload.
    TruncatedImage,
    BadMagic,
    WrongChip(u16),
    TooManySegments,
    /// ESP image segment lengths must be a multiple of four bytes.
    BadSegmentLength(u32),
    /// Segment at this load address is outside every known memory region.
    BadSegment(u32),
    /// Segment at this load address would overwrite the loader or its stack.
    OverlapsBootloader(u32),
    /// Flash offset and virtual address of a mapped segment disagree mod 64 KiB.
    Misaligned(u32),
    /// Two mapped segments need different pages in the same MMU entry.
    MmuConflict(u32),
    BadEntry(u32),
}

pub fn within(addr: u32, len: u32, (lo, hi): (u32, u32)) -> bool {
    addr >= lo && addr <= hi && len <= hi - addr
}

pub fn plan_from<F: ReadNorFlash, const N: usize>(
    flash: &mut F,
    source: u32,
    image: u32,
    image_size: u32,
    protected: u32,
) -> Result<Plan<N>, LoadError<F::Error>> {
    if image & 3 != 0 || source & 3 != 0 {
        return Err(LoadError::Misaligned(image));
    }
    let image_end = image
        .checked_add(image_size)
        .ok_or(LoadError::TruncatedImage)?;
    let mut read = |off: u32, buf: &mut [u8]| {
        if !within(off, buf.len() as u32, (image, image_end)) {
            return Err(LoadError::TruncatedImage);
        }
        let stored = source
            .checked_add(off - image)
            .ok_or(LoadError::TruncatedImage)?;
        flash.read(stored, buf).map_err(LoadError::Flash)
    };

    #[repr(align(4))]
    struct Buf([u8; IMAGE_HEADER_LEN as usize]);
    let mut hdr = Buf([0; IMAGE_HEADER_LEN as usize]);
    read(image, &mut hdr.0)?;
    let hdr = &hdr.0;
    if hdr[0] != ESP_IMAGE_MAGIC {
        return Err(LoadError::BadMagic);
    }
    let chip = u16::from_le_bytes([hdr[12], hdr[13]]);
    if chip != IMAGE_CHIP_ID {
        return Err(LoadError::WrongChip(chip));
    }
    let count = usize::from(hdr[1]);
    if count > MAX_SEGMENTS {
        return Err(LoadError::TooManySegments);
    }

    let mut plan = Plan {
        entry: u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]),
        boot_info: [0; N],
        copies: 0,
        maps: 0,
        copy: [CopyOp {
            dest: 0,
            flash: 0,
            len: 0,
        }; MAX_SEGMENTS],
        map: [MapOp {
            vaddr: 0,
            paddr: 0,
            pages: 0,
            ibus: 0,
        }; MAX_SEGMENTS],
    };
    let mut mmu_pages = [u32::MAX; mmu::ENTRIES as usize];
    let mut entry_ok = false;
    let mut off = image + IMAGE_HEADER_LEN;

    for _ in 0..count {
        #[repr(align(4))]
        struct SegHdr([u8; 8]);
        let mut sh = SegHdr([0; 8]);
        read(off, &mut sh.0)?;
        let vaddr = u32::from_le_bytes([sh.0[0], sh.0[1], sh.0[2], sh.0[3]]);
        let len = u32::from_le_bytes([sh.0[4], sh.0[5], sh.0[6], sh.0[7]]);
        if len & 3 != 0 {
            return Err(LoadError::BadSegmentLength(len));
        }
        let data = off + SEGMENT_HEADER_LEN;
        if !within(data, len, (image, image_end)) {
            return Err(LoadError::TruncatedImage);
        }
        // Both additions are bounded by the checked authenticated payload end.
        off = data + len;

        if plan.entry >= vaddr && plan.entry - vaddr < len {
            entry_ok = true;
        }

        if len == 0 || vaddr < 0x1_0000 {
            // ESP-IDF convention: padding / reserved segments are not loaded.
            continue;
        }

        let irom = within(vaddr, len, (mmu::IROM_BASE, mmu::IROM_BASE + mmu::BUS_SIZE));
        let drom = within(vaddr, len, (mmu::DROM_BASE, mmu::DROM_BASE + mmu::BUS_SIZE));
        if irom || drom {
            if data % mmu::PAGE_SIZE != vaddr % mmu::PAGE_SIZE {
                return Err(LoadError::Misaligned(vaddr));
            }
            let first = (vaddr & (mmu::BUS_SIZE - 1)) / mmu::PAGE_SIZE;
            let pages = ((vaddr % mmu::PAGE_SIZE) + len).div_ceil(mmu::PAGE_SIZE);
            for i in 0..pages {
                let page = data / mmu::PAGE_SIZE + i;
                let slot = &mut mmu_pages[((first + i) % mmu::ENTRIES) as usize];
                if *slot != u32::MAX && *slot != page {
                    return Err(LoadError::MmuConflict(vaddr));
                }
                *slot = page;
            }
            plan.map[plan.maps as usize] = MapOp {
                vaddr: vaddr & !(mmu::PAGE_SIZE - 1),
                paddr: data & !(mmu::PAGE_SIZE - 1),
                pages,
                ibus: irom as u32,
            };
            plan.maps += 1;
            continue;
        }

        let dest = if within(vaddr, len, IRAM) {
            // Write instruction RAM through its data-bus alias.
            vaddr - IRAM_DRAM_OFFSET
        } else if within(vaddr, len, DRAM) || within(vaddr, len, RTC_FAST) {
            vaddr
        } else {
            return Err(LoadError::BadSegment(vaddr));
        };
        // The handoff runs in normal IRAM, whose DRAM alias begins at
        // `protected`. No RAM segment may touch the handoff, protected
        // stack or boot-info region. RTC FAST is fully available to apps.
        if dest % 4 != 0
            || (dest < protected && dest + len > protected)
            || dest >= protected && dest < DRAM.1
        {
            return Err(LoadError::OverlapsBootloader(vaddr));
        }
        plan.copy[plan.copies as usize] = CopyOp {
            dest,
            flash: data,
            len,
        };
        plan.copies += 1;
    }

    if !entry_ok
        || !(within(
            plan.entry,
            2,
            (mmu::IROM_BASE, mmu::IROM_BASE + mmu::BUS_SIZE),
        ) || within(plan.entry, 2, IRAM))
    {
        return Err(LoadError::BadEntry(plan.entry));
    }
    Ok(plan)
}

/// Whether two plans load the same image the same way: entry point, every
/// RAM copy and every MMU mapping, including unused table entries. The
/// boot-info words are handoff metadata, built separately, and not compared.
/// Computed without data-dependent branches, for the handoff confirmation.
pub fn plans_agree<const N: usize>(a: &Plan<N>, b: &Plan<N>) -> Fih {
    let mut verdict = Fih::eq_u32(a.entry, b.entry)
        & Fih::eq_u32(a.copies, b.copies)
        & Fih::eq_u32(a.maps, b.maps);
    for (x, y) in a.copy.iter().zip(&b.copy) {
        verdict = verdict
            & Fih::eq_u32(x.dest, y.dest)
            & Fih::eq_u32(x.flash, y.flash)
            & Fih::eq_u32(x.len, y.len);
    }
    for (x, y) in a.map.iter().zip(&b.map) {
        verdict = verdict
            & Fih::eq_u32(x.vaddr, y.vaddr)
            & Fih::eq_u32(x.paddr, y.paddr)
            & Fih::eq_u32(x.pages, y.pages)
            & Fih::eq_u32(x.ibus, y.ibus);
    }
    verdict
}
