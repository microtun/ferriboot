//! QMI ATRANS1 mapping for a common application execution window.
use crate::{APP_XIP_WINDOW_SIZE, ERASE_SIZE, FlashBounds, HEADER_SIZE, LayoutError, Region};

/// Physical flash the QMI address translation can reach (12-bit BASE field
/// in 4 KiB units).
pub const MAX_FLASH_SIZE: usize = 0x100_0000;

/// Encode a physical slot as ATRANS1's BASE and SIZE fields (4 KiB units).
/// The application window starts at APP_XIP_BASE; offsets in that window
/// translate to `region.offset + offset`. ATRANS0 is left unchanged.
///
/// The slot must lie after the bootloader's reservation and inside the
/// board's flash (`bounds`), and fit the execution window.
pub const fn atrans1(bounds: FlashBounds, region: Region) -> Result<u32, LayoutError> {
    if !region.offset.is_multiple_of(ERASE_SIZE)
        || !region.size.is_multiple_of(ERASE_SIZE)
        || region.size <= HEADER_SIZE
    {
        return Err(LayoutError::Misaligned);
    }
    if region.offset < bounds.reserved {
        return Err(LayoutError::Overlap);
    }
    let in_flash = match region.offset.checked_add(region.size) {
        Some(end) => end as usize <= bounds.capacity && end as usize <= MAX_FLASH_SIZE,
        None => false,
    };
    if region.size > APP_XIP_WINDOW_SIZE || !in_flash {
        return Err(LayoutError::OutOfBounds);
    }
    Ok(((region.size / ERASE_SIZE) << 16) | (region.offset / ERASE_SIZE))
}
