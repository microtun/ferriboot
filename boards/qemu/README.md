# QEMU (test target)

**Not a real board.** A minimal Cortex-M3 platform (`thumbv7m-none-eabi`) on a
patched QEMU `mps2-an385` machine. It lets [`ferriboot-hil`](../../testing/ferriboot-hil/README.md) run the
full regression matrices (swap, overwrite, direct-XIP, safety, security
counter) quickly, in parallel and without hardware. Nothing here ships.

## Build, flash and run (same workflow as the hardware tools)

The `ferriboot-tools-qemu` CLI now supports `check`, `export-bin`, `sign`,
`sign-raw`, `flash-app`, `flash-bootloader`, `run` and `layout`. Like the ESP
and RP2350 board runners, `cargo run --release` from `firmware/` builds,
MCUboot-signs and flashes the application, while `cargo run --release` from
`ferriboot/` exports and flashes the bootloader. Unlike real hardware, QEMU
boots an **unsigned** checked Cortex-M3 bootloader (there is no ROM signature
stage) and flashing changes only a disk-backed 4 MiB NOR image.

Prerequisites: Rust **1.95+** with `thumbv7m-none-eabi`, upstream `imgtool`, a
P-256 firmware signing key (`./keys/generate.sh`), and the **patched** QEMU
10.1.0 build from [`qemu/README.md`](qemu/README.md). Install the host tool:

```sh
rustup target add thumbv7m-none-eabi
python3 -m pip install imgtool
./keys/generate.sh
cargo install --path ferriboot-qemu/tools --locked

# Build the patched emulator once (on a compatible Linux host).
mkdir -p target/qemu
docker build -f boards/qemu/qemu/Dockerfile.qemu \
  --output type=local,dest=target/qemu boards/qemu/qemu
export FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm"
```

From the repository root, flash the app **before** the bootloader:

```sh
cd boards/qemu/firmware
IMAGE_VERSION=1.0.0 cargo run --release
cd ../ferriboot
cargo run --release
cd ../../..
ferriboot-tools-qemu run  # -serial stdio, Ctrl+C to stop
```

The flash commands share `target/qemu/flash.bin` (4 MiB; initially all
`0xFF`). `flash-app` erases the *entire* selected slot including its old
MCUboot trailer, then writes only the signed image; `flash-bootloader` erases
only the 256 KiB bootloader partition. Both show erase, program and verify
percentage bars calculated from bytes actually written or verified, followed
by separate done/failed status. `run` displays elapsed-time process launch
status (not an estimated boot percentage). Both preserve all other partitions;
`run` extracts the bootloader from the NOR into a temporary `-kernel` file, launches the patched
emulator, and creates `target/qemu/efuses.bin` (192 bytes) **only when
missing**. That file survives repeated runs and reflashes, preserving the
virtual security counter. Do **not** flash while the emulator is running.

For explicit CLI usage or the alternate application slot:

```sh
ferriboot-tools-qemu flash-app --slot secondary \
  target/thumbv7m-none-eabi/release/qemu-firmware
ferriboot-tools-qemu flash-bootloader \
  target/thumbv7m-none-eabi/release/qemu-ferriboot
ferriboot-tools-qemu run
```

Set `FERRIBOOT_QEMU_FLASH=/path/to/flash.bin` or use `--flash PATH` on any
of the three commands to choose a separate virtual board (its `efuses.bin`
resides in the same directory). `FERRIBOOT_QEMU_BIN` selects the patched
`qemu-system-arm`. `FERRIBOOT_KEY`, `IMAGE_VERSION`, `SECURITY_COUNTER`,
`VID`, `CID` and remote signer variables have the same meaning as in the
hardware tools. With `NO_FLASH=1`, the Cargo runners validate and save
`<ELF>.signed.bin` (application) or `<ELF>.bootloader.bin` (raw bootloader),
without modifying either backing file or launching QEMU. The QEMU target has
no OTP provisioning acknowledgement because no physical fuses are burned.

### Full automated test suite

The existing HIL virtual-board backend still builds and owns *isolated*
flash/OTP fixtures for each test. It is separate from the manual backing
files used by the CLI above. To run the full regression suite from the root:

```sh
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression
```

Images use `--vid microtun.dev --cid qemu-an385`, enforced by
`ferriboot-qemu/src/policy.rs`. Firmware for other targets is rejected.

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

Images are signed with `--rom-fixed 0xFF000` and a `0x1000` header.

## Flash layout

Machine facts are defined in `chips/qemu`; board partition sizes live in
`layout/src/lib.rs`. Target code, host tools and linker scripts
use the same constants. The checked ELF exporter is `ferriboot-tools-qemu export-bin`.


| Region | Offset | Size |
|---|---|---|
| bootloader | `0x0` | 256 KiB |
| primary | `0x40000` | 1856 KiB |
| secondary | `0x210000` | 1856 KiB |
| scratch | `0x3E0000` | 64 KiB |
| storage | `0x3F0000` | 64 KiB (test-hook record) |

## Security counter

The counter lives in `efuses.bin` (192 bytes, epochs 0–1536 as a low-bit-first
thermometer), separate from `flash.bin`, so rewriting flash cannot reset it.
The bootloader checks the eFuse ID, validates the thermometer and reads back
every programmed bit; any inconsistency stops the boot.

`Dut::boot` creates a fresh virtual board each time. To keep one counter
across reflashes, hold a `VirtualBoard`:

```rust,ignore
let board = ferriboot_hil::qemu::VirtualBoard::new(3)?;
let dut = board.boot(&upgrade_fixture)?;
drop(dut); // confirm and reset first to commit epoch 4
let dut = board.boot(&rollback_fixture)?; // same counter, rewritten flash
```

Only one guest per board can run at a time; files are removed when the last
handle is dropped.

```sh
cargo test -p ferriboot-hil --features qemu,security-counter --test security_counter
cargo test -p ferriboot-hil --features qemu --lib   # host-side counter tests
```

## Limits

- QEMU writes SPI NOR backing files asynchronously, so killing QEMU right
  after a flash command is not a true power cut. Check that fault-injection
  tests still prove what they claim.
- The eFuse model is not a security boundary: the host can rewrite it, and
  host-side I/O errors are not reliably reported to the guest.
- The TRNG is not physical entropy; `-seed` or record/replay make it
  deterministic.
- Target-specific code is not covered: the ESP32-C3 segment loader, MMU and
  PMP, the RP2350 bootrom handoff and ATRANS, real flash timing and caches.
  The physical smoke suites cover those.
