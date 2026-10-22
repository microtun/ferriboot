# ferriboot-core

The platform-independent core of Ferriboot, an unofficial Rust implementation
of [MCUboot](https://github.com/mcu-tools/mcuboot)'s `bootutil`. It is
`no_std`, contains no `unsafe` code and no target-specific code, and is
generic over `embedded_storage::nor_flash::NorFlash`. That lets it be tested on
the host with a simulated flash that cuts power after every single erase and
write.

On-flash formats match upstream (`BOOT_MAX_ALIGN = 8`), so images from
`imgtool sign`, including `--pad --confirm`, work unchanged.

## What it provides

| Module | Purpose |
|---|---|
| `boot` | `BootLoader`: the boot decision for swap-using-scratch (test/permanent, revert, power-fail resume), overwrite-only, and direct-XIP with optional revert; downgrade prevention and bootstrap |
| `validate` | Image validation: SHA-256 over header, body and protected TLVs; ECDSA P-256 with key selection by `KEYHASH`/`PUBKEY`; allow-list for unprotected TLVs; `Policy` for minimum version, security counter and imgtool VID/CID |
| `image` | MCUboot image header and TLV areas |
| `trailer` | Image trailer: magic, `swap_info`, `copy_done`, `image_ok`, swap status |
| `updater` | `FirmwareUpdater`, the application side: stage an image, mark it pending, confirm |
| `upload` | Bounded, packet-at-a-time OTA sessions with caller-supplied time |
| `security_counter` | `SecurityCounterStore` trait for the hardware rollback floor |
| `shared_data` | MCUboot-compatible boot-info TLVs handed to the application in RAM |
| `layout` | `FlashMap`, slot geometry, handoff checks, and construction of the bootloader and updater |
| `fih` | Fault-injection hardening for the irreversible steps (handoff, counter burns) |

Only ECDSA P-256 over SHA-256 is supported. Encrypted and compressed images
are rejected.

## Using it

Most users should depend on a platform crate (`ferriboot-esp`, `ferriboot-rp`)
rather than on this crate directly. A platform supplies the flash driver, a
`SecurityCounterStore`, entropy for `fih`, and the jump to the application.
`boards/qemu/ferriboot/src/main.rs` is the smallest complete bootloader to read.

A bootloader crate selects its strategy from its own Cargo features
(`overwrite-only`, `direct-xip`, `direct-xip-revert`; none means swap) with
`ferriboot_core::upgrade_mode_from_features!()`. Applications learn the
strategy from the boot-info handoff and never hold signing keys: the updater
only stages images, and the bootloader alone decides what runs.

## Cargo features

| Feature | Default | Purpose |
|---|---|---|
| `uuid` | yes | Parse imgtool `--vid`/`--cid` names into UUIDs |
| `log` | no | Diagnostics through `log` |
| `defmt` | no | Diagnostics through `defmt` |

Enable at most one of `log` and `defmt`; with neither, logging is compiled out.

## Tests

```sh
cargo test -p ferriboot-core
```

The tests in `tests/` cover upgrades, power loss at every flash operation,
fault injection, validation, the security counter and the updater. Signed
fixtures in `tests/data/` are regenerated with
`python3 tools/generate-test-images.py`; their key is public test material.

The image envelope and policy types are adapted from `microtun-mcuboot`.
