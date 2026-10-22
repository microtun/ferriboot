# ESP32-C3

With Secure Boot V2 the signed second-stage bootloader must fit in 48 KiB,
which is too small for Ferriboot. Ferriboot therefore runs as a **third
stage**, loaded and verified by the stock ESP-IDF second stage:

1. The mask ROM verifies the ESP-IDF v5.5.5 second stage at `0x0`
   (Secure Boot V2, RSA-3072).
2. The second stage verifies Ferriboot, a factory app at `0x20000` signed
   with the **same** RSA-3072 key.
3. Ferriboot verifies the MCUboot-signed (ECDSA P-256) application in
   `image-0` or `image-1` and starts it through a small RAM handoff loader.

The cost is 64 KiB of flash for the second stage plus Ferriboot's 256 KiB
factory partition.

## Requirements and limitations

- ESP32-C3 **rev v0.3 (ECO3) or later** with **4 MiB** flash.
- Secure Boot V2 is always enabled; there is no unsigned boot chain.
- Flash encryption is not supported. Neither second-stage profile disables
  ROM download mode.
- Only the **clock** glitch detector is used. Ferriboot re-enables and checks
  it on every boot, but the ROM runs without it and the application can turn
  it off. See [fault-injection hardening](../../THREAT-MODEL.md#fault-injection-hardening).
- The RAM loader copies segments from flash after the final check, so a flash
  interposer is out of scope.

## Flash layout

Defined in `layout/partitions.csv` and checked by `ferriboot-tools-esp layout`:

| Region | Offset | Size |
|---|---|---|
| ESP-IDF second stage (signed) | `0x0` | ≤ 48 KiB |
| Partition table | `0x10000` | 4 KiB |
| `nvs`, `phy_init` (unused) | `0x11000` | 24 KiB |
| `ferriboot` (factory app) | `0x20000` | 256 KiB |
| `image-0` (primary) | `0x6F000` | 1764 KiB |
| `image-1` (secondary) | `0x22F000` | 1764 KiB |
| `image-scratch` | `0x3E8000` | 64 KiB |
| `storage` (application-owned) | `0x3F8000` | 32 KiB |

Slots start at `…F000` so the ESP image after the 4 KiB MCUboot header lands
on a 64 KiB MMU page. Ferriboot finds slots **by label at runtime**: changing
partitions needs no Rust rebuild, only re-signing if slot capacity or
alignment changes. The table's own offset is set in `.cargo/config.toml`
(`ESP_BOOTLOADER_ESP_IDF_CONFIG_PARTITION_TABLE_OFFSET`) and must match the
second stage's `sdkconfig.defaults.*`; the runner checks this.

```sh
ferriboot-tools-esp layout
ferriboot-tools-esp layout --table partitions.bin
```

## Build and run

### Prerequisites

- Rust **1.95+** with the `riscv32imc-unknown-none-elf` target.
- Upstream `imgtool` (and Python for that external tool). Secure
  Boot V2 signing is done locally; `esptool`/`espsecure` are not needed.
- Docker with BuildKit, to build the ESP-IDF second stage (or supply an
  unsigned one with `ESP_IDF_BOOTLOADER_BIN`).
- `espflash` CLI **4.6+**.
- The chip's native USB-Serial-JTAG port.
- Development keys: OpenSSL for MCUboot (see [keys/README.md](../../keys/README.md))
  and `ferriboot-tools-esp keys` for Secure Boot V2
  (see [keys](../../keys/README.md)).

```sh
rustup target add riscv32imc-unknown-none-elf
python3 -m pip install imgtool
cargo install --path ferriboot-esp/tools --locked
cargo install espflash --locked
```

### Build and sign offline

Run Cargo from **each binary's directory**; each installs its own runner.

```sh
# Ferriboot and the IDF second stage, built and signed, not flashed.
cd boards/esp32c3/ferriboot
NO_FLASH=1 cargo run --release

# Example firmware, MCUboot-signed.
cd ../firmware
cargo build --release
IMAGE_VERSION=1.0.0 ferriboot-tools-esp sign \
  ../target/riscv32imc-unknown-none-elf/release/esp32c3-firmware \
  ../target/riscv32imc-unknown-none-elf/release/esp32c3-firmware.signed.bin
```

Outputs in `boards/esp32c3/target/riscv32imc-unknown-none-elf/release/`:

| File | Flash address |
|---|---|
| `bootloader.idf.sbv2.bin` | `0x0` |
| `partitions.bin` | `0x10000` |
| `ferriboot.sbv2.bin` | `0x20000` |
| `esp32c3-firmware.signed.bin` | `image-0`, `0x6F000` |

`ferriboot-tools-esp` subcommands: `sign`, `inspect`, `sign-sbv2`,
`verify-sbv2`, `build-bootloader`, `flash-app`, `flash-ferriboot`. Extra
`imgtool` arguments can follow `sign ELF OUTPUT --`. `sign-sbv2` expects an
already padded ESP image; `flash-ferriboot` handles conversion and padding.

### Flash a development board

> **Irreversible.** On first boot the ESP-IDF second stage burns the digest of
> its signing key into eFuse and enables Secure Boot V2, in both the `dev`
> and `release` profiles. From then on the chip only boots images signed with
> that key. Plan provisioning first; see the
> [ESP-IDF Secure Boot V2 guide](https://docs.espressif.com/projects/esp-idf/en/v5.5.5/esp32c3/security/secure-boot-v2.html).

Flash the application first, then the boot chain:

```sh
export ESPPORT=/dev/ttyACM0
cd boards/esp32c3/firmware
IMAGE_VERSION=1.0.0 cargo run --release   # press Ctrl+C to leave the monitor

cd ../ferriboot
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND cargo run --release
```

The Ferriboot runner refuses to flash without that exact acknowledgement. It
writes the factory app and partition table before the second stage, holding
the chip in download mode until all three writes succeed. View logs with
`espflash monitor --port "$ESPPORT"`.

For later updates, rerun the firmware command with a new `IMAGE_VERSION`; it
writes the primary slot directly. `SLOT=image-1` writes the secondary slot
but does **not** mark an upgrade pending. `CONFIRM=1` adds `--pad --confirm`.
The example firmware has no MCUmgr/SMP server; a real update flow needs your
own transport and the updater API.

## Writing an application

Use the example firmware as a template. An application must:

- Depend on `ferriboot-esp` with the `esp32c3` feature, and in `build.rs` call
  `ferriboot_build::configure_esp_application(CHIP)` (from the
  board layout crate) and `emit_public_keys(...)`.
- Keep RAM segments clear of the handoff loader's protected region; the tools
  and Ferriboot reject overlaps.
- Re-arm a watchdog early. Ferriboot hands off with a freshly fed 15-second
  RTC watchdog, but `esp_hal::init` disables it.
- Confirm a trial upgrade only after its own health checks (the example
  confirms unconditionally).

## Configuration

| Variable | Purpose |
|---|---|
| `ESP_SBV2_KEY` | RSA-3072 key for the second stage and Ferriboot (default: dev key) |
| `FERRIBOOT_PUBKEYS` | Comma-separated P-256 public keys built into Ferriboot |
| `FERRIBOOT_KEY` | P-256 private key for MCUboot signing |
| `ESP_BOOTLOADER_PROFILE` | `dev` (default; JTAG and spare digest slots stay open) or `release` (disables JTAG, revokes spare slots) |
| `ESP_IDF_BOOTLOADER_BIN` | Absolute path to an unsigned IDF second stage; skips Docker |
| `ESPPORT` | Serial port; required for boot-chain flashing |
| `NO_FLASH=1` | Build and sign the boot chain without flashing |
| `IMAGE_VERSION` | MCUboot image version (default `0.0.0`) |
| `SECURITY_COUNTER`, `VID`, `CID` | Override signing defaults (epoch `0`, `--vid microtun.dev --cid esp32c3`) |
| `SLOT`, `CONFIRM` | Firmware runner target slot and trailer |

Neither profile restricts keys: for production set private `ESP_SBV2_KEY`,
`FERRIBOOT_PUBKEYS` and `FERRIBOOT_KEY`. Custom VID/CID values also require
changing `ferriboot-esp/src/policy.rs`, which both binaries enforce.

Cargo features on `esp32c3-ferriboot`:

- Strategy, at most one: default swap-using-scratch, `overwrite-only`,
  `direct-xip`, `direct-xip-revert`. The firmware learns the strategy at
  handoff.
- Default: `downgrade-prevention`, `bootstrap`, `uuid`.
- `bl-loader-diagnostics`: handoff-loader logging.

The example firmware always includes the unauthenticated
[test hooks](../../testing/ferriboot-test-hooks/README.md); do not ship it.

## Security counter

Ferriboot enforces an application rollback floor in eFuse
`BLOCK0.SECURE_VERSION` (epochs 0–16); ESP-IDF app anti-rollback must stay
disabled. Images need a protected `SEC_CNT` TLV (default epoch 0; set
`SECURITY_COUNTER=N` for security releases). Trial boots keep the old floor
and the new one is burned on the next reset after confirmation; permanent
installs burn before handoff. With only 16 epochs, reserve them for
security fixes.

## Hardware-in-the-loop tests

See [`ferriboot-hil`](../../testing/ferriboot-hil/README.md) for the harness itself.

> Use a **dedicated, expendable board.** The suite signs with the development
> RSA key and has no acknowledgement gate: on a blank chip the first test boot
> enrolls that key permanently. Every scenario reflashes all 4 MiB.

```sh
export FERRIBOOT_ESP32C3_PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_YOUR_DEVICE_ID
cargo test -p ferriboot-hil --features esp32c3,smoke -- --test-threads=1
```

HIL flashes through the ROM serial loader with the `espflash` crate and reads
the security counter over JTAG with `probe-rs`, so JTAG must remain enabled
(`dev` profile). Set `PROBE_RS_PROBE` if several probes are attached and
`FERRIBOOT_ESP32C3_FLASH_TIMEOUT_SECS` to change the 600-second flash timeout.
The `security-counter` feature adds anti-rollback tests that burn one more
epoch per run.

Partition-table tests need no device:
`cargo test -p ferriboot-hil --features esp32c3 --lib esp32c3::partitions::tests`.
