//! A board's flash map, checked against the RP2350's rules.

use crate::{ERASE_SIZE, FlashBounds, FlashMap, Region, Slot, SlotLayout, xip};

/// A board's [`FlashMap`] that satisfies the RP2350's rules. The bootloader
/// and updater constructors accept nothing else.
///
/// Construct it in a `const` so that a board layout violating the rules is
/// a compile error in the board crate:
///
/// ```ignore
/// const LAYOUT: ferriboot_rp::Layout = ferriboot_rp::Layout::new(ferriboot_layout_rp2350_pico2::FLASH_MAP);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    map: FlashMap,
}

impl Layout {
    /// Check `map` and wrap it. Panics (at compile time, in a `const`) if:
    ///
    /// * the flash size is not erase-aligned or exceeds what the QMI can map;
    /// * the bootloader reservation is empty, unaligned or not inside flash;
    /// * the image slots differ in size, overlap each other or the
    ///   reservation, or cannot be mapped into the common ATRANS1 execution
    ///   window (see [`xip::atrans1`]);
    /// * the scratch area is unaligned, outside flash or overlaps a slot.
    ///
    /// [`SlotLayout::validate`] repeats the generic checks at runtime, for the
    /// strategy the bootloader actually uses.
    pub const fn new(map: FlashMap) -> Self {
        let FlashMap { bounds, slots } = map;
        let erase = ERASE_SIZE as usize;
        if bounds.capacity == 0
            || !bounds.capacity.is_multiple_of(erase)
            || bounds.capacity > xip::MAX_FLASH_SIZE
        {
            panic!("RP2350 flash size must be erase-aligned and at most 16 MiB");
        }
        if bounds.reserved == 0
            || !bounds.reserved.is_multiple_of(ERASE_SIZE)
            || bounds.reserved as usize > bounds.capacity
        {
            panic!("RP2350 bootloader reservation must be erase-aligned and inside flash");
        }
        if slots.primary.size != slots.secondary.size {
            panic!("RP2350 image slots must have equal sizes");
        }
        if xip::atrans1(bounds, slots.primary).is_err()
            || xip::atrans1(bounds, slots.secondary).is_err()
        {
            panic!(
                "RP2350 image slots must be erase-aligned, after the bootloader, inside flash \
                 and at most the 4 MiB execution window"
            );
        }
        if overlaps(slots.primary, slots.secondary) {
            panic!("RP2350 image slots overlap");
        }
        if let Some(scratch) = slots.scratch {
            let in_flash = match scratch.offset.checked_add(scratch.size) {
                Some(end) => end as usize <= bounds.capacity,
                None => false,
            };
            if scratch.size == 0
                || !scratch.offset.is_multiple_of(ERASE_SIZE)
                || !scratch.size.is_multiple_of(ERASE_SIZE)
                || scratch.offset < bounds.reserved
                || !in_flash
                || overlaps(scratch, slots.primary)
                || overlaps(scratch, slots.secondary)
            {
                panic!(
                    "RP2350 scratch must be erase-aligned, inside flash and apart from the slots"
                );
            }
        }
        Self { map }
    }

    /// The checked flash map.
    pub const fn map(&self) -> &FlashMap {
        &self.map
    }

    /// The image slots and scratch area.
    pub const fn slots(&self) -> &SlotLayout {
        &self.map.slots
    }

    /// Flash size and bootloader reservation.
    pub const fn bounds(&self) -> FlashBounds {
        self.map.bounds
    }

    /// Size of the board's flash in bytes, e.g. for
    /// [`hardware::FlashStorage`](crate::hardware::FlashStorage).
    pub const fn flash_size(&self) -> usize {
        self.map.bounds.capacity
    }

    pub const fn region(&self, slot: Slot) -> Region {
        self.map.slots.region(slot)
    }
}

const fn overlaps(a: Region, b: Region) -> bool {
    let a_end = a.offset as u64 + a.size as u64;
    let b_end = b.offset as u64 + b.size as u64;
    (a.offset as u64) < b_end && (b.offset as u64) < a_end
}
