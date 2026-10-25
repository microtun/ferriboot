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
- **Boards:** ESP32-C3 SuperMini, RP2350 Pico 2 / Pico 2 W, QEMU (tests only).

Ferriboot is an experimental prototype. Read [SECURITY.md](SECURITY.md) and
[THREAT-MODEL.md](THREAT-MODEL.md) before using it in a product.

## Getting started

```sh
python3 -m pip install imgtool
# MCUboot development key pair; see keys/README.md for details.
./keys/generate.sh
cargo test                            # core, build helpers and test hooks
```

Then follow the board README for the target you want to build; each board
has its own tool binary (`ferriboot-tools-esp`, `ferriboot-tools-rp2350`,
`ferriboot-tools-qemu`).

**Optional remote signing:** The host tools can sign MCUboot firmware and the
ESP32-C3 / RP2350 ROM bootloader stages using `microtun-signer`, keeping
private keys on the service. Existing local PEM signing remains the default.
See [SIGNING.md](SIGNING.md) for setup, algorithms, public-key provisioning
and RP2350 in-process ROM sealing.

## Chips and boards

Chip support and board support are separate, so one platform crate serves
many boards.

- **Chip crates** (`chips/<chip>/`, named `ferriboot-chip-<chip>`) hold hardware
  addresses and the fixed Ferriboot platform ABI, independent of a board's
  flash capacity and partition allocation. They are small `no_std` crates.
- **Platform crates** (`ferriboot-esp`, `ferriboot-rp`, `ferriboot-qemu`) provide
  chip implementations, selected by features (`esp32c3`; `rp235xa`/`rp235xb`;
  `mps2-an385` for the QEMU machine). Their build scripts read the appropriate
  chip crate directly.
  `ferriboot-build` supplies generic linker generation, signing, ELF and CLI
  helpers without a registry of boards or chips.
- **Boards** (`boards/<board>/`) contain the bootloader
  (`<board>-ferriboot`), example firmware
  (`<board>-firmware`) and layout (`ferriboot-layout-<board>`). Layouts
  re-export their chip crate and add board-specific flash configuration.
  External partition files stay in the layout directory.
- **Board data reaches a platform crate only as values.** Boards with a fixed
  flash map pass the layout crate's `FLASH_MAP` to the platform's
  `Layout::new`, a `const fn`, so an invalid layout is a compile error. ESP
  boards pass nothing: their slots come from the partition table in flash.
- **Board-dependent linker regions** are written by the final binary's
  `build.rs` using `ferriboot-build`. Each platform crate publishes its chip
  through `links` metadata, and binaries check it with
  `ferriboot_build::check_platform_chip`.

A new board for an existing chip adds `boards/<board>/` with a layout that
re-exports `ferriboot-chip-<chip>`. A new chip adds `chips/<chip>/`, a platform
feature and implementation, then its first board. `ferriboot-build` does not
need updating for either case.

## Cargo workspaces

The repository uses **one main workspace** (core, chip, platform, board, host
tool and HIL crates) and two standalone workspaces: the embedded ESP RAM
handoff loader (`ferriboot-esp/bl-loader`) and the RP2350 probe-rs flash
algorithm (`boards/rp2350-pico2/flash-algorithm`). All ordinary crates share the
root `Cargo.lock` and dependency versions. Root Cargo commands default to
host packages; embedded builds explicitly select the board packages.

The ESP platform and its standalone RAM loader compile the same
`ferriboot-esp/src/loader_abi.rs` source module for their handoff structures;
no extra ABI crate or Cargo workspace is needed.

Build from the relevant board directory to load its `.cargo/config.toml`
(target, profile and environment). For example:

```sh
cd boards/esp32c3-supermini
cargo build --release -p esp32c3-supermini-ferriboot -p esp32c3-supermini-firmware
```

`cargo build --workspace` is intentionally **not** used for embedded images:
it would select incompatible targets and may unify chip features. Use one
Cargo invocation per board. Binaries can still be run from their individual
`ferriboot/` and `firmware/` directories to use their specific runners.

## Hardware-in-the-loop tests

`testing/ferriboot-hil` builds, signs and flashes complete boot chains and
checks the boot output. Select the board with a feature (`esp32c3-supermini`,
`rp2350-pico2`, `qemu`) and the suite with another:

| Feature | Suite |
|---|---|
| `smoke` | Basic boot and upgrade on physical boards |
| `regression` | Full swap, overwrite, direct-XIP and safety matrices (QEMU); includes `security-counter` |
| `security-counter` | Anti-rollback tests. **On physical boards this permanently burns one more eFuse/OTP epoch per run.** |

Physical tests reflash the whole device and need an expendable board. See
[`testing/ferriboot-hil`](testing/ferriboot-hil/README.md) for environment
variables and the board README for wiring.

## License

Microtun is **source-available software**, not open-source software.

The source code is made available for transparency, inspection, security research, auditing, and evaluation. You may:

- View and inspect the source code.
- Make reasonable copies for testing, auditing, evaluation, or security research.
- Compile, run, and modify the software as reasonably necessary for evaluation.
- Evaluate the software for up to **60 days** from the first time you compile or run it for a particular evaluation.

The license does **not** permit production, operational, or commercial use. You may not use Microtun to provide products or services, incorporate it into another product or service, commercially exploit it, or distribute modified versions except where expressly permitted by the license.

See [`LICENSE`](LICENSE) for the complete terms.