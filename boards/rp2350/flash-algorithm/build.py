#!/usr/bin/env python3
"""Build the Rust SRAM image and package it as probe-rs 0.32 target YAML.

Requires Cargo with the thumbv8m.main-none-eabi target and `pip install pyelftools`.
"""

import argparse
import base64
import os
import subprocess
from pathlib import Path

from elftools.elf.elffile import ELFFile

HERE = Path(__file__).resolve().parent
TARGET = "thumbv8m.main-none-eabi"
LOAD_ADDRESS = (
    0x20000004  # probe-rs 0.32 puts its 4-byte BKPT return trap at 0x20000000.
)
SRAM_BUDGET_END = 0x20004000
ENTRY_POINTS = ["Init", "UnInit", "ProgramPage", "EraseSector"]


def extract(path):
    """Return the image bytes, entry point offsets and data offset of the linked ELF."""
    with open(path, "rb") as file:
        elf = ELFFile(file)
        if elf["e_machine"] != "EM_ARM" or elf.elfclass != 32 or not elf.little_endian:
            raise ValueError("expected a little-endian 32-bit Arm ELF")
        symbols = {
            s.name: s["st_value"]
            for s in elf.get_section_by_name(".symtab").iter_symbols()
        }
        end = symbols["_end"]
        if not LOAD_ADDRESS < end < SRAM_BUDGET_END:
            raise ValueError("algorithm exceeds its SRAM code/data budget")
        image = bytearray(end - LOAD_ADDRESS)
        for section in elf.iter_sections():
            if not section["sh_flags"] & 0x2 or not section["sh_size"]:  # SHF_ALLOC
                continue
            start = section["sh_addr"] - LOAD_ADDRESS
            if start < 0 or start + section["sh_size"] > len(image):
                raise ValueError(f"section {section.name} lies outside the image")
            if section["sh_type"] != "SHT_NOBITS":
                image[start : start + section["sh_size"]] = section.data()
    entries = {name: symbols[name] - LOAD_ADDRESS for name in ENTRY_POINTS}
    if any(offset & 1 != 1 or offset >= len(image) for offset in entries.values()):
        raise ValueError("expected Thumb entry points inside the image")
    return image, entries, symbols["_data_start"] - LOAD_ADDRESS


def generate(elf):
    image, entries, data_offset = extract(elf)
    instructions = base64.b64encode(image).decode("ascii")
    return f"""# Generated from Rust src/ by build.py (Cargo). Requires probe-rs 0.32.x.
# Fixture command transport ONLY: not a normal flash programming target.
name: RP235x-ferriboot-fixture
manufacturer:
  id: 0x3b
  cc: 0x4
variants:
- name: RP235x-ferriboot-fixture
  part: 1225
  cores:
  - name: core0
    type: armv8m
    core_access_options: !Arm
      ap: !v2 0x2000
  memory_map:
  - !Nvm
    range:
      start: 0x10000000
      end: 0x10500000
    cores: [core0]
    access:
      boot: true
  - !Ram
    range:
      start: 0x20000000
      end: 0x20081000
    cores: [core0]
  # Reserve scratch Y at 0x20081000 for a compressed sector (4096 bytes).
  flash_algorithms: [fixture]
flash_algorithms:
- name: fixture
  description: Rust SRAM fixture command decoder and verified 4 MiB NOR restore
  default: true
  load_address: 0x{LOAD_ADDRESS:x}
  instructions: {instructions}
  pc_init: 0x{entries["Init"]:x}
  pc_uninit: 0x{entries["UnInit"]:x}
  pc_program_page: 0x{entries["ProgramPage"]:x}
  pc_erase_sector: 0x{entries["EraseSector"]:x}
  data_section_offset: 0x{data_offset:x}
  stack_size: 4096
  flash_properties:
    address_range:
      start: 0x10000000
      end: 0x10500000
    page_size: 4096
    erased_byte_value: 0xff
    program_page_timeout: 600000
    erase_sector_timeout: 1000
    sectors:
    - size: 4096
      address: 0
  cores: [core0]
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    parser.add_argument(
        "--elf", type=Path, help="package an already built ELF instead of building"
    )
    parser.add_argument("--output", type=Path, default=HERE / "target.yaml")
    args = parser.parse_args()
    elf = args.elf.resolve() if args.elf is not None else None
    args.output = args.output.resolve()
    if elf is None:
        subprocess.run(
            [args.cargo, "build", "--release", "--bin", "flash", "--target", TARGET],
            cwd=HERE,
            check=True,
        )
        elf = HERE / "target" / TARGET / "release" / "flash"
    args.output.write_text(generate(elf))
    print(f"Generated {args.output} from {elf}")


if __name__ == "__main__":
    main()
