# ferriboot-build

Host-side helpers for Ferriboot. Nothing in this crate runs on a target. It
has two audiences:

- **Build scripts** of bootloaders and applications use the default,
  dependency-light configuration.
- **Host tools** (`ferriboot-tools-esp`, `ferriboot-tools-rp2350`,
  `ferriboot-tools-qemu`) and the HIL harness enable the `tools` and `cli`
  features for signing and image handling.

## Build-script helpers

Add the crate under `[build-dependencies]` with no features.

| Function | Call from | Purpose |
|---|---|---|
| `configure_esp_application()` | ESP application or bootloader `build.rs` | Links `esp-hal`'s `linkall.x`, then the `ferriboot.x` contract from `ferriboot-esp` |
| `configure_rp2350_application(max_image_size)` | RP2350 application `build.rs` | Generates `memory.x` from the chip constants and the board's maximum image size, and links `cortex-m-rt`'s `link.x` |
| `emit_public_keys(default)` | Bootloader `build.rs` only | Writes `$OUT_DIR/keys.rs` with the trusted ECDSA P-256 public keys |
| `selected_chip`, `publish_chip`, `require_target` | Platform crates | Select one chip feature, check the target triple and publish the chip to dependants |
| `platform_chip`, `check_platform_chip` | Binaries | Check that the binary links the platform crate built for its chip |

Link arguments apply only to the package that emits them, so the
`configure_*` functions must run in the final binary's own `build.rs`.

```rust,ignore
// Application build.rs (ESP32-C3)
fn main() {
    ferriboot_build::configure_esp_application();
}
```

```rust,ignore
// Bootloader build.rs
fn main() {
    ferriboot_build::emit_public_keys("../../../keys/dev-p256.pub.pem");
}
```

Applications do not call `emit_public_keys`: only the bootloader verifies
signatures, so only it embeds keys.

### `FERRIBOOT_PUBKEYS`

`emit_public_keys` reads a comma-separated list of key files from
`FERRIBOOT_PUBKEYS`, or the `default` path when it is unset. Relative paths
are relative to the package being built. Each file may be a PEM `PUBLIC KEY`,
a DER SubjectPublicKeyInfo or a raw 65-byte uncompressed point, as produced by
`imgtool getpub -e pem` or `-e raw`. The build fails if a file is missing or
the list is empty; no key is bundled as a fallback.

## Cargo features

| Feature | Default | Purpose |
|---|---|---|
| *(none)* | yes | Build-script helpers only |
| `tools` | no | The `tools` module: MCUboot signing through `imgtool`, remote signing through `microtun-signer`, development key generation, checked ELF export and progress output |
| `cli` | no | `clap` argument types shared by the board tools; implies `tools` |

The `tools` module reads the signing environment described in the board
READMEs and [SIGNING.md](../SIGNING.md): `FERRIBOOT_KEY`, `IMAGE_VERSION`,
`SECURITY_COUNTER`, `VID`, `CID`, `IMGTOOL`, `NO_FLASH`, `FERRIBOOT_OTP_BURN`
and the `MICROTUN_SIGNER*` variables. It is an internal API for the tools in
this repository rather than a stable interface.

## Tests

```sh
cargo test -p ferriboot-build --features cli
```
