//! Generic ELF32 physical-load export. Board crates check their own memory
//! contract on top of these helpers.
use crate::word;
use ::elf::{ElfBytes, abi::*, endian::LittleEndian, file::Class, section::SectionHeader};
use anyhow::{Context, Result, ensure};

/// Re-exported so board crates can name ELF constants without their own `elf` dependency.
pub use ::elf::abi;

pub struct Image {
    pub origin: u32,
    pub payload: Vec<u8>,
}
pub struct Segment<'a> {
    pub storage: u32,
    pub runtime: u32,
    pub executable: bool,
    pub data: &'a [u8],
}
pub type File<'a> = ElfBytes<'a, LittleEndian>;

pub fn parse(data: &[u8], machine: u16) -> Result<File<'_>> {
    let file = File::minimal_parse(data)?;
    ensure!(
        file.ehdr.class == Class::ELF32
            && data.get(5) == Some(&ELFDATA2LSB)
            && file.ehdr.e_type == ET_EXEC
            && file.ehdr.e_machine == machine,
        "expected little-endian ELF32 executable for machine {machine}"
    );
    // Read every name, including unused sections, to fail closed on bad tables.
    if let (Some(sections), Some(names)) = file.section_headers_with_strtab()? {
        for section in sections {
            names.get(section.sh_name as usize)?;
        }
    } else {
        ensure!(file.ehdr.e_shnum == 0, "invalid ELF section name table");
    }
    Ok(file)
}

pub fn sections<'a>(file: &File<'a>) -> Result<Vec<(&'a str, SectionHeader)>> {
    let (sections, names) = file.section_headers_with_strtab()?;
    let sections = sections.context("ELF requires section headers")?;
    let names = names.context("ELF requires section names")?;
    sections
        .iter()
        .map(|s| Ok((names.get(s.sh_name as usize)?, s)))
        .collect()
}

pub fn symbol(file: &File<'_>, name: &str) -> Result<u64> {
    let (symbols, names) = file.symbol_table()?.context("missing ELF symbols")?;
    for symbol in symbols {
        if names.get(symbol.st_name as usize)? == name {
            return Ok(symbol.st_value);
        }
    }
    anyhow::bail!("missing ELF symbol {name}")
}

/// Initialized `PT_LOAD` segments, which must all lie in `base..base + capacity`.
pub fn load_segments<'a>(file: &File<'a>, base: u32, capacity: u32) -> Result<Vec<Segment<'a>>> {
    let mut segments = Vec::new();
    for p in file.segments().context("missing program headers")? {
        if p.p_type != PT_LOAD || p.p_filesz == 0 {
            continue;
        }
        ensure!(p.p_filesz <= p.p_memsz, "ELF filesz exceeds memsz");
        let data = file.segment_data(&p)?;
        ensure!(
            p.p_paddr >= u64::from(base)
                && p.p_paddr + p.p_filesz <= u64::from(base) + u64::from(capacity),
            "ELF load segment outside storage region"
        );
        segments.push(Segment {
            storage: p.p_paddr.try_into()?,
            runtime: p.p_vaddr.try_into()?,
            executable: p.p_flags & PF_X != 0,
            data,
        });
    }
    ensure!(
        !segments.is_empty(),
        "ELF contains no initialized load segments"
    );
    segments.sort_by_key(|s| s.storage);
    Ok(segments)
}

/// Concatenate sorted segments starting at `origin`, filling gaps with 0xff.
pub fn flatten(segments: &[Segment<'_>], origin: u32) -> Result<Vec<u8>> {
    ensure!(
        segments[0].storage == origin,
        "ELF is linked at the wrong origin (expected {origin:#x})"
    );
    let mut payload = Vec::new();
    for s in segments {
        let offset = (s.storage - origin) as usize;
        ensure!(
            offset >= payload.len(),
            "overlapping physical load segments"
        );
        payload.resize(offset, 0xff);
        payload.extend_from_slice(s.data);
    }
    Ok(payload)
}

pub fn check_vectors(data: &[u8], origin: u32, ram: u32, ram_end: u32) -> Result<()> {
    let (sp, reset) = (word(data, 0)?, word(data, 4)?);
    ensure!(
        sp.is_multiple_of(8) && ram < sp && sp <= ram_end,
        "invalid stack or overlap with boot info"
    );
    ensure!(
        reset & 1 == 1
            && u64::from(origin) + 8 <= u64::from(reset & !1)
            && u64::from(reset & !1) < u64::from(origin) + data.len() as u64,
        "reset handler must be Thumb code inside payload"
    );
    Ok(())
}
