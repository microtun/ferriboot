# QEMU (test target)

**Not a real board.** A minimal Cortex-M3 platform (`thumbv7m-none-eabi`) on a
patched QEMU `mps2-an385` machine. It lets
[`ferriboot-hil`](../../testing/ferriboot-hil/README.md) run the full
regression matrices quickly, in parallel and without hardware. Nothing here
ships.

## Setup

You need Rust **1.95+**, upstream `imgtool`, OpenSSL and a patched QEMU
10.1.0. From the repository root:

```sh
rustup target add thumbv7m-none-eabi
python3 -m pip install imgtool
./keys/generate.sh
cargo install --path ferriboot-qemu/tools --locked
```

Then build the patched QEMU once, with Docker or by hand, as described in
[`qemu/README.md`](qemu/README.md), and point the tools at it:

```sh
export FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm"
```

## Run the automated tests

```sh
cargo test -p ferriboot-hil --features qemu,regression
```

Each test creates its own isolated virtual board, separate from the files used
by the manual workflow below.

## Run manually

The workflow matches the hardware boards: flash the application, flash the
bootloader, then run.

```sh
cd boards/qemu/firmware
IMAGE_VERSION=1.0.0 cargo run --release   # sign and flash the application
cd ../ferriboot
cargo run --release                       # flash the bootloader
cd ../../..
ferriboot-tools-qemu run                  # serial on stdio, Ctrl+C to stop
```

"Flashing" writes to a virtual 4 MiB NOR image, `target/qemu/flash.bin`.
`run` also creates `target/qemu/efuses.bin` if it is missing; that file holds
the virtual security counter and survives reflashes. Do not flash while the
emulator is running.

Unlike real hardware, the bootloader is unsigned (there is no ROM signature
stage) and no physical fuses are burned, so no provisioning acknowledgement is
needed.

The tool can also be called directly:

```sh
ferriboot-tools-qemu flash-app --slot secondary \
  target/thumbv7m-none-eabi/release/qemu-firmware
ferriboot-tools-qemu flash-bootloader \
  target/thumbv7m-none-eabi/release/qemu-ferriboot
```

`flash-app` erases the whole selected slot, including its old trailer, then
writes the signed image. `flash-bootloader` erases only the bootloader
partition. Other subcommands are `check`, `export-bin`, `sign`, `sign-raw` and
`layout`.

| Variable | Purpose |
|---|---|
| `FERRIBOOT_QEMU_BIN` | Path to the patched `qemu-system-arm` |
| `FERRIBOOT_QEMU_FLASH` | Virtual board to use (also `--flash PATH`); its `efuses.bin` sits beside it |
| `NO_FLASH=1` | Save `<ELF>.signed.bin` or `<ELF>.bootloader.bin` without touching the virtual board |
| `FERRIBOOT_KEY`, `IMAGE_VERSION`, `SECURITY_COUNTER`, `VID`, `CID`, `IMGTOOL`, remote signer variables | Same as the hardware tools |

Images are signed with `--vid microtun.dev --cid qemu-an385`, enforced by
`ferriboot-qemu/src/policy.rs`, so firmware for other targets is rejected.
They also use `--rom-fixed 0xFF000` and a `0x1000` header.

## Emulated hardware

| Function | Implementation |
|---|---|
| Flash | QEMU `w25q32` SPI NOR (4 MiB, `flash.bin`) on PL022 at `0x40020000`; chip select at `0x40030000` |
| Execution | Bootloader at `0x0` via `-kernel`; the selected slot is copied over SPI to SSRAM1 at `0xFF000`, so both slots run at one address |
| Console | CMSDK UART0 at `0x40004000`, on `-serial stdio` |
| Watchdog | CMSDK watchdog with `-watchdog-action reset` |
| Security counter | QEMU `xlnx-efuse` at `0x40031000`, backed by `efuses.bin` |
| RNG | Test TRNG at `0x40032000`, from QEMU guest randomness |
| Handoff | Boot info at `0x203FFE00` (256 bytes), reset marker at `0x203FFF00`; PRIMASK=0 with SysTick stopped and NVIC IRQs disabled/cleared |

## Flash layout

| Region | Offset | Size |
|---|---|---|
| bootloader | `0x0` | 256 KiB |
| primary | `0x40000` | 1856 KiB |
| secondary | `0x210000` | 1856 KiB |
| scratch | `0x3E0000` | 64 KiB |
| storage | `0x3F0000` | 64 KiB (test-hook record) |

## Security counter

The counter lives in `efuses.bin` (192 bytes, epochs 0–1536 as a thermometer
code), separate from `flash.bin`, so rewriting flash cannot reset it. The
bootloader stops the boot if the eFuse contents are inconsistent.

Tests get a fresh counter for every boot unless they hold a `VirtualBoard`;
see the [ferriboot-hil README](../../testing/ferriboot-hil/README.md#how-it-works).

```sh
cargo test -p ferriboot-hil --features qemu,security-counter --test security_counter
cargo test -p ferriboot-hil --features qemu --lib   # host-side counter tests
```

## Limits

- Killing QEMU right after a flash command is not a true power cut, because
  QEMU writes its backing files asynchronously.
- The eFuse model is not a security boundary: the host can rewrite it.
- The TRNG is not physical entropy.
- Chip-specific code is not covered: the ESP32-C3 segment loader, MMU and PMP,
  the RP2350 bootrom handoff and ATRANS, real flash timing and caches. The
  physical smoke suites cover those.
