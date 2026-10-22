#!/usr/bin/env python3
"""Export bounded ELF32/Arm PT_LOAD segments by physical load address."""

import argparse
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
import board  # noqa: E402


def elf_payload(data, bootloader=False):
    if len(data) < 52 or data[:7] != b"\x7fELF\x01\x01\x01":
        raise ValueError("expected little-endian ELF32")
    kind, machine, version, entry, phoff = struct.unpack_from("<HHIII", data, 16)
    phsize, phnum = struct.unpack_from("<HH", data, 42)
    if kind != 2 or machine != 40 or version != 1 or phsize != 32:
        raise ValueError("expected executable Arm ELF32")
    if phoff + phsize * phnum > len(data):
        raise ValueError("truncated program headers")
    # The Rust layout crate and this exporter read the same board description.
    constants = board.load(Path(__file__).resolve().parents[1] / "layout")

    def value(name):
        return constants[name]

    vector = value("APP_XIP_BASE") + value("HEADER_SIZE")
    base, capacity = (
        (0, value("BOOTLOADER_SIZE"))
        if bootloader
        else (vector, value("SLOT_SIZE") - value("HEADER_SIZE"))
    )
    ram_base, ram_end = value("RAM_BASE"), value("BOOT_INFO_ADDRESS")
    segments = []
    for index in range(phnum):
        typ, offset, vma, lma, filesz, memsz, flags, align = struct.unpack_from(
            "<8I", data, phoff + index * phsize
        )
        if typ != 1 or filesz == 0:
            continue
        if filesz > memsz or offset + filesz > len(data):
            raise ValueError("truncated or invalid load segment")
        if lma < base or lma + filesz > base + capacity:
            raise ValueError("load segment outside execution partition")
        if vma != lma and not (ram_base <= vma and vma + memsz <= ram_end):
            raise ValueError("invalid SRAM data segment")
        start, end = lma - base, lma - base + filesz
        if any(start < b and a < end for a, b, _ in segments):
            raise ValueError("overlapping physical load segments")
        segments.append((start, end, data[offset : offset + filesz]))
    if not segments or min(a for a, _, _ in segments) != 0:
        raise ValueError("missing vector table at execution base")
    result = bytearray(b"\xff" * max(b for _, b, _ in segments))
    for a, b, contents in segments:
        result[a:b] = contents
    if len(result) < 8:
        raise ValueError("truncated vector table")
    stack, reset = struct.unpack_from("<II", result)
    if not (ram_base < stack <= ram_end) or stack % 8:
        raise ValueError("invalid stack or overlap with boot handoff")
    if reset & 1 == 0 or not (base + 8 <= (reset & ~1) < base + len(result)):
        raise ValueError("reset handler outside payload")
    if entry != reset:
        raise ValueError("ELF entry does not match reset vector")
    return bytes(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("elf", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--bootloader", action="store_true")
    args = parser.parse_args()
    args.elf = args.elf.resolve()
    args.output = args.output.resolve()
    try:
        args.output.write_bytes(elf_payload(args.elf.read_bytes(), args.bootloader))
    except (ValueError, OSError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
