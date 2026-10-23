//! Chip memory maps, and detection of the target chip from an image's metadata (port of
//! picotool's `get_access_model` / `find_best_block`).

use super::error::{Result, bail};
use super::memory::MemoryView;
use super::picobin::{self, Block, Item, image_type as it};

pub const FLASH_START: u32 = 0x1000_0000;
pub const SRAM_START: u32 = 0x2000_0000;
pub const XIP_SRAM_START_RP2350: u32 = 0x13ff_c000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Rp2040,
    Rp2350,
    /// picotool's "generic" model: the union of the RP2040 and RP2350 address ranges.
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub chip: Chip,
}

impl Model {
    pub fn flash_start(&self) -> u32 {
        FLASH_START
    }
    pub fn flash_end(&self) -> u32 {
        if self.chip == Chip::Rp2040 {
            0x1100_0000
        } else {
            0x1200_0000
        }
    }
    pub fn sram_start(&self) -> u32 {
        SRAM_START
    }
    pub fn sram_striped_end(&self) -> u32 {
        if self.chip == Chip::Rp2040 {
            0x2004_0000
        } else {
            0x2008_0000
        }
    }
    pub fn sram_end(&self) -> u32 {
        if self.chip == Chip::Rp2040 {
            0x2004_2000
        } else {
            0x2008_2000
        }
    }
    pub fn xip_sram_start(&self) -> u32 {
        if self.chip == Chip::Rp2040 {
            0x1500_0000
        } else {
            XIP_SRAM_START_RP2350
        }
    }
    pub fn xip_sram_end(&self) -> u32 {
        match self.chip {
            Chip::Rp2040 | Chip::Generic => 0x1500_4000,
            Chip::Rp2350 => 0x1400_0000,
        }
    }
    pub fn chip_name(&self) -> &'static str {
        match self.chip {
            Chip::Rp2040 => "RP2040",
            Chip::Rp2350 => "RP2350",
            Chip::Generic => "RP-series",
        }
    }
}

/// Follow the block loop that starts in the first 4K of the image.
pub fn block_loop(view: &MemoryView) -> Result<Vec<Block>> {
    let start = view.binary_start();
    let words = picobin::lsb_bytes_to_words(&view.read(start, 0x1000));
    let Some(first) = picobin::find_first_block(&words, start)? else {
        return Ok(Vec::new());
    };
    let rest = picobin::get_all_blocks(view, &first)?;
    Ok(std::iter::once(first).chain(rest).collect())
}

/// The block the bootrom would pick: the last RP2350 Arm-secure / RISC-V IMAGE_DEF.
pub fn find_best_block(view: &MemoryView) -> Result<Option<Block>> {
    let mut blocks = block_loop(view)?.into_iter();
    let Some(mut best) = blocks.next() else {
        return Ok(None);
    };
    let mut has_arch = false;
    for blk in blocks {
        let Some(f) = blk.image_type_flags() else {
            continue;
        };
        if it::image_type(f) != it::EXE || it::chip(f) != it::CHIP_RP2350 {
            continue;
        }
        match it::cpu(f) {
            it::CPU_RISCV if !has_arch => best = blk,
            it::CPU_VARMULET if !has_arch => best = blk,
            it::CPU_ARM if it::security(f) == it::SECURITY_S => {
                best = blk;
                has_arch = true;
            }
            _ => {}
        }
    }
    Ok(Some(best))
}

/// Determine the target chip of an image.
pub fn detect_model(view: &MemoryView) -> Result<Model> {
    let chip = |chip| Ok(Model { chip });
    let Some(best) = find_best_block(view)? else {
        // The RP2350 bootloader always starts with a PICOBIN IMAGE_DEF. The
        // caller rejects a missing block; no RP2040 boot2 CRC probe is needed.
        return chip(Chip::Generic);
    };
    let Some(Item::ImageType { flags: f }) = best.items.first() else {
        return chip(Chip::Generic); // e.g. a partition table
    };
    match (
        it::image_type(*f),
        it::chip(*f),
        it::cpu(*f),
        it::security(*f),
    ) {
        (it::DATA, ..) => chip(Chip::Generic),
        (it::EXE, it::CHIP_RP2040, ..) => chip(Chip::Rp2040),
        (it::EXE, it::CHIP_RP2350, it::CPU_RISCV, _) => chip(Chip::Rp2350),
        (it::EXE, it::CHIP_RP2350, it::CPU_ARM, it::SECURITY_S | it::SECURITY_NS) => {
            chip(Chip::Rp2350)
        }
        _ => bail!(Incompatible, "Unsupported IMAGE_DEF flags {:04x}", f),
    }
}
