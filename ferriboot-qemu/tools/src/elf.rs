//! AN385 ELF export with the SSRAM/boot-info memory contract checked first.
use anyhow::{Result, ensure};
use ferriboot_build::tools::{
    elf::{
        Image,
        abi::{EM_ARM, PT_LOAD},
        check_vectors, flatten, load_segments, parse,
    },
    word,
};
use ferriboot_layout_qemu as qemu;

/// Checked physical load bytes of an AN385 application or bootloader.
pub fn image(data: &[u8], bootloader: bool) -> Result<Image> {
    let file = parse(data, EM_ARM)?;
    let (origin, size) = if bootloader {
        (0, ferriboot_layout_qemu::BOOTLOADER_SIZE)
    } else {
        (
            qemu::APP_XIP_BASE + qemu::HEADER_SIZE,
            ferriboot_layout_qemu::SLOT_SIZE - qemu::HEADER_SIZE,
        )
    };
    let segments = load_segments(&file, origin, size)?;
    for p in file.segments().unwrap() {
        if p.p_type != PT_LOAD || p.p_filesz == 0 {
            continue;
        }
        ensure!(
            p.p_vaddr == p.p_paddr
                || (p.p_vaddr >= u64::from(qemu::RAM_BASE)
                    && p.p_vaddr + p.p_memsz <= u64::from(qemu::BOOT_INFO_ADDRESS)),
            "invalid SRAM data segment"
        );
    }
    let payload = flatten(&segments, origin)?;
    check_vectors(&payload, origin, qemu::RAM_BASE, qemu::BOOT_INFO_ADDRESS)?;
    ensure!(
        file.ehdr.e_entry == u64::from(word(&payload, 4)?),
        "ELF entry does not match reset vector"
    );
    Ok(Image { origin, payload })
}
