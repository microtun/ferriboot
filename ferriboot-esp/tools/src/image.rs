//! ESP application image parsing, SBv2 secure padding and slot inspection.
use anyhow::{Result, ensure};
use ferriboot_build::tools::{bytes, half, mcuboot, word};
use ferriboot_layout_esp32c3_supermini as chip;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

#[derive(Debug)]
pub struct Segment {
    pub address: u32,
    pub offset: usize,
    pub size: usize,
}
pub fn segments(image: &[u8]) -> Result<Vec<Segment>> {
    ensure!(
        image.len() >= 24 && image[0] == 0xe9 && (1..=16).contains(&image[1]),
        "invalid ESP image header/segment count"
    );
    let mut offset = 24;
    let mut out = Vec::new();
    for _ in 0..image[1] {
        let address = word(image, offset)?;
        let size = word(image, offset + 4)? as usize;
        offset += 8;
        bytes(image, offset, size)?;
        out.push(Segment {
            address,
            offset,
            size,
        });
        offset += size;
    }
    Ok(out)
}
fn checksum(image: &[u8], segments: &[Segment]) -> u8 {
    segments
        .iter()
        .flat_map(|s| &image[s.offset..s.offset + s.size])
        .fold(0xef, |xor, b| xor ^ b)
}
pub fn secure_pad(image: &[u8]) -> Result<Vec<u8>> {
    let parsed = segments(image)?;
    ensure!(
        image[23] == 1 && image[1] < 16,
        "SBv2 requires SHA256 and room for a padding segment"
    );
    let last = parsed.last().unwrap();
    let end = last.offset + last.size;
    let checksum_end = (end + 1).next_multiple_of(16);
    ensure!(
        image.len() == checksum_end + 32
            && image[checksum_end - 1] == checksum(image, &parsed)
            && image[checksum_end..] == Sha256::digest(&image[..checksum_end])[..],
        "invalid ESP checksum/digest boundary or value"
    );
    let base = end + 8;
    let padding = (65536 - (base + 16 + 32) % 65536) % 65536;
    let checksum_end = (base + padding + 1).next_multiple_of(16);
    ensure!(
        (checksum_end + 32).is_multiple_of(65536),
        "ESP secure-padding alignment failed"
    );
    let mut out = image[..end].to_vec();
    out[1] += 1;
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(padding as u32).to_le_bytes());
    out.resize(checksum_end + 32, 0);
    out[checksum_end - 1] = checksum(&out, &segments(&out)?);
    let digest = Sha256::digest(&out[..checksum_end]);
    out[checksum_end..].copy_from_slice(&digest);
    Ok(out)
}
fn within(address: u64, size: u64, low: u64, high: u64) -> bool {
    low <= address && address <= high && size <= high - address
}
pub fn inspect_slot(data: &[u8], slot: u32, geometry: mcuboot::Geometry) -> Result<()> {
    let signed = mcuboot::inspect(data, geometry, None)?;
    ensure!(
        signed.header_size == chip::HEADER_SIZE as usize && signed.flags == 0,
        "ESP MCUboot header/flags do not match policy"
    );
    let image = bytes(data, signed.header_size, signed.body_size)?;
    let parsed = segments(image)?;
    ensure!(half(image, 12)? == 5, "expected ESP32-C3 chip id 5");
    let entry = u64::from(word(image, 4)?);
    let page = u64::from(chip::MMU_PAGE_SIZE);
    let bus = 128 * page;
    let base = u64::from(slot) + signed.header_size as u64;
    let mut mappings = BTreeMap::new();
    let mut entry_segment = false;
    for s in parsed {
        let addr = u64::from(s.address);
        let len = s.size as u64;
        let flash = base + s.offset as u64;
        entry_segment |= addr <= entry && entry < addr + len;
        if addr < 65536 {
            continue;
        }
        if within(addr, len, 0x42000000, 0x42000000 + bus)
            || within(addr, len, 0x3c000000, 0x3c000000 + bus)
        {
            ensure!(
                addr % page == flash % page,
                "ESP mapped segment not congruent with flash mod 64 KiB"
            );
            let first = (addr & (bus - 1)) / page;
            for i in 0..(addr % page + len).div_ceil(page) {
                let mapped = flash / page + i;
                ensure!(
                    mappings
                        .insert(first + i, mapped)
                        .is_none_or(|old| old == mapped),
                    "ESP MMU entry needed for two flash pages"
                );
            }
        } else {
            let padded = (len + 3) & !3;
            let destination = if within(addr, padded, 0x40380000, 0x403e0000) {
                addr - u64::from(chip::IRAM_DRAM_OFFSET)
            } else if within(addr, padded, 0x3fc80000, 0x3fce0000)
                || within(
                    addr,
                    padded,
                    u64::from(chip::RTC_FAST_BASE),
                    u64::from(chip::RTC_FAST_BASE + chip::RTC_FAST_SIZE),
                )
            {
                addr
            } else {
                anyhow::bail!("ESP segment {addr:#x} outside known memory");
            };
            ensure!(
                destination.is_multiple_of(4)
                    && !(destination < 0x3fce0000
                        && destination + padded > u64::from(chip::PROTECTED_START)),
                "ESP segment reaches protected handoff SRAM"
            );
        }
    }
    ensure!(
        entry_segment
            && (within(entry, 2, 0x42000000, 0x42000000 + bus)
                || within(entry, 2, 0x40380000, 0x403e0000)),
        "ESP entry is outside executable segments"
    );
    Ok(())
}

pub fn convert(elf: &Path, secure: bool) -> Result<Vec<u8>> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("image.bin");
    ferriboot_build::tools::run(
        Command::new(ferriboot_build::tools::env("ESPFLASH", "espflash"))
            .args([
                "save-image",
                "--chip",
                "esp32c3",
                "--flash-mode",
                "dio",
                "--flash-freq",
                "80mhz",
                "--flash-size",
                "4mb",
            ])
            .arg(elf)
            .arg(&output),
    )?;
    let image = fs::read(output)?;
    if secure {
        secure_pad(&image)
    } else {
        Ok(image)
    }
}
