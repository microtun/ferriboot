# Development signing keys

Development keys are **not committed**; generate them locally. They are
unprotected files in your working tree: **never use them for production.**

| File | Purpose |
|---|---|
| `keys/dev-p256.pem`, `keys/dev-p256.pub.pem` | Ferriboot (MCUboot) image signing, all boards |
| `boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072*.pem` | ESP32-C3 Secure Boot V2 |
| `boards/rp2350/keys/dev-rp2350-rom-secp256k1*.pem` | RP2350 ROM secure boot (HIL only) |

```sh
ferriboot-tools keys                              # MCUboot key (all boards)
ferriboot-tools-esp keys                          # ESP32-C3 Secure Boot V2 key
ferriboot-tools-rp2350 keys                       # RP2350 ROM key
ferriboot-tools keys --force                      # replace existing keys
ferriboot-tools keys --output-dir /tmp/dev-keys   # elsewhere, same layout
```

Install the binaries with `cargo install --locked --path ferriboot-tools` (and likewise
`ferriboot-esp/tools` or `ferriboot-rp/tools`). Each one
creates only its own key pair, all or nothing, and accepts `--force` and
`--output-dir`.

CI generates fresh keys in every job and never uploads them.

**Replacing keys breaks existing signed images and provisioned boards.**
ESP32-C3 eFuse and RP2350 OTP secure-boot provisioning is irreversible and
binds the board to the key that signed its first boot chain.

For keys kept outside the repository, set `FERRIBOOT_KEY` and
`FERRIBOOT_PUBKEYS` (MCUboot), `ESP_SBV2_KEY` (ESP32-C3) or
`RP2350_SECURE_BOOT_KEY` (RP2350). Read the board README before provisioning
hardware.
