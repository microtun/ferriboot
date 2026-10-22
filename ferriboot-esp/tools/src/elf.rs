//! ESP32-C3 Ferriboot ELF checks before ROM image conversion.
use anyhow::{Context, Result, ensure};
use esp32c3_layout as esp;
use ferriboot_build::tools::elf::{
    abi::{EM_RISCV, PT_LOAD},
    parse, sections,
};

pub fn check_bootloader(data: &[u8]) -> Result<()> {
    let file = parse(data, EM_RISCV)?;
    for (name, _) in sections(&file)? {
        ensure!(name != ".bl_loader", "obsolete .bl_loader section");
    }
    let segments = file.segments().context("missing ESP program headers")?;
    ensure!(!segments.is_empty(), "missing ESP program headers");
    for p in segments {
        if p.p_type != PT_LOAD {
            continue;
        }
        ensure!(p.p_filesz <= p.p_memsz, "invalid ESP load sizes");
        file.segment_data(&p)?;
        let mut addr = p.p_vaddr;
        if (0x40380000..0x403e0000).contains(&addr) {
            addr -= u64::from(esp::IRAM_DRAM_OFFSET);
        }
        if addr < u64::from(esp::ROM_DATA_START)
            && u64::from(esp::PROTECTED_START) < addr + p.p_memsz
        {
            ensure!(
                p.p_filesz == 0
                    && addr == u64::from(esp::BOOT_INFO_ADDRESS)
                    && p.p_memsz == u64::from(esp::BOOT_INFO_SIZE),
                "ESP load overlaps protected SRAM"
            );
        }
    }
    Ok(())
}
