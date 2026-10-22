# QEMU (test target)

**Not a real board.** This is a minimal Cortex-M3 platform on a **patched**
QEMU `mps2-an385` machine (`thumbv7m-none-eabi`). It exists only so
`testing/ferriboot-hil` can run the full regression matrices (swap, overwrite,
direct-XIP, safety) quickly, in parallel and without hardware. Nothing here is
meant to ship, and the layout does not match any device.

Build patched QEMU 10.1.0 with Docker (from the repository root):

```sh
mkdir -p target/qemu
docker build -f boards/qemu/qemu/Dockerfile.qemu \
  --output type=local,dest=target/qemu boards/qemu/qemu
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression
```

For the manual source build, Linux runtime-library requirements, and the
persistence caveats, see [`qemu/README.md`](qemu/README.md).

There is no Cargo runner. `cargo build --release` here builds the bootloader,
the firmware and `ferriboot-qemu`; the harness drives everything else.

## Image identity

The QEMU bootloader enforces the board-specific policy in
`ferriboot-qemu/src/policy.rs`. The application updater merely stages images. The HIL signer emits protected MCUboot
VID/CID TLVs using `imgtool --vid microtun.dev --cid qemu-an385`.
These are development-only identifiers, not a vendor registration or security
secret. Older fixtures lacking those TLVs must be re-signed. The QEMU
policy is separate from ESP32-C3 and RP2350 to prevent cross-target updates.

## What is emulated

- **Flash:** an actual QEMU `w25q32` SPI-NOR device (4 MiB) backed by
  `flash.bin`, connected to the AN385 PL022 (SSP0, `0x40020000`) through a
  tiny test-board patch. The guest uses JEDEC READ, WREN, 256-byte page
  program, 4 KiB sector erase, and status-register polling. Chip select is
  controlled by a test-only MMIO register at `0x40030000`. **PSRAM is no
  longer used for flash storage.** See [`qemu/README.md`](qemu/README.md)
  for patch/build instructions and durability limitations.
- **Execution:** the bootloader is passed to QEMU with `-kernel` at `0x0`. The
  selected slot is **read over SPI** and copied into executable SSRAM1 at
  `0xFF000`, so both slots run at one address, as on RP2350. Images are
  signed with `--rom-fixed 0xFF000` and a `0x1000` header.
- **Console:** polled CMSDK APB UART0 at `0x40004000`, connected to QEMU's
  first `-serial stdio` backend; no semihosting.
- **Watchdog:** CMSDK APB watchdog, with QEMU's `-watchdog-action reset`.
- **Security counter:** QEMU's upstream `xlnx-efuse` model with a separate
  `efuses.bin` block backend, exposed at test-only MMIO
  `0x40031000` by `0002-an385-xlnx-efuse.patch`. All bootloader
  variants use `SecurityCounterStore`.
- **Test RNG:** a read-only AN385 MMIO frontend at `0x40032000` backed by
  QEMU guest randomness. The bootloader checks the `TRNG` ID and ready bit,
  uses it for `BootLoader::with_entropy()` during irreversible counter updates,
  and randomizes the final `confirm()` verdict check after the last UART log.
  No extra drive is required. This is simulated randomness, not physical entropy.
- **Handoff:** 256-byte boot-info block at `0x203FFE00`, reset marker at `0x203FFF00`.

## Layout

Two JSON files define the QEMU flash and RAM layout. The machine's facts
(SPI NOR size, execution window, handoff addresses, test peripherals) are in
`ferriboot-qemu/chips/mps2-an385.json`; `layout/board.json` names that file
and adds this board's bootloader, slot and scratch sizes. The `qemu-layout`
crate's `build.rs` generates Rust constants from both, and the crate derives
the partition offsets and `FLASH_MAP`, which both binaries check with
`ferriboot_qemu::Layout::new` at compile time. The guest, both linker
`build.rs` scripts, and the host harness import those constants as a normal
dependency. `tools/image.py` reads the same JSON (via `tools/board.py`) to
validate and export ELF binaries.
Hexadecimal addresses in the JSON are strings (JSON doesn't support hex literals);
other values, such as `BOOT_INFO_SIZE`, can be decimal integers.

| Region | Offset | Size |
|---|---|---|
| bootloader | `0x0` | 256 KiB |
| primary | `0x40000` | 1856 KiB |
| secondary | `0x210000` | 1856 KiB |
| scratch | `0x3E0000` | 64 KiB |
| storage | `0x3F0000` | 64 KiB, test-hook record |

## Limits

These tests exercise `ferriboot-core`'s boot decisions, trailer handling and
power-loss recovery under a real bootloader/firmware handoff. **However,
QEMU's SPI NOR model queues host backing-file writes asynchronously: a forced
QEMU kill immediately after a SPI command is not equivalent to the previous
synchronous writeback implementation.** Verify that fault-injection and
restart tests retain the intended guarantees before relying on them.
The simulated TRNG cannot validate physical timing or glitch resistance and
QEMU `-seed`/record-replay can produce deterministic random values.
These tests do not cover
target-specific code: the ESP32-C3 segment loader, MMU and PMP, the RP2350
bootrom handoff and `ATRANS`, real flash timing, or caches. The physical smoke
suite covers those on the real boards.

## Security counter

The counter is independent of `flash.bin`: erasing, replacing or restoring the
entire flash fixture cannot reset it. The harness initializes a fixed-size
192-byte file once per virtual board. It represents epochs `0..1536` as a
low-bit-first thermometer. The capacity is defined in `ferriboot-qemu/chips/mps2-an385.json` and
shared by the guest and host.

The QEMU process opens the backing image and loads it into the eFuse model;
the bootloader probes MMIO register `0x40031104` for `FUSE`, reads 48
32-bit fuse rows, and validates their thermometer encoding. A single-bit
program command at `0x40031100` can only set bits; each program operation
is followed by readback before continuing. Missing/unreadable images cause
QEMU to reject the drive or fail device realization. Missing MMIO or malformed
thermometer state stops boot. There is no fallback to a zero counter.

Ordinary `Dut::boot` calls create isolated virtual boards with zeroed eFuses
and read the current value from the backing image when signing fixtures. To model one board across full
reflashes and QEMU process restarts, hold a `ferriboot_hil::qemu::VirtualBoard`
and use `board.boot(&flash)` each time, dropping the previous `Dut` first. A
second simultaneous guest on the same board is rejected. Guest software resets,
QMP resets and process exits do not reinitialize its counter. The last board
handle owns cleanup; files are removed when it is dropped, so unrelated tests
and later test invocations start with fresh state.

```rust,ignore
let board = ferriboot_hil::qemu::VirtualBoard::new(3)?;
let dut = board.boot(&upgrade_fixture)?;
// Confirm and reset to commit epoch 4, then terminate this guest.
drop(dut);
let dut = board.boot(&rollback_fixture)?; // same counter, rewritten flash
```

The feature-gated regression checks trial reversion, confirmation on reset,
rejection of an older signed epoch after a full flash rewrite and recovery at
the new epoch. Each reflash starts a new QEMU process on the same virtual board.

```sh
cargo test -p ferriboot-hil --features qemu,security-counter --test security_counter
# Host-side MMIO counter algorithm tests and board isolation:
cargo test -p ferriboot-hil --features qemu --lib
```

This models anti-rollback behavior under flash rewriting. It is not a security
boundary against a malicious guest or host: the host
can rewrite the backing image. Persistence covers guest resets and QEMU
process termination, not host filesystem rollback or host power-loss
durability. It does not model physical fuse programming timing or electrical
failures. Backing-file I/O failures inside the upstream QEMU eFuse model are
logged to the QEMU host but not reliably reported to the guest.
