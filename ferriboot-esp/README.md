# ferriboot-esp

ESP chip support for Ferriboot, shared by the third-stage bootloader and by
applications that update themselves. It is `no_std`, built for exactly one
chip (currently only the ESP32-C3) and contains nothing board-specific: flash
size and slot locations come from the partition table at runtime.

For a complete, flashable setup see the
[ESP32-C3 SuperMini board](../boards/esp32c3-supermini/README.md).

## What it provides

- **Flash and slots:** `open_flash` wraps the HAL flash peripheral,
  `read_layout` finds the `image-0`, `image-1` and `image-scratch` partitions
  by label, and `partition` exposes each region as a `NorFlash`. No async
  executor is required.
- **`updater`:** builds a `ferriboot-core` `FirmwareUpdater` for the
  application from the bootloader's handoff. It needs no keys or policy.
- **`bootloader`:** builds the `ferriboot-core` boot state machine over the
  same checked layout.
- **`BootInfo`:** the handoff record in reserved SRAM, telling the application
  which slot it runs from and which upgrade mode the bootloader uses.
- **`watchdog`:** RTC watchdog helpers, including `start_rtc` for applications.
- **Bootloader-only pieces:** the eFuse security counter, the clock glitch
  detector, the image policy (`--vid microtun.dev --cid esp32c3`) and, with
  `boot-runtime`, start-up code and the RAM handoff `loader`.

## Using it from an application

As a member of this workspace (elsewhere, use path or git dependencies):

```toml
[dependencies]
ferriboot-esp = { workspace = true }   # the workspace selects `esp32c3`

[build-dependencies]
ferriboot-build = { workspace = true }
```

```rust,ignore
// build.rs
fn main() {
    ferriboot_build::configure_esp_application();
}
```

```rust,ignore
let boot = ferriboot_esp::BootInfo::read().expect("bootloader handoff");
let flash = ferriboot_esp::open_flash(peripherals.FLASH);
let layout = ferriboot_esp::read_layout(&flash).expect("partition table");
let mut updater = ferriboot_esp::updater(&layout, &boot, &flash).expect("layout");
```

The [core README](../ferriboot-core/README.md#updating-from-an-application)
shows how to stage, mark and confirm an image with the updater, and the board
README lists what else an application must do (watchdog, RAM layout).

## Cargo features

| Feature | Default | Purpose |
|---|---|---|
| `esp32c3` | no | Chip selection. Exactly one chip feature is required |
| `boot-runtime` | no | Bootloader only: `esp-hal` runtime start-up and the RAM handoff loader. Builds and embeds [`bl-loader`](bl-loader/README.md) |
| `bl-loader-diagnostics` | no | USB Serial/JTAG logging from the RAM loader; implies `boot-runtime` |
| `log` | yes | Diagnostics through `log` |
| `defmt` | no | Diagnostics through `defmt` |
| `uuid` | no | Parse imgtool `--vid`/`--cid` names into UUIDs |

Applications should leave `boot-runtime` off so they keep their own runtime
setup. The crate must be built for `riscv32imc-unknown-none-elf`; its build
script rejects other targets.

## Layout of this directory

- `src/` is the platform crate.
- `ld/ferriboot.x.in` is the linker contract (boot-info reservation) that
  `configure_esp_application` links into bootloaders and applications.
- [`bl-loader/`](bl-loader/README.md) is the separately linked RAM handoff
  payload, a nested workspace.
- `tools/` is the `ferriboot-tools-esp` host tool; its commands are described
  in the board README.

`unsafe` is confined to the modules that are marked for it in `src/lib.rs`.
