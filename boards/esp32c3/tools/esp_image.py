"""ESP32-C3 ROM application-image transformations.

`espflash save-image` converts ELF files to an ESP image, but espflash 4.6
has no released Secure Boot V2 padding option. The padding code below mirrors
the hardware-in-the-loop test implementation in
`testing/ferriboot-hil/src/esp32c3/esp_image.rs`.

`inspect_slot_image` pre-flights a signed application for a given slot.
Uses only the Python standard library.
"""

from dataclasses import dataclass, field
import hashlib
import struct

ESP_HEADER_SIZE = 24
SEGMENT_HEADER_SIZE = 8
MMU_PAGE_SIZE = 64 * 1024
MAX_SEGMENTS = 16


def _align_up(value: int, alignment: int) -> int:
    return (value + alignment - 1) // alignment * alignment


def _segments_end(image: bytes) -> int:
    """Validate the segment table and return the end of the segment data."""
    if len(image) < ESP_HEADER_SIZE or image[0] != 0xE9:
        raise ValueError("expected a complete ESP application image")
    count = image[1]
    if not 1 <= count <= MAX_SEGMENTS:
        raise ValueError("invalid ESP image segment count")
    pos = ESP_HEADER_SIZE
    for _ in range(count):
        if pos + SEGMENT_HEADER_SIZE > len(image):
            raise ValueError("truncated ESP image segment header")
        _addr, length = struct.unpack_from("<II", image, pos)
        pos += SEGMENT_HEADER_SIZE
        if length > len(image) - pos:
            raise ValueError("truncated ESP image segment data")
        pos += length
    return pos


def _checksum(image: bytes, data_end: int) -> int:
    result = 0xEF
    pos = ESP_HEADER_SIZE
    for _ in range(image[1]):
        _addr, length = struct.unpack_from("<II", image, pos)
        pos += SEGMENT_HEADER_SIZE
        for byte in image[pos : pos + length]:
            result ^= byte
        pos += length
    assert pos == data_end
    return result


def secure_pad_v2(image: bytes) -> bytes:
    """Append an authenticated zero segment, checksum and SHA256.

    The unsigned ESP image (including checksum and digest) must end exactly
    on a 64 KiB boundary *before* the 4 KiB SBv2 signature block is added.
    Reject malformed input instead of silently signing incorrect image bytes.
    """
    data_end = _segments_end(image)
    if image[23] != 1:
        raise ValueError("Secure Boot V2 requires an appended ESP SHA-256 digest")
    if image[1] >= MAX_SEGMENTS:
        raise ValueError("no room for the Secure Boot V2 padding segment")
    old_checksum_end = _align_up(data_end + 1, 16)
    if len(image) != old_checksum_end + 32:
        raise ValueError("malformed ESP checksum/digest boundary")
    if image[old_checksum_end - 1] != _checksum(image, data_end):
        raise ValueError("invalid ESP image checksum")
    if hashlib.sha256(image[:old_checksum_end]).digest() != image[old_checksum_end:]:
        raise ValueError("invalid ESP image SHA-256 digest")

    # Same zero-address dummy segment layout as esptool --secure-pad-v2 and
    # the HIL Rust implementation. Keep the padding inside the signed image.
    segment_data_start = data_end + SEGMENT_HEADER_SIZE
    padding_len = (
        MMU_PAGE_SIZE - (segment_data_start + 16 + 32) % MMU_PAGE_SIZE
    ) % MMU_PAGE_SIZE
    checksum_end = _align_up(segment_data_start + padding_len + 1, 16)
    if (checksum_end + 32) % MMU_PAGE_SIZE:
        raise ValueError("Secure Boot V2 alignment failed")
    padded = bytearray(image[:data_end])
    padded[1] += 1
    padded.extend(struct.pack("<II", 0, padding_len))
    padded.extend(b"\0" * padding_len)
    padded.extend(b"\0" * (checksum_end + 32 - len(padded)))
    padded[checksum_end - 1] = _checksum(padded, segment_data_start + padding_len)
    padded[checksum_end:] = hashlib.sha256(padded[:checksum_end]).digest()
    return bytes(padded)


# ----------------------------------------------------------- slot inspection
# ESP32-C3 memory map (TRM, "System and Memory"). MMU-mapped flash buses are
# 128 pages wide; IRAM is the instruction alias of the same SRAM as DRAM.
IMAGE_MAGIC = 0xE9
CHIP_ID_ESP32C3 = 5
MCUBOOT_MAGIC = 0x96F3B83D
MCUBOOT_HEADER_SIZE = 32
IROM_BASE = 0x4200_0000
DROM_BASE = 0x3C00_0000
IRAM = (0x4038_0000, 0x403E_0000)
DRAM = (0x3FC8_0000, 0x3FCE_0000)
BUS_PAGES = 128


def _within(address, length, region):
    low, high = region
    return low <= address <= high and length <= high - address


@dataclass(frozen=True)
class Segment:
    kind: str  # padding, IROM, DROM, IRAM, DRAM, RTC or "?" (unknown region)
    address: int
    length: int
    flash: int  # absolute flash offset of the segment data


@dataclass
class SlotImage:
    version: str
    header_size: int
    body_size: int
    flags: int
    segments: list = field(default_factory=list)
    notes: list = field(default_factory=list)
    errors: list = field(default_factory=list)

    def lines(self):
        yield (
            f"Ferriboot image {self.version}: header {self.header_size:#x},"
            f" body {self.body_size:#x}, flags {self.flags:#x}"
        )
        yield from (f"note: {note}" for note in self.notes)
        for s in self.segments:
            if s.kind != "?":
                yield f"  {s.kind:7} {s.address:#010x}  {s.length:#8x} bytes  flash {s.flash:#x}"
        yield from (f"ERROR: {error}" for error in self.errors)
        yield "FAILED" if self.errors else "OK"

    def require_valid(self):
        if self.errors:
            raise ValueError("invalid ESP image for the selected slot")


def inspect_slot_image(data, slot, platform):
    """Check an MCUboot-signed ESP application for execution from ``slot``.

    Malformed structure raises ValueError. Layout problems a user can fix by
    relinking or moving the slot are collected in ``errors``.
    """
    if len(data) < MCUBOOT_HEADER_SIZE:
        raise ValueError("truncated Ferriboot image")
    magic, _load, hdr_size, prot_size, img_size, flags = struct.unpack_from(
        "<IIHHII", data
    )
    if magic != MCUBOOT_MAGIC:
        raise ValueError("not a Ferriboot image")
    major, minor, revision, build = struct.unpack_from("<BBHI", data, 20)
    report = SlotImage(f"{major}.{minor}.{revision}+{build}", hdr_size, img_size, flags)
    if (
        hdr_size < MCUBOOT_HEADER_SIZE
        or img_size < ESP_HEADER_SIZE
        or hdr_size + img_size + prot_size > len(data)
    ):
        raise ValueError("truncated or invalid Ferriboot payload bounds")
    esp = data[hdr_size : hdr_size + img_size]
    if esp[0] != IMAGE_MAGIC:
        raise ValueError("payload is not an ESP image (magic 0xE9)")
    count = esp[1]
    (entry,) = struct.unpack_from("<I", esp, 4)
    (chip,) = struct.unpack_from("<H", esp, 12)
    if chip != CHIP_ID_ESP32C3:
        raise ValueError(
            f"unsupported ESP image chip id {chip} (expected ESP32-C3: {CHIP_ID_ESP32C3})"
        )
    if count > MAX_SEGMENTS:
        raise ValueError("ESP image has too many segments")

    page = platform["MMU_PAGE_SIZE"]
    bus = BUS_PAGES * page
    irom, drom = (IROM_BASE, IROM_BASE + bus), (DROM_BASE, DROM_BASE + bus)
    rtc = (
        platform["RTC_FAST_BASE"],
        platform["RTC_FAST_BASE"] + platform["RTC_FAST_SIZE"],
    )
    protected = platform["PROTECTED_START"]
    base = slot + hdr_size
    if base % page:
        report.notes.append(f"ESP image starts at {base:#x}, not 64 KiB aligned")

    offset, entry_in_segment, mmu = ESP_HEADER_SIZE, False, {}
    for _ in range(count):
        if offset + SEGMENT_HEADER_SIZE > len(esp):
            raise ValueError("truncated ESP segment header")
        address, length = struct.unpack_from("<II", esp, offset)
        if offset + SEGMENT_HEADER_SIZE + length > len(esp):
            raise ValueError("truncated ESP segment data")
        flash = base + offset + SEGMENT_HEADER_SIZE
        offset += SEGMENT_HEADER_SIZE + length
        entry_in_segment |= address <= entry < address + length
        if address < 0x1_0000:
            kind = "padding"
        elif _within(address, length, irom) or _within(address, length, drom):
            kind = "IROM" if _within(address, length, irom) else "DROM"
            _check_flash_mapping(report, kind, address, length, flash, page, bus, mmu)
        else:
            kind = _check_ram_segment(report, address, length, rtc, protected, platform)
        report.segments.append(Segment(kind, address, length, flash))

    executable = _within(entry, 2, irom) or _within(entry, 2, IRAM)
    if not entry_in_segment or not executable:
        report.errors.append(
            f"entry point {entry:#x} is not inside an executable segment"
        )
    return report


def _check_flash_mapping(report, kind, address, length, flash, page, bus, mmu):
    """Flash-mapped segments must be page-congruent and need consistent MMU pages."""
    if flash % page != address % page:
        report.errors.append(
            f"{kind} {address:#x}: flash {flash:#x} not congruent mod 64 KiB "
            f"(header size + slot offset must be a multiple of 0x10000)"
        )
    first = (address & (bus - 1)) // page
    pages = (address % page + length + page - 1) // page
    for i in range(pages):
        flash_page = flash // page + i
        if mmu.setdefault(first + i, flash_page) != flash_page:
            report.errors.append(f"MMU entry {first + i} needed for two flash pages")


def _check_ram_segment(report, address, length, rtc, protected, platform):
    """RAM segments must stay out of Ferriboot's protected handoff/stack."""
    padded = (length + 3) & ~3
    if _within(address, padded, IRAM):
        kind, destination = "IRAM", address - platform["IRAM_DRAM_OFFSET"]
    elif _within(address, padded, DRAM):
        kind, destination = "DRAM", address
    elif _within(address, padded, rtc):
        kind, destination = "RTC", address
    else:
        report.errors.append(
            f"segment {address:#x} is outside every known memory region"
        )
        return "?"
    if (
        destination % 4
        or destination < protected < destination + padded
        or protected <= destination < DRAM[1]
    ):
        report.errors.append(
            f"{kind} segment {address:#x}+{length:#x} reaches Ferriboot's"
            " protected normal-IRAM handoff/stack"
        )
    return kind
