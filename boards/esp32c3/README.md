# ESP32-C3

The ESP32 is a challenging target for Ferriboot because of constraints in its
boot process and memory layout.

The biggest constraint is that, with Secure Boot V2 enabled, the signed
second-stage bootloader must fit in 48 KiB (44 KiB before signing). Shrinking
Ferriboot to that size is not practical, so we got creative.

Ferriboot runs as a **third stage**, loaded by the stock ESP-IDF second stage.
The ESP-IDF bootloader includes just enough public key cryptography to verify
the third stage before executing it. The boot chain is:

1. Mask ROM verifies the ESP-IDF v5.5.5 second stage at `0x0` (Secure Boot V2,
   RSA-3072).
2. The second stage verifies Ferriboot, an ESP-IDF factory app at `0x20000`
   signed with the **same** RSA-3072 key.
3. Ferriboot verifies the MCUboot-signed (ECDSA P-256) application in `image-0`
   or `image-1`, then loads it via a small RAM handoff loader.

The cost is the first 64 KiB of flash used by the stock second stage, on top of
Ferriboot's own 256 KiB factory partition. Against the 4 MiB of flash the
ESP32-C3 typically has, this overhead is acceptable.

## Requirements and limitations

- ESP32-C3 **rev v0.3 (ECO3) or later** with **4 MiB** flash. The layout is
  configured for 4 MiB. The table defines Ferriboot, slots, scratch and storage.
- Secure Boot V2 is always enabled; there is no unsigned boot chain.
- Flash encryption is not supported yet. Neither second-stage profile disables
  ROM download mode; that is a separate manufacturing decision.
- Only the C3's **clock** glitch detector is used, as a reset source. ESP-IDF's
  second stage enables it on v0.3+; Ferriboot re-enables and verifies it on
  every boot and refuses to continue if it cannot. The ROM and the start of
  the second stage run without it, the setting has no lock (the application
  can turn it off), and it offers no protection against voltage or EM fault
  injection. A glitch reset is logged on the next boot. Ferriboot's software
  countermeasures (see the top-level README's
  [fault-injection hardening](../../README.md#fault-injection-hardening)
  section) re-authenticate the image, re-derive the load plan and re-check the
  eFuse floor before the handoff, with TRNG-randomised timing. The RAM loader
  still copies segments from flash *after* that check, so an attacker who can
  change flash contents at that moment (a flash interposer, not a glitch) is
  out of scope.

## Flash layout

The default table is defined in `layout/partitions.csv` and validated by
`tools/layout.py`:

| Region | Offset | Size |
|---|---|---|
| ESP-IDF second stage (signed) | `0x0` | ≤ 48 KiB |
| Partition table | `0x10000` | 4 KiB |
| `nvs`, `phy_init` (unused by Ferriboot) | `0x11000` | 24 KiB |
| `ferriboot` (app, factory) | `0x20000` | 256 KiB |
| `image-0` (primary) | `0x6F000` | 1764 KiB |
| `image-1` (secondary) | `0x22F000` | 1764 KiB |
| `image-scratch` | `0x3E8000` | 64 KiB |
| `storage` (application-owned) | `0x3F8000` | 32 KiB |

Slots start at `…F000` so the ESP image after the 4 KiB MCUboot header lands
on a 64 KiB MMU page boundary. Slots are found by label at runtime.

Partition offsets and sizes are not compiled into Ferriboot or the firmware.
The `esp32c3-layout` crate generates only the chip's hardware geometry and
protected-RAM ABI (from `ferriboot-esp/chips/esp32c3.json`) and the board's
flash size (from `layout/board.json`); the partition table cannot describe
those values.
Host signing and flashing read the CSV, and HIL signing/composition read the
same generated binary table that they flash. Factory placement and capacity
also come from the table; the factory partition must be MMU page aligned.

The table's own address is a bootstrap setting in `.cargo/config.toml`
(`ESP_BOOTLOADER_ESP_IDF_CONFIG_PARTITION_TABLE_OFFSET`), defaulting to
`0x10000`. Keep the IDF second-stage `sdkconfig.defaults.*` offset in agreement;
the host runner checks this before building. Changing partition placement or
sizes does not require rebuilding Rust binaries. Re-sign images if their slot
capacity or MMU/header alignment changes. Hardware/RAM ABI changes and changes
to the table locator still require rebuilding.

Partition-layout regressions can run without a connected device, from the
repository root:

```sh
python3 -m unittest discover -s boards/esp32c3/tools -p test_layout.py
cargo test -p ferriboot-hil --features esp32c3 --lib esp32c3::partitions::tests
```


## Build and Run

### Prerequisites

- Rust **1.95+** with the `riscv32imc-unknown-none-elf` target.
- Python **3.11+** with `imgtool`, `cryptography` and `pyelftools`. The board tools perform Secure
  Boot V2 signing, verification, and image padding locally; no host-side
  `esptool` or `espsecure` installation is required.
- Docker (with BuildKit) for the stock **ESP-IDF v5.5.5** second-stage build.
  Alternatively, supply an **unsigned** IDF bootloader with
  `ESP_IDF_BOOTLOADER_BIN`.
- `espflash` CLI **4.6+**, for ELF conversion and no-stub ROM flashing
  (`cargo install espflash --locked`); it also provides the serial monitor.
- A connection to the ESP32-C3's native USB-Serial-JTAG port. Set `ESPPORT`
  explicitly for boot-chain flashing so all three writes target the same chip.

For example, on Linux (from the repository root):

```sh
rustup target add riscv32imc-unknown-none-elf
python3 -m venv .venv
. .venv/bin/activate
python3 -m pip install imgtool cryptography pyelftools
cargo install espflash --locked
```

### Build and sign without flashing

Build from **each binary's directory** rather than running `cargo run` in
`boards/esp32c3`: their `.cargo/config.toml` files install different runners,
and Ferriboot is not a default workspace member. The Ferriboot build also
compiles and checks the standalone RAM handoff loader
(`ferriboot-esp/bl-loader`), through `ferriboot-esp`'s `boot-runtime` feature.

```sh
# From the repository root: build Ferriboot, the IDF second stage, and sign both.
cd boards/esp32c3/ferriboot
NO_FLASH=1 cargo run --release

# Build and separately sign the example MCUboot firmware.
cd ../firmware
cargo build --release
IMAGE_VERSION=1.0.0 python3 ../tools/firmware.py sign \
  ../target/riscv32imc-unknown-none-elf/release/esp32c3-firmware \
  ../target/riscv32imc-unknown-none-elf/release/esp32c3-firmware.signed.bin
python3 ../tools/firmware.py inspect \
  ../target/riscv32imc-unknown-none-elf/release/esp32c3-firmware.signed.bin
```

`NO_FLASH=1` on the **Ferriboot runner** still converts, signs, and verifies
the boot chain; it only suppresses writes to the chip. It does **not** disable
the separate firmware runner's flashing: use `cargo build` plus the `firmware.py sign` command (as above) to prepare the example firmware offline. The outputs are in
`boards/esp32c3/target/riscv32imc-unknown-none-elf/release/`:

| File | Purpose / flash address |
|---|---|
| `bootloader.idf.sbv2.bin` | Signed ESP-IDF second stage, `0x0` |
| `partitions.bin` | ESP partition table, `0x10000` |
| `ferriboot.sbv2.bin` | Signed factory Ferriboot, `0x20000` |
| `esp32c3-firmware.signed.bin` | MCUboot-signed example application, `image-0` at `0x6F000` |

The runners derive addresses and slot sizes from `layout/partitions.csv`; do
not hand-adjust the image header or flash offsets. To validate the layout or
generate the partition table without building firmware:

```sh
python3 boards/esp32c3/tools/layout.py --check
python3 boards/esp32c3/tools/layout.py --table partitions.bin
```

`tools/firmware.py` also provides `sign-sbv2`, `verify-sbv2`,
`build-bootloader`, `flash-app`, and `flash-ferriboot` subcommands. For
example, after an offline build, verify the signed factory image against the
trusted public key (from the repository root):

```sh
python3 boards/esp32c3/tools/firmware.py verify-sbv2 \
  boards/esp32c3/target/riscv32imc-unknown-none-elf/release/ferriboot.sbv2.bin \
  --key boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072.pub.pem
```

For manual `sign-sbv2` operations, the input must already be an **ESP image
with the required Secure Boot V2 padding** (for a factory application, a
64 KiB-aligned image). `sign-sbv2` appends the 4 KiB RSA signature sector;
it does not convert an ELF or add ESP-image padding. The `flash-ferriboot`
runner performs conversion, padding, signing, and self-verification for you.
See [key generation](../../keys/README.md) to replace the development keys.

Extra `imgtool sign` arguments can follow `firmware.py sign ELF OUTPUT --`.
The signing tool supplies the board's development image identity by default:
`--vid microtun.dev --cid esp32c3`. The bootloader and firmware updater
both enforce the corresponding protected VID/CID TLVs in
`ferriboot-esp/src/policy.rs`. `SECURITY_COUNTER`, `VID`, and `CID`
environment variables override signing defaults; extra `imgtool` arguments
take precedence. **Custom VID/CID values also require changing the policy in
both binaries**, otherwise the signed image is rejected. Re-sign older images
that lack these TLVs before upgrading to this policy (see `tools/mcuboot.py`).

### Flash and boot a development board

**Security warning:** Both `dev` (the default) and `release` second-stage
profiles **enable Secure Boot V2**. On the first successful boot, the ESP-IDF
second stage **permanently burns the digest of the key that signed it** into
eFuse and enables Secure Boot. The committed RSA-3072 and P-256 development
private keys are public and must **never** protect a production device. The
`dev` profile keeps JTAG available and leaves unused digest slots open, but
does *not* make eFuse provisioning reversible. The `release` profile lets
ESP-IDF disable JTAG and revoke unused digest slots. Inspect your chip's eFuse
state and plan key provisioning *before* flashing; to pre-provision a key
digest instead, see the
[ESP-IDF v5.5.5 Secure Boot V2 guide](https://docs.espressif.com/projects/esp-idf/en/v5.5.5/esp32c3/security/secure-boot-v2.html).
Do not use this workflow on a device you need to keep unprovisioned.

For a fresh **dedicated development board**, it is convenient to write the
initial firmware slot *before* installing the secure-boot chain. Set `ESPPORT`
to your device's port (`/dev/ttyACM0` is an example):

```sh
# From the repository root; the first command signs and flashes image-0 only.
export ESPPORT=/dev/ttyACM0
cd boards/esp32c3/firmware
IMAGE_VERSION=1.0.0 cargo run --release

# Review the warning above first: this explicitly opts in to eFuse risk.
cd ../ferriboot
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND cargo run --release
```

The Ferriboot runner signs and verifies both boot stages, checks their size
limits, generates the partition table, and uses `espflash write-bin --no-stub`
to flash the factory application and partition table **before** the signed
ESP-IDF second stage. It holds the chip in ROM download mode between writes
and resets only after all three succeed. Set `ESPPORT` to the device port; this
is required for boot-chain flashing to keep every write on the same board.
Without the exact `FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND`
value it **refuses to flash**. The firmware runner starts `espflash monitor`
after flashing; press Ctrl+C before running the Ferriboot command. If needed,
release BOOT/GPIO9 and reset the chip after flashing.

Observe the logs on the USB-Serial-JTAG console:

```sh
espflash monitor --port "$ESPPORT"    # optional; or use another serial monitor
```

A successful boot prints Ferriboot's slot decision, then the example
firmware's version, a USB-Serial-JTAG log-console message, and a periodic
heartbeat. The example firmware has **no MCUmgr/SMP server**: serial console
access only provides logs, not image upload or management commands.

For subsequent development updates, run `IMAGE_VERSION=1.0.1 cargo run --release`
from `boards/esp32c3/firmware` to flash **the primary slot directly**. The
runner also accepts `SLOT=image-1` to flash the secondary slot, but writing
that partition alone does **not** mark an update as pending or exercise a trial
upgrade. `CONFIRM=1` adds `--pad --confirm` (for example, for an initial image
in `direct-xip-revert` mode). To use the updater's normal staging/trial flow,
provide your own transport and updater integration; an `mcumgr image upload`
command will not work with this example.

### `esp-hal` integration boundary

The `ferriboot-esp` platform crate now owns the HAL-backed third-stage
startup (`boot_runtime::init`), the RTC watchdog (armed at bootloader start,
fed after every flash operation, still running at handoff) and delayed reset.
The shared `watchdog::start_rtc` HAL driver is used by both the bootloader
and example firmware, with the same 15-second timeout.
The `boot-runtime` crate feature enables `esp-hal/rt` only for the bootloader;
normal OTA firmware continues to initialise its own HAL and RTOS. Both
executables can construct the shared partitioned flash through
`ferriboot_esp::open_flash(peripherals.FLASH)` using the HAL FLASH singleton.
The eFuse security-counter driver's busy timeout uses `esp-hal`'s monotonic
`Instant` rather than a CPU-frequency-dependent spin budget.

A few operations intentionally remain register/ROM-level: clearing TIMG0's
flash-boot-only bit (not exposed by the HAL WDT driver), the clock glitch
reset enable (`RTC_CNTL` PAC fields; the HAL has no driver for it), eFuse programming
with tightly controlled staging, and the standalone SRAM loader's cache/MMU,
interrupt, and flash-read sequence. The SRAM loader cannot call regular HAL
code after it begins overwriting the bootloader's memory.

### Writing your own application

Use the example firmware as a template. An application must:

- Depend on `ferriboot-esp` with the `esp32c3` feature and call
  `ferriboot_build::configure_esp_application(CHIP, CHIP_JSON)` (from the
  board layout crate, a build dependency) and `emit_public_keys(...)` in its
  `build.rs`. This checks that `ferriboot-esp` was built for the board's chip
  and links the Ferriboot contract that reserves the boot-info record handed
  over in RAM.
- Keep its RAM segments clear of the handoff loader's protected region;
  `firmware.py` and Ferriboot reject images that overlap it.
- Re-arm a watchdog early. Ferriboot runs a 15-second RTC watchdog from its
  own start (fed by flash progress) and hands off with it freshly fed, but
  `esp_hal::init` disables it.
- Confirm a trial upgrade only after its own health checks. The example
  confirms unconditionally.

### Signing keys, profiles, and features

- `ESP_SBV2_KEY`: RSA-3072 key for the IDF second stage **and** factory
  Ferriboot; defaults to `boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072.pem`. See
  `keys/README.md` for generating replacement keys.
- `FERRIBOOT_PUBKEYS`: comma-separated trusted P-256 public keys embedded in
  Ferriboot only at build time.
  `FERRIBOOT_KEY`: matching P-256 private key used for MCUboot signing.
  Defaults are under `keys/`; use absolute paths for custom keys.
- `ESP_BOOTLOADER_PROFILE=dev|release` selects the IDF second-stage profile
  (`bootloader/sdkconfig.defaults.*`); `dev` is the default. The profile
  does not restrict signing keys: both profiles default to the committed
  development keys unless you override them. For production, configure a
  private `ESP_SBV2_KEY`, `FERRIBOOT_PUBKEYS`, and matching `FERRIBOOT_KEY`,
  and build *both* executables with the intended trust keys. Replacing
  keys on an already provisioned device also requires a free, unrevoked
  digest slot.
- `ESP_IDF_BOOTLOADER_BIN=/absolute/path/to/unsigned.bin` skips the Docker
  build; the runner still signs it and enforces the size limit.

Bootloader strategies are Cargo features on `esp32c3-ferriboot`: default
swap-using-scratch, `overwrite-only`, `direct-xip`, or `direct-xip-revert`
(select at most one). `downgrade-prevention` and `bootstrap` are on by
default; `bl-loader-diagnostics` enables handoff-loader logging. The firmware
learns the strategy from the bootloader's handoff and needs no matching
features. Test hooks are always built into the example firmware and read
unauthenticated records from `storage`; do not ship them in production.

### Hardware-in-the-loop tests

For a **dedicated, expendable test board**, the root workspace has a smoke
suite:

```sh
# From the repository root.
export FERRIBOOT_ESP32C3_PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_YOUR_DEVICE_ID
cargo test -p ferriboot-hil --features esp32c3,smoke -- --test-threads=1
```

For an already-burned application counter, HIL reads `BLOCK0.SECURE_VERSION`
**over JTAG with `probe-rs` by default**, through the ESP32-C3 USB-Serial/JTAG
peripheral or an external probe. It counts burned bits and checks eFuse
programming-error flags; it **does not flash a preflight fixture or rely on
firmware output**. If multiple probes are available, select one with
`PROBE_RS_PROBE` (optionally set `PROBE_RS_SPEED_KHZ`). The development profile
keeps JTAG enabled, but the release profile disables it during Secure Boot
provisioning. **JTAG access is required** for the HIL counter read; a board
with JTAG disabled causes an error rather than using another transport.
The [opt-in security-counter HIL test](../../README.md#opt-in-security-counter-hil-test)
requires the `security-counter` Cargo feature and permanently consumes one more
epoch. The command above does not enable it.

**The suite always signs the boot chain with the committed development RSA
key and has no acknowledgement gate.** On an unprovisioned chip the first test
boot permanently enrolls that public key; a chip already provisioned with a
different key will not boot the test images. Each scenario also reflashes the
**entire 4 MiB** device.

HIL flashes through the ROM serial loader using the Rust `espflash` crate in a
supervised `ferriboot-flash` worker (no `esptool`, stub, or debug probe), then
reopens USB CDC for boot capture and reset.
`FERRIBOOT_ESP32C3_FLASH_TIMEOUT_SECS` overrides the default 600-second flash
timeout. HIL uses the Rust `espflash` library for **both** ELF conversion and
fixture flashing; the standalone `espflash` CLI is required for the normal
board runners, not for these HIL operations. HIL still needs Python with
`cryptography`, `pyelftools` and `imgtool`, plus Docker (or a supplied unsigned ESP-IDF
bootloader). The normal board runner implements Secure Boot V2 image padding
in `tools/esp_image.py` before RSA signing.
## Application rollback epochs

The bootloader always enforces the hardware security counter using `BLOCK0.SECURE_VERSION` (0..16); ESP-IDF app anti-rollback must stay disabled.
Applications require a protected `SEC_CNT` TLV. Signing defaults to epoch 0;
use `SECURITY_COUNTER=N` for deliberate security releases. Trial boots preserve
the previous floor; confirmation is committed to OTP on the next reset, and
permanent installs commit before handoff. See [hardware application
anti-rollback](../../README.md#hardware-application-anti-rollback) for provisioning,
capacity and failure handling.
