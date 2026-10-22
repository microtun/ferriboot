"""Check the ESP32-C3 Ferriboot ELF against the protected handoff SRAM.

The RAM handoff loader and boot info live in protected SRAM below the ROM's
data. Ferriboot itself may only place the zero-initialized boot-info
reservation there.

Requires pyelftools.
"""

import io
from pathlib import Path
import struct

from elftools.common.exceptions import ELFError
from elftools.elf.elffile import ELFFile

from esp_image import IRAM

#: The RAM handoff loader used to be linked into Ferriboot as this section.
OBSOLETE_LOADER_SECTION = b".bl_loader"
ELF32_PHDR_SIZE = 32
ELF32_SHDR_SIZE = 40


def check_ferriboot_elf(path, platform):
    """Reject load segments that reach protected SRAM (except boot info)."""
    blob = Path(path).read_bytes()
    try:
        _check(ELFFile(io.BytesIO(blob)), len(blob), platform)
    except (ELFError, struct.error) as error:
        raise ValueError(f"invalid Ferriboot ELF: {error}") from None


def _check(elf, file_size, platform):
    if elf.elfclass != 32 or not elf.little_endian:
        raise ValueError("expected a little-endian ELF32 Ferriboot executable")
    if (
        elf["e_machine"] != "EM_RISCV"
        or not elf.num_segments()
        or elf["e_phentsize"] < ELF32_PHDR_SIZE
    ):
        raise ValueError("expected a RISC-V ELF32 with program headers")
    if OBSOLETE_LOADER_SECTION in _section_names(elf):
        raise ValueError(
            "rejected: old directly-loaded .bl_loader section still present"
        )

    protected = (platform["PROTECTED_START"], platform["ROM_DATA_START"])
    boot_info = (platform["BOOT_INFO_ADDRESS"], platform["BOOT_INFO_SIZE"])
    for segment in elf.iter_segments():
        if segment["p_type"] != "PT_LOAD":
            continue
        address, file_bytes, memory_bytes = (
            segment["p_vaddr"],
            segment["p_filesz"],
            segment["p_memsz"],
        )
        if file_bytes > memory_bytes or segment["p_offset"] + file_bytes > file_size:
            raise ValueError("invalid Ferriboot PT_LOAD bounds")
        # IRAM is an alias of the same SRAM as DRAM.
        if IRAM[0] <= address < IRAM[1]:
            address -= platform["IRAM_DRAM_OFFSET"]
        if address < protected[1] and protected[0] < address + memory_bytes:
            if file_bytes == 0 and (address, memory_bytes) == boot_info:
                continue
            raise ValueError(
                f"rejected: Ferriboot LOAD {segment['p_vaddr']:#x} overlaps protected SRAM"
            )


def _section_names(elf):
    """Every section's name, failing closed if any cannot be read.

    pyelftools tolerates a damaged name table; a name that cannot be read
    could hide the obsolete loader section, so reject it here.
    """
    count = elf.num_sections()
    if not count:
        return []
    if elf["e_shentsize"] < ELF32_SHDR_SIZE or elf["e_shstrndx"] >= count:
        raise ValueError("invalid Ferriboot ELF section table")
    table = elf.get_section(elf["e_shstrndx"])
    strings = table.data()
    if len(strings) != table["sh_size"]:
        raise ValueError("truncated ELF section names")
    names = []
    for index in range(count):
        start = elf.get_section(index)["sh_name"]
        end = strings.find(b"\0", start)
        if start >= len(strings) or end < 0:
            raise ValueError("invalid ELF section name")
        names.append(strings[start:end])
    return names
