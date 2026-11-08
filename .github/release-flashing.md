<!-- ferriboot-flashing-instructions -->
## Flash the bootloaders

Download the attached `.bin` files. Choose `dev` or `release` to match the device's secure-boot key (**key provisioning is irreversible**).

The Ferriboot tools can flash these **already signed** CI/release binaries directly, without an ELF, a signing key, or a signer service. Run from the repository root with the relevant USB/SWD hardware attached. The acknowledgement is mandatory because a first boot can irrevocably provision ROM secure boot. `FERRIBOOT_OTP_BURN` accepts `YES` in any letter case.

**ESP32-C3 SuperMini** — Install `espflash` (`cargo install espflash --locked`) and set your serial port:

```sh
PROFILE=dev  # or release
export ESPPORT=/dev/ttyACM0   # change to your serial port
export FERRIBOOT_OTP_BURN=YES
cargo run --release -p ferriboot-tools-esp -- flash-bootloader --signed-bin \
  "esp32c3-supermini-${PROFILE}-boot-chain.sbv2.bin"
```

The complete signed image (ESP-IDF second stage, partition table, and signed Ferriboot) is verified and written at `0x0`. It does not re-sign or rebuild anything.

**RP2350 Pico 2** — Connect an SWD probe:

```sh
PROFILE=dev  # or release
export FERRIBOOT_OTP_BURN=YES
cargo run --release -p ferriboot-tools-rp2350 -- flash-bootloader --signed-bin \
  "rp2350-pico2-${PROFILE}-ferriboot.signed.bin"
```

The ROM-sealed binary is verified and downloaded at `0x10000000`, then the board is reset, without generating a new signature. `PROBE_RS_PROBE`, `PROBE_RS_SPEED_KHZ`, and `PROBE_RS_CHIP` work as usual.