# Firmware test hooks

The example firmware in `boards/{esp32c3,qemu,rp2350}/firmware` reads a
control record that lets `ferriboot-hil` make it reset, panic, hang or erase
itself. This crate implements the shared record format and version matching;
each board's `main.rs` implements the actions. The hooks are always compiled
in; there is no feature flag.

> **Do not ship the example firmware.** The record is unauthenticated: anything
> that can write the `storage` partition can trigger these actions, including
> destroying the active image. Ferriboot still authenticates every image it
> boots.

## Record format

Little-endian, at the start of the board's `storage` partition:

| Offset | Contents |
|---|---|
| 0–7 | ASCII `FBTHOOK1` |
| 8 | Number of entries (0–16) |
| 9 | Default behaviour |
| 10 | Default watchdog timeout in seconds (0 = board default) |
| 11 | Reserved |
| 12– | Up to 16 entries of 12 bytes |

Entry: `major: u8`, `minor: u8`, `revision: u16`, `build: u32`,
`behaviour: u8`, `watchdog_secs: u8`, two reserved bytes. The first entry
matching the running image version wins; otherwise the default applies.

| Code | Behaviour |
|---|---|
| 0 | Normal |
| 1 | Confirm, then idle |
| 2 | Idle without confirming |
| 3 | Reset without confirming |
| 4 | Panic |
| 5 | Illegal-instruction fault |
| 6 | Hang without feeding the watchdog |
| 7 | Erase the active image |

## Output

A missing or malformed record, or a `Normal` result, takes the normal path:
confirm any pending trial, print the board's startup lines, idle, and emit no
`@fbt` lines. A selected hook prints, in order:

```text
@fbt hello version=<version> behaviour=<code> reset=<board reset reason>
@fbt bootinfo mode=<mode> slot=<slot> max=<bytes> bootloader=<version>
@fbt image version=<version> pending=<bool>
@fbt confirmed | idle | resetting | hang | fault | erased-active | error <reason>
```

An unknown behaviour code reports `@fbt error unknown-behaviour`.

Board specifics: ESP32-C3 uses the RTC watchdog and USB-Serial-JTAG, RP2350
its hardware watchdog and UART0, and QEMU the CMSDK watchdog and UART.

## Tests

```sh
cargo test -p ferriboot-test-hooks
```

End-to-end behaviour is covered by `ferriboot-hil` through the full
bootloader, signing and flashing path.
