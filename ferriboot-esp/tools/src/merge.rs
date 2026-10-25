//! Merge flash regions into one raw image, as a single `write-bin` at 0x0.
//!
//! Bytes not covered by a region are 0xFF, the NOR flash erased state, so
//! writing the merged image leaves the gaps exactly as erasing them would.
use anyhow::{Result, ensure};
use ferriboot_layout_esp32c3_supermini as chip;

/// Erased flash byte.
pub const ERASED: u8 = 0xff;

/// Place every `(address, bytes)` region into a `size`-byte erased image.
///
/// Regions must start on an erase sector, be non-empty, fit inside the image
/// and not overlap one another; order does not matter.
pub fn merge(size: u32, regions: &[(u32, &[u8])]) -> Result<Vec<u8>> {
    ensure!(!regions.is_empty(), "no regions to merge");
    ensure!(
        size > 0 && size.is_multiple_of(chip::ERASE_SIZE) && size <= chip::FLASH_SIZE,
        "merged image size {size:#x} must be a non-zero multiple of {:#x} within flash",
        chip::ERASE_SIZE
    );
    let mut spans: Vec<(u64, u64)> = Vec::with_capacity(regions.len());
    for &(address, bytes) in regions {
        let start = u64::from(address);
        let end = start + bytes.len() as u64;
        ensure!(!bytes.is_empty(), "empty region at {address:#x}");
        ensure!(
            address.is_multiple_of(chip::ERASE_SIZE),
            "region at {address:#x} is not sector aligned"
        );
        ensure!(
            end <= u64::from(size),
            "region {start:#x}..{end:#x} exceeds merged image size {size:#x}"
        );
        if let Some(&(a, b)) = spans.iter().find(|&&(a, b)| start < b && a < end) {
            anyhow::bail!("region {start:#x}..{end:#x} overlaps {a:#x}..{b:#x}");
        }
        spans.push((start, end));
    }
    let mut image = vec![ERASED; size as usize];
    for &(address, bytes) in regions {
        let start = address as usize;
        image[start..start + bytes.len()].copy_from_slice(bytes);
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u32 = chip::ERASE_SIZE;

    #[test]
    fn places_regions_and_fills_gaps_with_erased_bytes() {
        let image = merge(4 * S, &[(2 * S, &[3, 4][..]), (0, &[1, 2][..])]).unwrap();
        let mut expected = vec![ERASED; 4 * S as usize];
        expected[..2].copy_from_slice(&[1, 2]);
        expected[2 * S as usize..][..2].copy_from_slice(&[3, 4]);
        assert_eq!(image, expected);
    }

    #[test]
    fn region_may_end_exactly_at_image_end() {
        let full = vec![0u8; S as usize];
        let image = merge(2 * S, &[(S, &full)]).unwrap();
        assert!(image[S as usize..].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_invalid_regions() {
        let two = vec![0u8; 2 * S as usize];
        // Overlap, either order.
        assert!(merge(4 * S, &[(0, &two), (S, &[0][..])]).is_err());
        assert!(merge(4 * S, &[(S, &[0][..]), (0, &two)]).is_err());
        // Past the end, misaligned, empty, none.
        assert!(merge(2 * S, &[(S, &two)]).is_err());
        assert!(merge(2 * S, &[(1, &[0][..])]).is_err());
        assert!(merge(2 * S, &[(0, &[][..])]).is_err());
        assert!(merge(2 * S, &[]).is_err());
        // Bad image sizes.
        assert!(merge(0, &[(0, &[0][..])]).is_err());
        assert!(merge(S + 1, &[(0, &[0][..])]).is_err());
        assert!(merge(chip::FLASH_SIZE + S, &[(0, &[0][..])]).is_err());
    }
}
