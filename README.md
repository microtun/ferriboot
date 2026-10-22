# Ferriboot

Unofficial Rust implementation of [MCUboot](https://github.com/mcu-tools/mcuboot)'s
`bootutil` for a single image pair. On-flash formats (image header, TLVs,
trailer with `BOOT_MAX_ALIGN = 8`) match upstream, so `imgtool sign` output
works unchanged.

- **Signatures:** ECDSA P-256 over SHA-256 only. Encrypted and compressed images are rejected.
- **Strategies:** swap-using-scratch (test/permanent, revert, power-fail resume),
  overwrite-only, direct-XIP with or without revert.
- **Policy:** downgrade prevention, bootstrap, minimum version, hardware
  security counter, imgtool `--vid`/`--cid`.
- **Targets:** ESP32-C3, RP2350, QEMU (tests only).

Ferriboot is an experimental prototype. Read [SECURITY.md](SECURITY.md) and
[THREAT-MODEL.md](THREAT-MODEL.md) before using it in a product.

## Getting started

```sh
python3 -m pip install cryptography imgtool pyelftools
python3 tools/generate-dev-keys.py   # development keys only, see keys/README.md
cargo test                            # core, build helpers and test hooks
```

Then follow the board README for the target you want to build.

## Chips and boards

Chip support and board support are separate, so one platform crate serves
many boards.

- **Platform crates** hold everything about a chip and nothing about a board.
  Each is its own Cargo workspace, built for one chip selected by a feature
  (`esp32c3`; `rp235xa`/`rp235xb`; `mps2-an385`). Chip facts (addresses,
  image header policy, handoff ABI) live in `<crate>/chips/<chip>.json`, read
  by the build script, the board layout crates and the Python tools alike.
- **Boards** (`boards/<board>/`) are workspaces with the bootloader, example
  firmware and a layout crate. `layout/board.json` names the chip description
  and adds board values (flash size, partitions). Platform crates are path
  dependencies, never workspace members.
- **Board data reaches a platform crate only as values.** Boards with a fixed
  flash map pass the layout crate's `FLASH_MAP` to the platform's
  `Layout::new`, a `const fn`, so an invalid layout is a compile error. ESP
  boards pass nothing: their slots come from the partition table in flash.
- **Board-dependent linker regions** are written by the final binary's
  `build.rs` using `ferriboot-build`. Each platform crate publishes its chip
  through `links` metadata, and binaries check it with
  `ferriboot_build::check_platform_chip`.

A new board for an existing chip needs only `boards/<board>/`. A new chip
needs a `chips/<chip>.json`, its Rust-only facts under the platform crate's
`src/chip/`, a feature, and any chip-specific code.

## Hardware-in-the-loop tests

`testing/ferriboot-hil` builds, signs and flashes complete boot chains and
checks the boot output. Select the target with a feature (`esp32c3`, `rp2350`,
`qemu`) and the suite with another:

| Feature | Suite |
|---|---|
| `smoke` | Basic boot and upgrade on physical boards |
| `regression` | Full swap, overwrite, direct-XIP and safety matrices (QEMU); includes `security-counter` |
| `security-counter` | Anti-rollback tests. **On physical boards this permanently burns one more eFuse/OTP epoch per run.** |

Physical tests reflash the whole device and need an expendable board. See
[`testing/ferriboot-hil`](testing/ferriboot-hil/README.md) for environment
variables and the board README for wiring.

## License

Dual-licensed under the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option
(`MIT OR Apache-2.0`). See [LICENSE](LICENSE).
