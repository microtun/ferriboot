"""RP2350 ELF to flash-image conversion and Ferriboot's linking contract.

Applications run in place from the XIP window behind an MCUboot header.
The bootloader is copied by the ROM from flash into SRAM: its ELF has SRAM
VMAs and flash LMAs, so the flash bytes are assembled from the LMAs here
(``objcopy`` by VMA would be wrong).

Requires pyelftools.
"""

from dataclasses import dataclass
import struct

from elftools.common.exceptions import ELFError
from elftools.elf.constants import P_FLAGS, SH_FLAGS
from elftools.elf.elffile import ELFFile

from layout import max_image_size
import picobin

#: 16 Cortex-M system vectors and 53 RP2350 interrupts (``bootloader.x``
#: asserts the same size).
BOOTLOADER_VECTOR_BYTES = (16 + 53) * 4
BOOT_INFO_SECTION = ".ferriboot_boot_info"
START_BLOCK_SECTION = ".start_block"


@dataclass(frozen=True)
class FlashImage:
    origin: int  # flash (storage) address of payload[0]
    payload: bytes


@dataclass(frozen=True)
class LoadSegment:
    address: int  # LMA: where the bytes are stored in flash
    runtime: int  # VMA
    data: bytes
    executable: bool


def load(path, g, *, bootloader=False):
    """Check an application or bootloader ELF and return its flash bytes."""
    try:
        with open(path, "rb") as stream:
            return _load(ELFFile(stream), g, bootloader)
    except ELFError as error:
        raise ValueError(f"invalid ELF {path}: {error}") from None


def _load(elf, g, bootloader):
    if elf.elfclass != 32 or not elf.little_endian or elf["e_machine"] != "EM_ARM":
        raise ValueError("expected a little-endian 32-bit Arm RP2350 ELF")
    if bootloader:
        flash_start, capacity = g["XIP_BASE"], g["BOOTLOADER_SIZE"]
        expected_origin, limit = flash_start, capacity
    else:
        flash_start, capacity = g["APP_XIP_BASE"], max_image_size(g)
        expected_origin = flash_start + g["HEADER_SIZE"]
        limit = capacity - g["HEADER_SIZE"]
    segments = _flash_segments(elf, flash_start, flash_start + capacity)
    origin = segments[0].address
    if origin != expected_origin:
        raise ValueError(
            "bootloader must be linked at flash base"
            if bootloader
            else "application must be linked with the Ferriboot RP2350 memory layout"
        )
    payload = _flatten(segments, origin, limit)
    if bootloader:
        _check_bootloader(elf, segments, origin, payload, g)
    else:
        check_vectors(payload, origin, g)
        # Apps omit the ROM image definition so the ROM cannot bypass
        # Ferriboot by discovering an application directly.
        start_block = elf.get_section_by_name(START_BLOCK_SECTION)
        if start_block is not None and start_block["sh_size"]:
            raise ValueError("application must enable embassy-rp imagedef-none")
    _check_boot_info_reservation(elf, g)
    return FlashImage(origin, payload)


def _flash_segments(elf, start, end):
    """Initialized PT_LOAD segments, sorted by flash address."""
    segments = []
    for segment in elf.iter_segments():
        if segment["p_type"] != "PT_LOAD" or not segment["p_filesz"]:
            continue
        data = segment.data()
        if len(data) != segment["p_filesz"]:
            raise ValueError("truncated ELF load segment")
        address = segment["p_paddr"]
        if not start <= address < end or address + len(data) > end:
            raise ValueError(
                "initialized load segments must fit the flash storage region"
            )
        executable = bool(segment["p_flags"] & P_FLAGS.PF_X)
        segments.append(LoadSegment(address, segment["p_vaddr"], data, executable))
    if not segments:
        raise ValueError("ELF contains no flash load segments")
    return sorted(segments, key=lambda segment: segment.address)


def _flatten(segments, origin, limit):
    end = max(segment.address + len(segment.data) for segment in segments)
    if end - origin > limit:
        raise ValueError("ELF extends past usable flash region")
    payload = bytearray(b"\xff" * (end - origin))
    previous_end = origin
    for segment in segments:
        if segment.address < previous_end:
            raise ValueError("overlapping ELF load segments")
        offset = segment.address - origin
        payload[offset : offset + len(segment.data)] = segment.data
        previous_end = segment.address + len(segment.data)
    return bytes(payload)


def check_vectors(payload, origin, g):
    """The initial stack and Thumb reset handler of a vector table at ``origin``."""
    if len(payload) < 8:
        raise ValueError("payload lacks a vector table")
    sp, reset = struct.unpack_from("<II", payload)
    if sp % 8 or not g["RAM_BASE"] < sp <= g["BOOT_INFO_ADDRESS"]:
        raise ValueError(
            "initial stack overlaps reserved boot info or is outside usable SRAM"
        )
    if not reset & 1 or not origin + 8 <= (reset & ~1) < origin + len(payload):
        raise ValueError("reset handler is not Thumb code inside the runtime payload")


def _allocated(section):
    return bool(section["sh_flags"] & SH_FLAGS.SHF_ALLOC)


def _check_boot_info_reservation(elf, g):
    """The handoff region is reserved (NOLOAD) and nothing else is placed in it."""
    start, size = g["BOOT_INFO_ADDRESS"], g["BOOT_INFO_SIZE"]
    handoff = elf.get_section_by_name(BOOT_INFO_SECTION)
    if (
        handoff is None
        or handoff["sh_type"] != "SHT_NOBITS"
        or handoff["sh_addr"] != start
        or handoff["sh_size"] != size
        or not _allocated(handoff)
    ):
        raise ValueError(
            f"ELF must reserve the {size}-byte NOLOAD Ferriboot boot-info region"
        )
    for section in elf.iter_sections():
        if section.name == BOOT_INFO_SECTION or not _allocated(section):
            continue
        address = section["sh_addr"]
        if (
            g["RAM_BASE"] <= address < start + size
            and address + section["sh_size"] > start
        ):
            raise ValueError(
                f"allocated SRAM section {section.name} overlaps reserved boot info"
            )


# ------------------------------------------------------------------ bootloader
def _check_bootloader(elf, segments, origin, payload, g):
    """The ROM must copy the complete bootloader into SRAM and run it there."""
    image_def = _check_rom_metadata(elf, origin, payload, g)
    _check_load_map(image_def, origin, payload, g)
    _check_sram_vectors(elf, payload, image_def, g)
    _check_load_segments(segments, origin, image_def)
    _check_sections_in_sram(elf, g)
    symbols = elf.get_section_by_name(".symtab")
    sidata = symbols.get_symbol_by_name("__sidata") if symbols else None
    sdata = symbols.get_symbol_by_name("__sdata") if symbols else None
    if not sidata or not sdata or sidata[0]["st_value"] != sdata[0]["st_value"]:
        raise ValueError("startup .data must already be initialized in SRAM by ROM")


def _check_rom_metadata(elf, origin, payload, g):
    block = elf.get_section_by_name(START_BLOCK_SECTION)
    header = picobin.ROM_HEADER_SIZE
    if (
        block is None
        or block["sh_addr"] != origin
        or block["sh_size"] != header
        or len(payload) < header
    ):
        raise ValueError(
            f"RAM bootloader needs its {header}-byte ROM metadata at flash base"
        )
    if block.data() != payload[:header]:
        raise ValueError("ROM metadata section must match the exported flash bytes")
    image_def = picobin.read_linker_image_def(payload, origin)
    if image_def.link != 0:
        raise ValueError("unsealed ROM metadata must be a self-contained block loop")
    if image_def.entry != elf["e_entry"]:
        raise ValueError("ROM entry point must match the ELF entry")
    if image_def.stack != g["BOOT_INFO_ADDRESS"]:
        raise ValueError("ROM initial stack must be the top of usable SRAM")
    return image_def


def _check_load_map(image_def, origin, payload, g):
    source, size = image_def.load_source, image_def.load_size
    destination = image_def.load_destination
    if source != origin + picobin.ROM_HEADER_SIZE:
        raise ValueError("ROM load map must copy from directly after the ROM metadata")
    if destination != g["RAM_BASE"]:
        raise ValueError("ROM load map must copy to the start of SRAM")
    if image_def.vector_table != destination:
        raise ValueError("ROM vector table must start the SRAM image")
    if not size or size % 4:
        raise ValueError("ROM load map size must be a nonzero multiple of 4")
    if destination + size > g["BOOT_INFO_ADDRESS"]:
        raise ValueError("ROM load map overlaps reserved boot info")
    if source + size != origin + len(payload):
        raise ValueError("ROM load map must cover the complete initialized SRAM image")


def _check_sram_vectors(elf, payload, image_def, g):
    sram = payload[picobin.ROM_HEADER_SIZE :]
    check_vectors(sram, image_def.load_destination, g)
    vectors = elf.get_section_by_name(".vector_table")
    if (
        vectors is None
        or vectors["sh_addr"] != image_def.vector_table
        or vectors["sh_size"] != BOOTLOADER_VECTOR_BYTES
    ):
        raise ValueError(
            f"bootloader must provide all {BOOTLOADER_VECTOR_BYTES // 4} SRAM vector entries"
        )
    sp, reset = struct.unpack_from("<II", sram)
    if sp != g["BOOT_INFO_ADDRESS"]:
        raise ValueError("bootloader initial stack must use the top of usable SRAM")
    if elf["e_entry"] != reset:
        raise ValueError("ELF entry must match the SRAM Thumb reset vector")


def _check_load_segments(segments, origin, image_def):
    """Only the metadata stays in flash; everything else maps into the copy."""
    source, size = image_def.load_source, image_def.load_size
    destination = image_def.load_destination
    reset = image_def.entry & ~1
    reset_is_executable = False
    for segment in segments:
        length = len(segment.data)
        if segment.address == origin:
            if (
                segment.runtime != origin
                or length != picobin.ROM_HEADER_SIZE
                or segment.executable
            ):
                raise ValueError("only non-executable ROM metadata may reside in flash")
        elif (
            segment.address < source
            or segment.address + length > source + size
            or segment.runtime != destination + segment.address - source
        ):
            raise ValueError("initialized ELF segment is outside the ROM SRAM load map")
        elif segment.executable and segment.runtime <= reset < segment.runtime + length:
            reset_is_executable = True
    if not reset_is_executable:
        raise ValueError("reset vector must point into executable SRAM")


def _check_sections_in_sram(elf, g):
    for section in elf.iter_sections():
        if not _allocated(section) or not section["sh_size"]:
            continue
        if section.name in (START_BLOCK_SECTION, BOOT_INFO_SECTION):
            continue
        start = section["sh_addr"]
        if (
            not g["RAM_BASE"]
            <= start
            <= start + section["sh_size"]
            <= g["BOOT_INFO_ADDRESS"]
        ):
            raise ValueError(
                f"bootloader section {section.name} is not in SRAM: all code,"
                " constants and data must run from SRAM"
            )
