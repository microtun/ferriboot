# QEMU (test target)

**Not a real board.** A minimal Cortex-M3 platform (`thumbv7m-none-eabi`) on a
patched QEMU `mps2-an385` machine. It lets [`ferriboot-hil`](../../testing/ferriboot-hil/README.md) run the
full regression matrices (swap, overwrite, direct-XIP, safety, security
counter) quickly, in parallel and without hardware. Nothing here ships.

## Build and run

Build the patched QEMU (details in [`qemu/README.md`](qemu/README.md)), then
run the suite from the repository root:

```sh
# Generate the MCUboot key pair once; see ../../keys/README.md.
./keys/generate.sh
mkdir -p target/qemu
docker build -f boards/qemu/qemu/Dockerfile.qemu \
  --output type=local,dest=target/qemu boards/qemu/qemu
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression
```

There is no Cargo runner: run `cargo build --release -p qemu-ferriboot -p qemu-firmware`
in `boards/qemu` to build both binaries. The harness does the rest. Images are signed
with `--vid microtun.dev --cid qemu-an385`, enforced by
`ferriboot-qemu/src/policy.rs`, so images for other targets are rejected.

## Emulated hardware

| Function | Implementation |
|---|---|
| Flash | QEMU `w25q32` SPI NOR (4 MiB, `flash.bin`) on PL022 at `0x40020000`; chip select at `0x40030000` |
| Execution | Bootloader at `0x0` via `-kernel`; the selected slot is copied over SPI to SSRAM1 at `0xFF000`, so both slots run at one address |
| Console | CMSDK UART0 at `0x40004000`, on `-serial stdio` |
| Watchdog | CMSDK watchdog with `-watchdog-action reset` |
| Security counter | QEMU `xlnx-efuse` at `0x40031000`, backed by `efuses.bin` |
| RNG | Test TRNG at `0x40032000`, from QEMU guest randomness |
| Handoff | Boot info at `0x203FFE00` (256 bytes), reset marker at `0x203FFF00` |

Images are signed with `--rom-fixed 0xFF000` and a `0x1000` header.

## Flash layout

Machine facts are defined in `chips/qemu`; board partition sizes live in
`layout/src/board.rs`. Target code, host tools and linker scripts
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
