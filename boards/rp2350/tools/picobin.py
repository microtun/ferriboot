"""RP2350 PICOBIN metadata blocks (datasheet §5.9, pico-sdk ``boot/picobin.h``).

Ferriboot's linker script (``boards/rp2350/ferriboot/ld/bootloader.x``) emits
one fixed IMAGE_DEF block at the start of flash. ``picotool seal --sign``
re-links it to a second, signed block that it appends. On the device,
``boards/rp2350/ferriboot/src/otp_provision.rs`` walks the same two-block
loop to find the ROM signing key; keep the two parsers in step.

Uses only the Python standard library.
"""

from dataclasses import dataclass
import struct

BLOCK_START = 0xFFFFDED3
BLOCK_END = 0xAB123579
#: The ROM reads at most this many bytes of one block.
MAX_BLOCK_BYTES = 0x180
#: Flash reserved by the linker for the first block; the SRAM image follows.
ROM_HEADER_SIZE = 512

ITEM_VECTOR_TABLE = 0x03
ITEM_LOAD_MAP = 0x06
ITEM_SIGNATURE = 0x09
ITEM_IMAGE_TYPE = 0x42
ITEM_ENTRY_POINT = 0x44
ITEM_HASH_DEF = 0x47
ITEM_LAST = 0xFF

#: IMAGE_TYPE item for an Arm Secure RP2350 executable.
IMAGE_TYPE_ARM_SECURE = 0x10210142
#: picotool's appended block sets EXTRA_SECURITY (bit 11 of the image-type
#: flags in the upper halfword). The linker's first block does not.
IMAGE_TYPE_ARM_SECURE_SIGNED = IMAGE_TYPE_ARM_SECURE | (0x0800 << 16)

SIGNATURE_SECP256K1 = 1
HASH_SHA256 = 1
#: SIGNATURE item: header, 64-byte public key, 64-byte signature.
SIGNATURE_ITEM_WORDS = 1 + 16 + 16


def item_header(kind, words, high=0):
    """Header word of an item whose size fits in one byte."""
    return high << 24 | words << 8 | kind


#: The linker's first block, item by item (kind, exact header word).
LINKER_ITEMS = (
    (ITEM_IMAGE_TYPE, IMAGE_TYPE_ARM_SECURE),
    (ITEM_VECTOR_TABLE, item_header(ITEM_VECTOR_TABLE, 2)),
    (ITEM_ENTRY_POINT, item_header(ITEM_ENTRY_POINT, 3)),
    # Relative (bit 31 clear) load map with one entry.
    (ITEM_LOAD_MAP, item_header(ITEM_LOAD_MAP, 4, high=1)),
)
#: Byte offset of the LAST item in the linker's first block.
LINKER_LAST_OFFSET = 4 + 4 * (1 + 2 + 3 + 4)
LINKER_LAST = item_header(ITEM_LAST, (LINKER_LAST_OFFSET - 4) // 4)


@dataclass(frozen=True)
class Item:
    offset: int  # byte offset of the header word
    header: int

    @property
    def kind(self):
        return self.header & 0xFF

    @property
    def words(self):
        """Size in words, including the header."""
        return (self.header >> 8) & (0xFFFF if self.kind & 0x80 else 0xFF)


@dataclass(frozen=True)
class Block:
    offset: int
    items: tuple
    last: Item
    link: int  # signed byte offset of the next block, relative to this one


def _word(data, offset):
    if offset % 4 or not 0 <= offset <= len(data) - 4:
        raise ValueError(f"invalid PICOBIN metadata address {offset:#x}")
    return struct.unpack_from("<I", data, offset)[0]


def parse_block(data, offset):
    """Parse the item structure of the block at ``offset``; no item semantics."""
    if _word(data, offset) != BLOCK_START:
        raise ValueError(f"missing PICOBIN block header at {offset:#x}")
    end = min(offset + MAX_BLOCK_BYTES, len(data))
    items = []
    at = offset + 4
    while at + 12 <= end:
        item = Item(at, _word(data, at))
        if item.kind == ITEM_LAST:
            if (item.header >> 8) & 0xFFFF != (at - offset - 4) // 4:
                raise ValueError("PICOBIN LAST item size does not match the block")
            if _word(data, at + 8) != BLOCK_END:
                raise ValueError("missing PICOBIN block footer")
            link = struct.unpack_from("<i", data, at + 4)[0]
            return Block(offset, tuple(items), item, link)
        if not item.words or at + 4 * item.words + 12 > end:
            raise ValueError(f"invalid PICOBIN item size at {at:#x}")
        items.append(item)
        at += 4 * item.words
    raise ValueError("unterminated PICOBIN metadata block")


# --------------------------------------------------------------- linker block
@dataclass(frozen=True)
class LinkerImageDef:
    """Ferriboot's linker-built first block, decoded."""

    vector_table: int
    entry: int
    stack: int
    load_source: int  # absolute flash address of the copied image
    load_destination: int
    load_size: int
    link: int


def read_linker_image_def(data, origin):
    """Decode the first block of a bootloader linked at flash ``origin``.

    Requires exactly the items ``bootloader.x`` emits, in order.
    """
    try:
        block = parse_block(data, 0)
    except ValueError as error:
        raise ValueError(f"bootloader ROM metadata: {error}") from None
    found = [(item.kind, item.header) for item in block.items]
    if found != list(LINKER_ITEMS) or block.last.header != LINKER_LAST:
        raise ValueError(
            "bootloader ROM metadata must be exactly IMAGE_TYPE (Arm Secure),"
            " VECTOR_TABLE, ENTRY_POINT and one relative LOAD_MAP entry; found "
            + ", ".join(f"{header:#010x}" for _, header in found)
            + f", LAST {block.last.header:#010x}"
        )
    vector, entry, load_map = (item.offset + 4 for item in block.items[1:])
    (vector_table,) = struct.unpack_from("<I", data, vector)
    entry_point, stack = struct.unpack_from("<II", data, entry)
    relative, destination, size = struct.unpack_from("<iII", data, load_map)
    load_map_address = origin + block.items[3].offset
    return LinkerImageDef(
        vector_table=vector_table,
        entry=entry_point,
        stack=stack,
        load_source=load_map_address + relative,
        load_destination=destination,
        load_size=size,
        link=block.link,
    )


# --------------------------------------------------------------- signed loop
@dataclass(frozen=True)
class SecurityItems:
    block: Block
    arm_secure: bool
    sha256: bool
    public_key: bytes | None


def read_security_items(data, offset, *, signed):
    """Parse a block and check its IMAGE_TYPE, HASH_DEF and SIGNATURE items."""
    block = parse_block(data, offset)
    expected_type = IMAGE_TYPE_ARM_SECURE_SIGNED if signed else IMAGE_TYPE_ARM_SECURE
    arm_secure = sha256 = False
    key = None
    for item in block.items:
        if item.kind == ITEM_IMAGE_TYPE:
            if item.header != expected_type:
                raise ValueError(
                    f"wrong PICOBIN image type: {item.header:#010x},"
                    f" expected {expected_type:#010x}"
                )
            arm_secure = True
        elif item.kind == ITEM_HASH_DEF:
            if item.words != 2 or item.header >> 24 != HASH_SHA256:
                raise ValueError("wrong PICOBIN SHA-256 hash definition")
            sha256 = True
        elif item.kind == ITEM_SIGNATURE:
            if (
                item.words != SIGNATURE_ITEM_WORDS
                or item.header >> 24 != SIGNATURE_SECP256K1
                or key is not None
            ):
                raise ValueError("invalid PICOBIN secp256k1 SIGNATURE item")
            key = bytes(data[item.offset + 4 : item.offset + 68])
            if not any(key):
                raise ValueError("empty PICOBIN signing key")
    return SecurityItems(block, arm_secure, sha256, key)


def signing_public_key(sealed):
    """The raw 64-byte secp256k1 key from a sealed image's two-block loop.

    Follows only the exact loop picotool builds from Ferriboot's block:
    never scan flash for something that looks like a SIGNATURE item.
    """
    if len(sealed) < ROM_HEADER_SIZE:
        raise ValueError("sealed bootloader is shorter than its ROM metadata")
    first = read_security_items(sealed, 0, signed=False)
    items = first.block.items
    if (
        not items
        or items[0].offset != 4
        or items[0].kind != ITEM_IMAGE_TYPE
        or first.block.last.header != LINKER_LAST  # also places LAST at word 11
    ):
        raise ValueError(
            "first PICOBIN block is not Ferriboot's linker-built IMAGE_DEF"
        )
    if first.public_key is not None or first.block.link < ROM_HEADER_SIZE:
        raise ValueError("first PICOBIN block does not link to sealed metadata")
    link = first.block.link
    sealed_block = read_security_items(sealed, link, signed=True)
    if sealed_block.block.link != -link:
        raise ValueError("sealed PICOBIN block does not link back to the first block")
    if not sealed_block.arm_secure or not sealed_block.sha256:
        raise ValueError(
            "sealed PICOBIN block lacks the Arm Secure image type or SHA-256 hash"
        )
    if sealed_block.public_key is None:
        raise ValueError("sealed PICOBIN block has no SIGNATURE item")
    return sealed_block.public_key
