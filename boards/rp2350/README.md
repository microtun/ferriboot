# RP2350 (Pico 2 / Pico 2 W)

The RP2350 is a comparatively friendly target for Ferriboot: its mask ROM can
authenticate a signed image itself, and its QMI address translation lets an
application run from either flash slot without being relinked. Ferriboot
therefore runs as the **only** stage after the ROM; there is no vendor second
stage as on the ESP32-C3.

Ferriboot is linked to run entirely from **SRAM**. A custom PICOBIN block at
the start of flash tells the ROM to copy the whole bootloader into SRAM, so no
external-flash code executes after the ROM's signature check. The boot chain
is:

1. The mask ROM loads Ferriboot from the `bootloader` partition at flash
   offset `0x0` into SRAM. Once secure boot is enabled in OTP, the ROM only
   runs it if its **secp256k1** signature matches the key fingerprint in
   `BOOTKEY0`.
2. Ferriboot verifies the MCUboot-signed (ECDSA **P-256**) application in
   `image-0` or `image-1`, performing any pending swap, then points QMI
   **ATRANS1** at the chosen slot so it appears at `0x10400000`, and jumps to
   the application's vector table at `0x10400200`.

Because both slots execute through the same address window, one firmware
build works in either slot, and direct-XIP needs no per-slot link addresses.
The cost is a 128 KiB bootloader partition, which is generous for Ferriboot
and leaves room for the ROM signature block.

The two signatures are independent and use different keys and curves; see
[Secure boot](#secure-boot) for the ROM side and its **irreversible** OTP
provisioning, which the default build performs on first boot.

## Requirements and limitations

- Raspberry Pi **Pico 2** or **Pico 2 W** (RP2350A) with **4 MiB** flash.
  The layout is fixed to 4 MiB and the bootloader partition to
  `0x0`/`0x20000`.
- **Arm Secure** mode on core 0 only (`thumbv8m.main-none-eabihf`). All
  partitions are marked as ignored for RISC-V boot; the Hazard3 cores are not
  supported. Ferriboot never starts core 1.
- **Every build** enrolls ROM secure boot in OTP on its first successful boot,
  or verifies that OTP already matches. By default the OTP policy is for
  development (SWD and spare ROM key slots remain open). `production` selects
  irreversible SWD disablement and revocation of spare key slots. Enrollment
  is irreversible; there is no unsigned/no-provisioning profile.
- The ROM-sealed bootloader, signature included, must fit in **128 KiB**, and
  its SRAM image must end below the boot-info reservation at `0x2007FF00`.
- No native RP2350 partition table is written to flash, and RP2350 ROM A/B
  (`link`) partitions are not used. `layout/partitions.json` is only the
  layout source for Ferriboot and the tools.
- The application rollback counter uses **504 triplicated raw OTP epochs**
  in page 32. Boards with a pre-existing single-row counter are incompatible
  with this layout. See [Application rollback epochs](#application-rollback-epochs).
- Both security profiles permanently arm the glitch detectors in OTP. Neither
  configures BOOTSEL/PICOBOOT, the ROM rollback counter or *permanent* OTP page
  locks. Both profiles tighten the relevant OTP software locks at each
  application handoff. See
  [Reset-scoped OTP handoff locks](#reset-scoped-otp-handoff-locks),
  [Glitch detectors](#glitch-detectors) and
  [Production security limitations](#production-security-limitations-and-hardening).

## Flash layout

Defined in `layout/partitions.json` (picotool's partition-table JSON schema)
together with the board's flash size in `layout/board.json` and the RP2350's
fixed addresses in `ferriboot-rp/chips/rp2350.json`, and validated by
`tools/layout.py`, the `rp2350-layout` crate and, at compile time,
`ferriboot_rp::Layout::new`:

| Region | Offset | Size |
|---|---|---|
| `bootloader` (ROM-loaded into SRAM, Arm Secure) | `0x0` | 128 KiB |
| `image-0` (primary) | `0x20000` | 1920 KiB |
| `image-1` (secondary) | `0x200000` | 1920 KiB |
| *(unallocated)* | `0x3E0000` | 32 KiB |
| `image-scratch` | `0x3E8000` | 64 KiB |
| `storage` (application-owned) | `0x3F8000` | 32 KiB |

Offsets are relative to the flash base at `0x10000000`. The bootloader keeps
the identity-mapped ATRANS0 window for physical flash access and maps the
selected slot into the 4 MiB application window at `0x10400000`. Each slot
holds a 512-byte MCUboot header, so the application vector table lands at
`0x10400200` and satisfies VTOR's 512-byte alignment; slots need only 4 KiB
alignment. After the MCUboot trailer, the largest signed image is
**1912 KiB** (`0x1DE000`).

In SRAM, the top 256 bytes (`0x2007FF00`–`0x2007FFFF`) hold the boot-info
record Ferriboot hands to the application. Both linker contracts exclude it
from the stack, heap and data, and the tools reject images that overlap it.

To print the validated geometry:

```sh
python3 boards/rp2350/tools/layout.py --check
```

## Build and Run

### Prerequisites

- Rust **1.95+** with the `thumbv8m.main-none-eabihf` target.
- Python 3 with `pyelftools`, `cryptography` and `imgtool` for the image
  tools.
- `picotool` **2.3+** (including `seal` and `info`) for ROM signatures; 2.2
  does not mark the sealed block EXTRA_SECURITY, which both the tools and the
  bootloader require. Set
  `PICOTOOL` if it is not on `PATH`. It is needed only to seal the
  bootloader, not to build or to sign applications.
- The `probe-rs` CLI and an SWD debug probe (for example the Raspberry Pi
  Debug Probe) wired to the board's SWD header. All flashing goes through
  SWD; the runners do not use BOOTSEL/UF2.
- A 3.3 V serial adapter on **GPIO0** (UART0 TX, Pico pin 1) and GND, at
  **115200 8N1**, to read logs. The Debug Probe's UART port works. Only TX is
  used; the board reads nothing from the console.
- OpenSSL for generating an **RP2350-only** secp256k1 ROM signing key. Never
  reuse the MCUboot P-256 key or the ESP32-C3 RSA key.

For example, on Linux (from the repository root):

```sh
rustup target add thumbv8m.main-none-eabihf
python3 -m venv .venv
. .venv/bin/activate
python3 -m pip install pyelftools cryptography imgtool
cargo install probe-rs-tools --locked

# Outside the repository, with protected permissions:
mkdir -p "$HOME/secure-keys"
umask 077
openssl ecparam -name secp256k1 -genkey -noout -out "$HOME/secure-keys/rp2350-rom.pem"
export RP2350_SECURE_BOOT_KEY="$HOME/secure-keys/rp2350-rom.pem"
```

The regular board runners never fall back to the committed development ROM
key; `RP2350_SECURE_BOOT_KEY` (or `--key`) must always name one explicitly.
Keep this private key and back it up: every future Ferriboot build for a board
enrolled with it must be signed with the same key.

### Build and sign without flashing

Build from **each binary's directory** rather than running `cargo run` in
`boards/rp2350`: their `.cargo/config.toml` files install different runners.
Neither `cargo build` nor offline signing touches hardware or OTP, and
neither needs the provisioning acknowledgement.

```sh
# From the repository root. No ROM signing key is needed to compile.
cd boards/rp2350/ferriboot
cargo build --release  # development OTP policy (even in Cargo release mode)
# For devices intended for production, instead use:
cargo build --release --features production
cd ../firmware
cargo build --release
cd ../../..

# ROM-seal the bootloader. The same ELF can be sealed with different keys.
python3 boards/rp2350/tools/firmware.py sign-bootloader \
  boards/rp2350/target/thumbv8m.main-none-eabihf/release/rp2350-ferriboot \
  /tmp/ferriboot.key-A.signed.bin --key /secure/customer-A.pem
python3 boards/rp2350/tools/firmware.py sign-bootloader \
  boards/rp2350/target/thumbv8m.main-none-eabihf/release/rp2350-ferriboot \
  /tmp/ferriboot.key-B.signed.bin --key /secure/customer-B.pem
picotool info -a /tmp/ferriboot.key-A.signed.bin

# MCUboot-sign the example firmware as a confirmed factory image.
IMAGE_VERSION=1.0.0 python3 boards/rp2350/tools/firmware.py sign --confirm \
  boards/rp2350/target/thumbv8m.main-none-eabihf/release/rp2350-firmware \
  /tmp/rp2350-firmware.signed.bin
```

The ELFs are in `boards/rp2350/target/thumbv8m.main-none-eabihf/release/`.
Neither can be flashed as-is: the bootloader ELF has SRAM run addresses and
the application ELF lacks its MCUboot header. Flash the signed files instead:

| File | Purpose / flash address |
|---|---|
| `ferriboot.*.signed.bin` | ROM-sealed Ferriboot, `bootloader` at `0x10000000` |
| `rp2350-firmware.signed.bin` | MCUboot-signed example application, `image-0` at `0x10020000` |

`--key /absolute/path/to/private.pem` is an alternative to
`RP2350_SECURE_BOOT_KEY` at signing time. `sign-bootloader` checks that
picotool reports the ROM signature as verified, that the image is Arm Secure,
that sealing left the SRAM executable and load map unchanged, that the
embedded `DEV`/`REL` profile marker is valid, that the 64-byte public key in
the sealed PICOBIN `SIGNATURE` item matches the PEM used to sign, and that
the result fits the **128 KiB bootloader partition**. Do not flash an image
that fails these checks.

The sealed `.signed.bin` is the **physical image at `0x10000000`**, not an
SRAM ELF and not an MCUboot image. `picotool seal` may update the first
block's link to point at an appended signature block; that link is
intentionally outside the SRAM image. Do not hand-edit either block.

For the application, `sign --confirm` writes a padded slot image with a
confirmed trailer, for factory programming of the primary slot. `--pad` marks
it pending instead. Without either flag the output is a bare signed image, as
an update transport would deliver it to the updater API. The tools derive all
addresses and slot sizes from `layout/partitions.json`; do not hand-adjust
the image header or flash offsets.

To program the offline files with your own SWD tooling, write each to its
address and reset, for example:

```sh
probe-rs download --chip RP235x --protocol swd --binary-format bin \
  --base-address 0x10020000 /tmp/rp2350-firmware.signed.bin
probe-rs download --chip RP235x --protocol swd --binary-format bin \
  --base-address 0x10000000 /tmp/ferriboot.key-A.signed.bin
probe-rs reset --chip RP235x --protocol swd
```

**This bypasses the runner's provisioning acknowledgement.** A
bootloader built with or without `production` written this way still
burns OTP on its first successful boot; read [Secure boot](#secure-boot)
first.

### Image tool reference

`boards/rp2350/tools/firmware.py` is the single entry point for RP2350
application and bootloader validation, signing and SWD loading. There are no
RP2350 shell wrappers. From the repository root (replace `APP.elf`,
`BOOTLOADER.elf` and the output names with your own paths):

```sh
# Validate an ELF without signing or flashing.
python3 boards/rp2350/tools/firmware.py check APP.elf
python3 boards/rp2350/tools/firmware.py check BOOTLOADER.elf --bootloader

# Export a validated unsigned flash payload (no signing or flashing).
python3 boards/rp2350/tools/firmware.py export-bin APP.elf app.bin
python3 boards/rp2350/tools/firmware.py export-bin BOOTLOADER.elf bootloader.bin --bootloader

# Sign an MCUboot application or ROM-seal the RP2350 bootloader.
python3 boards/rp2350/tools/firmware.py sign APP.elf app.signed.bin
python3 boards/rp2350/tools/firmware.py sign-bootloader BOOTLOADER.elf bootloader.signed.bin

# Install via SWD; these validate and sign as needed before flashing.
python3 boards/rp2350/tools/firmware.py load-app APP.elf
python3 boards/rp2350/tools/firmware.py load-bootloader BOOTLOADER.elf
```

`export-bin` applies the same ELF layout, flash load-address and boot-info
checks as `check`, and emits only the raw, unsigned payload bytes. The older
`bootloader-bin` command remains as shorthand for
`export-bin BOOTLOADER.elf OUTPUT --bootloader`. `sign-bootloader
--otp-output FILE` additionally saves picotool's **unapplied** OTP plan for a
separate, manually controlled provisioning workflow; neither security profile
needs it.

`load-app` signs and installs a **confirmed** image in the primary slot
(`--slot secondary` targets the other slot). `load-bootloader` ROM-seals the
image when `RP2350_SECURE_BOOT_KEY` is set and enforces the OTP
acknowledgement for provisioning profiles. Both write with
`probe-rs download` and then reset the chip. The per-crate Cargo runners call
these two subcommands, so `cargo run --release` works from
`boards/rp2350/firmware` and `boards/rp2350/ferriboot`, and arguments after
`--` are passed through (`cargo run --release -- --slot secondary`).

`IMAGE_VERSION` (default `0.0.0`) sets the MCUboot image version. Signing
uses the development defaults `--vid microtun.dev --cid rp2350`, matching
the protected VID/CID values required by `ferriboot-rp/src/policy.rs` in
both the bootloader and updater. The `SECURITY_COUNTER`, `VID` and `CID`
environment variables override the imgtool signing defaults, but **changing
VID/CID also requires changing the policy in both binaries**, or those images
will be rejected. Older images without these TLVs need to be re-signed.
Unlike the ESP32-C3 tool, `sign` does not forward extra `imgtool` arguments.

### Flash and boot a development board

**Security warning:** the default Ferriboot build (without `production`)
**permanently** writes the SHA-256 fingerprint of its ROM signing key into
OTP `BOOTKEY0` and enables ROM secure boot on its first successful boot. From
then on the chip boots only bootloaders signed with that key. SWD remains
available in this profile, but the enrollment itself cannot be undone. Use a
key you control and will keep, never the committed development ROM key, and
review [Secure boot](#secure-boot) before flashing. RP2350 Ferriboot
requires secure-boot provisioning; do not flash it onto a board whose OTP
must remain untouched.

Connect the SWD probe and the serial adapter, then install a valid
MCUboot-signed application **first**: Ferriboot provisions OTP only after it
has verified an application it can boot.

```sh
# From the repository root; RP2350_SECURE_BOOT_KEY is exported (see Prerequisites).
cd boards/rp2350/firmware
IMAGE_VERSION=1.0.0 cargo run --release

# Review the warning above first: this explicitly opts in to OTP enrollment.
cd ../ferriboot
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND cargo run --release
```

The bootloader runner validates the ELF, ROM-seals it, checks the seal as
described above, writes it at `0x10000000` with `probe-rs download` and resets
the chip. Without the exact
`FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND` value it
**refuses to flash** any provisioning-capable build, on every flash, even of
an already provisioned board. Without `RP2350_SECURE_BOOT_KEY` it refuses to
flash such a build at all. If more than one probe is attached, select one
with `PROBE_RS_PROBE`.

Observe the logs on the UART0 console with any serial terminal (the device
path is an example):

```sh
picocom -b 115200 /dev/ttyACM1
```

Ferriboot prints its version and upgrade mode and the slot geometry. On the
first boot of a provisioning build it then verifies the application, enrolls
OTP, reports that it is rebooting into the authenticated ROM path, and boots
again, this time through the ROM's signature check. Just before the jump it
prints the image version, slot, performed operation and vector addresses. The example firmware prints its version,
the running image and slot, confirms itself, reports that the UART log console
is ready, and logs a heartbeat every 10 seconds. It also toggles **GPIO15**
every 500 ms; attach an LED there if you want a visual heartbeat (the Pico 2's
on-board LED is on GPIO25, and the Pico 2 W's is driven by the wireless chip).

Ferriboot runs a 15-second watchdog from its own start, fed after every
completed flash operation, so a flash, OTP or TRNG operation that hangs
resets the device and the interrupted operation resumes on the next boot.
If no slot holds a bootable image, Ferriboot logs the reason and **halts**
with the watchdog stopped, so you can inspect the chip over SWD. A flash
error instead resets after a short delay to resume the interrupted operation.

The example firmware has **no MCUmgr/SMP server or UART OTA**: the console
only carries logs, and an MCUmgr client cannot upload, query or test images.
For subsequent development updates, run
`IMAGE_VERSION=1.0.1 cargo run --release` from `boards/rp2350/firmware` to
write a confirmed image **directly into the primary slot**.
`cargo run --release -- --slot secondary` writes the secondary slot instead,
but that does **not** mark an update as pending or exercise a trial upgrade.
To use the updater's normal staging and trial flow, provide your own
transport and drive the `ferriboot-core` updater API.

### Mandatory ROM secure-boot provisioning

There is no RP2350 Ferriboot build that skips ROM secure-boot OTP enrollment.
Without `production`, the **development OTP policy** applies, regardless
of whether Cargo defaults are enabled. The `production` feature only selects
the hardened OTP policy; it does not enable `downgrade-prevention`,
`bootstrap` or `uuid`. These remain enabled by default, so use
`--features production` **without** `--no-default-features` to retain them.
Merely passing Cargo `--release` does **not** select production OTP policy.
On each successful boot Ferriboot verifies OTP and programs any missing
required bits before executing a verified application. An already-complete
profile results in no new OTP writes.

The flashing runner **always requires** a ROM signing key and the explicit
first-boot provisioning acknowledgement, even for a previously enrolled board.
Offline `cargo build` and ROM signing still do not write hardware OTP.

### Writing your own application

Use the example firmware as a template. An application must:

- Target `thumbv8m.main-none-eabihf` (Arm Secure), depend on `ferriboot-rp`
  and `cortex-m-rt`, and enable `embassy-rp`'s `imagedef-none` feature. The
  tools reject applications that carry a ROM image definition, so the ROM can
  never boot an application directly and bypass Ferriboot.
- Enable the `ferriboot-rp` feature for its RP2350 package (`rp235xa` or
  `rp235xb`) and build its flash map with
  `const LAYOUT: ferriboot_rp::Layout = ferriboot_rp::Layout::new(...)` from
  the board layout crate's `FLASH_MAP`; pass `&LAYOUT` to
  `ferriboot_rp::updater` and `LAYOUT.flash_size()` to `hardware::flash`.
- Call `ferriboot_build::configure_rp2350_application(CHIP, CHIP_JSON,
  MAX_IMAGE_SIZE)` (from the board layout crate) in its `build.rs`,
  and **not** provide its own `memory.x`. The generated
  `memory.x` links the application at `0x10400200`, limits it to the slot's
  usable size, reserves the boot-info record at the top of SRAM and includes
  the contract `ferriboot-rp.x` exported by `ferriboot-rp`.
- Enable interrupts itself after initializing its HAL. Ferriboot hands off on
  core 0 in privileged Secure thread mode with PRIMASK set, SysTick stopped,
  NVIC interrupts disabled and stack limits cleared. OTP pages 1, 2 and 32
  are already software-locked against writes in both security states; the
  application must not try to enroll keys or advance the OTP rollback epoch.
- Leave QMI ATRANS1 alone: the application executes through that mapping.
  Start core 1, if needed, with Embassy's multicore APIs so the flash driver
  can pause it while erasing or writing.
- Keep feeding the watchdog. Ferriboot hands off with its 15-second watchdog
  (paused while a debugger halts the core) running and freshly fed, and creating
  `embassy_rp::watchdog::Watchdog` does not stop it. The example wraps its
  flash in `ProgressFlash` so long erases feed it too.
- Read the handoff with `BootInfo::read()`, construct the updater without
  signing keys or policy, and confirm a trial upgrade only after its own
  health checks. The bootloader alone validates image signatures and policy. The
  example confirms unconditionally after a 10 ms placeholder. A panic or reset
  before confirmation returns to Ferriboot, which reverts the trial under the
  swap and `direct-xip-revert` strategies.

### Signing keys, profiles, and features

- `RP2350_SECURE_BOOT_KEY`: secp256k1 private key for the ROM signature. The
  board runners require it to seal or flash a provisioning build and have no
  default; `sign-bootloader --key` overrides it. HIL alone defaults to
  `boards/rp2350/keys/dev-rp2350-rom-secp256k1.pem`.
- `FERRIBOOT_PUBKEYS`: comma-separated trusted P-256 public keys embedded in
  the **bootloader only** at build time.
  `FERRIBOOT_KEY`: matching P-256 private key used for MCUboot signing.
  Defaults are under `keys/`; use absolute paths for custom keys and replace
  both for production. The ROM key cannot stand in for the MCUboot key. See
  [key generation](../../keys/README.md).
- `FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND`: required by
  `load-bootloader` for every flash, regardless of the `production` feature.
- `IMAGE_VERSION`, `SECURITY_COUNTER`, `VID`, `CID`: MCUboot signing options
  (see [Image tool reference](#image-tool-reference)).
- `PROBE_RS_CHIP` (default `RP235x`), `PROBE_RS_PROBE` and
  `PROBE_RS_SPEED_KHZ` select the chip, probe and SWD speed for `probe-rs`.
  `PICOTOOL` overrides the picotool executable.

Bootloader strategies are Cargo features on `rp2350-ferriboot`: default
swap-using-scratch, `overwrite-only`, `direct-xip`, or `direct-xip-revert`
(select at most one). `downgrade-prevention`, `bootstrap` and `uuid` are
on by default. `production` additionally selects hardened ROM OTP policy;
it does not change which other features are enabled. In particular,
`--no-default-features --features production` disables these default features
unless they are explicitly re-enabled. Without `production`, the ROM OTP
policy remains development-oriented, but all builds still provision or verify
ROM secure boot. OTP provisioning is always part of the boot process.
Both binaries always enforce the board's fixed development
VID/CID byte values. The optional `uuid` feature supports runtime conversion
of imgtool-style names via `Policy::with_imgtool_ids` but is not required by
the preset policy. The firmware learns the strategy from the bootloader's handoff and
needs no matching features. Test hooks are always built into the example
firmware and read unauthenticated records from `storage`; do not ship them
in production.

For example, a direct-XIP build with the default security profile:

```sh
cd boards/rp2350/ferriboot
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND \
  cargo run --release --features direct-xip-revert
```

### Hardware-in-the-loop tests

RP2350 HIL **always** builds Ferriboot without `production` and ROM-seals
**every bootloader variant** with the same checked `picotool seal` pipeline as
the board runner. There is no unsigned fallback. Like ESP32-C3 HIL, it
defaults to a committed **development-only** ROM signing key,
`boards/rp2350/keys/dev-rp2350-rom-secp256k1.pem`, so no key variable
is needed. The fixture must remain SWD-accessible and use the **same** secp256k1 key as its enrolled
`BOOTKEY0`; a different key is rejected on reboot. For a board enrolled with
another key, set `RP2350_SECURE_BOOT_KEY=/path/to/existing/rom-key.pem`.

**WARNING: irreversible OTP.** On a blank RP2350, the first successful HIL
boot enrolls the **publicly known dev key** in OTP and enables hardware secure
boot. This provides test coverage, **not production security**: anyone with
the committed private key can sign an accepted ROM bootloader. HIL does not
require the first-boot provisioning acknowledgement. Each scenario also
rewrites the **entire 4 MiB** flash. Only use an expendable development
fixture, never a production-enrolled board.

The suite needs the SWD probe and the UART0 console wired as for normal
development. Before the first run, rebuild the embedded SRAM flash algorithm
(`target.yaml`) as described in
[`flash-algorithm/README.md`](flash-algorithm/README.md); the restore protocol
has no version check, and a stale algorithm can corrupt the restore.

```sh
# From the repository root; destructive full-flash restoration on each test.
export FERRIBOOT_RP2350_PORT=/dev/serial/by-id/usb-Arm_DAPLink_CMSIS-DAP_YOUR_DEVICE_ID
cargo test -p ferriboot-hil --features rp2350,smoke -- --test-threads=1
```

For an already-burned application counter, HIL reads the current epoch
directly over the existing **SWD probe**, reading the 64 raw OTP rows of
page 32 (`0x800..0x83f`) through RP2350's guarded memory-mapped aperture
and applying the same triple-majority decoder as the bootloader (requiring
row `0x83f` to remain blank).
The [opt-in security-counter HIL test](../../README.md#opt-in-security-counter-hil-test)
requires the `security-counter` Cargo feature and permanently consumes one
more triple-voted epoch (three physical counter bits). The command above does
not enable it.

`FERRIBOOT_RP2350_PORT` is the UART console. HIL restores each fixture over
SWD through the Rust `probe-rs` library with a single-pass LZ4 sector stream,
then resets the chip and captures the boot output; the `probe-rs` CLI is not
used. Set `PROBE_RS_PROBE` if more than one probe is attached, and
`PROBE_RS_SPEED_KHZ` to slow down a marginal SWD link.
`FERRIBOOT_RP2350_PROBE_TIMEOUT_SECS` overrides the probe timeout (600
seconds for programming, 30 seconds otherwise). HIL still needs `picotool`
2.3+, Python with `pyelftools`, `cryptography` and `imgtool`, and the Rust
target listed above. MCUboot application images are signed independently
with the P-256 development key.

## Secure boot

The RP2350 variant of Ferriboot has **two independent signatures**:

1. **RP2350 ROM → Ferriboot**: ECDSA **secp256k1**, verified by the RP2350
   mask ROM against a **SHA-256 public-key fingerprint in OTP**. The ROM
   authenticates the complete bootloader before loading its code and data
   from external flash into SRAM. This is **not** the MCUboot/`imgtool`
   signature.
2. **Ferriboot → application**: MCUboot-compatible ECDSA **P-256
   (secp256r1)**, verified with the public key(s) compiled into Ferriboot.
   This uses `tools/firmware.py sign`, `FERRIBOOT_KEY` and
   `FERRIBOOT_PUBKEYS`, and applies to every signed application image in the
   flash slots.

Only the **bootloader** is sealed with `picotool seal`. Application slots are
ROM-ignored data (they have **no** RP2350 `IMAGE_DEF`) and must never be
re-signed as native RP2350 boot images. Ferriboot's custom `.start_block` and
ROM `LOAD_MAP` load the bootloader into SRAM; the signing tool checks this
contract before and after sealing.

**Key-agnostic binary:** Ferriboot embeds **only the dev/release OTP policy**.
Its build does *not* require `RP2350_SECURE_BOOT_KEY`, a ROM public key, or an
OTP fingerprint. `picotool seal --sign` appends a PICOBIN `SIGNATURE` item
containing the **64 raw bytes of the secp256k1 public key**, not its digest.
On boot Ferriboot follows the signed-image metadata block loop in XIP flash,
extracts exactly that public key, computes `SHA256(public_key[0..64])`, and
uses the result as the expected `BOOTKEY0` fingerprint. The host signer checks
that the signed image's embedded key equals the selected PEM's public key and
that `picotool info` verifies the signature. This lets one compiled ELF be
signed later with different keys for different customers or batches. **A
board whose OTP is already provisioned remains bound to its enrolled signing
key**; re-signing the image with another key does not rotate its OTP.

**Important:** signing a binary does *not* enable secure boot on its own.
Hardware verification begins after the chip's OTP key and security flags have
been programmed. The **default build uses the development OTP policy**, which
on its first successful boot can irreversibly enroll the ROM signing key and
secure-boot flags in OTP. The runner requires an external ROM signing key and
explicit acknowledgement before flashing; `cargo build` alone does not flash
hardware or change OTP. Enable `production` to select the hardened OTP policy
(which irrevocably disables SWD and revokes spare key slots). There is no OTP
JSON file or manual `picotool otp load` in this workflow.

### Development vs production OTP policy

| Behavior | Default (no `production`) | `production` feature |
| --- | --- | --- |
| RP2350 ROM authenticates Ferriboot | Yes | Yes |
| Ferriboot verifies P-256 MCUboot app | Yes | Yes |
| Boot key slot 0 | Registered as valid | Registered as valid |
| BOOTKEY1, BOOTKEY2, BOOTKEY3 | Unrevoked; left open for future use | `BOOT_FLAGS1.KEY_INVALID[1..3]` programmed permanently |
| Arm SWD processor/system debugging | Left enabled | `CRIT1.DEBUG_DISABLE` + `SECURE_DEBUG_DISABLE` programmed |
| Factory test JTAG interface | Disabled automatically by ROM secure boot | Disabled automatically by ROM secure boot |
| Glitch detectors | `CRIT1.GLITCH_DETECTOR_ENABLE` programmed; also locked armed each boot | `CRIT1.GLITCH_DETECTOR_ENABLE` programmed; also locked armed each boot |
| ROM USB/UART recovery | Not additionally hardened | Not additionally hardened (outside current profile scope) |

**RP2350 terminology:** the regular debug connection used by `probe-rs` is
**Arm SWD**, not the factory test JTAG interface. Secure boot itself turns off
the factory JTAG interface, even in dev mode. Dev mode keeps **SWD**
available; production mode disables SWD processor debugging too.

Neither policy writes a private key to OTP. Both burn the **same type of
SHA-256 secp256k1 public-key fingerprint** into `BOOTKEY0` and enable ROM
signature enforcement. The P-256 MCUboot application-signing key is separate.

**These are irreversible OTP operations even in dev mode.** Do not use an
untrusted or committed dev ROM signing key, and test on expendable boards. The
private key must be retained for all future ROM-signed Ferriboot builds.

#### Provision a new dev board

Follow [Flash and boot a development board](#flash-and-boot-a-development-board):
install a valid MCUboot-signed application first, then the default
bootloader (without `production`) with the acknowledgement.

On the first successful boot, Ferriboot verifies the application, reads the
secp256k1 public key from its flash-resident ROM signature metadata, hashes it,
checks the OTP state, enrolls `BOOTKEY0` if blank, marks key 0 valid, arms the
glitch detectors in OTP and enables ROM secure boot **last**. It reboots
through ROM. On subsequent boots it verifies the OTP profile and does not
write it again. **SWD remains available**, so you can continue using
`probe-rs` to flash correctly ROM-signed bootloader images. The three spare ROM signing key slots remain
unrevoked and can be enrolled later (all three redundant validity copies must
agree). Treat this as a *development* device, not tamper-resistant production
hardware. If you enroll another signing key in development mode, you must revoke or
manage it separately before enabling `production`: the production build
refuses to silently revoke an already-enrolled independent key.

#### Provision a new production board

On an unprovisioned board, with the required signed application installed:

```sh
cd boards/rp2350/ferriboot
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND \
  cargo run --release --features production
```

In addition to enrolling key 0 and arming the glitch detectors, Ferriboot
invalidates signing key slots 1 through 3 and burns both critical
debug-disable flags, verifying each
redundant OTP copy. **Only then** does it set `SECURE_BOOT_ENABLE` and reboot.
After this reboot normal `probe-rs`/SWD debugging and flashing are no longer
available. `KEY_INVALID` prevents the ROM from accepting these spare slots; it
does not itself write-lock the OTP pages. Ferriboot separately applies
reset-scoped OTP software locks at every successful application handoff.
Existing signed ROM recovery/service paths are not additionally disabled by
this profile.

#### Promote an existing development-enrolled board to production

This is supported **only using the exact same RP2350 ROM signing key** that is
already installed in key slot 0. With SWD still available and a verified
application present, flash a `production` build with that key:

```sh
cd boards/rp2350/ferriboot
# RP2350_SECURE_BOOT_KEY must still be the ORIGINAL device ROM key.
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND \
  cargo run --release --features production
```

The `production` build recognizes an enrolled development state, preserves
`BOOTKEY0` and
the existing `SECURE_BOOT_ENABLE` bit, then irreversibly revokes unused slots
and disables SWD. **Production → development is impossible**: a build
without `production` refuses to boot an OTP state where debug is disabled or
spare key slots are revoked, rather than incorrectly reporting that debug is
available.

#### Safeguards and limits

- The flashing runner requires an explicit
  `FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND` for both
  profiles, like the ESP32-C3's first-boot eFuse acknowledgement. This
  prevents accidental OTP enrollment from `cargo run`; normal `cargo build`
  and offline signing never need it. It does **not** make irreversible burns
  safe, and it does not apply to images you flash with your own tools.
- **Use `--features production` for production boards, keeping default
  features enabled.** The flag adds hardened ROM OTP enrollment only; the
  default `downgrade-prevention`, `bootstrap` and `uuid` features remain on
  because Cargo enables them by default, not because `production` implies
  them. Do not pass `--no-default-features` unless you deliberately re-enable
  the policy features you need. Without `production` (even with `--release`),
  the development OTP policy is selected. No feature disables OTP provisioning.
- **Compilation is key-agnostic.** The signing/flashing runner requires
  `RP2350_SECURE_BOOT_KEY` only when sealing or flashing the image. It checks
  the embedded profile and matches the **PICOBIN signature public key** to the
  chosen PEM before publishing the signed BIN. No permanent OTP change happens
  on the host, and first-boot provisioning creates no OTP JSON.
- The hash **derived at runtime from the ROM signature item** is checked
  against OTP before updates. Unexpected valid or revoked signing slots,
  unknown critical flags, a conflicting hash, and production → development
  downgrades
  halt without executing an application. Partially written expected redundant
  bits are completed when possible.
- OTP flags take effect at the following reset. An interruption while burning
  fuses can leave the unit unbootable; validate a signed boot chain and
  recovery procedure on a sacrificial board first.
- **On the very first boot the ROM has not authenticated Ferriboot yet.**
  Parsing PICOBIN metadata and hashing its embedded key is **not equivalent to
  verifying a signature on-device before initial enrollment**. An attacker who
  can replace the first bootloader before OTP enrollment can provision their
  own key. Manufacturing must control the initial flashing, protect the
  signing process, and independently verify the signed image before first
  reset. Dev mode is not an assurance of resistance to physical access: its
  open SWD port can defeat the trust chain.
- This profile choice does **not** configure the ROM USB/UART boot policy,
  bootloader rollback counter, or permanent OTP page locks.
  `production` controls only the documented ROM OTP and boot policy, not these
  manufacturing restrictions. Ferriboot instead tightens software page locks for the current boot at the
  application handoff. Review the remaining controls separately before
  claiming production threat-model coverage.

### Reset-scoped OTP handoff locks

Before transferring control to the authenticated Arm Secure application,
Ferriboot uses RP2350 `OTP_SW_LOCKx` to make the OTP pages needed for its
security guarantees **read-only** to both Secure and Non-secure code:

| Page | OTP rows | Protected data |
| --- | --- | --- |
| 1 | `0x040..0x07f` | `CRIT1`, `BOOT_FLAGS` (ROM secure-boot and key-validity flags) |
| 2 | `0x080..0x0bf` | `BOOTKEY0..3` (ROM public-key fingerprints) |
| 32 | `0x800..0x83f` | Ferriboot application rollback counter |

The lock operation occurs **after** Ferriboot has validated the application,
completed ROM security enrollment/verification and performed any necessary
rollback-counter writes. It ORs the `READ_ONLY` bit into *both* security-state
lock fields and reads each register back; failure to establish write
protection halts the boot rather than handing off. An already-inaccessible
page stays inaccessible. Ferriboot also sets the OTP and POWMAN `BOOTDIS.NEXT`
latches, preventing a watchdog/power-manager scratch-vector reboot from
skipping Ferriboot while those locks are reset. The ordinary boot path remains
available.

These are **volatile locks, not additional OTP burns**. They can only be
tightened until the OTP resets. On the next normal reset, ROM launches
Ferriboot again, which can advance the rollback counter or perform authorized
provisioning before reapplying the locks. Applications do not need OTP write
access for trial confirmation; Ferriboot commits the counter on the next boot.
Software locks do not protect other OTP pages, guard against a hostile debugger
in dev mode, or replace permanent provisioning policy. Hardware verification
is required before treating this as a complete mitigation for the platform's
OTP availability risks.

### Glitch detectors

Both profiles burn `CRIT1.GLITCH_DETECTOR_ENABLE` (all eight redundant copies,
each read back) during OTP enrollment, before `SECURE_BOOT_ENABLE`. From the
following reset the RP2350's four glitch detectors are armed by the power-on
sequence, so they also cover the mask ROM and its signature check of
Ferriboot. **This is irreversible**, like the rest of enrollment. Boards
enrolled by an earlier Ferriboot have the bit added on their next boot, followed
by one extra reboot.

Ferriboot also arms the detectors through the `GLITCH_DETECTOR` registers and
sets `LOCK` as its first action on every boot, before clock setup, slot
validation or any OTP access. This covers the enrollment boot itself, before
the OTP bit takes effect, and stops the Secure application using `DISARM` to
override OTP arming or changing the sensitivity until the next reset. Every
register is read back; if the detectors are not armed, locked and at the
OTP-default sensitivity, Ferriboot halts instead of booting.

Sensitivity stays at the factory default. Ferriboot never programs
`CRIT1.GLITCH_DETECTOR_SENS`, but accepts a value provisioned separately (for
example with picotool) and uses it.

When armed, a detector trigger resets the switched core power domain. On the
next boot Ferriboot logs a warning if `POWMAN_CHIP_RESET.HAD_GLITCH_DETECT`
is set, and reports (then clears) any `TRIG_STATUS` bits recorded while the
detectors were not armed. It takes no other action: there is no retry
counter or lockout after repeated glitch resets.

This is **basic** protection and has clear limits:

- The very first boot of an unprovisioned board, which performs the
  enrollment, still runs the ROM with the detectors unarmed (as it also runs
  without signature enforcement). Initial provisioning must happen in a
  controlled environment.
- Detection is probabilistic. Faults that are short, localised (EMFI, laser)
  or tuned below the detector threshold can still succeed. The software
  countermeasures described in the top-level README's
  [fault-injection hardening](../../README.md#fault-injection-hardening)
  section (a second authentication before the jump and before every OTP
  counter burn, TRNG-randomised timing, glitch-resistant checks) raise the
  number of precisely placed faults an attack needs; they do not prove
  resistance.
- Applications that change the core voltage regulator or run outside the
  normal clock/voltage envelope should confirm on hardware that they do not
  cause spurious glitch resets. With OTP arming, a board whose supply or
  clock trips the detectors cannot boot at all; validate the hardware before
  enrolling it.

### Production security limitations and hardening

- ROM verification happens **once during boot**. Running Ferriboot from SRAM
  avoids executing unauthenticated external flash bytes after the ROM
  signature check. Ferriboot independently authenticates each application
  before its execution. Physical-access attacks against the mutable XIP
  application flash remain a separate threat and require appropriate
  system-level mitigation.
- ROM secure boot alone does not protect SWD/debug access, OTP permissions, or
  all ROM recovery interfaces. Ferriboot software-locks its critical OTP pages
  at handoff; **other pages remain outside this protection**. The **release**
  profile burns debug-disable and unused-boot-key revocation, but **does not**
  disable BOOTSEL/PICOBOOT or permanently program OTP page locks. Account for these in the full production threat model.
- Ferriboot's firmware `downgrade-prevention` feature does not burn the ROM
  rollback counter. A separately versioned, OTP-backed bootloader rollback
  policy would be necessary for *hardware-enforced* bootloader anti-rollback.
- The build produces **no native signed partition table**:
  `layout/partitions.json` is Ferriboot's flash-layout source of truth, and
  only the flash-base bootloader is a ROM-discoverable image. Don't add other
  ROM-discoverable executable partitions without signing and auditing them.
- **Test on a sacrificial board before provisioning a production unit.** The
  Python tools validate file structure and picotool's reported signature but
  cannot prove an OTP-enabled boot without actual hardware testing.
## Application rollback epochs

The bootloader always enforces the hardware security counter using **raw OTP
page 32** (`0x800..0x83f`). Reserve the *entire* page exclusively; no ECC
access is allowed. Every consecutive group of three 24-bit rows (`0x800..0x802`,
`0x803..0x805`, …, `0x83c..0x83e`) encodes the same thermometer prefix.
Ferriboot bitwise-majority-votes the three copies and rejects invalid voted
thermometers. This yields **504 security epochs (0..504)** instead of 1536.
The remaining row (`0x83f`) is **unused and must stay blank**. There is **no
format marker**, version autodetection or ECC. This follows the ROM's
three-row raw-OTP redundancy model.

Each increment burns its bit in all three rows, verifying each physical
write by reading it back, and checks the majority floor before application
handoff. A power cut after one copy leaves the previous voted epoch; after
two copies the new epoch is active. A later advancement repairs missing
replicas of previously voted bits. A blank page represents epoch 0 and no
marker or other metadata is ever burned.

**Existing single-row boards:** Do **not** boot this firmware on a board
with an already-programmed single-row rollback counter. The two OTP
encodings are ambiguous and could produce a lower interpreted rollback
floor. This bootloader supports only the triple-redundant layout; it
does **not** provide an in-place conversion mechanism. OTP bits cannot
be erased. Likewise, a board with a previously programmed marker in
row `0x83f` is incompatible with the current marker-free layout.

Applications require a protected `SEC_CNT` TLV. Signing defaults to epoch 0;
use `SECURITY_COUNTER=N` for deliberate security releases. Trial boots preserve
the previous floor; confirmation is committed to OTP on the next reset, and
permanent installs commit before handoff. See [hardware application
anti-rollback](../../README.md#hardware-application-anti-rollback) for provisioning,
capacity and failure handling.
