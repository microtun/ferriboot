# RP2350 HIL flash algorithm

A probe-rs flash algorithm used by `ferriboot-hil` as a **command transport**,
not a general flash target. The host compresses each 4 KiB sector of a
complete 4 MiB image with LZ4 and sends it in one SWD pass; the target
decodes and restores all 1024 sectors in order.

## Rebuild

`target.yaml` embeds compiled machine code and has no version check. After
changing the Rust sources, regenerate it and commit it with the sources;
a stale copy misreads the stream and can corrupt the restore.

```sh
rustup target add thumbv8m.main-none-eabi
cd boards/rp2350-pico2/flash-algorithm
ferriboot-tools-rp2350 flash-algorithm   # cargo build --release, then package target.yaml
cd ../../..
cargo test -p ferriboot-hil --features rp2350-pico2 --lib -- --test-threads=1
```

`ferriboot-tools-rp2350 flash-algorithm` fails if the SRAM image ends at or above `0x20004000`. Reduce code
size rather than bypassing the check.

## Wire format

probe-rs sees a 5 MiB command window at `0x10000000..0x10500000`.
`EraseSector` is a no-op. `ProgramPage` takes one 4096-byte packet at
`0x10000000 + sequence * 4096`, with a little-endian header:

```text
0   sequence  u32   0, 1, 2, ...
4   length    u32   payload bytes (4084 for non-final packets)
8   last      u32   1 on the final packet, else 0
12  payload   [u8; length]; remaining bytes ignored
```

The concatenated payloads hold exactly 1024 records, in sector order, which
may span packets:

```text
length   u16   0..4096
payload  [u8; length]
```

- `0`: erased sector (`0xff` × 4096).
- `4096`: raw sector.
- `1..4095`: raw LZ4 block (not a frame) that must decode to exactly 4096 bytes.

For every record the target compares with flash, skips identical sectors,
erases only when bits must go from 0 to 1, programs, and verifies modified
sectors. It rejects malformed packets, oversized records, invalid LZ4, extra
data and truncated streams. The transport is unauthenticated; the boot chain's
signature checks are unaffected.

## Status codes

Entry points return 0 on success; errors persist until the next `Init`.
`UnInit(1)` accepts an empty stream; any other `UnInit` fails with
`Truncated` unless all 1024 records arrived.

| Code | Meaning |
|---|---|
| 1 | `Format`: bad packet, record length or LZ4 block |
| 2 | `RomMissing` |
| 3 | `Verify`: readback mismatch (or XIP mode mismatch at `Init`) |
| 4 | `Sequence`: out-of-order packet or packet after last |
| 5 | `Truncated`: fewer than 1024 records |
| 6 | `Hardware`: core 1, PSM or QMI direct-mode timeout |
| 7 | `RomAbi` |
| 8 | `RcpNotReady` |
| 9 | `ClockTimeout` |
| 10 | `FlashUnsupported` |
| 255 | Panic |

## Memory use

- Code and data load at `0x20000004` and must end below `0x20004000`.
- SRAM scratch Y at `0x20081000` (excluded from probe-rs's RAM map) holds one
  4096-byte input block. No allocator is used.
- `Init` disables interrupts and the watchdog, parks core 1 and sets up clocks
  and QMI. Core 1 is released on `UnInit` or on error.
