# RP2350 HIL flash algorithm

A probe-rs flash algorithm that `ferriboot-hil` uses to restore a complete
4 MiB flash image quickly. It is a **command transport**, not a general flash
target: the host compresses each 4 KiB sector with LZ4 and sends the stream in
one SWD pass, and the target decodes and writes all 1024 sectors in order.

## Rebuild

`target.yaml` embeds the compiled machine code and has no version check.
After changing the Rust sources, regenerate it and commit it with them; a
stale copy misreads the stream and can corrupt the restore.

```sh
rustup target add thumbv8m.main-none-eabi
cd boards/rp2350-pico2/flash-algorithm
ferriboot-tools-rp2350 flash-algorithm   # build, then package target.yaml
cd ../../..
cargo test -p ferriboot-hil --features rp2350-pico2 --lib -- --test-threads=1
```

The command fails if the SRAM image ends at or above `0x20004000`. Reduce code
size instead of bypassing the check.

## Wire format

This section is the reference for the protocol; the sources point here rather
than restating it. Two implementations must match it and change together: the
target decoder in `src/protocol.rs` and the host encoder in
`testing/ferriboot-hil/src/flash_transport.rs`. The harness's unit tests
(command above) exercise the encoder.

probe-rs sees a 5 MiB command window at `0x10000000..0x10500000`.
`EraseSector` is a no-op. `ProgramPage` takes one 4096-byte packet at
`0x10000000 + sequence * 4096`, with a little-endian header:

```text
0   sequence  u32   0, 1, 2, ...
4   length    u32   payload bytes (4084 for non-final packets)
8   last      u32   1 on the final packet, else 0
12  payload   [u8; length]; remaining bytes ignored
```

The concatenated payloads hold exactly 1024 records, in sector order. A
record may span packets:

```text
length   u16   0..4096
payload  [u8; length]
```

| `length` | Sector contents |
|---|---|
| `0` | Erased (`0xff` × 4096) |
| `4096` | Raw sector |
| `1..4095` | Raw LZ4 block (not a frame) that must decode to exactly 4096 bytes |

For each record the target compares with flash, skips identical sectors,
erases only when needed, programs, and verifies. It rejects malformed packets,
oversized records, invalid LZ4, extra data and truncated streams.

The transport is unauthenticated. This does not affect the boot chain's
signature checks.

## Status codes

Entry points return 0 on success. An error persists until the next `Init`.
`UnInit(1)` accepts an empty stream; any other `UnInit` fails with `Truncated`
unless all 1024 records arrived.

The non-zero codes are the `Error` enum in `src/main.rs`, which is the only
list of them: the value probe-rs reports is the variant's discriminant. `255`
is not in the enum; it means the algorithm panicked.

## Memory use

- Code and data load at `0x20000004` and must end below `0x20004000`.
- SRAM scratch Y at `0x20081000` (excluded from probe-rs's RAM map) holds one
  4096-byte input block. No allocator is used.
- `Init` disables interrupts and the watchdog, parks core 1 and sets up clocks
  and QMI. Core 1 is released on `UnInit` or on error.
