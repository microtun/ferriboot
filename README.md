# Ferriboot

A secure boot bootloader written in Rust, with
[MCUboot](https://github.com/mcu-tools/mcuboot) compatibility. Ferriboot
manages a single image pair and uses MCUboot's on-flash formats (image header,
TLVs, trailer with `BOOT_MAX_ALIGN = 8`), so `imgtool sign` output works
unchanged. It is an independent project, not affiliated with MCUboot.

- **Signatures:** ECDSA P-256 over SHA-256 only. Encrypted and compressed
  images are rejected.
- **Upgrade strategies:** swap-using-scratch (test/permanent, revert,
  power-fail resume), overwrite-only, direct-XIP with or without revert.
- **Policy:** downgrade prevention, bootstrap, minimum version, hardware
  security counter, imgtool `--vid`/`--cid`.
- **Boards:** [ESP32-C3 SuperMini](boards/esp32c3-supermini/README.md),
  [RP2350 Pico 2 / Pico 2 W](boards/rp2350-pico2/README.md) and
  [QEMU](boards/qemu/README.md) (tests only).

Ferriboot is an experimental prototype. Read [SECURITY.md](SECURITY.md) and
[THREAT-MODEL.md](THREAT-MODEL.md) before using it in a product.

## Getting started

You need Rust **1.95+**, OpenSSL and upstream `imgtool` (needs Python). Each
board README lists its Rust target and any extra tools.

```sh
python3 -m pip install imgtool
./keys/generate.sh    # development signing keys; see keys/README.md
cargo test            # core, build helpers, host tools and test hooks
```

Then follow the README for your board. Each board has its own host tool:
`ferriboot-tools-esp`, `ferriboot-tools-rp2350` or `ferriboot-tools-qemu`.

Signing uses local PEM keys by default. The tools can instead sign through
`microtun-signer`, which keeps private keys on a remote service; see
[SIGNING.md](SIGNING.md).

## Repository layout

- [`ferriboot-core`](ferriboot-core/README.md) is the platform-independent
  bootloader logic: image verification, upgrades and the application-side
  updater.
- [`ferriboot-esp`](ferriboot-esp/README.md),
  [`ferriboot-rp`](ferriboot-rp/README.md) and
  [`ferriboot-qemu`](ferriboot-qemu/README.md) are the platform crates that
  bootloaders and applications depend on. Each has its own host tool in
  `tools/` (`ferriboot-tools-esp`, `ferriboot-tools-rp2350`,
  `ferriboot-tools-qemu`).
- [`ferriboot-build`](ferriboot-build/README.md) holds the `build.rs` helpers
  and the signing code shared by the host tools.
- [`chips/`](chips/README.md) holds one small constants crate per chip, shared
  by the platform crates, build scripts and host tools.
- `boards/` holds a bootloader, example firmware and flash layout for each
  board.
- `testing/` holds the hardware-in-the-loop tests and the example firmware's
  [test hooks](testing/ferriboot-test-hooks/README.md).
- [`keys/`](keys/README.md) is where development signing keys are generated;
  none are committed.
- `tools/set-version.sh` sets the release version in every workspace.

Chip support and board support are separate, so supporting another board on
an existing chip only needs a new directory under `boards/`.

Two SRAM payloads are separate Cargo workspaces with their own lock files,
because they are linked independently:
[`ferriboot-esp/bl-loader`](ferriboot-esp/bl-loader/README.md) (built
automatically by `ferriboot-esp`) and
[`boards/rp2350-pico2/flash-algorithm`](boards/rp2350-pico2/flash-algorithm/README.md)
(rebuilt by hand).

## Building

Commands run from the repository root build the host packages only. Build
embedded images from the board directory, which sets the target and profile,
with one Cargo invocation per board:

```sh
cd boards/esp32c3-supermini
cargo build --release -p esp32c3-supermini-ferriboot -p esp32c3-supermini-firmware
```

Do not use `cargo build --workspace` for embedded images: it mixes incompatible
targets. `cargo run --release` from a board's `ferriboot/` or `firmware/`
directory builds, signs and flashes that binary.

## Tests

`testing/ferriboot-hil` builds, signs and flashes complete boot chains and
checks the boot output. Choose a board feature and a suite feature that
supports it:

| Suite | Boards | Covers |
|---|---|---|
| `smoke` | `esp32c3-supermini`, `rp2350-pico2` | Basic boot and upgrade |
| `regression` | `qemu` | Full swap, overwrite, direct-XIP and safety matrices; includes `security-counter` |
| `security-counter` | all | Anti-rollback. **On a physical board each run permanently burns one eFuse/OTP epoch.** |

Other combinations compile to no tests. Physical tests reflash the whole
device and need an expendable board. See the
[ferriboot-hil README](testing/ferriboot-hil/README.md).

## Releases

Pushing a `vMAJOR.MINOR.PATCH` tag runs `.github/workflows/release.yml`, which
builds the ESP32-C3 and RP2350 bootloaders in `dev` and `release` variants,
signs them through `microtun-signer` and attaches the signed binaries to the
GitHub release. Example firmware is not published. To cut a release:

```sh
./tools/set-version.sh 0.2.0   # every manifest and lock file
git commit -am "Release 0.2.0" && git tag v0.2.0
```

[SIGNING.md](SIGNING.md) describes the signing keys and their pinned
fingerprints. Each board README explains how to flash a released binary.

## License

Ferriboot is **source-available software**, not open-source software, released
under the Microtun Source Available License. It may be inspected and evaluated
for a limited period; production, operational and commercial use are not
permitted. [LICENSE](LICENSE) has the complete terms and is the only
authoritative statement of them.
