# ferriboot-rp

RP2350 support for Ferriboot, shared by the bootloader and by applications
that update themselves. It is `no_std`, runs in Arm Secure mode and contains
nothing board-specific: a board passes its flash map to `Layout::new`, which
checks it against the chip's rules at compile time.

For a complete, flashable setup see the
[RP2350 Pico 2 board](../boards/rp2350-pico2/README.md).

## What it provides

- **`Layout`:** a board flash map checked for the RP2350: erase alignment,
  and two equal, non-overlapping slots that fit the common execution window
  at `0x10400000`.
- **Flash:** `hardware::flash` builds the driver, `share` makes it usable by
  several partitions, and `Partition` exposes each region as a `NorFlash`.
- **`updater`:** builds a `ferriboot-core` `FirmwareUpdater` for the
  application from the bootloader's handoff. It needs no keys or policy.
- **`bootloader`:** builds the `ferriboot-core` boot state machine over the
  same checked layout.
- **`BootInfo`:** the handoff record in the top 256 bytes of SRAM, telling the
  application which slot it runs from and which upgrade mode is in use.
- **Bootloader-only pieces:** QMI address translation (`xip`), vector-table
  checks, OTP access and the OTP security counter, glitch detectors, the image
  policy (`--vid microtun.dev --cid rp2350`) and the final jump.
- **`uart_logger`** (feature `uart-log`): a polling UART0 backend for `log`.

## Using it from an application

As a member of this workspace (elsewhere, use path or git dependencies):

```toml
[dependencies]
ferriboot-rp = { workspace = true }   # the workspace selects `rp235xa`
ferriboot-layout-rp2350-pico2 = { workspace = true }
cortex-m-rt = { workspace = true }
embassy-rp = { workspace = true, features = ["imagedef-none"] }

[build-dependencies]
ferriboot-build = { workspace = true }
ferriboot-layout-rp2350-pico2 = { workspace = true }
```

```rust,ignore
// build.rs: generates memory.x, so do not provide your own.
fn main() {
    ferriboot_build::configure_rp2350_application(
        ferriboot_layout_rp2350_pico2::MAX_IMAGE_SIZE,
    );
}
```

```rust,ignore
const LAYOUT: ferriboot_rp::Layout =
    ferriboot_rp::Layout::new(ferriboot_layout_rp2350_pico2::FLASH_MAP);

let boot = ferriboot_rp::BootInfo::read().expect("bootloader handoff");
let flash = ferriboot_rp::share(
    ferriboot_rp::hardware::flash::<{ LAYOUT.flash_size() }>(peripherals.FLASH),
);
let mut updater = ferriboot_rp::updater(&LAYOUT, &boot, &flash).expect("layout");
```

The [core README](../ferriboot-core/README.md#updating-from-an-application)
shows how to stage, mark and confirm an image with the updater, and the board
README lists what else an application must do (watchdog, interrupts, core 1).

## Cargo features

No feature is enabled by default.

| Feature | Purpose |
|---|---|
| `rp235xa`, `rp235xb` | Package variant; exactly one is required. RP2354A/B use the same features |
| `uart-log` | Polling UART0 `log` backend |
| `log` | Diagnostics through `log` |
| `defmt` | Diagnostics through `defmt` |
| `uuid` | Parse imgtool `--vid`/`--cid` names into UUIDs |

## Layout of this directory

- `src/` is the platform crate.
- `ld/ferriboot-rp.x` is the linker contract that
  `configure_rp2350_application` includes from the generated `memory.x`.
- `tools/` is the `ferriboot-tools-rp2350` host tool; its commands are
  described in the board README.

`unsafe` is confined to the modules that are marked for it in `src/lib.rs`.
