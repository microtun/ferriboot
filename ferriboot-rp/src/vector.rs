//! Validate the Cortex-M vector table inside an authenticated image body.
use ferriboot_core::image::{ImageHeader, flags};

use crate::{
    APP_XIP_BASE, BOOT_INFO_ADDRESS, FlashBounds, HEADER_SIZE, RAM_BASE, Region, VECTOR_ALIGN, xip,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorError {
    Header,
    Stack,
    Reset,
}

pub fn validate(
    header: &ImageHeader,
    bounds: FlashBounds,
    region: Region,
    sp: u32,
    reset: u32,
) -> Result<u32, VectorError> {
    let vector = address(header, bounds, region)?;
    let end = vector + header.img_size; // address checked this addition.
    // The initial SP may equal the top of usable SRAM. The shared 256 bytes
    // above it are excluded from the app's RAM, stack, heap and BSS.
    if sp <= RAM_BASE || sp > BOOT_INFO_ADDRESS || !sp.is_multiple_of(8) {
        return Err(VectorError::Stack);
    }
    let pc = reset & !1;
    if reset & 1 == 0 || pc < vector + 8 || pc >= end {
        return Err(VectorError::Reset);
    }
    Ok(vector)
}

/// Check physical storage bounds and return the common runtime vector address.
pub fn address(
    header: &ImageHeader,
    bounds: FlashBounds,
    region: Region,
) -> Result<u32, VectorError> {
    xip::atrans1(bounds, region).map_err(|_| VectorError::Header)?;
    // Exact header size is part of this platform's link/signing contract.
    if header.hdr_size as u32 != HEADER_SIZE
        || header.img_size < 8
        || header.flags & !flags::ROM_FIXED != 0
    {
        return Err(VectorError::Header);
    }
    let vector = APP_XIP_BASE
        .checked_add(HEADER_SIZE)
        .ok_or(VectorError::Header)?;
    let end = vector
        .checked_add(header.img_size)
        .ok_or(VectorError::Header)?;
    let region_end = APP_XIP_BASE
        .checked_add(region.size)
        .ok_or(VectorError::Header)?;
    if end > region_end
        || !vector.is_multiple_of(VECTOR_ALIGN)
        || (header.flags & flags::ROM_FIXED != 0 && header.load_addr != vector - HEADER_SIZE)
    {
        return Err(VectorError::Header);
    }
    Ok(vector)
}
