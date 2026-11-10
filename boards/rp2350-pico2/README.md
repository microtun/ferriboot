# RP2350 Pico 2 / Pico 2 W

On RP2350, Ferriboot is the **only** stage after the mask ROM and runs
entirely from SRAM:

1. The ROM copies Ferriboot from flash offset `0x0` into SRAM and, once secure
   boot is enrolled in OTP, checks its **secp256k1** signature against the key
   fingerprint in `BOOTKEY0`.
2. Ferriboot verifies the application (MCUboot format, ECDSA **P-256**) in
   `image-0` or `image-1`, performs any pending upgrade, maps the chosen slot
   to `0x10400000` with QMI address translation and jumps to it.

Because of the address translation, one firmware build runs from either slot,
including with direct-XIP.

> **Every build enrolls ROM secure boot in OTP on its first successful boot.**
> This is irreversible. Read [Secure boot](#secure-boot) before flashing.

## Requirements and limitations

- Pico 2 or Pico 2 W (RP2350A) with **4 MiB** flash.
- Arm Secure mode on core 0 (`thumbv8m.main-none-eabihf`). RISC-V cores are
  not supported, and Ferriboot never starts core 1.
- The signed bootloader must fit in **128 KiB**.
- No RP2350 partition table is written and ROM A/B partitions are not used.
  `layout/partitions.json` is only the layout source for Ferriboot and the
  tools.
- The rollback counter needs all of OTP page 32 (see
  [Security counter](#security-counter)).
- BOOTSEL/PICOBOOT, the ROM rollback counter and permanent OTP page locks are
  not configured. See [THREAT-MODEL.md](../../THREAT-MODEL.md) (L9, L10).

## Flash layout

Defined in `layout/partitions.json` (picotool schema) and checked at compile
time by `ferriboot_rp::Layout::new`.

| Region | Offset | Size |
|---|---|---|
| `bootloader` | `0x0` | 128 KiB |
| `image-0` (primary) | `0x20000` | 1920 KiB |
| `image-1` (secondary) | `0x200000` | 1920 KiB |
| *(unallocated)* | `0x3E0000` | 32 KiB |
| `image-scratch` | `0x3E8000` | 64 KiB |
| `storage` (application-owned) | `0x3F8000` | 32 KiB |

Offsets are relative to `0x10000000`. Each slot starts with a 512-byte MCUboot
header, and the largest signed image is **1912 KiB**. The top 256 bytes of
SRAM (`0x2007FF00`–`0x2007FFFF`) hold the boot-info record; the tools reject
images that overlap it.

```sh
ferriboot-tools-rp2350 layout
```

## Setup

- Rust **1.95+** with the `thumbv8m.main-none-eabihf` target.
- Upstream `imgtool` (needs Python).
- An SWD probe such as the Raspberry Pi Debug Probe. All flashing uses SWD,
  not BOOTSEL/UF2. The `probe-rs` CLI is not required.
- A 3.3 V serial adapter on **GPIO0** (UART0 TX, Pico pin 1), 115200 8N1, to
  view logs.
- Your own **RP2350-only** secp256k1 ROM signing key, as a local PEM or
  through `microtun-signer` (see [SIGNING.md](../../SIGNING.md)). Never reuse
  the application or ESP key, and keep it available: every future bootloader
  for an enrolled board must be signed with the same key.

```sh
rustup target add thumbv8m.main-none-eabihf
python3 -m pip install imgtool
cargo install --path ferriboot-rp/tools --locked
./keys/generate.sh    # application signing key; see ../../keys/README.md

mkdir -p "$HOME/secure-keys" && umask 077
openssl ecparam -name secp256k1 -genkey -noout -out "$HOME/secure-keys/rp2350-rom.pem"
export RP2350_SECURE_BOOT_KEY="$HOME/secure-keys/rp2350-rom.pem"
```

The board runners never fall back to a development ROM key.

## Flash a development board

Flash a valid application **first**: Ferriboot provisions OTP only after it
has verified an application it can boot. Run Cargo from each binary's
directory:

```sh
cd boards/rp2350-pico2/firmware
IMAGE_VERSION=1.0.0 cargo run --release

cd ../ferriboot
FERRIBOOT_OTP_BURN=YES cargo run --release
```

`FERRIBOOT_OTP_BURN=YES` acknowledges the irreversible OTP provisioning; the
bootloader runner refuses to flash without it. Flashing the signed file with
your own SWD tooling skips that check but still provisions OTP.

On first boot Ferriboot verifies the application, enrolls OTP and reboots
through the ROM's signature check. The example firmware then confirms itself,
logs a heartbeat every 10 s and toggles **GPIO15** every 500 ms. View logs
with any serial terminal, for example `picocom -b 115200 /dev/ttyACM1`.

Ferriboot runs a 15-second watchdog, so a hang resets and the interrupted
operation resumes. Every bootloader error also resets after a two-second
delay instead of halting. A persistent problem therefore causes a reboot
loop, but never starts an unverified application.

To update the application later, rerun the firmware command with a new
`IMAGE_VERSION`. It erases the entire primary slot and writes the signed
image. `-- --slot secondary` writes the other slot instead, but does **not**
mark an upgrade pending.

The example firmware has no MCUmgr/SMP server. A real update flow needs your
own transport and the
[updater API](../../ferriboot-core/README.md#updating-from-an-application).

## Build and sign without flashing

Set `NO_FLASH=1` to have the runners sign and save both images without
connecting a board. The signing keys are still needed, but no SWD session is
opened and no OTP acknowledgement is required.

```sh
cd boards/rp2350-pico2/firmware
IMAGE_VERSION=1.0.0 NO_FLASH=1 cargo run --release

cd ../ferriboot
NO_FLASH=1 cargo run --release
```

This saves `rp2350-pico2-firmware.signed.bin` and
`rp2350-pico2-ferriboot.signed.bin` in
`target/thumbv8m.main-none-eabihf/release/` at the repository root.

| File | Flash address |
|---|---|
| `rp2350-pico2-ferriboot.signed.bin` | `0x10000000` |
| `rp2350-pico2-firmware.signed.bin` | `image-0`, `0x10020000` |

The ELFs cannot be flashed as-is. Add `--features production` to the
bootloader build for the production OTP policy (see
[Secure boot](#secure-boot)); `--release` alone does not select it.

### Tool commands

The image commands of `ferriboot-tools-rp2350` take an ELF as input:

| Command | Action |
|---|---|
| `check [--bootloader]` | Validate without signing |
| `export-bin OUT [--bootloader]` | Export unsigned flash bytes |
| `sign OUT [--confirm \| --pad]` | Sign the application: confirmed slot image, pending slot image, or bare image for the updater |
| `sign-raw PAYLOAD OUT` | Sign an arbitrary payload with this board's image geometry (not an ELF) |
| `sign-bootloader OUT [--key PEM] [--otp-output FILE]` | Sign the bootloader for the ROM; no OTP writes |
| `flash-app [--slot secondary]` | Erase the selected slot, then flash the signed image over SWD |
| `flash-bootloader [--key PEM] [--signed-bin]` | Sign and flash the bootloader over SWD |
| `layout` | Print and check the flash layout |
| `keys` | Generate a development ROM key |
| `flash-algorithm` | Rebuild the HIL [flash algorithm](flash-algorithm/README.md) and its `target.yaml` |

The Cargo runners call `flash-app` and `flash-bootloader`; arguments after
`--` are passed through. The bootloader binary is key-agnostic, so one ELF can
be signed with different keys.

### Flash a signed release binary

To flash an already signed bootloader, such as a CI release artifact, pass
`--signed-bin` in place of an ELF:

```sh
FERRIBOOT_OTP_BURN=YES ferriboot-tools-rp2350 flash-bootloader --signed-bin \
  rp2350-pico2-release-ferriboot.signed.bin
```

This checks the signature, ROM load map and OTP profile, then programs the
file at `0x10000000` without re-signing, so no signing key is needed. The
flag is required for any signed binary. With `NO_FLASH=1` it only verifies.
Verification does not prove that the board's OTP key matches the key in the
file.

## Writing an application

Use the example firmware as a template. An application must:

- Target `thumbv8m.main-none-eabihf`, depend on
  [`ferriboot-rp`](../../ferriboot-rp/README.md) (feature
  `rp235xa` or `rp235xb`) and `cortex-m-rt`, and enable `embassy-rp`'s
  `imagedef-none`. The tools reject images the ROM could boot directly.
- Build its flash map with `ferriboot_rp::Layout::new(FLASH_MAP)` from the
  board layout crate.
- Call `ferriboot_build::configure_rp2350_application(MAX_IMAGE_SIZE)` in
  `build.rs`, and not provide its own `memory.x`.
- Enable its own interrupt sources. Ferriboot hands off on core 0 in
  privileged Secure mode with interrupts unmasked but all NVIC interrupts
  disabled and cleared, SysTick stopped, and OTP pages 1, 2 and 32
  write-locked.
- Leave QMI ATRANS1 alone, and start core 1 with Embassy's multicore APIs so
  flash operations can pause it.
- Keep feeding the watchdog, which is still running at handoff.
- Read `BootInfo::read()`, construct the updater without keys or policy, and
  confirm a trial upgrade only after its own health checks. A reset before
  confirmation reverts under swap and `direct-xip-revert`.

Do not ship the example firmware: it always includes the unauthenticated
[test hooks](../../testing/ferriboot-test-hooks/README.md).

## Configuration

| Variable | Purpose |
|---|---|
| `RP2350_SECURE_BOOT_KEY` | secp256k1 ROM key as a local PEM (`--key` overrides) |
| `MICROTUN_SIGNER_URL`, `MICROTUN_SIGNER_KEY_ID` | Optional remote signing; see [SIGNING.md](../../SIGNING.md) |
| `FERRIBOOT_PUBKEYS` | Comma-separated P-256 public keys built into the bootloader |
| `FERRIBOOT_KEY` | P-256 private key for application signing |
| `FERRIBOOT_OTP_BURN=YES` | Required to flash the bootloader |
| `NO_FLASH=1` | Sign and save images without touching the board |
| `IMAGE_VERSION` | Application image version (default `0.0.0`) |
| `SECURITY_COUNTER`, `VID`, `CID` | Override signing defaults (epoch `0`, `--vid microtun.dev --cid rp2350`) |
| `PROBE_RS_CHIP`, `PROBE_RS_PROBE`, `PROBE_RS_SPEED_KHZ` | probe-rs chip (default `RP235x`), probe and SWD speed |
| `IMGTOOL` | Override the `imgtool` executable |

Custom VID/CID values also require changing `ferriboot-rp/src/policy.rs`.

Cargo features on `rp2350-pico2-ferriboot`:

- Upgrade strategy, at most one: swap-using-scratch (default),
  `overwrite-only`, `direct-xip`, `direct-xip-revert`.
- Enabled by default: `downgrade-prevention`, `bootstrap`, `uuid`.
- `production`: hardened OTP policy. It is additive, so keep the default
  features on.

## Secure boot

Two independent signatures protect the chain:

1. **ROM → Ferriboot:** secp256k1. The ROM checks it against the SHA-256 key
   fingerprint in OTP `BOOTKEY0`.
2. **Ferriboot → application:** ECDSA P-256 (MCUboot format) with the keys
   compiled into Ferriboot. Application slots are plain data to the ROM.

Ferriboot reads the public key from its own ROM signature block and uses its
hash as the expected `BOOTKEY0`. On every successful boot it enrolls or
verifies OTP; secure boot is enabled **last**, followed by a reboot. An
enrolled board stays bound to its key: signing with another key does not
rotate OTP.

| | Default | `production` |
|---|---|---|
| ROM authenticates Ferriboot | Yes | Yes |
| `BOOTKEY0` enrolled and valid | Yes | Yes |
| `BOOTKEY1..3` | Left open | Revoked |
| Arm SWD debugging | Enabled | Permanently disabled |
| Factory test JTAG | Disabled by secure boot | Disabled by secure boot |
| Glitch detectors | Armed in OTP and locked each boot | Armed in OTP and locked each boot |

**Promoting to production** works with the **same** ROM key: flash a
`production` build to a development-enrolled board while SWD is still
available. Going back is impossible: a default build refuses to boot a board
with debug disabled or spare key slots revoked. A development board with
extra keys enrolled must revoke them first.

Safety notes:

- **First boot is trust-on-first-use.** The ROM has not yet verified
  Ferriboot, so whoever controls the first image chooses the key. Do initial
  flashing in a controlled environment.
- An interrupted OTP burn can leave a board unbootable. Validate the full
  chain on a sacrificial board first.
- If OTP holds unexpected key slots, unknown critical flags or a conflicting
  key hash, Ferriboot refuses to hand off and reboots, without changing OTP.

### Handoff locks

Before handoff Ferriboot write-locks OTP page 1 (`CRIT1`, `BOOT_FLAGS`),
page 2 (`BOOTKEY0..3`) and page 32 (rollback counter) for both security
states, and reads each lock back. It also sets the `BOOTDIS.NEXT` latches so a
scratch-vector reboot cannot skip Ferriboot. These locks last until reset;
other OTP pages are not protected.

### Glitch detectors

Enrollment burns `CRIT1.GLITCH_DETECTOR_ENABLE`, so from the next reset the
four detectors also cover the ROM. Ferriboot additionally arms and locks them
as its first action on every boot and resets if that fails. A glitch reset is
logged on the next boot; there is no lockout.

Boards whose supply or clock trips the detectors cannot boot once armed, so
validate hardware before enrolling. See
[fault-injection hardening](../../THREAT-MODEL.md#fault-injection-hardening).

## Security counter

The application rollback floor uses **OTP page 32** (`0x800..0x83f`),
reserved exclusively. It stores a thermometer code in three majority-voted
copies, giving **504 epochs**. Row `0x83f` must stay blank.

**Do not boot Ferriboot on a board with a single-row counter or a marker in
row `0x83f`** from an older layout: the encodings are ambiguous and OTP cannot
be erased.

Images are signed at epoch 0 unless `SECURITY_COUNTER=N` is set. The
[core README](../../ferriboot-core/README.md#security-counter) describes when
a new floor is burned.

## Hardware-in-the-loop tests

> Use a **dedicated, expendable board.** The tests build without `production`
> and, unless `RP2350_SECURE_BOOT_KEY` is set, sign with the development key
> `boards/rp2350-pico2/keys/dev-rp2350-rom-secp256k1.pem`. On a blank chip the
> first test boot enrolls that key permanently, with no acknowledgement. Every
> scenario rewrites all 4 MiB.

Wire SWD and UART0 as above, and rebuild the
[flash algorithm](flash-algorithm/README.md) whenever its sources change.

```sh
export FERRIBOOT_RP235X_PORT=/dev/serial/by-id/usb-Arm_DAPLink_CMSIS-DAP_YOUR_DEVICE_ID
cargo test -p ferriboot-hil --features rp2350-pico2,smoke -- --test-threads=1
```

`FERRIBOOT_RP235X_PORT` is the UART console. Adding the `security-counter`
feature burns one more epoch (three OTP bits) per run. See
[`ferriboot-hil`](../../testing/ferriboot-hil/README.md) for the harness and
its other settings.
