# Patched QEMU for the test board

Stock `mps2-an385` has no SPI flash, OTP or RNG. The patch
`0001-an385-ferriboot-test-peripherals.patch` adds **test-only** wiring for
all three, on top of **QEMU v10.1.0**.

## Build with Docker

From the repository root:

```sh
mkdir -p target/qemu
docker build -f boards/qemu/qemu/Dockerfile.qemu \
  --output type=local,dest=target/qemu boards/qemu/qemu
target/qemu/qemu-system-arm --version
```

This needs Docker with BuildKit. The result is a dynamically linked Linux
binary built on Debian trixie, so run it on a Linux host of the same
architecture with glibc 2.41+ and QEMU's runtime libraries (glib2, pixman,
zlib, libfdt). On macOS or Windows, run it and the tests inside Linux.

## Build manually

```sh
git clone --branch v10.1.0 https://gitlab.com/qemu-project/qemu.git
cd qemu
git apply /path/to/ferriboot/boards/qemu/qemu/0001-an385-ferriboot-test-peripherals.patch
./configure --target-list=arm-softmmu --disable-docs
ninja -C build qemu-system-arm
```

Point `FERRIBOOT_QEMU_BIN` at `build/qemu-system-arm`.

## Peripherals

The test harness starts QEMU with:

```sh
-drive if=mtd,format=raw,file=flash.bin \
-drive if=pflash,index=1,format=raw,file=efuses.bin
```

All register accesses are 32-bit.

**SPI NOR.** `flash.bin` (4 MiB) backs a `w25q32` on the existing PL022 at
`0x40020000`. The register at `0x40030000` drives its chip select: write 1 to
assert, 0 to release. Reset releases it.

**eFuse** at `0x40031000`. QEMU's `xlnx-efuse` model with 1536 bits, backed by
`efuses.bin` (192 bytes).

| Offset | Access | Meaning |
|---|---|---|
| `0x000`–`0x0bc` | read | 48 fuse words, lowest bit first |
| `0x100` | write | Index of the bit to program (0–1535) |
| `0x104` | read | ID `0x46555345` (`FUSE`) |

**TRNG** at `0x40032000`. Read-only and always present.

| Offset | Access | Meaning |
|---|---|---|
| `0x000` | read | ID `0x474e5254` (`TRNG`) |
| `0x004` | read | STATUS, `1` = ready |
| `0x008` | read | DATA, a new 32-bit sample per read |

The bootloader halts if the eFuse or TRNG ID is missing. It uses TRNG samples
to randomize its fault-injection timing; this exercises the code path only and
is not real entropy or fault-injection protection.