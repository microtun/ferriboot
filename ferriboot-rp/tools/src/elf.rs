//! RP2350 ELF export with the application/bootloader memory contract checked first.
use crate::picobin;
use anyhow::{Context, Result, ensure};
use ferriboot_build::tools::{
    bytes,
    elf::{
        Image,
        abi::{EM_ARM, SHF_ALLOC, SHT_NOBITS},
        check_vectors, flatten, load_segments, parse, sections, symbol,
    },
    word,
};
use rp2350_layout as rp;

/// Checked physical load bytes of an RP2350 application or ROM-loaded bootloader.
pub fn image(data: &[u8], bootloader: bool) -> Result<Image> {
    let file = parse(data, EM_ARM)?;
    let (base, size, origin) = if bootloader {
        (rp::XIP_BASE, rp2350_layout::BOOTLOADER_SIZE, rp::XIP_BASE)
    } else {
        (
            rp::APP_XIP_BASE,
            rp2350_layout::MAX_IMAGE_SIZE,
            rp::APP_XIP_BASE + rp::HEADER_SIZE,
        )
    };
    let segments = load_segments(&file, base, size)?;
    let payload = flatten(&segments, origin)?;
    let sections = sections(&file)?;
    let handoff = sections
        .iter()
        .find(|(name, _)| *name == ".ferriboot_boot_info")
        .context("missing boot-info reservation")?;
    ensure!(
        handoff.1.sh_type == SHT_NOBITS
            && handoff.1.sh_addr == u64::from(rp::BOOT_INFO_ADDRESS)
            && handoff.1.sh_size == u64::from(rp::BOOT_INFO_SIZE)
            && handoff.1.sh_flags & u64::from(SHF_ALLOC) != 0,
        "invalid boot-info NOLOAD reservation"
    );
    for (name, s) in &sections {
        if s.sh_flags & u64::from(SHF_ALLOC) == 0
            || *name == ".ferriboot_boot_info"
            || s.sh_size == 0
        {
            continue;
        }
        ensure!(
            s.sh_addr >= u64::from(rp::BOOT_INFO_ADDRESS + rp::BOOT_INFO_SIZE)
                || s.sh_addr + s.sh_size <= u64::from(rp::BOOT_INFO_ADDRESS),
            "{name} overlaps boot info"
        );
        if bootloader && *name != ".start_block" {
            ensure!(
                s.sh_addr >= u64::from(rp::RAM_BASE)
                    && s.sh_addr + s.sh_size <= u64::from(rp::BOOT_INFO_ADDRESS),
                "bootloader section {name} must be in SRAM"
            );
        }
    }
    if bootloader {
        let start = file
            .section_header_by_name(".start_block")?
            .context("missing ROM metadata")?;
        ensure!(
            start.sh_addr == u64::from(origin) && start.sh_size == picobin::ROM_HEADER_SIZE as u64,
            "ROM metadata must be 512 bytes at flash base"
        );
        ensure!(
            file.section_data(&start)?.0 == bytes(&payload, 0, picobin::ROM_HEADER_SIZE)?,
            "ROM metadata differs from load bytes"
        );
        let def = picobin::linker_image_def(&payload, origin)?;
        ensure!(
            def.link == 0
                && u64::from(def.entry) == file.ehdr.e_entry
                && def.stack == rp::BOOT_INFO_ADDRESS,
            "invalid unsealed ROM entry/stack/link"
        );
        ensure!(
            def.source == origin + picobin::ROM_HEADER_SIZE as u32
                && def.destination == rp::RAM_BASE
                && def.vector == rp::RAM_BASE
                && def.size > 0
                && def.size.is_multiple_of(4)
                && u64::from(def.destination) + u64::from(def.size)
                    <= u64::from(rp::BOOT_INFO_ADDRESS)
                && u64::from(def.source) + u64::from(def.size)
                    == u64::from(origin) + payload.len() as u64,
            "ROM load map must copy the complete SRAM image"
        );
        let vectors = file
            .section_header_by_name(".vector_table")?
            .context("missing SRAM vectors")?;
        ensure!(
            vectors.sh_addr == u64::from(def.vector) && vectors.sh_size == (16 + 53) * 4,
            "bootloader must supply all 69 vectors"
        );
        let ram = &payload[picobin::ROM_HEADER_SIZE..];
        check_vectors(ram, def.destination, rp::RAM_BASE, rp::BOOT_INFO_ADDRESS)?;
        ensure!(
            word(ram, 0)? == rp::BOOT_INFO_ADDRESS && word(ram, 4)? == def.entry,
            "ROM entry/stack differs from vectors"
        );
        let mut executable_reset = false;
        for s in &segments {
            if s.storage == origin {
                ensure!(
                    s.runtime == origin
                        && s.data.len() == picobin::ROM_HEADER_SIZE
                        && !s.executable,
                    "only ROM metadata may stay in flash"
                );
            } else {
                ensure!(
                    s.storage >= def.source
                        && u64::from(s.storage) + s.data.len() as u64
                            <= u64::from(def.source) + u64::from(def.size)
                        && s.runtime == def.destination + s.storage - def.source,
                    "segment outside ROM load map"
                );
                executable_reset |= s.executable
                    && s.runtime <= def.entry & !1
                    && u64::from(def.entry & !1) < u64::from(s.runtime) + s.data.len() as u64;
            }
        }
        ensure!(executable_reset, "reset vector must be executable SRAM");
        ensure!(
            symbol(&file, "__sidata")? == symbol(&file, "__sdata")?,
            "ROM must initialize .data in SRAM"
        );
    } else {
        check_vectors(&payload, origin, rp::RAM_BASE, rp::BOOT_INFO_ADDRESS)?;
        ensure!(
            file.section_header_by_name(".start_block")?
                .is_none_or(|s| s.sh_size == 0),
            "application must enable imagedef-none"
        );
    }
    Ok(Image { origin, payload })
}
