# RP2350 (Pico 2 / Pico 2 W)

The RP2350 mask ROM can authenticate the bootloader itself, and QMI address
translation lets an application run from either slot without relinking, so
Ferriboot is the **only** stage after the ROM. It runs entirely from SRAM:

1. The mask ROM copies Ferriboot from flash offset `0x0` into SRAM and, once
   secure boot is enrolled in OTP, checks its **secp256k1** signature against
   the key fingerprint in `BOOTKEY0`.
2. Ferriboot verifies the MCUboot-signed (ECDSA **P-256**) application in
   `image-0` or `image-1`, performs any pending upgrade, maps the chosen slot
   to `0x10400000` with QMI ATRANS1 and jumps to the vector table at
   `0x10400200`.

One firmware build therefore works in either slot, including direct-XIP.

> **Every build enrolls ROM secure boot in OTP on its first successful boot.**
> This is irreversible. Read [Secure boot](#secure-boot) before flashing.

## Requirements and limitations

- Pico 2 or Pico 2 W (RP2350A) with **4 MiB** flash.
- Arm Secure mode on core 0 (`thumbv8m.main-none-eabihf`). RISC-V cores are
  not supported; Ferriboot never starts core 1.
- The ROM-sealed bootloader must fit in **128 KiB**, and its SRAM image must
  end below the boot-info record at `0x2007FF00`.
- No native RP2350 partition table is written, and ROM A/B partitions are not
  used. `layout/partitions.json` is only the layout source for Ferriboot and
  the tools.
- The application rollback counter needs all of OTP page 32; boards with a
  pre-existing single-row counter are incompatible (see
  [Security counter](#security-counter)).
- BOOTSEL/PICOBOOT, the ROM rollback counter and permanent OTP page locks are
  not configured. See [THREAT-MODEL.md](../../THREAT-MODEL.md) (L9, L10).

## Flash layout

Defined in `layout/partitions.json` (picotool schema), with the flash size and
chip addresses in `layout/src/board.rs`.
Checked by `ferriboot-tools-rp2350 layout`, the `rp2350-layout` crate and, at compile time,
`ferriboot_rp::Layout::new`.

| Region | Offset | Size |
|---|---|---|
| `bootloader` | `0x0` | 128 KiB |
| `image-0` (primary) | `0x20000` | 1920 KiB |
| `image-1` (secondary) | `0x200000` | 1920 KiB |
| *(unallocated)* | `0x3E0000` | 32 KiB |
| `image-scratch` | `0x3E8000` | 64 KiB |
| `storage` (application-owned) | `0x3F8000` | 32 KiB |

Offsets are relative to `0x10000000`. Each slot starts with a 512-byte MCUboot
header, so the vector table meets VTOR alignment. The largest signed image is
**1912 KiB**. The top 256 bytes of SRAM (`0x2007FF00`–`0x2007FFFF`) hold the
boot-info record; the tools reject images that overlap it.

```sh
ferriboot-tools-rp2350 layout
```

## Build and run

### Prerequisites

- Rust **1.95+** with the `thumbv8m.main-none-eabihf` target.
- Upstream `imgtool` (and Python for that external tool).
- `probe-rs` CLI and an SWD probe such as the Raspberry Pi Debug Probe. All
  flashing uses SWD, not BOOTSEL/UF2.
- A 3.3 V serial adapter on **GPIO0** (UART0 TX, Pico pin 1), 115200 8N1.
- Your own **RP2350-only** secp256k1 ROM signing key, provided as a local PEM
  or through `microtun-signer`. Never reuse the MCUboot or ESP key. Keep
  signing access available: all future bootloaders for enrolled boards must
  use the same trusted key.

```sh
rustup target add thumbv8m.main-none-eabihf
python3 -m pip install imgtool
cargo install --path ferriboot-rp/tools --locked
cargo install probe-rs-tools --locked
# Generate the MCUboot key pair once; see ../../keys/README.md.
./keys/generate.sh

mkdir -p "$HOME/secure-keys" && umask 077
openssl ecparam -name secp256k1 -genkey -noout -out "$HOME/secure-keys/rp2350-rom.pem"
export RP2350_SECURE_BOOT_KEY="$HOME/secure-keys/rp2350-rom.pem"
```

The board runners never fall back to a development ROM key.

### Build and sign offline

Run Cargo from **each binary's directory**. Building and signing never touch
hardware or OTP, and compiling needs no ROM key.

```sh
cd boards/rp2350/ferriboot
cargo build --release                        # development OTP policy
cargo build --release --features production  # production OTP policy
cd ../firmware && cargo build --release && cd ../../..

ferriboot-tools-rp2350 sign-bootloader \
  boards/rp2350/target/thumbv8m.main-none-eabihf/release/rp2350-ferriboot \
  /tmp/ferriboot.signed.bin
IMAGE_VERSION=1.0.0 ferriboot-tools-rp2350 sign --confirm \
  boards/rp2350/target/thumbv8m.main-none-eabihf/release/rp2350-firmware \
  /tmp/rp2350-firmware.signed.bin
```

| File | Flash address |
|---|---|
| `ferriboot.signed.bin` | `0x10000000` |
| `rp2350-firmware.signed.bin` | `image-0`, `0x10020000` |

The ELFs cannot be flashed as-is. `sign-bootloader` uses the embedded Rust
seal/reseal implementation to independently verify the SHA-256 hash and
secp256k1 signature, the precise ROM flash-to-SRAM load map, the V3 OTP profile,
the embedded public key and the 128 KiB bootloader size limit.
The binary is key-agnostic, so one ELF can be sealed with different keys.

Alternatively, use the normal Cargo runners with `NO_FLASH=1` to sign and save
both images **without connecting a board**:

```sh
cd boards/rp2350/firmware
IMAGE_VERSION=1.0.0 NO_FLASH=1 cargo run --release

cd ../ferriboot
NO_FLASH=1 cargo run --release
```

The runners save `rp2350-firmware.signed.bin` and
`rp2350-ferriboot.signed.bin`, respectively, next to their ELFs in
`boards/rp2350/target/thumbv8m.main-none-eabihf/release/`. Both commands
still need their signing keys, but neither calls `probe-rs`, resets the chip,
or requires the first-boot OTP acknowledgement. Unset `NO_FLASH` (or set it
to `0`) to restore normal flashing.

`ferriboot-tools-rp2350` subcommands (all take an ELF):

| Command | Action |
|---|---|
| `check [--bootloader]` | Validate without signing |
| `export-bin OUT [--bootloader]` | Export unsigned flash bytes |
| `sign OUT [--confirm \| --pad]` | MCUboot-sign: confirmed slot image, pending slot image, or bare image for the updater |
| `sign-bootloader OUT [--key PEM] [--otp-output FILE]` | ROM-seal the bootloader; no OTP writes |
| `load-app [--slot secondary]` | Sign a confirmed image and flash it over SWD (or save it with `NO_FLASH=1`) |
| `load-bootloader` | Seal and flash over SWD (requires the acknowledgement unless `NO_FLASH=1`) |

The Cargo runners call `load-app` and `load-bootloader`; arguments after `--`
are passed through. Unlike ESP32-C3, `sign` does not forward extra `imgtool`
arguments.

### Flash a development board

Install a valid application **first**: Ferriboot provisions OTP only after it
has verified an application it can boot.

```sh
cd boards/rp2350/firmware
IMAGE_VERSION=1.0.0 cargo run --release

cd ../ferriboot
FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND cargo run --release
```

The bootloader runner refuses to flash without the acknowledgement and
either `RP2350_SECURE_BOOT_KEY` or configured `MICROTUN_SIGNER_URL` plus
`MICROTUN_SIGNER_KEY_ID`, on every flash. Flashing the sealed file with your
own SWD tooling bypasses that check but still provisions OTP. View logs with
any serial terminal, for example `picocom -b 115200 /dev/ttyACM1`.

On first boot Ferriboot verifies the application, enrolls OTP and reboots
through the ROM's signature check. The example firmware then confirms itself,
logs a heartbeat every 10 s and toggles **GPIO15** every 500 ms.

Ferriboot runs a 15-second watchdog, fed after each flash operation, so a hang
resets and the interrupted operation resumes. If no slot is bootable it logs
the reason and halts with the watchdog stopped for SWD inspection.

For later updates, rerun the firmware command with a new `IMAGE_VERSION`; it
writes the primary slot directly. `-- --slot secondary` writes the other slot
but does **not** mark an upgrade pending. The example firmware has no
MCUmgr/SMP server; a real update flow needs your own transport and the
`ferriboot-core` updater API.

## Writing an application

Use the example firmware as a template. An application must:

- Target `thumbv8m.main-none-eabihf`, depend on `ferriboot-rp` (feature
  `rp235xa` or `rp235xb`) and `cortex-m-rt`, and enable `embassy-rp`'s
  `imagedef-none`. The tools reject images the ROM could boot directly.
- Build its flash map with `ferriboot_rp::Layout::new(FLASH_MAP)` from the
  board layout crate.
- Call `ferriboot_build::configure_rp2350_application(CHIP,
  MAX_IMAGE_SIZE)` in `build.rs` and not provide its own `memory.x`.
- Enable interrupts itself. Ferriboot hands off on core 0 in privileged Secure
  mode with PRIMASK set, SysTick stopped, NVIC disabled and OTP pages 1, 2 and
  32 write-locked.
- Leave QMI ATRANS1 alone, and start core 1 with Embassy's multicore APIs so
  flash operations can pause it.
- Keep feeding the watchdog, which is still running at handoff.
- Read `BootInfo::read()`, construct the updater without keys or policy, and
  confirm a trial upgrade only after its own health checks. A reset before
  confirmation reverts under swap and `direct-xip-revert`.

## Configuration

| Variable | Purpose |
|---|---|
| `RP2350_SECURE_BOOT_KEY` | secp256k1 local PEM ROM key; used when no remote signer is configured (`--key` overrides) |
| `MICROTUN_SIGNER_URL`, `MICROTUN_SIGNER_KEY_ID` | Optional remote signing endpoint and secp256k1 key ID; see [SIGNING.md](../../SIGNING.md) |
| `FERRIBOOT_PUBKEYS` | Comma-separated P-256 public keys built into the bootloader |
| `FERRIBOOT_KEY` | P-256 private key for MCUboot signing |
| `FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND` | Required by `load-bootloader` when flashing |
| `NO_FLASH=1` | Have `load-app` and `load-bootloader` sign to `<ELF stem>.signed.bin` beside the ELF without SWD flashing, resetting or provisioning OTP |
| `IMAGE_VERSION` | MCUboot image version (default `0.0.0`) |
| `SECURITY_COUNTER`, `VID`, `CID` | Override signing defaults (epoch `0`, `--vid microtun.dev --cid rp2350`) |
| `PROBE_RS_CHIP`, `PROBE_RS_PROBE`, `PROBE_RS_SPEED_KHZ` | probe-rs chip (default `RP235x`), probe and SWD speed |

Custom VID/CID values also require changing `ferriboot-rp/src/policy.rs`,
which both binaries enforce.

Cargo features on `rp2350-ferriboot`:

- Strategy, at most one: default swap-using-scratch, `overwrite-only`,
  `direct-xip`, `direct-xip-revert`. The firmware learns the strategy at
  handoff.
- Default: `downgrade-prevention`, `bootstrap`, `uuid`.
- `production`: hardened OTP policy. It is additive, so keep default features
  on; `--release` alone does **not** select it.

The example firmware always includes the unauthenticated
[test hooks](../../testing/ferriboot-test-hooks/README.md); do not ship it.

## Secure boot

Two independent signatures protect the chain:

1. **ROM → Ferriboot:** secp256k1, sealed in-process in Rust. The ROM checks
   it against the SHA-256 key fingerprint in OTP `BOOTKEY0`.
2. **Ferriboot → application:** MCUboot ECDSA P-256 with the keys compiled
   into Ferriboot. Application slots are plain data to the ROM and must never
   be sealed.

Ferriboot reads the public key from its own ROM signature block and uses its
hash as the expected `BOOTKEY0`. On every successful boot it enrolls or
verifies OTP and completes any missing bits; secure boot is enabled **last**,
followed by a reboot. An enrolled board stays bound to its key: re-sealing
with another key does not rotate OTP.

| | Default | `production` |
|---|---|---|
| ROM authenticates Ferriboot | Yes | Yes |
| `BOOTKEY0` enrolled and valid | Yes | Yes |
| `BOOTKEY1..3` | Left open | Revoked |
| Arm SWD debugging | Enabled | Permanently disabled |
| Factory test JTAG | Disabled by secure boot | Disabled by secure boot |
| Glitch detectors | Armed in OTP and locked each boot | Armed in OTP and locked each boot |

**Promoting to production** is supported with the **same** ROM key: flash a
`production` build to a development-enrolled board while SWD is still
available. Production → development is impossible; a default build refuses to
boot a board with debug disabled or spare slots revoked. A development board
with extra keys enrolled must revoke them separately first.

Safety notes:

- **First boot is trust-on-first-use.** The ROM has not yet verified Ferriboot,
  so whoever controls the first image chooses the key. Do initial flashing in
  a controlled environment.
- An interrupted OTP burn can leave a board unbootable. Validate the full
  chain on a sacrificial board first.
- Unexpected key slots, unknown critical flags or a conflicting hash halt the
  boot without running an application.

### Handoff locks

Before handoff Ferriboot sets `OTP_SW_LOCK` read-only, for both security
states, on page 1 (`CRIT1`, `BOOT_FLAGS`), page 2 (`BOOTKEY0..3`) and page 32
(rollback counter), and reads each lock back. It also sets the `BOOTDIS.NEXT`
latches so a scratch-vector reboot cannot skip Ferriboot. These locks last
until reset; other OTP pages are not protected.

### Glitch detectors

Both profiles burn `CRIT1.GLITCH_DETECTOR_ENABLE` during enrollment, so from
the next reset the four detectors also cover the ROM. Ferriboot additionally
arms and locks them as its first action on every boot and halts if that
fails. Sensitivity stays at the OTP value. A glitch reset is logged on the next
boot; there is no lockout. Boards whose supply or clock trips the detectors
cannot boot once armed, so validate hardware before enrolling. See
[fault-injection hardening](../../THREAT-MODEL.md#fault-injection-hardening).

## Security counter

The application rollback floor uses **raw OTP page 32** (`0x800..0x83f`),
reserved exclusively. Each group of three 24-bit rows stores the same
thermometer code, majority-voted, giving **504 epochs**. Row `0x83f` must stay
blank. Each increment burns all three copies with readback; a power cut after
one copy keeps the old epoch, after two the new one.

**Do not boot Ferriboot on a board with a single-row counter or a marker in
row `0x83f`** from an older layout: the encodings are ambiguous and OTP cannot
be erased.

Images need a protected `SEC_CNT` TLV (default epoch 0; set
`SECURITY_COUNTER=N` for security releases). Trial boots keep the old floor
and the new one is burned on the next reset after confirmation; permanent
installs burn before handoff.

## Hardware-in-the-loop tests

See [`ferriboot-hil`](../../testing/ferriboot-hil/README.md) for the harness itself.

> Use a **dedicated, expendable board**. HIL always builds without
> `production` and, unless `RP2350_SECURE_BOOT_KEY` is set, seals with the
> development key `boards/rp2350/keys/dev-rp2350-rom-secp256k1.pem`. On a blank
> chip the first test boot enrolls that key permanently, with no
> acknowledgement. Every scenario rewrites all 4 MiB.

Wire SWD and UART0 as above, and rebuild the
[flash algorithm](flash-algorithm/README.md) whenever its sources change.

```sh
export FERRIBOOT_RP2350_PORT=/dev/serial/by-id/usb-Arm_DAPLink_CMSIS-DAP_YOUR_DEVICE_ID
cargo test -p ferriboot-hil --features rp2350,smoke -- --test-threads=1
```

`FERRIBOOT_RP2350_PORT` is the UART console. HIL restores flash over SWD with
the `probe-rs` library and reads the security counter directly from OTP. Set
`PROBE_RS_PROBE` and `PROBE_RS_SPEED_KHZ` as needed, and
`FERRIBOOT_RP2350_PROBE_TIMEOUT_SECS` to change the probe timeout (600 s for
programming, 30 s otherwise). The `security-counter` feature adds
anti-rollback tests that burn one more epoch (three OTP bits) per run.
