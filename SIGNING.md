# Signing with microtun-signer (optional)

Ferriboot's host-side image tools support [microtun-signer](https://github.com/microtun/microtun-signer) **in addition to** existing private-PEM signing. Remote signing keeps private keys on the signing service. Nothing in the embedded firmware needs to contact the service.

## Select a signer

Install the `microtun-signer` command-line client and configure access to the service (for example, a `MICROTUN_SIGNER_TOKEN` bearer token for GitHub Actions). Export:

```sh
export MICROTUN_SIGNER_URL=https://signer.example.com/
export MICROTUN_SIGNER_KEY_ID=my-p256-firmware-key
# One possible authentication mechanism:
export MICROTUN_SIGNER_TOKEN="$TOKEN"
# Or, for an interactive user, set MICROTUN_SIGNER_GITHUB_LOGIN=1.
```

`MICROTUN_SIGNER` can override the CLI executable name/path. When both `MICROTUN_SIGNER_URL` and `MICROTUN_SIGNER_KEY_ID` are present, host signing uses the service instead of reading a local private PEM. Both variables must be set. A remote signing failure is an error: the tools **never silently fall back to a development key**. Without those variables, the existing PEM signing flow is unchanged.

**Recommended:** Pin the service's expected public signing key before releasing any binaries:

```sh
microtun-signer public-key --url "$MICROTUN_SIGNER_URL" \
    --key-id "$MICROTUN_SIGNER_KEY_ID" > firmware-p256.pub.pem
export MICROTUN_SIGNER_EXPECTED_PUBLIC_KEY="$PWD/firmware-p256.pub.pem"
```

The tools compare the returned SubjectPublicKeyInfo with this file before signing. The file is public (no private signing PEM needed). Use separate key IDs, and corresponding expected-public-key files, for different algorithms.

## Firmware: MCUboot images (ESP32-C3, RP2350, QEMU)

Use a **P-256 / secp256r1** key in microtun-signer. All existing Ferriboot bootloaders verify P-256; no new embedded signature format is introduced. The host tool exports imgtool's SHA-256 digest, signs it remotely, verifies the returned fixed-width `r || s` signature with the service's public key, converts it to DER, assembles the final image with `imgtool --fix-sig`, and independently verifies the signed image.

To trust this signing key on-device, embed its public key in the **Ferriboot bootloader build** using the existing build-script setting:

```sh
export FERRIBOOT_PUBKEYS="$PWD/firmware-p256.pub.pem"
# Rebuild the appropriate boards/<board>/ferriboot binary with this setting.
```

`FERRIBOOT_PUBKEYS` also accepts a comma-separated list of P-256 public keys for key rotation. Signing firmware with a key not already trusted by the device's bootloader will result in an unbootable image.

The board tool `sign` and `sign-raw` commands additionally expose `--signer-url`, `--signer-key-id`, and `--signer-github-login` arguments; with the environment variables set, these arguments are optional. `flash-app` (ESP32-C3) and `load-app` (RP2350) use the environment variables directly. Set `IMAGE_VERSION`, `SECURITY_COUNTER`, `VID`, and `CID` as before.

## ESP32-C3 ROM Secure Boot V2 boot chain

Use a **3072-bit RSA-PSS** key (SHA-256, 32-byte salt). Change the key ID and expected public key file to the RSA key before running the ROM signing commands. The host verifies the returned RSA-PSS signature, emits a standard ESP32 Secure Boot V2 signature sector, then verifies the result. `flash-ferriboot` signs **both** the IDF second-stage bootloader and Ferriboot with the same configured RSA key; their ROM public-key fields are checked for equality. `sign-sbv2` signs an individual image.

```sh
export MICROTUN_SIGNER_KEY_ID=my-esp-rsa3072-key
export MICROTUN_SIGNER_EXPECTED_PUBLIC_KEY="$PWD/esp-rsa3072.pub.pem"
# Then use ferriboot-tools-esp sign-sbv2 or flash-ferriboot as usual.
```

Make sure the device's actual secure-boot eFuse digest corresponds to this **ROM RSA key**, not the separate MCUboot P-256 application signing key. Consult the ESP board's provisioning instructions before burning irreversible fuses.

## RP2350 ROM bootloader sealing

Use a **secp256k1** key for the RP2350 ROM stage. `sign-bootloader` and `load-bootloader` fetch its public key and request a signature over the RP2350 ROM SHA-256 digest generated directly in Rust. The host converts the service's fixed-width ECDSA signature to DER, verifies it, reseals the image and checks the RP2350 ROM metadata and optional OTP provisioning plan.

```sh
export MICROTUN_SIGNER_KEY_ID=my-rp2350-secp256k1-key
export MICROTUN_SIGNER_EXPECTED_PUBLIC_KEY="$PWD/rp2350-secp256k1.pub.pem"
# Then use ferriboot-tools-rp2350 sign-bootloader or load-bootloader.
```

## Local PEM fallback

Unset both `MICROTUN_SIGNER_URL` and `MICROTUN_SIGNER_KEY_ID` (and optionally the token and expected-public-key setting) to use the original private PEM behavior. Key options and defaults remain:

| Image | Local key | Algorithm |
|---|---|---|
| MCUboot firmware | `FERRIBOOT_KEY` / `--key` | P-256 |
| ESP32-C3 Secure Boot V2 stages | `ESP_SBV2_KEY` / `--key` | RSA-3072 |
| RP2350 ROM bootloader | `RP2350_SECURE_BOOT_KEY` / `--key` | secp256k1 |

For a typical release, you will use **two separate remote key IDs**: one for MCUboot application firmware and one for the board-specific ROM bootloader. Configure and verify each key's public trust anchor *before* flashing or provisioning devices.
