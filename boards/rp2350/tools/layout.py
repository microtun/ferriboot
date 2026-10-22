#!/usr/bin/env python3
"""Read picotool partition JSON; validate and print Ferriboot geometry.

Uses only the Python standard library. firmware.py uses this parser; Cargo builds
use the Rust layout crate instead. picotool is optional and is not invoked
while building or signing.
"""

import argparse
import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
import board  # noqa: E402
from mcuboot import max_image_size as mcuboot_max_image_size

LAYOUT = Path(__file__).resolve().parents[1] / "layout"
PARTITIONS = LAYOUT / "partitions.json"

# RP2350 hardware and Ferriboot's runtime ABI (ferriboot-rp/chips/rp2350.json)
# plus the board's flash size (layout/board.json), not flash partitions.
# ATRANS1 maps either physical slot into the common application XIP window.
# The bootloader runs in SRAM; ATRANS0 remains identity-mapped for flash access.
PLATFORM = board.load(LAYOUT)


def number(value):
    """Picotool sizes/starts are byte integers or decimal K/k strings."""
    if type(value) is int and value >= 0:
        return value
    if isinstance(value, str) and re.fullmatch(r"[0-9]+[kK]", value):
        return int(value[:-1]) * 1024
    raise ValueError(f"expected a nonnegative byte count or K size, got {value!r}")


def geometry(path=PARTITIONS):
    table = json.loads(Path(path).read_text())
    regions = {}
    for row in table["partitions"]:
        name = row["name"]
        if name in regions:
            raise ValueError(f"duplicate partition {name!r}")
        # Require explicit starts so the ROM table and Ferriboot cannot disagree
        # about picotool's default first offset or implicit placement.
        start, size = number(row["start"]), number(row["size"])
        if not size or start % PLATFORM["ERASE_SIZE"] or size % PLATFORM["ERASE_SIZE"]:
            raise ValueError(f"{name}: start and nonzero size must be 4 KiB aligned")
        if start + size > PLATFORM["FLASH_SIZE"]:
            raise ValueError(f"{name}: partition exceeds the 4 MiB flash")
        for other, (offset, length) in regions.items():
            if start < offset + length and offset < start + size:
                raise ValueError(f"{name}: overlaps {other}")
        regions[name] = (start, size)
        if name == "bootloader":
            if (
                row["families"] != ["rp2350-arm-s"]
                or row.get("ignored_during_arm_boot", False)
                or row.get("ignored_during_riscv_boot") is not True
                or "link" in row
            ):
                raise ValueError("bootloader must be an Arm Secure ROM boot partition")
        if name in ("image-0", "image-1", "image-scratch", "storage"):
            if (
                row["families"] != ["data"]
                or row.get("ignored_during_arm_boot") is not True
                or row.get("ignored_during_riscv_boot") is not True
                or "link" in row
            ):
                raise ValueError(
                    f"{name}: must be ROM-ignored data without native A/B links"
                )
    if set(regions) != {"bootloader", "image-0", "image-1", "image-scratch", "storage"}:
        raise ValueError(
            "expected bootloader, image-0, image-1, image-scratch and storage partitions"
        )
    if regions["bootloader"][0] != 0:
        raise ValueError("bootloader must start at flash offset zero")
    if regions["image-0"][1] != regions["image-1"][1]:
        raise ValueError("image slots must have equal sizes")
    if regions["image-scratch"][1] < 2 * PLATFORM["ERASE_SIZE"]:
        raise ValueError("scratch must hold a data sector and a status sector")
    if regions["storage"][1] < 32 * 1024:
        raise ValueError("storage must reserve at least 32 KiB")
    result = dict(PLATFORM)
    result.update(
        {
            "BOOTLOADER_SIZE": regions["bootloader"][1],
            "PRIMARY_OFFSET": regions["image-0"][0],
            "SECONDARY_OFFSET": regions["image-1"][0],
            "SLOT_SIZE": regions["image-0"][1],
            "SCRATCH_OFFSET": regions["image-scratch"][0],
            "SCRATCH_SIZE": regions["image-scratch"][1],
            "STORAGE_OFFSET": regions["storage"][0],
            "STORAGE_SIZE": regions["storage"][1],
        }
    )
    if max_image_size(result) <= result["HEADER_SIZE"]:
        raise ValueError("slots cannot hold a header, payload and MCUboot trailer")
    return result


def max_image_size(g):
    return mcuboot_max_image_size(g["SLOT_SIZE"], g["ERASE_SIZE"], g["WRITE_SIZE"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--partitions", type=Path, default=PARTITIONS)
    parser.add_argument(
        "--check",
        action="store_true",
        required=True,
        help="validate and print geometry",
    )
    args = parser.parse_args()
    args.partitions = args.partitions.resolve()
    try:
        g = geometry(args.partitions)
        print(json.dumps({**g, "MAX_IMAGE_SIZE": max_image_size(g)}, indent=2))
    except (ValueError, KeyError, TypeError, OSError) as error:
        sys.exit(f"invalid RP2350 partition layout: {error}")


if __name__ == "__main__":
    main()
