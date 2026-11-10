# Chip descriptions

One small `no_std` crate per chip, holding only constants: memory addresses,
flash geometry, the MCUboot header size and the location of the boot-info
handoff record. They contain no code and nothing board-specific.

| Crate | Directory | Chip |
|---|---|---|
| `ferriboot-chip-esp32c3` | `esp32c3/` | ESP32-C3 |
| `ferriboot-chip-rp235x` | `rp235x/` | RP2350A/B and RP2354A/B (one die) |
| `ferriboot-chip-qemu` | `qemu/` | Patched QEMU `mps2-an385` test machine |

Keeping these values in one place lets everything that must agree on them use
the same definition: the platform crates (`ferriboot-esp`, `ferriboot-rp`,
`ferriboot-qemu`), their build scripts and linker scripts, the board layout
crates under `boards/*/layout`, `ferriboot-build` and the host tools.

Board facts do not belong here. Flash size on RP2350, partition offsets and
slot sizes live in the board's layout crate.

Several constants are an ABI between the bootloader and the application, such
as `BOOT_INFO_ADDRESS` and `APP_XIP_BASE`. Changing one requires rebuilding
and reflashing both sides, and on the ESP32-C3 the RAM handoff addresses must
stay consistent with the ESP-IDF second stage's memory map.
