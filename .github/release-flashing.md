<!-- ferriboot-flashing-instructions -->
## Flash the bootloaders

Download the attached `.bin` files. Choose `dev` or `release` to match the device's secure-boot key (**key provisioning is irreversible**).

**ESP32-C3 SuperMini** — Install `espflash` (`cargo install espflash --locked`) and set your serial port:

```sh
PROFILE=dev  # or release
ESPPORT=/dev/ttyACM0  # change to your port

espflash write-bin --chip esp32c3 --no-stub --port "$ESPPORT" \
  --after hard-reset 0x0 "esp32c3-supermini-${PROFILE}-boot-chain.sbv2.bin"
```

The image covers the signed ESP-IDF second stage, the partition table and signed Ferriboot.

**RP2350 Pico 2** — Install `probe-rs` (`cargo install probe-rs-tools --locked`) and connect an SWD probe:

```sh
PROFILE=dev  # or release
probe-rs download --chip RP235x --protocol swd --binary-format bin \
  --base-address 0x10000000 "rp2350-pico2-${PROFILE}-ferriboot.signed.bin"
probe-rs reset --chip RP235x --protocol swd
```
