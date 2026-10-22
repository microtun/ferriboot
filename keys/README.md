# Development signing keys

All bundled keys are **for development only**. The shared Ferriboot MCUboot
payload-signing P-256 pair stays in `keys/`; board ROM secure-boot keys live
with their boards:

- `keys/dev-p256.pem` and `keys/dev-p256.pub.pem` — Ferriboot payload signing.
- `boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072*.pem` — ESP32-C3 Secure Boot V2.
- `boards/rp2350/keys/dev-rp2350-rom-secp256k1*.pem` — RP2350 ROM secure boot.

Generate new RSA-3072, P-256, and secp256k1 key pairs in a separate directory
using the same layout:

```sh
python3 -m pip install cryptography
python3 tools/generate-dev-keys.py --output-dir /tmp/ferriboot-dev
```

To replace all six development PEM files **in their repository locations**:

```sh
python3 tools/generate-dev-keys.py --force
```

**Warning:** Replacing keys can break existing signed images and provisioned
boards. The included private keys are public; never use them for production.
ESP32-C3 eFuse and RP2350 OTP secure-boot provisioning is irreversible.

For private keys stored outside this repository, configure `ESP_SBV2_KEY`
(ESP32-C3 RSA), `FERRIBOOT_KEY` and `FERRIBOOT_PUBKEYS` (MCUboot), or
`RP2350_SECURE_BOOT_KEY` (RP2350 ROM). Read the relevant board README before
provisioning hardware.
