# Firmware test hooks

The example firmware in `boards/*/firmware` reads a control record that lets
`ferriboot-hil` make it reset, panic, hang or erase itself. This crate defines
the record format and version matching; each board's `main.rs` implements the
actions. The hooks are always compiled in.

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

Each entry is `major: u8`, `minor: u8`, `revision: u16`, `build: u32`,
`behaviour: u8`, `watchdog_secs: u8` and two reserved bytes. The first entry
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

With no record, a malformed record or behaviour `Normal`, the firmware runs
normally: it confirms any pending trial, prints its startup lines and idles,
with no `@fbt` lines. Any other behaviour prints, in order:

```text
@fbt hello version=<version> behaviour=<code> reset=<board reset reason>
@fbt bootinfo mode=<mode> slot=<slot> max=<bytes> bootloader=<version>
@fbt image version=<version> pending=<bool>
@fbt confirmed | idle | resetting | hang | fault | erased-active | error <reason>
```

An unknown behaviour code reports `@fbt error unknown-behaviour`.

## Tests

```sh
cargo test -p ferriboot-test-hooks
```

End-to-end behaviour is covered by `ferriboot-hil`.
