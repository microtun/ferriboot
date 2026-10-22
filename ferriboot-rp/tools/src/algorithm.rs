//! Package the SRAM fixture algorithm for probe-rs 0.32.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ferriboot_build::tools::elf::{
    self,
    abi::{EM_ARM, SHF_ALLOC, SHT_NOBITS},
};

pub const TARGET: &str = "thumbv8m.main-none-eabi";
pub fn generate(data: &[u8]) -> Result<String> {
    const LOAD_ADDRESS: u64 = 0x20000004;
    let file = elf::parse(data, EM_ARM)?;
    let end = elf::symbol(&file, "_end")?;
    ensure!(
        LOAD_ADDRESS < end && end < 0x20004000,
        "algorithm exceeds SRAM code/data budget"
    );
    let mut image = vec![0; (end - LOAD_ADDRESS) as usize];
    for (name, s) in elf::sections(&file)? {
        if s.sh_flags & u64::from(SHF_ALLOC) == 0 || s.sh_size == 0 {
            continue;
        }
        let start = s
            .sh_addr
            .checked_sub(LOAD_ADDRESS)
            .context("section below load address")? as usize;
        let end = start
            .checked_add(s.sh_size as usize)
            .context("section overflow")?;
        ensure!(end <= image.len(), "section {name} outside algorithm");
        if s.sh_type != SHT_NOBITS {
            image[start..end].copy_from_slice(file.section_data(&s)?.0);
        }
    }
    let mut entries = Vec::new();
    for name in ["Init", "UnInit", "ProgramPage", "EraseSector"] {
        let offset = elf::symbol(&file, name)?
            .checked_sub(LOAD_ADDRESS)
            .context("entry below load address")?;
        ensure!(
            offset & 1 == 1 && offset < image.len() as u64,
            "expected Thumb entry inside image"
        );
        entries.push(offset);
    }
    let data_offset = elf::symbol(&file, "_data_start")?
        .checked_sub(LOAD_ADDRESS)
        .context("data below load address")?;
    ensure!(
        data_offset <= image.len() as u64,
        "data section outside algorithm"
    );
    Ok(include_str!("algorithm.yaml")
        .replace("{LOAD_ADDRESS:x}", &format!("{LOAD_ADDRESS:x}"))
        .replace("{instructions}", &STANDARD.encode(image))
        .replace("{init:x}", &format!("{:x}", entries[0]))
        .replace("{uninit:x}", &format!("{:x}", entries[1]))
        .replace("{program:x}", &format!("{:x}", entries[2]))
        .replace("{erase:x}", &format!("{:x}", entries[3]))
        .replace("{data_offset:x}", &format!("{data_offset:x}")))
}
