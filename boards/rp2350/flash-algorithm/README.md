# RP2350 fixture flash algorithm: single-pass LZ4 restore

A CMSIS-style probe-rs flash *command transport*, not a normal flash target.
The HIL host builds the desired **complete 4 MiB image**, compresses each 4 KiB
physical sector independently using `lz4_flex`, and sends **one** SWD commit.
The target decodes and restores all 1024 sectors in order. There is no scan,
CRC manifest, SRAM reply, bitmap, or second commit.

## Required rebuild before using a board

`target.yaml` embeds **compiled Arm machine code**, not just configuration.
The copy included in the input project is the old scan/restore algorithm and is
**not usable with this protocol**. There is no version check: regenerate it
*from these updated Rust sources* before using the fixture, or the old
algorithm will misinterpret the compressed stream:

```sh
rustup target add thumbv8m.main-none-eabi
python3 -m pip install pyelftools
cd boards/rp2350/flash-algorithm
python3 build.py  # cargo build --release, then package target.yaml
cd ../../..
cargo test -p ferriboot-hil --lib
cargo test -p ferriboot-hil --features rp2350 --lib  -- --test-threads=1
```

`build.py` checks that the linked SRAM image ends below `0x20004000`
(the 16 KiB region for code, statics and decoder state). If the LZ4 decoder
exceeds this limit, adjust optimization or memory layout; don't bypass the
check. The two Cargo lockfiles also need regeneration when resolving the new
`lz4_flex = 0.14.0` dependency. After rebuilding, commit the updated
`target.yaml` and lockfiles together with the Rust sources.

## Wire format

probe-rs sees a 5 MiB virtual command window at `0x10000000..0x10500000`.
`EraseSector` is a no-op. `ProgramPage` accepts one 4096-byte packet at
`WINDOW_START + sequence * 4096`. Little-endian packet header:

```text
0   sequence  u32   0, 1, 2, ...
4   length    u32   payload bytes: 4084 for non-final packets
8   last      u32   0 or 1 (only 1 on final packet)
12  payload   [u8; length]; remaining bytes ignored
```

Concatenate each packet's payload. Exactly 1024 records follow in physical
sector order (record indexes are implicit), each with this representation:

```text
encoded_length  u16          0..4096
payload         [u8; length]
```

- `length == 0`: erased sector (`0xff` × 4096), no payload.
- `length == 4096`: raw sector (the 4096 bytes follow).
- `1 <= length < 4096`: independent **raw LZ4 block** (not an LZ4 frame),
  required to decompress into *exactly* 4096 bytes.

Record boundaries may span packet boundaries. The target assembles at most
4096 compressed bytes in the otherwise unused SRAM scratch Y bank at
`0x20081000`, decodes to the 4096-byte sector buffer in its algorithm image,
and invokes `write_sector` for **every** record. That routine compares with
physical flash, skips identical sectors, erases only if bits must go from 0 to
1, programs the required pages, and readback-verifies any modified sector.
Blank records are processed too, erasing stale sectors on the target.

The target rejects malformed packet sequences, record sizes greater than 4096,
invalid LZ4 blocks, extra data after sector 1024, and incomplete streams.
The decoder runs without an allocator. Transport-level LZ4 has no authenticity
mechanism; the application/bootloader image's secure boot checks are unchanged.

## Status codes

Entry points return 0 on success. Errors are sticky until the next `Init`.
`UnInit(1)` (end of probe-rs erase phase) accepts an empty stream; any other
`UnInit` fails with `Truncated` unless 1024 complete records were received.

| Code | Meaning |
|---|---|
| 1 | `Format`: bad packet, record length, or invalid LZ4 block |
| 2 | `RomMissing` |
| 3 | `Verify`: flash readback mismatch (or XIP mode mismatch at Init) |
| 4 | `Sequence`: out-of-order packet or packet after last |
| 5 | `Truncated`: fewer than 1024 complete sectors |
| 6 | `Hardware`: core1, PSM or QMI direct-mode timeout |
| 7 | `RomAbi` |
| 8 | `RcpNotReady` |
| 9 | `ClockTimeout` |
| 10 | `FlashUnsupported` |
| 255 | panic (BKPT returns status 255) |

## Target specifics

- Loads at `0x20000004`; code/data must end below `0x20004000`.
- The SRAM scratch Y bank at `0x20081000` is reserved from probe-rs's RAM
  map and used only to hold one raw/compressed 4096-byte input block.
- `Init` disables interrupts/watchdog, parks core1, initializes clocks and
  QMI; core1 is released on `UnInit` after the program phase or on error.
- The protocol intentionally has **no differential SWD optimization**: it
  reduces traffic by compression, but always sends populated sectors.
