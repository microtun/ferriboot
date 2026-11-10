# bl-loader

The RAM handoff payload for ESP chips: a tiny, separately linked executable
that `ferriboot-esp` embeds and runs as the last step before the application.

Loading an application's RAM segments overwrites the bootloader itself, so the
final copy cannot run from ordinary bootloader code. Ferriboot validates the
image and builds a plan, copies this payload into a protected 4 KiB IRAM
region at `0x403CE710`, switches to a dedicated stack and jumps to it. The
payload then copies the RAM segments from flash, maps the flash segments
through the MMU, publishes the boot-info record and jumps to the application
entry point. It never returns, and on a trap it resets.

It uses only mask ROM functions: no HAL, heap, logger or flash-backed code.

## Building

You do not build this directly. `ferriboot-esp`'s build script runs a nested
`cargo build --release` here when its `boot-runtime` feature is enabled,
checks that the result fits the handoff region and embeds the raw payload. It
passes the chip, load address, size and boot-info location through
`FERRIBOOT_BL_LOADER_*` variables; a build without them fails with an
explanatory error.

This directory is its own Cargo workspace because the payload needs its own
linker script and size-optimised profile. As a consequence:

- it has its own `Cargo.lock` and its own `[workspace.package]` version, which
  `tools/set-version.sh` keeps in step with the repository;
- root `cargo` commands such as `cargo fmt`, `cargo clippy` and `cargo test`
  do not cover it.

## Constraints

- The payload must fit in 4 KiB. Reduce code size instead of enlarging the
  region, which is fixed by the chip description in `chips/esp32c3`.
- `src/loader_abi.rs` in `ferriboot-esp` is compiled into both sides by path.
  Changing the plan layout there rebuilds the payload automatically.
- `ld/esp32c3.x.in` carries the ESP32-C3 mask ROM entry points. Another chip
  needs its own script and ROM addresses.
- The default build is silent, so the handoff cannot stall on USB output. The
  `log` feature adds USB Serial/JTAG diagnostics; enable it through
  `bl-loader-diagnostics` on the board's bootloader crate.
