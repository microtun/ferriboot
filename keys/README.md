# Development signing keys

Development keys are **not committed**; generate them locally. They are
unprotected files in your working tree: **never use them for production.**

| File | Purpose |
|---|---|
| `keys/dev-p256.pem`, `keys/dev-p256.pub.pem` | Ferriboot (MCUboot) image signing, all boards |
| `boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072*.pem` | ESP32-C3 Secure Boot V2 |
| `boards/rp2350/keys/dev-rp2350-rom-secp256k1*.pem` | RP2350 ROM secure boot (HIL only) |

Generate the shared MCUboot key pair with OpenSSL from the repository root.
The subshell refuses to overwrite existing files and restricts private-key
permissions:

```sh
(
    set -euC
    umask 077
    mkdir -p keys
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 > keys/dev-p256.pem
    openssl pkey -in keys/dev-p256.pem -pubout > keys/dev-p256.pub.pem
)
```

This produces an unencrypted PKCS#8 P-256 private key and an SPKI public key,
the formats used by the signing tools and build scripts. To check an existing
private key:

```sh
openssl pkey -in keys/dev-p256.pem -check -noout
```

The signing tools also enforce the MCUboot key format when loading it.
For a pair outside the repository, change the paths in the generation commands
and set the environment variables below.

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
