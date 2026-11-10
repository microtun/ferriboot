# ferriboot-qemu

**Test only.** Platform support for the patched QEMU `mps2-an385` machine
(Cortex-M3), the counterpart of `ferriboot-esp` and `ferriboot-rp`. It exists
so the regression suites can run without hardware, and it is not a security
boundary: the host can rewrite the emulated eFuses and the TRNG is not real
entropy.

It provides an SPI NOR flash driver for the emulated `w25q32`, a checked
`Layout`, the eFuse-backed security counter, a UART console, the test TRNG and
the image policy (`--vid microtun.dev --cid qemu-an385`).

The only Cargo feature is `mps2-an385`, which selects the machine and must be
enabled. `tools/` is the `ferriboot-tools-qemu` host tool.

See the [QEMU board README](../boards/qemu/README.md) for setup, the manual
workflow and the emulated hardware, and
[`boards/qemu/qemu`](../boards/qemu/qemu/README.md) for the QEMU patch.
`boards/qemu/ferriboot/src/main.rs` is the smallest complete Ferriboot
bootloader and a good starting point for a new platform.
