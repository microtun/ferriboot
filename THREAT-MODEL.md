# Ferriboot Threat Model
## Purpose and basis

Ferriboot guarantees that only application images signed by a trusted ECDSA P-256 key, and permitted by the configured policy, are executed by the boot chain it is part of. This document states which attacks that guarantee covers, which it does not, and what an integrator must provide for it to hold in production.

It is written for firmware integrators, security reviewers and product owners deciding whether Ferriboot fits a threat model.

Terms used throughout: **slot** is a flash area holding one MCUboot-format image; **trailer** is the MCUboot metadata at the end of a slot; **security counter** is the hardware-backed monotonic rollback floor (`SEC_CNT`); **handoff** is the final jump from Ferriboot into the application.

## System overview

Ferriboot is the last link in a chain of signature checks rooted in each chip's mask ROM. It authenticates the application image, applies policy and the rollback floor, performs any pending upgrade, and hands control to the application.

| Stage | ESP32 | RP2350 |
| --- | --- | --- |
| 1. Mask ROM | Verifies the ESP-IDF second stage (Secure Boot V2, RSA-3072); anchor: key digest in eFuse | Verifies Ferriboot (secp256k1); anchor: BOOTKEY0 hash in OTP, burned on first boot |
| 2. Second stage | ESP-IDF second stage verifies Ferriboot with the same Secure Boot V2 key; reads the unsigned partition table | None; the ROM verifies Ferriboot directly |
| 3. Ferriboot | Runs as the factory application; validates the image (P-256, policy, eFuse counter), performs upgrades, re-authenticates before handoff | Runs from SRAM; validates the image (P-256, policy, OTP counter), performs upgrades, locks OTP pages 1, 2 and 32, re-authenticates before handoff |
| 4. Handoff | RAM loader in protected IRAM copies RAM segments and programs the MMU, re-reading flash after the final check | Maps the selected slot into the application window and jumps to the reset vector |
| 5. Application | Full privilege; can write flash and eFuses | Arm Secure; can write flash and any OTP page not locked at handoff |

On ESP32 the ROM's guarantee reaches Ferriboot through the ESP-IDF second stage; on RP2350 the ROM checks Ferriboot directly. Both chips execute the application from external QSPI flash.

| Trust anchor | Stored in | Protects |
| --- | --- | --- |
| ESP Secure Boot V2 RSA-3072 key digest | ESP32 eFuse | IDF second stage and Ferriboot |
| RP2350 secp256k1 key hash (BOOTKEY0) | RP2350 OTP page 2 | Ferriboot |
| P-256 key ring | Compiled into Ferriboot only | Application images |
| Security counter | ESP32 eFuse `SECURE_VERSION` (16 epochs); RP2350 OTP page 32 (504 epochs) | Rollback floor for application images |

Ferriboot also consumes inputs that are **not** authenticated: slot trailers (upgrade requests, confirmation, swap progress), the image header before validation, and on ESP32 the partition table. Each limitation that follows from this is listed under Known limitations.

## Security objectives in scope

Ferriboot commits to seven properties. Each holds only under the attacker model and integrator responsibilities described later in this document.

| # | Objective | Mechanism | When enforced |
| --- | --- | --- | --- |
| O1 | Only authentic images execute | SHA-256 over header, body and protected TLVs; ECDSA P-256 signature checked against a compiled-in key ring, selected by `KEYHASH` or `PUBKEY` TLV | Before an image is installed, and again immediately before handoff |
| O2 | Signed metadata cannot be swapped | TLVs outside the signed area are allow-listed; `SEC_CNT`, VID and CID are accepted only from the protected area | During every image validation |
| O3 | Images are bound to a product class | Protected imgtool VID/CID UUIDs must match the board's image policy | During every image validation |
| O4 | Hardware rollback floor | Monotonic eFuse (ESP32) or OTP (RP2350) counter; images below the floor are rejected; burned for selected permanent/confirmed images or for confirmations preserved before swap/overwrite; always after re-authentication | On every boot, before each counter burn, and at handoff |
| O5 | Malformed executables are rejected | ESP segment plan and Cortex-M vector checks confine RAM loads, MMU maps, stack and entry point | Before an image is installed, and again at handoff |
| O6 | Power-loss safety of upgrades | Restartable swap, overwrite and direct-XIP state machines; trailer destroyed back to front | During upgrades and on resume after power loss |
| O7 | Bootloader remains the only firmware execution authority | The application updater holds no trusted keys and can stage invalid or policy-rejected candidates; Ferriboot independently authenticates signatures, policy and rollback counter before installing or executing an image | At boot selection and again before handoff |

Version-based downgrade prevention (`downgrade-prevention` feature) is **not** listed as an objective. It protects only the legitimate update path; see Known limitations.

## Attacker model

Ferriboot fully defends against attackers who can only supply image bytes; it partially defends against attackers who already run code on the device or hold it physically. No attacker in any class is assumed to hold a trusted signing key.

| Attacker | Assumed capability | Ferriboot prevents | Ferriboot does not prevent | Coverage |
| --- | --- | --- | --- | --- |
| A1 Remote image supplier | Delivers arbitrary bytes through the application's update transport | Booting unsigned or wrong-class images; executing images below the bootloader version or counter floor | Aborting transfers, filling the inactive slot, flash wear through repeated erase cycles | In scope |
| A2 Compromised application | Code execution in the running app with full privilege (RISC-V M-mode, Arm Secure) and raw flash write | Persisting unsigned code across reset; booting below the hardware security counter floor; on RP2350, writing OTP pages 1, 2 and 32 | Reinstalling an **older signed** image whose `SEC_CNT` is at or above the floor; bricking the device; on ESP32, burning eFuses including `SECURE_VERSION` | Partial |
| A3 Physical, interfaces only | ROM download mode, BOOTSEL/PICOBOOT, or SWD/JTAG where not disabled; no probing of the flash bus | Same as A2 once the ROM secure-boot chain is enrolled and debug is disabled | Everything, while debug access remains enabled (dev profiles) | Partial |
| A4 Physical, flash bus | Flash interposer or emulator able to serve different data on successive reads | Nothing beyond A3 | Substituting code after validation (time-of-check/time-of-use) | Out of scope |
| A5 Fault injection | Voltage, clock, EM or laser glitches timed against the bootloader | Raises the number of precisely placed faults needed at the handoff and at counter burns | A determined, resynchronising attacker | Best effort |
| A6 Key holder | Access to a trusted P-256 signing key or ROM signing key | Nothing | Everything | Out of scope |

## Out of scope

The following are explicitly not provided by Ferriboot and must be covered elsewhere if the product needs them.

- **Confidentiality.** Images are not encrypted; encrypted and compressed images are rejected. Flash contents, including application secrets, are readable by anyone with flash access.
- **Key management.** Generation, storage, rotation and revocation of P-256, ESP Secure Boot V2 RSA-3072 and RP2350 secp256k1 keys.
- **Transport security.** Authentication, encryption and rate limiting of the channel that delivers updates. The upload session API has no authentication of its own.
- **Runtime protection of the application.** Once the handoff completes, the application runs with full privilege; Ferriboot offers no isolation, measurement or attestation after that point.
- **Physical flash-bus attacks.** Interposers and flash emulators (attacker A4).
- **Side channels.** Power and EM analysis of signature verification. Verification uses only public keys, so no secret is at stake in Ferriboot itself.
- **Signature schemes other than ECDSA P-256 / SHA-256**, including RSA, Ed25519, `--pure` signatures, multi-image dependencies and RAM-load mode.

## Fault-injection hardening

Ferriboot hardens only the two irreversible steps, the jump into the application and each security-counter burn. Every other check is a single software decision that one well-placed fault can flip.

At those two steps the image is authenticated a second time, independently of the boot decision, and the board re-derives what will run: the segment and MMU plan on ESP32, the XIP translation and vector table on RP2350. The rollback floor is read twice. The combined verdict is a 32-bit pattern consumed by a hardened check that evaluates it three times behind a TRNG-randomised delay of up to 4,096 loop iterations. Failure diverges into a repeated trap that resets the device.

Limits of this hardening:

- The compiler hints that keep redundant checks apart do not guarantee machine-code structure; the compiler can still merge or outline checks. Resistance must be measured with instruction-skip campaigns against the shipped binary.
- The random delay does not stop an attacker who resynchronises on the power or EM trace.
- There is no persistent lockout after repeated detections, so attempts are bounded only by time.
- Wrong rejections are not hardened. A fault that rejects a valid image fails closed (denial of service).
- Primary-slot rejection, which triggers a destructive bootstrap overwrite, is confirmed by a second validation; other erase decisions are not.

Hardware detectors complement the software:

| Board | Detector | Coverage | Can the application disable it? |
| --- | --- | --- | --- |
| ESP32 | Clock glitch reset only | From IDF second stage onward; ROM unprotected | Yes, no lock exists |
| RP2350 | Four power/clock glitch detectors, armed from OTP after enrollment | From power-on, including ROM signature check | No, configuration locked until reset |

## Known limitations

The hardware security counter is the only rollback control that survives an attacker with flash write access; everything version-based below it can be bypassed by attacker A2. The table lists every limitation identified, core first, then per board.

| ID | Applies to | Limitation | Consequence | Mitigation available |
| --- | --- | --- | --- | --- |
| L1 | Core, swap mode | A revert skips the version check and is selected purely from the primary trailer, which sits in its own sectors. A forged mid-swap status is resumed without any candidate check. | A2 can reinstall any older signed image and have it marked permanent | Bump `SEC_CNT` on every security-fix release |
| L2 | Core, all modes | The downgrade reference version is read from the primary header, which is not authenticated at that point | A2 can erase the header to defeat the check | Same as L1 |
| L4 | Both boards | Images are validated, then executed from or re-read out of external flash | A4 can substitute code after validation | Flash encryption or in-package flash; physical protection |
| L5 | Both boards | No hardware anti-rollback for Ferriboot itself. On ESP32 `SECURE_VERSION` is consumed by the application counter and IDF anti-rollback is disabled; on RP2350 the ROM rollback version is unused | An older, vulnerable Ferriboot signed with the same root key can be reflashed | Plan key revocation (ESP Secure Boot V2 digest slots) or ROM rollback versions before the first Ferriboot fix ships |
| L6 | Both boards | Default builds embed the committed development P-256 key when `FERRIBOOT_PUBKEYS` is unset, and host tools sign with its public private half | A production build can silently trust a public key | Set `FERRIBOOT_PUBKEYS`; verify the key ring of every release binary |
| L7 | ESP32 | Slot locations come from the unsigned partition table. Only flash below the table end is reserved, so slots may overlap the `ferriboot`, `nvs`, `phy_init` or `storage` partitions | A corrupted or edited table turns Ferriboot's erase and copy operations against its own partition or application data | Write-protect or verify the table at manufacture |
| L8 | ESP32 | Flash encryption is unsupported; the RAM loader reads segments with raw ROM flash reads; neither IDF profile disables ROM download mode | Flash is readable and writable over USB/UART; enabling encryption later requires loader changes | Manufacturing lockdown of download mode |
| L9 | ESP32 | The counter has 16 epochs for the product's lifetime, and eFuses stay programmable by the application | A2 can exhaust the epoch budget or burn unrelated eFuses | Reserve epochs for security releases only |
| L10 | RP2350 | Enrollment trusts whatever key signed the running Ferriboot (trust on first use) and runs unconditionally; without `production` the default OTP policy keeps SWD and spare ROM key slots open; the host-tool acknowledgement is bypassed by flashing directly | A fresh board can be irreversibly locked to the public development ROM key or to an unintended key | Build with `--features production` and provision only in a controlled environment using an approved ROM signing key |
| L11 | RP2350 | BOOTSEL/PICOBOOT stays enabled; OTP page locks last only until reset; OTP pages other than 1, 2 and 32 are unprotected | A3 retains flash access; other OTP data is writable by A2 | Burn permanent page locks and disable BOOTSEL as a manufacturing step |

### Resolved: direct-XIP counter pre-selection burn (formerly L3)

Direct-XIP previously advanced the hardware counter for any authenticated slot
with permanent/confirmed semantics **before** determining which image would
run. This could make a newer-version image (e.g. version 2, `SEC_CNT=1`)
unbootable because an unselected, older-version image (version 1,
`SEC_CNT=2`) raised the rollback floor. Direct-XIP now selects and validates
an image first; **only that selected image** may advance the counter, and only
when permanent (direct-XIP without revert) or confirmed (`image_ok` in
XIP/revert). An unconfirmed trial never burns the counter. The persistent
floor is re-read and enforced before handoff. Swap/overwrite still consume
existing confirmations before modifying trailers. Covered by regressions in
`ferriboot-core/tests/security_counter.rs` for both direct-XIP variants.

This change does not remove the obligation to assign sensible monotonically
increasing versions and security counters to security-fix releases: once a
**selected** image raises the floor, any older-counter images remain blocked.

## Integrator responsibilities

The objectives above hold in production only if every item below is done. Each one closes an assumption Ferriboot makes but cannot enforce itself.

**All boards**

- [ ] Generate production P-256, Secure Boot V2 and ROM signing keys; keep private halves off build machines and out of the repository.
- [ ] Build with `FERRIBOOT_PUBKEYS` set, and confirm the dev key hash is absent from every release key ring.
- [ ] Provision trusted signing keys and image policy only in the bootloader; verify that the application firmware does not embed a redundant signing-key ring.
- [ ] Increment `SEC_CNT` in every release that fixes a vulnerability.
- [ ] Authenticate and rate-limit the update transport in the application.
- [ ] Decide how a Ferriboot vulnerability would be remediated without hardware bootloader rollback (L5).

**ESP32**

- [ ] Use the `release` IDF profile so JTAG is disabled and unused Secure Boot V2 digest slots are revoked.
- [ ] Lock down ROM download mode at manufacture.
- [ ] Keep IDF `APP_ANTI_ROLLBACK` disabled; `SECURE_VERSION` belongs to Ferriboot.
- [ ] Verify the flashed partition table matches the board's reference partition layout at end of line.
- [ ] Budget the 16 available rollback epochs across the product lifetime.

**RP2350**

- [ ] Build with `--features production`, leaving default features enabled (`downgrade-prevention`, `bootstrap`, `uuid`), and provision only in a controlled environment. `production` adds hardened OTP lockdowns only; if using `--no-default-features`, explicitly re-enable any required policy features. Cargo `--release` alone still uses development OTP policy.
- [ ] Reserve OTP page 32 (rows `0x800`–`0x83f`) exclusively for the counter, raw mode.
- [ ] Disable BOOTSEL/PICOBOOT and burn permanent OTP page locks where the product allows it.
- [ ] Validate on a sacrificial board that the supply and clock never trip the glitch detectors before enrolling production units.