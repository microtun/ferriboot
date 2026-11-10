# ferriboot-core

The platform-independent core of Ferriboot, a secure boot bootloader with
[MCUboot](https://github.com/mcu-tools/mcuboot) compatibility. It is `no_std`,
has no `unsafe` or target-specific code, and is generic over
`embedded_storage::nor_flash::NorFlash`, so it is tested on the host against a
simulated flash that cuts power after every erase and write.

On-flash formats match MCUboot (`BOOT_MAX_ALIGN = 8`), so images from
`imgtool sign`, including `--pad --confirm`, work unchanged. Only ECDSA P-256
over SHA-256 is supported; encrypted and compressed images are rejected.

## What it provides

- **Bootloader:** the boot decision for swap-using-scratch, overwrite-only
  and direct-XIP upgrades, with downgrade prevention and power-fail resume.
- **Image validation:** SHA-256 hash, ECDSA P-256 signature, and policy for
  minimum version, hardware security counter and imgtool VID/CID.
- **Updater** (`updater`): the application side, which stages a new image,
  marks it pending and confirms it.
- **Upload sessions** (`upload`): an optional layer over the updater for
  packet-at-a-time transfers, with offsets, retries, a transfer hash, a
  60-second idle timeout and cleanup of abandoned uploads. The caller
  supplies the transport and the clock.
- **Boot info** (`shared_data`): the record the bootloader leaves in reserved
  RAM for the application, using MCUboot's shared-data TLVs. It carries the
  running slot, upgrade mode, bootloader version, maximum image size and
  security counter.
- **Fault-injection hardening** (`fih`) for the irreversible steps (handoff
  and security-counter burns).

## Using it

Most users should depend on a platform crate (`ferriboot-esp`, `ferriboot-rp`)
instead of this crate. A platform supplies the flash driver, the security
counter storage, entropy and the jump to the application.
`boards/qemu/ferriboot/src/main.rs` is the smallest complete bootloader to
read.

Applications never hold signing keys: the updater only stages images, and the
bootloader alone decides what runs.

## Updating from an application

The platform crate builds a `FirmwareUpdater` from the bootloader's handoff,
so the application does not choose slots or an upgrade mode itself. Bring your
own transport; the example firmware has none.

```rust,ignore
let boot = BootInfo::read().expect("bootloader handoff");
let mut updater = ferriboot_esp::updater(&layout, &boot, &flash)?;

// Stage an image signed without `--pad`, in chunks of any size.
let mut writer = updater.begin_update()?;
while let Some(chunk) = transport.next_chunk() {
    writer.write(chunk)?;
}
writer.finish()?;
updater.mark_for_test()?; // or mark_permanent()
// Reset. The bootloader authenticates the image before installing it.

// In the new image, after its own health checks:
if updater.needs_confirmation()? {
    updater.confirm()?;
}
```

A test image that resets before `confirm()` is reverted under swap and
direct-XIP with revert. `begin_update()` fails with `UpdateSlotInUse` while
the running image is an unconfirmed trial, because the update slot then holds
its fallback. For a transport that delivers numbered packets and may retry or
stall, drive `upload::FirmwareUpload::receive` instead of the writer.

## Security counter

Every image carries a protected `SEC_CNT` TLV, and the platform stores a
rollback floor in eFuse or OTP. An image below the floor is never booted. A
trial boot keeps the old floor, and the new one is burned on the next reset
after confirmation. A permanent install burns it before handoff. Epochs are a
finite hardware budget, so raise the counter only for security fixes.

## Cargo features

| Feature | Default | Purpose |
|---|---|---|
| `uuid` | yes | Parse imgtool `--vid`/`--cid` names into UUIDs |
| `log` | no | Diagnostics through `log` |
| `defmt` | no | Diagnostics through `defmt` |

Enable at most one of `log` and `defmt`. With neither, logging is compiled out.

## Tests

```sh
cargo test -p ferriboot-core
```

The tests cover upgrades, power loss at every flash operation, fault
injection, validation, the security counter and the updater. The signed
fixtures in `tests/data/` are generated with upstream `imgtool`; their key is
public test material.
