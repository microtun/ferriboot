//! A board's flash map, checked against the machine's rules.

use ferriboot_core::{FlashBounds, FlashMap, Region, Slot, SlotLayout};

use crate::{APP_VECTOR, ERASE_SIZE, EXEC_END, FLASH_SIZE, HEADER_SIZE, Partition};

/// A board's [`FlashMap`] that fits the machine. Construct it in a `const`,
/// so an invalid layout is a compile error in the board crate:
///
/// ```ignore
/// const LAYOUT: ferriboot_qemu::Layout = ferriboot_qemu::Layout::new(ferriboot_layout_qemu::FLASH_MAP);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    map: FlashMap,
}

impl Layout {
    /// Check `map` and wrap it. Panics (at compile time, in a `const`)
    /// unless the map describes the machine's SPI NOR, reserves an
    /// erase-aligned bootloader area, and has equal, erase-aligned slots
    /// after it whose image bodies fit the SSRAM execution window, plus an
    /// optional erase-aligned scratch area, none of them overlapping.
    pub const fn new(map: FlashMap) -> Self {
        let FlashMap { bounds, slots } = map;
        if bounds.capacity != FLASH_SIZE as usize {
            panic!("the QEMU flash map must describe the machine's whole SPI NOR");
        }
        if bounds.reserved == 0 || !bounds.reserved.is_multiple_of(ERASE_SIZE) {
            panic!("the bootloader reservation must be erase-aligned and nonempty");
        }
        if slots.primary.size != slots.secondary.size {
            panic!("image slots must have equal sizes");
        }
        if !area_ok(bounds, slots.primary) || !area_ok(bounds, slots.secondary) {
            panic!("image slots must be erase-aligned, after the bootloader and inside flash");
        }
        if slots.primary.size <= HEADER_SIZE
            || APP_VECTOR as u64 + slots.primary.size as u64 > EXEC_END as u64
        {
            panic!("an image body must fit the SSRAM execution window");
        }
        if overlaps(slots.primary, slots.secondary) {
            panic!("image slots overlap");
        }
        if let Some(scratch) = slots.scratch
            && (!area_ok(bounds, scratch)
                || overlaps(scratch, slots.primary)
                || overlaps(scratch, slots.secondary))
        {
            panic!("scratch must be erase-aligned, inside flash and apart from the slots");
        }
        Self { map }
    }

    pub const fn map(&self) -> &FlashMap {
        &self.map
    }

    pub const fn slots(&self) -> &SlotLayout {
        &self.map.slots
    }

    pub const fn bounds(&self) -> FlashBounds {
        self.map.bounds
    }

    pub const fn region(&self, slot: Slot) -> Region {
        self.map.slots.region(slot)
    }

    /// One image slot as a flash partition.
    pub fn partition(&self, slot: Slot) -> Partition {
        Partition::new(self.region(slot))
    }

    /// The largest image body (without the header) a slot can hold.
    pub const fn max_body_size(&self) -> u32 {
        self.map.slots.primary.size - HEADER_SIZE
    }
}

const fn area_ok(bounds: FlashBounds, region: Region) -> bool {
    let in_flash = match region.offset.checked_add(region.size) {
        Some(end) => end as usize <= bounds.capacity,
        None => false,
    };
    region.size > 0
        && region.offset.is_multiple_of(ERASE_SIZE)
        && region.size.is_multiple_of(ERASE_SIZE)
        && region.offset >= bounds.reserved
        && in_flash
}

const fn overlaps(a: Region, b: Region) -> bool {
    let a_end = a.offset as u64 + a.size as u64;
    let b_end = b.offset as u64 + b.size as u64;
    (a.offset as u64) < b_end && (b.offset as u64) < a_end
}
