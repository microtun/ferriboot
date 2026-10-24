# Development signing keys

Development keys are **not committed**; generate them locally. Neither the
private nor the public halves are supplied by the repository. Locally generated
private keys are unencrypted files: **never use them for production.**

| File | Purpose |
|---|---|
| `keys/dev-p256.pem`, `keys/dev-p256.pub.pem` | Ferriboot (MCUboot) image signing, all boards |
| `boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072*.pem` | ESP32-C3 Secure Boot V2 |
| `boards/rp2350/keys/dev-rp2350-rom-secp256k1*.pem` | RP2350 ROM secure boot (HIL only) |

Generate the shared MCUboot key pair from the repository root (requires
OpenSSL):

```sh
./keys/generate.sh
```

The script works from any current directory and writes to `keys/` by default.
It refuses to overwrite either existing file, restricts key permissions to
the current user, and validates the private key before publishing the pair.
This produces an unencrypted PKCS#8 P-256 private key and an SPKI public key,
the formats used by the signing tools and build scripts.

When `FERRIBOOT_PUBKEYS` is unset, Ferriboot builds load the locally generated
`keys/dev-p256.pub.pem` as their default trust anchor. When `FERRIBOOT_KEY` is
unset and remote signing is not configured, host signing tools use the local
`keys/dev-p256.pem` by default. These are **file-path defaults, not bundled
keys**: the operation fails if its required file is missing. Development
private keys must stay private even though their public halves are embedded
in development bootloaders.

To keep the pair outside the repository, pass an output directory:

```sh
./keys/generate.sh "$HOME/secure-keys/mcuboot"
export FERRIBOOT_KEY="$HOME/secure-keys/mcuboot/dev-p256.pem"
export FERRIBOOT_PUBKEYS="$HOME/secure-keys/mcuboot/dev-p256.pub.pem"
```

The signing tools also enforce the MCUboot key format when loading it.

Board-specific development keys still use the board tools:

```sh
ferriboot-tools-esp keys                         # ESP32-C3 Secure Boot V2 key
ferriboot-tools-rp2350 keys                      # RP2350 ROM key
```

Install those binaries with `cargo install --locked --path ferriboot-esp/tools`
or `cargo install --locked --path ferriboot-rp/tools`. Each creates only its
own key pair and accepts `--force` and `--output-dir`.

CI generates fresh keys in every job and never uploads them.

**Replacing keys breaks existing signed images and provisioned boards.**
ESP32-C3 eFuse and RP2350 OTP secure-boot provisioning is irreversible and
binds the board to the key that signed its first boot chain.

For keys kept outside the repository, set `FERRIBOOT_KEY` and
`FERRIBOOT_PUBKEYS` (MCUboot), `ESP_SBV2_KEY` (ESP32-C3) or
`RP2350_SECURE_BOOT_KEY` (RP2350). Read the board README before provisioning
hardware.
