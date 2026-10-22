# Ferriboot

Unofficial Rust implementation of [MCUboot](https://github.com/mcu-tools/mcuboot)'s
`bootutil` for a single image pair. On-flash formats (image header, TLVs,
trailer with `BOOT_MAX_ALIGN = 8`) match upstream, so `imgtool sign` output
remains compatible.

- Signatures: ECDSA P-256 over SHA-256 only. Encrypted and compressed images are rejected.
- Strategies: swap-using-scratch (test/permanent, revert, power-fail resume),
  overwrite-only, direct-XIP with or without revert.
- Policy: downgrade prevention, bootstrap, minimum version, security counter,
  imgtool `--vid/--cid`.
- Targets: ESP32-C3, RP2350, QEMU (tests only).

## Chips and boards

Chip support and boards are separate, so one chip crate serves many boards:

- **Platform crates** (`ferriboot-esp`, `ferriboot-rp`, `ferriboot-qemu`) hold
  everything about a chip and nothing about a board. Each is its own Cargo
  workspace and is built for exactly one chip, selected by a feature
  (`esp32c3`; `rp235xa`/`rp235xb`; `mps2-an385`). The chip's addresses,
  image header policy and handoff ABI are in `<crate>/chips/<chip>.json`,
  read by the crate's build script, the board layout crates and the Python
  tools alike.
- **Boards** (`boards/<board>/`) are workspaces with the bootloader, the
  example firmware and a layout crate (`esp32c3-layout`, `rp2350-layout`,
  `qemu-layout`). `layout/board.json` names the chip description it builds on
  and adds the board's own values (flash size, partitions). Platform crates
  are path dependencies with a chip feature, never workspace members.
- **Board data reaches a platform crate only as values.** Boards with a fixed
  flash map pass the layout crate's `FLASH_MAP` (a `ferriboot_core::FlashMap`)
  through the platform's `Layout::new`, a `const fn` that checks it against
  the chip's rules, so an invalid board layout is a compile error. ESP boards
  pass nothing: their slots come from the partition table in flash.
- **Board-dependent linker regions** are written by the final binary's
  `build.rs` (helpers in `ferriboot-build`); platform crates only export
  chip-level linker contracts. Each platform crate publishes the chip it was
  built for through its `links` metadata, and binaries check it against
  their board's chip (`ferriboot_build::check_platform_chip`).

Adding a board for an existing chip needs only a new `boards/<board>/`. Adding
a chip means a `chips/<chip>.json`, its Rust-only facts under the platform
crate's `src/chip/` and a feature, plus any chip-specific code it needs.

## License

Ferriboot is dual-licensed under **either the [MIT License](LICENSE-MIT) or
the [Apache License, Version 2.0](LICENSE-APACHE)**, at your option
(`MIT OR Apache-2.0`). See [LICENSE](LICENSE) for the overview.