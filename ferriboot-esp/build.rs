//! Generate the selected chip's constants and linker contract from
//! `boards/<board>/layout/src/board.rs`, publish the chip to dependent binaries and, for the
//! third-stage bootloader (`boot-runtime`), build the RAM handoff loader.
//!
//! Only chip facts are used here. Board decisions reach this crate as values
//! (a partition table read from flash), never at build time; the one
//! board-level build input is the partition-table offset that
//! esp-bootloader-esp-idf also reads from its esp-config variable.
use std::{env, fmt::Write as _};

use ferriboot_build::{
    chip_values, out_dir, publish_chip, require_target, selected_chip, write_out,
};

#[cfg(feature = "boot-runtime")]
mod loader {
    //! Package the standalone RAM handoff loader (`bl-loader`) for embedding.
    //! Compiled into build.rs only with the `boot-runtime` feature.
    use std::{env, fs, path::Path, process::Command};

    use elf::{
        ElfBytes,
        abi::{
            self, PF_W, PF_X, PT_LOAD, SHF_ALLOC, SHF_TLS, SHF_WRITE, SHT_NOBITS, SHT_REL,
            SHT_RELA, SHT_SYMTAB,
        },
        endian::AnyEndian,
        file::Class,
    };

    use ferriboot_build::Values;

    /// Only the linked, file-backed IRAM instructions (and any in-segment constants)
    /// may be installed. The handoff copies u32 words, so pad RVC's final halfword.
    fn extract_loader(image: &[u8], start: u32, capacity: u32) -> Result<Vec<u8>, String> {
        let file = ElfBytes::<AnyEndian>::minimal_parse(image)
            .map_err(|error| format!("invalid loader ELF: {error}"))?;
        if file.ehdr.class != Class::ELF32 || image.get(5) != Some(&abi::ELFDATA2LSB) {
            return Err("standalone loader must be a little-endian ELF32".into());
        }
        if file.ehdr.e_type != abi::ET_EXEC || file.ehdr.e_machine != abi::EM_RISCV {
            return Err("standalone loader must be RISC-V ET_EXEC (not PIE or relocatable)".into());
        }
        let entry = file.ehdr.e_entry;
        if entry != u64::from(start) {
            return Err(format!(
                "loader ELF entry {entry:#010x}, expected {start:#010x}; check linker script"
            ));
        }

        let segments = file.segments().ok_or("loader ELF has no program headers")?;
        let (Some(sections), Some(section_names)) = file
            .section_headers_with_strtab()
            .map_err(|error| format!("invalid loader section headers: {error}"))?
        else {
            return Err("loader ELF has no section headers/names".into());
        };

        let executable: Vec<_> = segments
            .iter()
            .filter(|segment| {
                segment.p_type == PT_LOAD
                    && segment.p_flags & PF_X != 0
                    && segment.p_vaddr <= entry
                    && segment
                        .p_vaddr
                        .checked_add(segment.p_filesz)
                        .is_some_and(|end| entry < end)
            })
            .collect();
        if executable.len() != 1 {
            return Err(format!(
                "expected exactly one executable PT_LOAD covering {entry:#x}; check linker script"
            ));
        }
        let code = &executable[0];
        if code.p_flags & PF_W != 0 {
            return Err("loader executable segment is writable".into());
        }
        if code.p_paddr != code.p_vaddr {
            return Err("loader physical and virtual addresses differ".into());
        }
        if code.p_memsz != code.p_filesz {
            return Err("loader executable segment contains zero-initialized data".into());
        }
        let end = code
            .p_vaddr
            .checked_add(code.p_filesz)
            .ok_or("loader segment address overflow")?;
        let size = end - entry;
        if size == 0 || size > u64::from(capacity) {
            return Err(format!(
                "loader payload exceeds protected IRAM: {entry:#x}..{end:#x} (capacity {capacity:#x})"
            ));
        }

        for section in sections.iter() {
            let name = section_names
                .get(section.sh_name as usize)
                .map_err(|error| format!("invalid loader section name: {error}"))?;
            if matches!(section.sh_type, SHT_REL | SHT_RELA) && section.sh_size != 0 {
                return Err(format!("loader has relocations: {name}"));
            }
            if section.sh_flags & u64::from(SHF_ALLOC) == 0 || section.sh_size == 0 {
                continue;
            }
            if section.sh_flags & u64::from(SHF_WRITE | SHF_TLS) != 0 {
                return Err(format!("writable/TLS allocated section: {name}"));
            }
            if section.sh_type == SHT_NOBITS {
                return Err(format!("uninitialized allocated section: {name}"));
            }
            if section.sh_addr < entry
                || section
                    .sh_addr
                    .checked_add(section.sh_size)
                    .is_none_or(|section_end| section_end > end)
            {
                return Err(format!(
                    "allocated section outside copied IRAM payload: {name} {:#x}+{:#x}",
                    section.sh_addr, section.sh_size
                ));
            }
        }

        // A valid executable at the right address isn't enough: require the
        // exported C-ABI handoff function, with no unresolved named symbols.
        let mut has_symtab = false;
        for section in sections
            .iter()
            .filter(|section| section.sh_type == SHT_SYMTAB)
        {
            has_symtab = true;
            if section.sh_entsize != 16
                || section.sh_size % section.sh_entsize != 0
                || usize::try_from(section.sh_link)
                    .ok()
                    .is_none_or(|index| index >= sections.len())
            {
                return Err("invalid ELF symbol table".into());
            }
        }
        if !has_symtab {
            return Err("missing exported bl_loader_entry function symbol".into());
        }
        let (symbols, strings) = file
            .symbol_table()
            .map_err(|error| format!("invalid loader symbol table: {error}"))?
            .ok_or("missing exported bl_loader_entry function symbol")?;
        let mut found_entry = false;
        for symbol in symbols.iter() {
            let name = strings
                .get(symbol.st_name as usize)
                .map_err(|error| format!("invalid loader symbol name: {error}"))?;
            if !name.is_empty() && symbol.st_shndx == abi::SHN_UNDEF {
                return Err(format!("unresolved ELF symbol: {name}"));
            }
            if name == "bl_loader_entry" {
                if symbol.st_value != entry
                    || symbol.st_shndx == abi::SHN_UNDEF
                    || usize::from(symbol.st_shndx) >= sections.len()
                {
                    return Err("bl_loader_entry is not at the ELF entry address".into());
                }
                if symbol.st_info >> 4 != abi::STB_GLOBAL || symbol.st_info & 0xf != abi::STT_FUNC {
                    return Err("bl_loader_entry must be a global function".into());
                }
                found_entry = true;
            }
        }
        if !found_entry {
            return Err("missing exported bl_loader_entry function symbol".into());
        }

        // PT_LOAD may begin before the entry and contain ELF headers. Never copy
        // those bytes: the actual payload starts at the entry's file offset.
        let offset = code
            .p_offset
            .checked_add(entry - code.p_vaddr)
            .ok_or("loader segment file offset overflow")?;
        let end_offset = offset
            .checked_add(size)
            .ok_or("loader segment file range overflow")?;
        let raw = image
            .get(
                usize::try_from(offset).map_err(|_| "invalid loader offset")?
                    ..usize::try_from(end_offset).map_err(|_| "invalid loader file range")?,
            )
            .ok_or("truncated executable PT_LOAD data")?;
        let padded = raw.len().checked_add(3).ok_or("loader length overflow")? & !3;
        if padded > capacity as usize {
            return Err("word alignment makes loader larger than IRAM reservation".into());
        }
        let mut payload = raw.to_vec();
        payload.resize(padded, 0);
        Ok(payload)
    }

    /// Build the bl-loader executable for `chip` in its own workspace, check and
    /// extract its IRAM payload and return the path of the raw payload.
    ///
    /// The nested Cargo uses a separate target directory under OUT_DIR, so it
    /// never waits on the outer build's lock. It inherits the outer build's
    /// environment, including the target's CARGO_ENCODED_RUSTFLAGS.
    pub fn build(chip: &str, values: &Values, out: &Path) -> std::path::PathBuf {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("bl-loader");
        for input in ["src", "ld", "build.rs", "Cargo.toml", "Cargo.lock"] {
            println!(
                "cargo:rerun-if-changed={}",
                manifest_dir.join(input).display()
            );
        }
        let target = env::var("TARGET").expect("TARGET");
        let iram = values["IDF_IRAM_LOADER_START"];
        let capacity = values["IRAM_HANDOFF_SIZE"];
        let target_dir = out.join("loader-target");
        let mut cargo = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cargo
            .arg("build")
            .arg("--release")
            .arg("--manifest-path")
            .arg(manifest_dir.join("Cargo.toml"))
            .arg("--bin")
            .arg("bl-loader")
            .arg("--target")
            .arg(&target)
            .arg("--target-dir")
            .arg(&target_dir)
            .current_dir(&manifest_dir)
            .env("FERRIBOOT_BL_LOADER_CHIP", chip)
            .env("FERRIBOOT_BL_LOADER_IRAM", format!("{iram:#x}"))
            .env("FERRIBOOT_BL_LOADER_SIZE", format!("{capacity:#x}"))
            .env(
                "FERRIBOOT_BL_LOADER_BOOT_INFO_ADDRESS",
                format!("{:#x}", values["BOOT_INFO_ADDRESS"]),
            )
            .env(
                "FERRIBOOT_BL_LOADER_BOOT_INFO_SIZE",
                format!("{:#x}", values["BOOT_INFO_SIZE"]),
            );
        if env::var_os("CARGO_FEATURE_BL_LOADER_DIAGNOSTICS").is_some() {
            cargo.arg("--features").arg("log");
        }
        assert!(
            cargo.status().expect("launch loader Cargo build").success(),
            "cannot build standalone bl-loader ELF"
        );
        let elf = target_dir.join(&target).join("release").join("bl-loader");
        let image =
            fs::read(&elf).unwrap_or_else(|error| panic!("read {}: {error}", elf.display()));
        let payload = extract_loader(&image, iram, capacity)
            .unwrap_or_else(|error| panic!("bl-loader packaging rejected: {error}"));
        let blob = out.join("bl_loader.bin");
        fs::write(&blob, &payload)
            .unwrap_or_else(|error| panic!("write {}: {error}", blob.display()));
        blob
    }
}

/// Supported chips, the Cargo feature selecting each and its target triple.
const CHIPS: &[(&str, &str)] = &[("esp32c3", "riscv32imc-unknown-none-elf")];

fn main() {
    let names: Vec<&str> = CHIPS.iter().map(|(chip, _)| *chip).collect();
    let chip = selected_chip("ferriboot-esp", &names);
    let target = CHIPS.iter().find(|(name, _)| *name == chip).unwrap().1;
    require_target("ferriboot-esp", chip, target);

    let values = chip_values(chip);
    publish_chip(chip);

    let mut rust = String::from("// @generated by ferriboot-esp/build.rs from layout/src/board.rs.\n");
    for (name, value) in &values {
        writeln!(rust, "pub const {name}: u32 = {value:#010x};").unwrap();
    }
    write_out("chip.rs", rust);

    // Bootstrap locator shared with esp-bootloader-esp-idf. The table cannot
    // supply its own address; every partition inside it is discovered at
    // runtime and checked against the flash's reported capacity.
    const OFFSET_ENV: &str = "ESP_BOOTLOADER_ESP_IDF_CONFIG_PARTITION_TABLE_OFFSET";
    println!("cargo:rerun-if-env-changed={OFFSET_ENV}");
    let offset = env::var(OFFSET_ENV)
        .unwrap_or_else(|_| panic!("{OFFSET_ENV} is not set; build from a board workspace"));
    let parsed: u32 = match offset
        .strip_prefix("0x")
        .or_else(|| offset.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => offset.parse(),
    }
    .unwrap_or_else(|_| panic!("invalid {OFFSET_ENV}: {offset}"));
    assert!(
        parsed > 0 && parsed.is_multiple_of(values["ERASE_SIZE"]),
        "{OFFSET_ENV} must be a nonzero erase-aligned address"
    );
    let end = parsed
        .checked_add(values["PARTITION_TABLE_SIZE"])
        .expect("partition table end overflows");
    write_out(
        "partition_table.rs",
        format!(
            "/// End of the configured partition-table sector (bootstrap locator).\n\
             pub const PARTITION_TABLE_END: u32 = {end:#010x};\n"
        ),
    );

    let script = include_str!("ld/ferriboot.x.in")
        .replace(
            "@BOOT_INFO_ADDRESS@",
            &format!("{:#010x}", values["BOOT_INFO_ADDRESS"]),
        )
        .replace(
            "@BOOT_INFO_SIZE@",
            &format!("{:#x}", values["BOOT_INFO_SIZE"]),
        );
    write_out("ferriboot.x", script);
    println!("cargo:rustc-link-search={}", out_dir().display());
    println!("cargo:rerun-if-changed=ld/ferriboot.x.in");

    #[cfg(feature = "boot-runtime")]
    {
        let blob = loader::build(chip, &values, &out_dir());
        println!("cargo:rustc-env=BL_LOADER_BIN={}", blob.display());
    }
}
