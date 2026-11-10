# Development signing keys

No keys are committed to the repository; generate them locally. They are
unencrypted files: **never use them for production.**

| File | Purpose |
|---|---|
| `keys/dev-p256.pem`, `keys/dev-p256.pub.pem` | Application image signing (MCUboot format), all boards |
| `boards/esp32c3-supermini/keys/dev-esp32c3-sbv2-rsa3072*.pem` | ESP32-C3 Secure Boot V2 |
| `boards/rp2350-pico2/keys/dev-rp2350-rom-secp256k1*.pem` | RP2350 ROM secure boot (HIL only) |

## Image signing key

Requires OpenSSL:

```sh
./keys/generate.sh
```

This writes a PKCS#8 P-256 private key and an SPKI public key to `keys/`. The
script never overwrites existing files and restricts the private key to the
current user.

By default, builds trust `keys/dev-p256.pub.pem` and the signing tools sign
with `keys/dev-p256.pem`. These are file paths, not bundled keys, so a build
or signing step fails if its file is missing.

To keep the pair outside the repository, pass an output directory and point
the tools at it:

```sh
./keys/generate.sh "$HOME/secure-keys/mcuboot"
export FERRIBOOT_KEY="$HOME/secure-keys/mcuboot/dev-p256.pem"
export FERRIBOOT_PUBKEYS="$HOME/secure-keys/mcuboot/dev-p256.pub.pem"
```

## Board keys

```sh
ferriboot-tools-esp keys       # ESP32-C3 Secure Boot V2 key
ferriboot-tools-rp2350 keys    # RP2350 ROM key
```

Install the tools with `cargo install --locked --path ferriboot-esp/tools` or
`cargo install --locked --path ferriboot-rp/tools`. Each command creates only
its own key pair and accepts `--force` and `--output-dir`. To use keys stored
elsewhere, set `ESP_SBV2_KEY` (ESP32-C3) or `RP2350_SECURE_BOOT_KEY` (RP2350).

## Replacing keys

**Replacing keys breaks existing signed images and provisioned boards.**
ESP32-C3 eFuse and RP2350 OTP secure-boot provisioning is irreversible and
binds the board to the key that signed its first boot chain. Read the board
README before provisioning hardware.

CI generates fresh keys in every job and never uploads them.
