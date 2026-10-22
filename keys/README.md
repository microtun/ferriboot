# Development signing keys

Development keys are **not committed**; generate them locally. They are
unprotected files in your working tree: **never use them for production.**

| File | Purpose |
|---|---|
| `keys/dev-p256.pem`, `keys/dev-p256.pub.pem` | Ferriboot (MCUboot) image signing, all boards |
| `boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072*.pem` | ESP32-C3 Secure Boot V2 |
| `boards/rp2350/keys/dev-rp2350-rom-secp256k1*.pem` | RP2350 ROM secure boot (HIL only) |

```sh
python3 -m pip install cryptography
python3 tools/generate-dev-keys.py                              # create missing keys
python3 tools/generate-dev-keys.py --force                      # replace existing keys
python3 tools/generate-dev-keys.py --output-dir /tmp/dev-keys   # elsewhere, same layout
```

CI generates fresh keys in every job and never uploads them.

**Replacing keys breaks existing signed images and provisioned boards.**
ESP32-C3 eFuse and RP2350 OTP secure-boot provisioning is irreversible and
binds the board to the key that signed its first boot chain.

For keys kept outside the repository, set `FERRIBOOT_KEY` and
`FERRIBOOT_PUBKEYS` (MCUboot), `ESP_SBV2_KEY` (ESP32-C3) or
`RP2350_SECURE_BOOT_KEY` (RP2350). Read the board README before provisioning
hardware.
