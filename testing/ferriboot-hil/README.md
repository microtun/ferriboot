# ferriboot-hil

End-to-end tests for the Ferriboot boot chains. For each scenario the harness
builds the real bootloader and example firmware, signs them with upstream
`imgtool`, composes a complete 4 MiB flash image, flashes it, resets the
target and checks the console output.

> **Physical boards are expendable.** Every scenario reflashes the whole
> device, and the first test boot on a blank chip permanently enrolls the
> development secure-boot key, with no acknowledgement. See the board README
> before connecting one.

## Running

Pick exactly one target feature and one suite feature:

| Target | Backend |
|---|---|
| `qemu` | Patched QEMU, no hardware ([setup](../../boards/qemu/README.md)) |
| `esp32c3-supermini` | ESP32-C3 SuperMini over USB-Serial-JTAG ([setup](../../boards/esp32c3-supermini/README.md#hardware-in-the-loop-tests)) |
| `rp2350-pico2` | RP2350 Pico 2 over SWD and UART0 ([setup](../../boards/rp2350-pico2/README.md#hardware-in-the-loop-tests)) |

| Suite | Tests | Targets |
|---|---|---|
| `smoke` | `tests/smoke.rs`: upgrade, trial, watchdog and malformed-image checks | `esp32c3-supermini`, `rp2350-pico2` |
| `regression` | `swap`, `overwrite`, `direct_xip`, `direct_xip_no_revert`, `safety`, plus `security_counter` | `qemu` |
| `security-counter` | `tests/security_counter.rs`: anti-rollback | all |

```sh
# From the repository root.
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression

cargo test -p ferriboot-hil --features esp32c3-supermini,smoke -- --test-threads=1
cargo test -p ferriboot-hil --features rp2350-pico2,smoke -- --test-threads=1
```

QEMU scenarios run in parallel on isolated virtual boards. Physical suites
hold an OS lock on the board for each test, but use `--test-threads=1` anyway.

**`security-counter` on a physical board permanently burns one eFuse/OTP
epoch per run**, and ESP32-C3 has only 16. Under QEMU it uses a temporary
file-backed counter.

The harness's own unit tests need no hardware:

```sh
cargo test -p ferriboot-hil --lib                       # board-independent
cargo test -p ferriboot-hil --features <target> --lib   # one board's helpers
```

## Prerequisites

- Development keys: generate the MCUboot pair with OpenSSL (see
  [keys/README.md](../../keys/README.md)), plus `ferriboot-tools-esp keys`
  or `ferriboot-tools-rp2350 keys` for those boards. Test builds always
  trust, and fixtures are always signed with, `keys/dev-p256.pem`, ignoring
  `FERRIBOOT_KEY` and `FERRIBOOT_PUBKEYS`.
- Upstream `imgtool` (and Python for that external tool).
- The board's Rust target and tools, as listed in its README (Docker for the
  ESP-IDF second stage, `picotool` 2.3+ for RP2350, patched QEMU for `qemu`).
  The `espflash` and `probe-rs` CLIs are not needed; the harness uses the
  Rust crates.

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

| Module | Role |
|---|---|
| `artifacts` | Builds the bootloader variants and firmware once per test process |
| `sign` | `imgtool` signing (same arguments as `ferriboot_build::tools::mcuboot`) and envelope mutations |
| `flash` | Composes the 4 MiB image with `ferriboot-core`'s own trailer and updater code |
| `dut` | Flashing, reset and console capture for one device under test |
| `worker` | The `ferriboot-flash` binary that flashes physical boards (`espflash` crate, or LZ4 restore over SWD) |
| `flash_transport` | RP2350 side of the [flash algorithm](../../boards/rp2350-pico2/flash-algorithm/README.md) protocol |
| `esp32c3`, `rp2350`, `qemu` | Per-board layout, image format and flashing |

Fixtures are signed at the device's current security-counter floor, read over
JTAG (ESP32-C3), SWD (RP2350) or from the QEMU backing file. The example
firmware is driven through the
[test-hook record](../ferriboot-test-hooks/README.md) in its `storage`
partition.

Adding a scenario: put it in the relevant `tests/*.rs` file, use the helpers in
`tests/common/` and `src/scenario.rs`, and register any new test binary in
`Cargo.toml` with its `required-features`.
