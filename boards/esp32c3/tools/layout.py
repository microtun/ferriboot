#!/usr/bin/env python3
"""Validate ESP32-C3 flash geometry and generate an ESP-IDF partition table.

Partition geometry comes only from layout/partitions.csv. Target code reads
the resulting binary table. Chip facts (ferriboot-esp/chips/esp32c3.json) and
the flash size (layout/board.json) come from the board description, as in
the Rust layout crate.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
import board  # noqa: E402

LAYOUT = Path(__file__).resolve().parents[1] / "layout"
PARTITIONS = LAYOUT / "partitions.csv"
PLATFORM = board.load(LAYOUT)

TYPE = {"app": 0, "data": 1}
SUBTYPE = {
    ("app", "factory"): 0,
    ("app", "test"): 32,
    ("data", "ota"): 0,
    ("data", "phy"): 1,
    ("data", "nvs"): 2,
    ("data", "coredump"): 3,
    ("data", "nvs_keys"): 4,
    ("data", "efuse"): 5,
    ("data", "undefined"): 6,
    ("data", "fat"): 129,
    ("data", "spiffs"): 130,
    ("data", "littlefs"): 131,
}


def number(value):
    text = str(value).strip()
    factor = 1
    if text.endswith(("K", "k", "M", "m")):
        factor = 1024 if text[-1].lower() == "k" else 1024 * 1024
        text = text[:-1]
    if not re.fullmatch(r"(?:0[xX][0-9a-fA-F]+|[0-9]+)", text):
        raise ValueError(f"invalid number: {value}")
    return int(text, 16 if text.lower().startswith("0x") else 10) * factor


def partition_type(value):
    code = TYPE.get(value, None)
    if code is None:
        code = number(value)
    if not 0 <= code <= 255:
        raise ValueError(f"invalid partition type: {value}")
    return code


def partition_subtype(kind, value):
    code = SUBTYPE.get((kind, value), None)
    if code is None:
        if (
            kind == "app"
            and value.startswith("ota_")
            and re.fullmatch(r"(?:[0-9]|1[0-5])", value[4:])
        ):
            code = 16 + int(value[4:])
        else:
            code = number(value)
    if not 0 <= code <= 255:
        raise ValueError(f"invalid partition subtype: {value}")
    return code


def partition_table_offset():
    """Bootstrap locator shared with Cargo's ESP-IDF partition reader."""
    key = "ESP_BOOTLOADER_ESP_IDF_CONFIG_PARTITION_TABLE_OFFSET"
    config = tomllib.loads((LAYOUT.parent / ".cargo/config.toml").read_text())
    value = os.environ.get(key, config["env"][key])
    return number(value)


def geometry(partitions=PARTITIONS, table_offset=None):
    """Return validated constants and ordered partitions (name, type, subtype, offset, size)."""
    table = partition_table_offset()
    if (
        table <= 0
        or table % PLATFORM["ERASE_SIZE"]
        or table + PLATFORM["PARTITION_TABLE_SIZE"] > PLATFORM["FLASH_SIZE"]
    ):
        raise ValueError("invalid partition table offset")
    if table_offset is not None and number(table_offset) != table:
        raise ValueError(
            f"partition table offset is {table:#x}, but IDF uses {number(table_offset):#x}"
        )
    entries = []
    regions = {}
    for line in Path(partitions).read_text().splitlines():
        line = line.partition("#")[0].strip()
        if not line:
            continue
        columns = [field.strip() for field in line.split(",")]
        if len(columns) != 5:
            raise ValueError(f"expected five partition fields: {line}")
        name, kind, subtype, offset_text, size_text = columns
        if (
            not name
            or len(name.encode("utf-8")) > 16
            or not offset_text
            or not size_text
        ):
            raise ValueError(f"{name}: invalid partition name or missing offset/size")
        if name in regions:
            raise ValueError(f"duplicate partition {name}")
        typ, sub = partition_type(kind), partition_subtype(kind, subtype)
        offset, size = number(offset_text), number(size_text)
        if not size or offset % PLATFORM["ERASE_SIZE"] or size % PLATFORM["ERASE_SIZE"]:
            raise ValueError(f"{name}: offset and nonzero size must be 4 KiB aligned")
        if offset < table + PLATFORM["PARTITION_TABLE_SIZE"]:
            raise ValueError(f"{name}: overlaps bootloader or partition table")
        if offset + size > PLATFORM["FLASH_SIZE"]:
            raise ValueError(f"{name}: partition exceeds 4 MiB flash")
        for other, (_, _, other_offset, other_size) in regions.items():
            if offset < other_offset + other_size and other_offset < offset + size:
                raise ValueError(f"{name}: overlaps {other}")
        if name in ("image-0", "image-1", "image-scratch", "storage") and (
            typ,
            sub,
        ) != (1, 6):
            raise ValueError(f"{name}: must be data,undefined")
        entries.append((name, typ, sub, offset, size))
        regions[name] = (typ, sub, offset, size)

    for required in ("image-0", "image-1", "image-scratch", "storage", "ferriboot"):
        if required not in regions:
            raise ValueError(f"missing partition: {required}")
    factory = regions["ferriboot"]
    if factory[:2] != (0, 0):
        raise ValueError("ferriboot must be app,factory")
    if factory[2] % PLATFORM["MMU_PAGE_SIZE"] or factory[3] % PLATFORM["MMU_PAGE_SIZE"]:
        raise ValueError("ferriboot offset and size must be MMU page aligned")
    primary = regions["image-0"]
    secondary = regions["image-1"]
    scratch = regions["image-scratch"]
    storage = regions["storage"]
    if primary[3] != secondary[3]:
        raise ValueError("image slots must have equal sizes")
    if (primary[2] + PLATFORM["HEADER_SIZE"]) % PLATFORM["MMU_PAGE_SIZE"] or (
        secondary[2] + PLATFORM["HEADER_SIZE"]
    ) % PLATFORM["MMU_PAGE_SIZE"]:
        raise ValueError("image payloads must be 64 KiB aligned")
    if scratch[3] < 2 * PLATFORM["ERASE_SIZE"]:
        raise ValueError("scratch must hold data and status sectors")
    if storage[3] < 32768:
        raise ValueError("storage must reserve at least 32 KiB")
    if (
        PLATFORM["BOOT_INFO_ADDRESS"] + PLATFORM["BOOT_INFO_SIZE"]
        != PLATFORM["ROM_DATA_START"]
    ):
        raise ValueError("boot info must end where ROM data starts")
    values = {
        **PLATFORM,
        "PARTITION_TABLE_OFFSET": table,
        "BOOTLOADER_SIZE": table,
        "FERRIBOOT_OFFSET": factory[2],
        "FERRIBOOT_SIZE": factory[3],
        "PRIMARY_OFFSET": primary[2],
        "SECONDARY_OFFSET": secondary[2],
        "SLOT_SIZE": primary[3],
        "SCRATCH_OFFSET": scratch[2],
        "SCRATCH_SIZE": scratch[3],
        "STORAGE_OFFSET": storage[2],
        "STORAGE_SIZE": storage[3],
    }
    return values, entries


def table_binary(entries):
    """The standard 0xc00-byte ESP-IDF table (partition flags and MD5 trailer)."""
    raw = bytearray()
    for name, typ, subtype, offset, size in entries:
        raw.extend(
            struct.pack(
                "<HBBII16sI",
                0x50AA,
                typ,
                subtype,
                offset,
                size,
                name.encode("utf-8"),
                0,
            )
        )
    if len(raw) + 32 > 0xC00:
        raise ValueError("partition table exceeds 0xc00 bytes")
    return (
        bytes(raw)
        + b"\xeb\xeb"
        + b"\xff" * 14
        + hashlib.md5(raw).digest()
        + b"\xff" * (0xC00 - len(raw) - 32)
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--partitions", type=Path, default=PARTITIONS)
    parser.add_argument(
        "--table-offset", help="assert ESP-IDF's partition table offset"
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--table", type=Path, metavar="FILE")
    mode.add_argument("--get", metavar="LABEL")
    mode.add_argument("--constant", metavar="NAME")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    try:
        values, entries = geometry(args.partitions, args.table_offset)
        if args.table:
            output = args.table.expanduser().resolve()
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(table_binary(entries))
        elif args.get:
            for name, _, _, offset, size in entries:
                if name == args.get:
                    print(f"{offset:#x} {size:#x}")
                    break
            else:
                raise ValueError(f"no partition '{args.get}'")
        elif args.constant:
            if args.constant not in values:
                raise ValueError(f"unknown constant: {args.constant}")
            print(f"{values[args.constant]:#x}")
        else:
            print(json.dumps(values, indent=2))
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"invalid ESP32-C3 partition layout: {error}\n")


if __name__ == "__main__":
    main()
