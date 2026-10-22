//! ESP-IDF application images: conversion with espflash, and in-place
//! mutations that produce images the bootloader must refuse.
//!
//! Mutations edit espflash's output (header fields and segment headers) and
//! then refresh the trailing checksum and SHA-256 where the segment table is
//! still walkable, so a rejection is caused by the layout under test rather
//! than by a stale digest (which Ferriboot does not check; the MCUboot
//! signature covers the whole image).

use espflash::{
    flasher::{FlashData, FlashFrequency, FlashMode, FlashSettings, FlashSize},
    image_format::idf::IdfBootloaderFormat,
    target::{Chip, XtalFrequency},
};
use sha2::{Digest, Sha256};

pub const HEADER_LEN: usize = 24;
pub const SEGMENT_HEADER_LEN: usize = 8;
const CHECKSUM_MAGIC: u8 = 0xEF;

/// Flash parameters of the 4 MiB layout (as `ferriboot-tools-esp flash-ferriboot` uses).
pub fn flash_data() -> FlashData {
    FlashData::new(
        FlashSettings::new(
            Some(FlashMode::Dio),
            Some(FlashSize::_4Mb),
            Some(FlashFrequency::_80Mhz),
        ),
        0,
        None,
        Chip::Esp32c3,
        XtalFrequency::_40Mhz,
    )
}

/// Convert an ELF into an ESP image with espflash (`espflash save-image`).
pub fn from_elf(elf: &[u8]) -> crate::Result<EspImage> {
    let image = IdfBootloaderFormat::new(elf, &flash_data(), None, None, None, None)
        .map_err(crate::Error::external)?;
    let segment = image
        .ota_segments()
        .next()
        .ok_or("espflash produced no application segment")?;
    Ok(EspImage(segment.data().to_vec()))
}

/// One entry in the segment table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Offset of the 8-byte segment header within the image.
    pub header: usize,
    pub vaddr: u32,
    pub len: u32,
}

impl Segment {
    pub fn data_offset(&self) -> usize {
        self.header + SEGMENT_HEADER_LEN
    }
    pub fn contains(&self, range: (u32, u32)) -> bool {
        self.vaddr >= range.0 && self.vaddr < range.1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EspImage(pub Vec<u8>);

impl EspImage {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.0[off..off + 4].try_into().unwrap())
    }
    fn put_u32(&mut self, off: usize, v: u32) {
        self.0[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// The segment table as declared by the header (stops at the image end).
    pub fn segments(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut off = HEADER_LEN;
        for _ in 0..usize::from(self.0[1]) {
            if off + SEGMENT_HEADER_LEN > self.0.len() {
                break;
            }
            let s = Segment {
                header: off,
                vaddr: self.u32_at(off),
                len: self.u32_at(off + 4),
            };
            out.push(s);
            off = s.data_offset().saturating_add(s.len as usize);
        }
        out
    }

    /// ESP Secure Boot V2 requires the app checksum and SHA-256 digest to
    /// end on a 64 KiB MMU-page boundary before the signature sector. Add
    /// the same zero-filled dummy segment used by `esptool --secure-pad-v2`,
    /// updating the segment count, checksum and image SHA-256. Padding is
    /// part of the authenticated ESP image, not unverified trailing bytes.
    pub fn secure_pad_v2(self) -> crate::Result<Self> {
        ferriboot_tools_esp::image::secure_pad(&self.0)
            .map(Self)
            .map_err(crate::Error::external)
    }

    /// First segment whose load address lies in `range`.
    pub fn segment_in(&self, range: (u32, u32)) -> Segment {
        *self
            .segments()
            .iter()
            .find(|s| s.contains(range) && s.len > 0)
            .unwrap_or_else(|| panic!("no segment in {:#x}..{:#x}", range.0, range.1))
    }

    /// Move a segment's load address; its data stays where it is in flash.
    pub fn set_segment_vaddr(mut self, seg: Segment, vaddr: u32) -> Self {
        self.put_u32(seg.header, vaddr);
        self.fix_digest()
    }

    /// Recompute the XOR checksum and appended SHA-256 (ESP-IDF image format)
    /// if the segment table describes the image consistently.
    pub fn fix_digest(mut self) -> Self {
        let segments = self.segments();
        if segments.len() != usize::from(self.0[1]) {
            return self;
        }
        let end = segments
            .last()
            .map_or(HEADER_LEN, |s| s.data_offset() + s.len as usize);
        let checksum_at = (end + 1).next_multiple_of(16) - 1;
        let append_digest = self.0[23] == 1;
        let needed = checksum_at + 1 + if append_digest { 32 } else { 0 };
        if needed != self.0.len() {
            return self;
        }
        let mut checksum = CHECKSUM_MAGIC;
        for s in &segments {
            for b in &self.0[s.data_offset()..s.data_offset() + s.len as usize] {
                checksum ^= b;
            }
        }
        self.0[checksum_at] = checksum;
        if append_digest {
            let hash = Sha256::digest(&self.0[..checksum_at + 1]);
            self.0[checksum_at + 1..].copy_from_slice(&hash);
        }
        self
    }
}

#[cfg(test)]
mod secure_pad_tests {
    use super::*;

    fn image_with_data_len(len: usize) -> EspImage {
        assert_eq!(len % 4, 0);
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[0] = 0xe9; // ESP application image magic
        bytes[1] = 1; // one segment
        bytes[23] = 1; // append SHA-256
        bytes.extend_from_slice(&0x3fc8_0000u32.to_le_bytes());
        bytes.extend_from_slice(&(len as u32).to_le_bytes());
        bytes.resize(HEADER_LEN + SEGMENT_HEADER_LEN + len, 0xa5);
        let checksum_end = (bytes.len() + 1).next_multiple_of(16);
        bytes.resize(checksum_end + 32, 0);
        EspImage(bytes).fix_digest()
    }

    fn assert_padded(original: EspImage) {
        let old_data_end =
            original.segments()[0].data_offset() + original.segments()[0].len as usize;
        let padded = original.clone().secure_pad_v2().unwrap();
        assert_eq!(padded.bytes().len() % (64 * 1024), 0);
        assert_eq!(padded.bytes()[1], 2);
        let segments = padded.segments();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], original.segments()[0]);
        assert_eq!(segments[1].header, old_data_end);
        assert_eq!(segments[1].vaddr, 0);
        assert!(
            padded.bytes()[segments[1].data_offset()..][..segments[1].len as usize]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(
            &Sha256::digest(&padded.bytes()[..padded.bytes().len() - 32])[..],
            &padded.bytes()[padded.bytes().len() - 32..]
        );
        assert_eq!(padded.clone().fix_digest(), padded);
    }

    #[test]
    fn pads_image_to_mmu_page_and_refreshes_checksum_and_digest() {
        assert_padded(image_with_data_len(12));
        assert_padded(image_with_data_len(8192));
    }

    #[test]
    fn appends_a_dummy_segment_even_when_original_is_page_aligned() {
        // App end + checksum alignment + SHA256 = exactly one 64 KiB page.
        let original = image_with_data_len(65536 - (HEADER_LEN + SEGMENT_HEADER_LEN) - 48);
        assert_eq!(original.bytes().len(), 65536);
        assert_padded(original);
    }

    #[test]
    fn rejects_missing_sha256_and_truncated_segments() {
        let mut missing_hash = image_with_data_len(16);
        missing_hash.0[23] = 0;
        assert!(missing_hash.secure_pad_v2().is_err());
        let mut truncated = image_with_data_len(16);
        truncated.0.pop();
        assert!(truncated.secure_pad_v2().is_err());
    }
}
