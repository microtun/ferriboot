# ESP32-C3 SuperMini

With Secure Boot V2, the signed second-stage bootloader must fit in 48 KiB,
which is too small for Ferriboot. Ferriboot therefore runs as a **third
stage**:

1. The mask ROM verifies the ESP-IDF v5.5.5 second stage at `0x0`
   (Secure Boot V2, RSA-3072).
2. The second stage verifies Ferriboot, a factory app at `0x20000` signed
   with the **same** RSA-3072 key.
3. Ferriboot verifies the application (MCUboot format, ECDSA P-256) in
   `image-0` or `image-1` and starts it through a small RAM handoff loader.

> **Flashing the boot chain is irreversible.** On first boot the second stage
> burns its signing key digest into eFuse and enables Secure Boot V2. From
> then on the chip only boots images signed with that key.

## Requirements and limitations

- ESP32-C3 **rev v0.3 (ECO3) or later** with **4 MiB** flash.
- Secure Boot V2 is always enabled; there is no unsigned boot chain.
- Flash encryption is not supported, and ROM download mode is left enabled.
- Only the **clock** glitch detector is used. Ferriboot re-enables and checks
  it on every boot, but the ROM runs without it and the application can turn
  it off. See [fault-injection hardening](../../THREAT-MODEL.md#fault-injection-hardening).
- The RAM loader copies segments from flash after the final check, so a flash
  interposer is out of scope.

On any error (flash, eFuse, configuration, image validation or panic)
Ferriboot resets after a two-second delay instead of halting. A persistent
problem therefore causes a reboot loop, but never boots an image that fails
verification. The RTC watchdog also resets stalled work.

## Flash layout

Defined in `layout/partitions.csv`:

| Region | Offset | Size |
|---|---|---|
| ESP-IDF second stage (signed) | `0x0` | ≤ 48 KiB |
| Partition table | `0x10000` | 4 KiB |
| `nvs`, `phy_init` (unused) | `0x11000` | 24 KiB |
| `ferriboot` (factory app) | `0x20000` | 256 KiB |
| *(unallocated)* | `0x60000` | 60 KiB |
| `image-0` (primary) | `0x6F000` | 1764 KiB |
| *(unallocated)* | `0x228000` | 28 KiB |
| `image-1` (secondary) | `0x22F000` | 1764 KiB |
| `image-scratch` | `0x3E8000` | 64 KiB |
| `storage` (application-owned) | `0x3F8000` | 32 KiB |

Slots start at `…F000` so the ESP image after the 4 KiB MCUboot header lands
on a 64 KiB MMU page. Ferriboot finds slots **by label at runtime**, so
changing the partitions needs no rebuild, only re-signing if slot capacity or
alignment changes.

The partition table offset is set in `.cargo/config.toml`
(`ESP_BOOTLOADER_ESP_IDF_CONFIG_PARTITION_TABLE_OFFSET`) and must match the
second stage's `sdkconfig.defaults.*`. Check a layout with:

```sh
ferriboot-tools-esp layout                           # layout/partitions.csv
ferriboot-tools-esp layout --partitions other.csv    # another CSV
```

Adding `--table partitions.bin` also **writes** the binary partition table to
that file.

## Setup

- Rust **1.95+** with the `riscv32imc-unknown-none-elf` target.
- Upstream `imgtool` (needs Python).
- Docker with BuildKit, to build the ESP-IDF second stage. Alternatively
  supply an unsigned one with `ESP_IDF_BOOTLOADER_BIN`.
- The chip's native USB-Serial-JTAG port.
- Development keys; see [keys/README.md](../../keys/README.md).

No `espflash`, `esptool` or `espsecure` CLI is needed: the tool signs and
flashes by itself.

```sh
rustup target add riscv32imc-unknown-none-elf
python3 -m pip install imgtool
cargo install --path ferriboot-esp/tools --locked
./keys/generate.sh          # application signing key
ferriboot-tools-esp keys    # Secure Boot V2 key
```

## Flash a development board

Flash the application first, then the boot chain. Run Cargo from each
binary's directory:

```sh
export ESPPORT=/dev/ttyACM0
cd boards/esp32c3-supermini/firmware
IMAGE_VERSION=1.0.0 cargo run --release   # press Ctrl+C to leave the monitor

cd ../ferriboot
FERRIBOOT_OTP_BURN=YES cargo run --release
```

The bootloader runner refuses to flash unless `FERRIBOOT_OTP_BURN=YES` is
set, as an acknowledgement of the irreversible eFuse burn. Plan provisioning
first; see the
[ESP-IDF Secure Boot V2 guide](https://docs.espressif.com/projects/esp-idf/en/v5.5.5/esp32c3/security/secure-boot-v2.html).

After flashing the application, the tool resets the chip and opens a
115200-baud serial monitor.

To update the application later, rerun the firmware command with a new
`IMAGE_VERSION`. It erases the entire primary slot and writes the signed
image. `SLOT=image-1` writes the secondary slot instead, but does **not**
mark an upgrade pending.

The example firmware has no MCUmgr/SMP server. A real update flow needs your
own transport and the
[updater API](../../ferriboot-core/README.md#updating-from-an-application).

## Build and sign without flashing

```sh
# Ferriboot and the IDF second stage, built and signed.
cd boards/esp32c3-supermini/ferriboot
NO_FLASH=1 cargo run --release

# Example firmware.
cd ../firmware
cargo build --release
IMAGE_VERSION=1.0.0 ferriboot-tools-esp sign \
  ../../../target/riscv32imc-unknown-none-elf/release/esp32c3-supermini-firmware \
  ../../../target/riscv32imc-unknown-none-elf/release/esp32c3-supermini-firmware.signed.bin
```

Outputs, in `target/riscv32imc-unknown-none-elf/release/` at the repository
root:

| File | Flash address |
|---|---|
| `bootloader.idf.sbv2.bin` | `0x0` |
| `partitions.bin` | `0x10000` |
| `ferriboot.sbv2.bin` | `0x20000` |
| `boot-chain.sbv2.bin` (the three above merged) | `0x0` |
| `esp32c3-supermini-firmware.signed.bin` | `image-0`, `0x6F000` |

`ferriboot-tools-esp` subcommands: `sign`, `sign-raw`, `inspect`,
`sign-sbv2`, `verify-sbv2`, `build-bootloader`, `flash-app`,
`flash-bootloader`, `layout` and `keys`. `sign-raw` signs an arbitrary payload
with this board's image geometry instead of converting an ELF. Extra `imgtool`
arguments can follow `sign ELF OUTPUT --`.

### Flash a signed release binary

To flash an already signed boot chain, such as a CI release artifact, pass
`--signed-bin`:

```sh
FERRIBOOT_OTP_BURN=YES ferriboot-tools-esp flash-bootloader --signed-bin \
  esp32c3-supermini-release-boot-chain.sbv2.bin
```

This verifies both Secure Boot V2 signatures and the partition table, then
flashes the chain at `0x0` without re-signing, so no signing key is needed.
The flag is required for any signed binary; without it the command expects an
ELF. With `NO_FLASH=1` it only verifies.

## Writing an application

Use the example firmware as a template. An application must:

- Target `riscv32imc-unknown-none-elf` and depend on
  [`ferriboot-esp`](../../ferriboot-esp/README.md) with the `esp32c3` feature
  and without `boot-runtime`, which is for the bootloader only.
- Call `ferriboot_build::configure_esp_application()` in `build.rs`. Do not
  call `emit_public_keys`: only the bootloader embeds verification keys.
- Keep RAM segments clear of the handoff loader's protected region; the tools
  and Ferriboot reject overlaps.
- Re-arm a watchdog early, for example with
  `ferriboot_esp::watchdog::start_rtc`. Ferriboot hands off with a freshly fed
  15-second RTC watchdog, but `esp_hal::init` disables it.
- Read `BootInfo::read()`, find the slots with `ferriboot_esp::read_layout`
  and construct the updater with `ferriboot_esp::updater`, without keys or
  policy. The upgrade mode comes from the bootloader's handoff.
- Confirm a trial upgrade only after its own health checks (the example
  confirms unconditionally). A reset before confirmation reverts under swap
  and `direct-xip-revert`.

Do not ship the example firmware: it always includes the unauthenticated
[test hooks](../../testing/ferriboot-test-hooks/README.md).

## Configuration

| Variable | Purpose |
|---|---|
| `ESP_SBV2_KEY` | RSA-3072 key for the second stage and Ferriboot (default: dev key) |
| `FERRIBOOT_PUBKEYS` | Comma-separated P-256 public keys built into Ferriboot |
| `FERRIBOOT_KEY` | P-256 private key for application signing |
| `MICROTUN_SIGNER_URL`, `MICROTUN_SIGNER_KEY_ID` | Optional remote signing; see [SIGNING.md](../../SIGNING.md) |
| `ESP_BOOTLOADER_PROFILE` | `dev` (default; JTAG and spare digest slots stay open) or `release` (disables JTAG, revokes spare slots) |
| `ESP_IDF_BOOTLOADER_BIN` | Absolute path to an unsigned IDF second stage; skips Docker |
| `ESPPORT` | Serial port; auto-detected if only one ESP32-C3 is attached |
| `FERRIBOOT_OTP_BURN=YES` | Required to flash the boot chain |
| `NO_FLASH=1` | Build and sign without erasing or flashing |
| `IMAGE_VERSION` | Application image version (default `0.0.0`) |
| `SECURITY_COUNTER`, `VID`, `CID` | Override signing defaults (epoch `0`, `--vid microtun.dev --cid esp32c3`) |
| `SLOT` | Slot the firmware runner writes (`image-0` by default) |
| `IMGTOOL` | Override the `imgtool` executable |

Neither profile restricts keys: for production set your own `ESP_SBV2_KEY`,
`FERRIBOOT_PUBKEYS` and `FERRIBOOT_KEY`. Custom VID/CID values also require
changing `ferriboot-esp/src/policy.rs`.

Cargo features on `esp32c3-supermini-ferriboot`:

- Upgrade strategy, at most one: swap-using-scratch (default),
  `overwrite-only`, `direct-xip`, `direct-xip-revert`.
- Enabled by default: `downgrade-prevention`, `bootstrap`, `uuid`.
- `bl-loader-diagnostics`: handoff-loader logging.

## Security counter

Ferriboot enforces an application rollback floor in eFuse
`BLOCK0.SECURE_VERSION` (epochs 0–16). ESP-IDF app anti-rollback must stay
disabled.

Images are signed at epoch 0 unless `SECURITY_COUNTER=N` is set. The
[core README](../../ferriboot-core/README.md#security-counter) describes when
a new floor is burned. With only 16 epochs, reserve them for security fixes.

## Hardware-in-the-loop tests

> Use a **dedicated, expendable board.** The suite signs with the development
> RSA key and has no acknowledgement gate: on a blank chip the first test boot
> enrolls that key permanently. Every scenario reflashes all 4 MiB.

```sh
export FERRIBOOT_ESP32C3_PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_YOUR_DEVICE_ID
cargo test -p ferriboot-hil --features esp32c3-supermini,smoke -- --test-threads=1
```

The tests read the security counter over JTAG, so JTAG must remain enabled
(`dev` profile). With several debug probes attached, select the board with
`PROBE_RS_PROBE`. Adding the `security-counter` feature burns one more epoch
per run. See [`ferriboot-hil`](../../testing/ferriboot-hil/README.md) for the
harness and its other settings.
