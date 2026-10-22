# Example firmware test-hook protocol

All three `boards/{esp32c3,qemu,rp2350}/firmware` crates are **example firmware** used in the
hardware tests, not production applications. Each has one `src/main.rs`;
the `ferriboot-test-hooks` crate handles only the shared wire format and
version selection. The hooks are always compiled in (there is no `test-hooks`
Cargo feature). The hardware test harness builds the example firmware without hook feature flags.

**Do not deploy these examples in production.** An unauthenticated record at the start of the
`storage` partition can request resets, panics, a watchdog hang, or destruction
of the active image. The bootloader still authenticates executable images;
this test-only control record itself is *not* authenticated.

## Record format

Little-endian, starting at the beginning of the board's `storage` partition:

| Byte offset | Contents |
|---|---|
| 0–7 | ASCII `FBTHOOK1` magic |
| 8 | Number of entries (0–16) |
| 9 | Default behaviour code |
| 10 | Default watchdog timeout in seconds (0 = board default) |
| 11 | Reserved |
| 12 onward | Up to 16 entries of 12 bytes each |

Each entry contains `major: u8`, `minor: u8`, `revision: u16 LE`,
`build: u32 LE`, `behaviour: u8`, `watchdog_secs: u8`, then two reserved
bytes. The first matching version takes precedence over the default.
Behaviour codes: `0` normal, `1` confirm + idle, `2` idle without confirmation,
`3` reset without confirmation, `4` panic, `5` illegal-instruction fault,
`6` hang without feeding the watchdog, `7` invalidate/erase the active image.
An unknown behaviour code is reported as `@fbt error unknown-behaviour`.

A missing/malformed record, an unmatched version with a `Normal` default,
or an explicit `Normal` entry takes the normal path: confirm a pending trial,
print the board's normal startup lines, and stay idle. This path emits no
`@fbt` records. A selected test hook emits, in order:

```text
@fbt hello version=<version> behaviour=<code> reset=<board reset reason>
@fbt bootinfo mode=<mode> slot=<slot> max=<bytes> bootloader=<version>
@fbt image version=<version> pending=<bool>
@fbt confirmed | idle | resetting | hang | fault | erased-active | error <reason>
```

Test actions and watchdog implementations are board-specific: ESP32-C3
uses its RTC watchdog and USB-Serial-JTAG, RP2350 uses its inherited hardware
watchdog and polling UART0, and QEMU uses the CMSDK watchdog for intentional
hangs and a virtual UART. ESP32-C3 and RP2350 continue feeding their watchdog
in idle paths, before their Embassy executors start. In normal operation they run
async heartbeat and watchdog-feeding loops.

From the repository root, run the host-side record unit tests with
`cargo test -p ferriboot-test-hooks`. The `ferriboot-hil` suite builds the example firmware
for each selected target and checks their behavior through the full
bootloader, signing and flash setup. Physical tests require expendable boards.
