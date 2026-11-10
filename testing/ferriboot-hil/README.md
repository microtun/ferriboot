# ferriboot-hil

End-to-end tests for the Ferriboot boot chains. Each scenario builds the real
bootloader and example firmware, signs them with upstream `imgtool`, flashes a
complete 4 MiB image, resets the target and checks the console output.

> **Physical boards are expendable.** Every scenario reflashes the whole
> device, and the first test boot on a blank chip permanently enrolls the
> development secure-boot key, with no acknowledgement. Read the board README
> before connecting one.

## Running

Pick one target feature and one suite feature.

| Target | Backend |
|---|---|
| `qemu` | Patched QEMU, no hardware ([setup](../../boards/qemu/README.md)) |
| `esp32c3-supermini` | ESP32-C3 SuperMini over USB-Serial-JTAG ([setup](../../boards/esp32c3-supermini/README.md#hardware-in-the-loop-tests)) |
| `rp2350-pico2` | RP2350 Pico 2 over SWD and UART0 ([setup](../../boards/rp2350-pico2/README.md#hardware-in-the-loop-tests)) |

| Suite | Tests | Targets |
|---|---|---|
| `smoke` | Upgrade, trial, watchdog and malformed-image checks | `esp32c3-supermini`, `rp2350-pico2` |
| `regression` | Swap, overwrite, direct-XIP and safety matrices, plus `security-counter` | `qemu` |
| `security-counter` | Anti-rollback | all |

```sh
# From the repository root.
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression

cargo test -p ferriboot-hil --features esp32c3-supermini,smoke -- --test-threads=1
cargo test -p ferriboot-hil --features rp2350-pico2,smoke -- --test-threads=1
```

QEMU scenarios run in parallel on isolated virtual boards. Run physical
suites with `--test-threads=1`.

**`security-counter` on a physical board permanently burns one eFuse/OTP
epoch per run**, and ESP32-C3 has only 16. Under QEMU the counter is a
temporary file.

The harness's own unit tests need no hardware:

```sh
cargo test -p ferriboot-hil --lib                       # board-independent
cargo test -p ferriboot-hil --features <target> --lib   # one board's helpers
```

## Prerequisites

- Development keys: `./keys/generate.sh`, plus `ferriboot-tools-esp keys` or
  `ferriboot-tools-rp2350 keys` for those boards (see
  [keys/README.md](../../keys/README.md)). Tests always sign with and trust
  `keys/dev-p256.pem`, ignoring `FERRIBOOT_KEY` and `FERRIBOOT_PUBKEYS`.
- Upstream `imgtool` (needs Python).
- The board's Rust target and tools, as listed in its README: Docker for the
  ESP-IDF second stage, or the patched QEMU for `qemu`. The `espflash` and
  `probe-rs` CLIs are not needed.

## Environment

| Variable | Purpose |
|---|---|
| `FERRIBOOT_QEMU_BIN` | Patched `qemu-system-arm` (default: `qemu-system-arm` on `PATH`) |
| `FERRIBOOT_ESP32C3_PORT` | ESP32-C3 USB-Serial-JTAG port |
| `FERRIBOOT_ESP32C3_FLASH_TIMEOUT_SECS` | Flash timeout (default 600) |
| `FERRIBOOT_RP235X_PORT` | RP2350 UART0 console |
| `FERRIBOOT_RP235X_PROBE_TIMEOUT_SECS` | SWD timeout (default 600 for programming, 30 otherwise) |
| `RP2350_SECURE_BOOT_KEY` | ROM key for a board enrolled with a non-development key |
| `PROBE_RS_PROBE`, `PROBE_RS_SPEED_KHZ` | Probe selection and SWD speed |
| `FERRIBOOT_TEST_PROFILE` | Cargo profile for the board builds (default `release`) |
| `FERRIBOOT_TEST_CARGO_ARGS` | Extra arguments for the board builds |
| `IMGTOOL` | Override the `imgtool` executable |

## How it works

The harness builds the bootloader variants and firmware once per test
process, then for each scenario composes a flash image, flashes it and
captures the console. RP2350 boards are restored over SWD with a custom
[flash algorithm](../../boards/rp2350-pico2/flash-algorithm/README.md).

Physical boards are flashed by a helper process, the `ferriboot-flash` binary
in this crate, so that a stalled USB or SWD operation can be killed before the
next test. Cargo builds it with the tests; it is not meant to be run by hand.

Under QEMU, `Dut::boot` creates a fresh virtual board each time. To keep one
security counter across reflashes, hold a `VirtualBoard`:

```rust,ignore
let board = ferriboot_hil::qemu::VirtualBoard::new(3)?;
let dut = board.boot(&upgrade_fixture)?;
drop(dut); // confirm and reset first to commit epoch 4
let dut = board.boot(&rollback_fixture)?; // same counter, rewritten flash
```

Only one guest per board can run at a time.

Fixtures are signed at the device's current security-counter floor, read over
JTAG (ESP32-C3), SWD (RP2350) or from the QEMU backing file. The example
firmware is driven through the
[test-hook record](../ferriboot-test-hooks/README.md) in its `storage`
partition.

To add a scenario, put it in the relevant `tests/*.rs` file, use the helpers
in `tests/common/` and `src/scenario.rs`, and register any new test binary in
`Cargo.toml` with its `required-features`.
