# Ferriboot QEMU SPI-NOR, eFuse and test TRNG board patch

The guest uses the real QEMU `w25q32` flash model over `pl022` rather than
simulating flash inside PSRAM. Stock `mps2-an385` does not connect a flash
or its CS line. The checked-in patch adds **test-only board wiring** for
the SPI NOR, Xilinx eFuse (OTP) backing device, and a simulated TRNG.
It is pinned to **QEMU v10.1.0**.

## Build using Docker (recommended)

As with the ESP-IDF bootloader, the multi-stage Dockerfile builds the pinned
upstream source, applies the checked-in patch and exports a single executable
via BuildKit's `--output=type=local`. From the Ferriboot repository root:

```sh
mkdir -p target/qemu
docker build \
  --file boards/qemu/qemu/Dockerfile.qemu \
  --output type=local,dest=target/qemu \
  boards/qemu/qemu

target/qemu/qemu-system-arm --version
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression
```

Requires Docker with BuildKit and network access to clone the QEMU 10.1.0
source on the initial build. Repeated builds can reuse Docker's layer cache.
The output is a **Linux ELF executable for the Docker build architecture**,
not a container runner or a statically linked universal binary. Run it on a
compatible Linux host (glibc >= 2.36) with the shared libraries QEMU
needs (notably glib2, pixman, zlib and libfdt), and keep the Docker
platform aligned with your host architecture. Use `ldd target/qemu/qemu-system-arm` to inspect dependencies.
On macOS/Windows, build and run the Linux binary and Cargo HIL tests in a
Linux environment; simply exporting the ELF will not make it run natively.

## Manual build

From a QEMU 10.1.0 Git checkout:

```sh
git checkout v10.1.0
git apply /path/to/ferriboot/boards/qemu/qemu/0001-an385-ferriboot-test-peripherals.patch
./configure --target-list=arm-softmmu --disable-docs
ninja -C build qemu-system-arm
```

Run tests with `FERRIBOOT_QEMU_BIN` pointing to the resulting
`build/qemu-system-arm` executable.

## Wiring and backing storage

The harness creates a 4 MiB `flash.bin` image per boot, and starts QEMU
with `-drive if=mtd,format=raw,file=flash.bin`. The patched board attaches
`IF_MTD[0]` to a QEMU `w25q32` on the existing APB `pl022` at `0x40020000`.
The chip's active-low CS input is controlled through a test-only **32-bit**
MMIO register at `0x40030000`: write **1** to assert CS, **0** to release.
The board wiring resets CS to inactive on system reset.

The independent 192-byte `efuses.bin` is attached as QEMU's
`IF_PFLASH[1]` backing image and opened:

```sh
-drive if=mtd,format=raw,file=flash.bin \
-drive if=pflash,index=1,format=raw,file=efuses.bin
```

The additional board MMIO frontend at `0x40031000` uses QEMU's upstream
`xlnx-efuse` storage model, configured for 1,536 bits (`efuse-nr=1`,
`efuse-size=1536`, `init-factory-tbits=false`). It is not a memory-mapped
backend: the MMIO interface delegates read/program operations to the model.

| Offset | Access | Meaning |
|---|---|---|
| `0x000`–`0x0bc` | read, 32-bit | 48 fuse words, lowest bit first |
| `0x100` | write, 32-bit | Fuse bit index to permanently program (0..1535) |
| `0x104` | read, 32-bit | Peripheral ID `0x46555345` (`FUSE`) |

SPI NOR and eFuse require their respective backing drives; the TRNG is always
present on the patched AN385 machine and needs no additional drive.
The firmware verifies the eFuse ID before trusting any counter value. The patch does not change the upstream PL022, m25p80, or eFuse models.

## Simulated TRNG for fault-injection jitter

The patched AN385 board also maps a read-only, 32-bit test peripheral at
`0x40032000`. It uses QEMU 10.1's `qemu_guest_getrandom_nofail()` on **every
DATA read**, not a constant or an in-firmware pseudo-random generator.

| Offset | Access | Meaning |
|---|---|---|
| `0x000` | read, 32-bit | ID `0x474e5254` (`TRNG`, little-endian) |
| `0x004` | read, 32-bit | STATUS `1` (data ready) |
| `0x008` | read, 32-bit | DATA: new 32-bit guest random sample per read |

`ferriboot-qemu::trng::Trng::try_new()` probes both the ID and ready bit. The
bootloader aborts if the patched peripheral is not found; it does not silently
fall back to a constant seed or remove the delays. It supplies DATA samples to
`BootLoader::with_entropy()`, randomizing FIH checks before irreversible eFuse
counter writes. The bootloader's final `confirm()` also calls
`fih::random_delay(random())` directly before `verdict.require()`, after flash
reads and the last UART log. Other MPS2 FPGA variants are unchanged.

**This is not true hardware entropy or physical fault-injection protection.**
QEMU normally obtains the samples from host-provided randomness; `-seed` and
record/replay can make the stream deterministic, and an adversary who controls
the emulator can manipulate it. This models a hardware-like register interface
and exercises the jitter path in tests; real hardware still needs a qualified
TRNG and platform-specific fault-injection evaluation.

A standalone register smoke test compiles the shim from the actual patch
against minimal QEMU API stubs (a C compiler is needed):

```sh
python3 boards/qemu/qemu/tests/test_trng_mmio.py
```

After rebuilding the patched QEMU and the QEMU guest firmware, exercise both
boot handoff and anti-rollback/eFuse advancement with:

```sh
FERRIBOOT_QEMU_BIN="$PWD/target/qemu/qemu-system-arm" \
  cargo test -p ferriboot-hil --features qemu,regression
```

The guest emits logs via the **existing CMSDK APB UART0** (`0x40004000`,
115,200 baud, 8N1); the harness connects `-serial stdio` to the same
stdout/stderr capture pipe that handled the old semihosting console. QEMU
runs with semihosting disabled.

## Licensing note

The original Ferriboot code is dual-licensed as `MIT OR Apache-2.0` (see the
repository's top-level `LICENSE`, `LICENSE-MIT`, and `LICENSE-APACHE`). This
QEMU patch includes context from QEMU's upstream files and is intended to be
applied to QEMU. Upstream QEMU code and redistributed patched QEMU binaries
remain subject to QEMU's own applicable license obligations. Ferriboot's dual
license does not replace those third-party terms.
