#!/usr/bin/env python3
"""ESP32-C3 host tooling: build, sign, verify, inspect, and flash images.

Requires cryptography, pyelftools and imgtool; espflash to convert and
flash; Docker to build the ESP-IDF second stage. External commands always
use argument arrays.

Environment (Cargo runners pass only the ELF, so these configure them):
  FERRIBOOT_KEY             MCUboot P-256 private key (default keys/dev-p256.pem)
  IMAGE_VERSION             MCUboot image version (default 0.0.0)
  HEADER_SIZE               MCUboot header size (default: the chip's)
  ESP_SBV2_KEY              Secure Boot V2 RSA-3072 key
                            (default boards/esp32c3/keys/dev-esp32c3-sbv2-rsa3072.pem)
  ESP_BOOTLOADER_PROFILE    dev (default) or release ESP-IDF second stage
  ESP_IDF_BOOTLOADER_BIN    prebuilt unsigned second stage instead of Docker
  ESPPORT                   serial port; required to write several regions
  SLOT=image-0|image-1      flash-app target slot (default image-0)
  CONFIRM=1                 flash-app writes a padded, confirmed image
  NO_FLASH=1                flash-ferriboot builds and signs only
  FERRIBOOT_ESP_IMAGE       prebuilt padded Ferriboot ESP image (needs NO_FLASH=1)
  FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND
                            required to flash Ferriboot (first boot burns eFuses)
SECURITY_COUNTER, VID and CID are read by tools/mcuboot.py.
"""

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
import hosttools  # noqa: E402
from hosttools import (  # noqa: E402
    env_flag,
    find_executable,
    provisioning_acknowledged,
    required_file,
    resolved_path,
    run,
)
from mcuboot import sign_image  # noqa: E402

import esp_image  # noqa: E402
from ferriboot_elf import check_ferriboot_elf  # noqa: E402
from layout import geometry, table_binary  # noqa: E402
import sbv2  # noqa: E402

BOARD = Path(__file__).resolve().parents[1]
DEFAULT_SBV2_KEY = BOARD / "keys/dev-esp32c3-sbv2-rsa3072.pem"
PROFILES = ("dev", "release")
APP_SLOTS = ("image-0", "image-1")
#: MCUboot trailer write alignment of the ESP32-C3 flash driver.
FLASH_WRITE_SIZE = 4
#: The unsigned ESP-IDF second stage, and its signed form, must fit below
#: Ferriboot's partition table and the ROM's loader limits.
IDF_UNSIGNED_MAX = 0xB000
IDF_SIGNED_MAX = 0xC000
#: espflash ELF conversion settings for this board's 4 MiB DIO flash.
ESPFLASH_IMAGE_OPTIONS = (
    "--chip",
    "esp32c3",
    "--flash-mode",
    "dio",
    "--flash-freq",
    "80mhz",
    "--flash-size",
    "4mb",
)


@dataclass(frozen=True)
class Settings:
    mcuboot_key: str
    image_version: str
    header_size: str | None
    sbv2_key: str
    profile: str
    idf_bootloader: str | None
    port: str | None
    slot: str
    confirm: bool
    no_flash: bool
    prebuilt_ferriboot: str | None

    @classmethod
    def from_environment(cls):
        env = os.environ.get
        return cls(
            mcuboot_key=env("FERRIBOOT_KEY", str(hosttools.DEFAULT_MCUBOOT_KEY)),
            image_version=env("IMAGE_VERSION", "0.0.0"),
            header_size=env("HEADER_SIZE"),
            sbv2_key=env("ESP_SBV2_KEY", str(DEFAULT_SBV2_KEY)),
            profile=env("ESP_BOOTLOADER_PROFILE", "dev"),
            idf_bootloader=env("ESP_IDF_BOOTLOADER_BIN") or None,
            port=env("ESPPORT") or None,
            slot=env("SLOT", "image-0"),
            confirm=env_flag("CONFIRM"),
            no_flash=env_flag("NO_FLASH"),
            prebuilt_ferriboot=env("FERRIBOOT_ESP_IMAGE") or None,
        )

    def checked_profile(self):
        if self.profile not in PROFILES:
            raise ValueError(
                f"invalid ESP_BOOTLOADER_PROFILE: {self.profile} (expected dev or release)"
            )
        return self.profile


def partition_regions(entries):
    return {name: (offset, size) for name, _, _, offset, size in entries}


# ------------------------------------------------------------------ espflash
def espflash():
    return find_executable("espflash")


def elf_to_esp_image(elf, output, *, secure=False):
    """Convert with espflash, optionally adding Secure Boot V2 padding.

    espflash 4.6 has no released --secure-pad-v2 flag; pad and refresh the
    checksum/digest locally before signing.
    """
    run(espflash(), "save-image", *ESPFLASH_IMAGE_OPTIONS, elf, output)
    if secure:
        output.write_bytes(esp_image.secure_pad_v2(output.read_bytes()))


def write_flash(port, *regions):
    """Write (address, file) regions with espflash's ROM loader, never its RAM stub.

    espflash cannot share one connection between commands, so the target
    stays in download mode between writes and resets only after the last.
    Callers order regions so a new secure boot chain cannot start until all
    of it is present.
    """
    if not regions:
        raise ValueError("no flash regions given")
    if len(regions) > 1 and not port:
        raise ValueError(
            "ESPPORT must identify one device when writing multiple flash regions"
        )
    flasher = espflash()
    for index, (address, binary) in enumerate(regions):
        binary = required_file(binary, "flash image")
        if address < 0 or address % sbv2.SECTOR_SIZE:
            raise ValueError(f"flash address must be sector aligned: {address:#x}")
        command = [flasher, "write-bin", "--chip", "esp32c3", "--no-stub"]
        if port:
            command += ["--port", port]
        if index:
            command += ["--before", "no-reset"]
        last = index == len(regions) - 1
        command += [
            "--after",
            "hard-reset" if last else "no-reset",
            hex(address),
            binary,
        ]
        run(*command)


def monitor(port):
    run(espflash(), "monitor", *(["--port", port] if port else []))


# ------------------------------------------------------------------ signing
def sign_sbv2(raw, signed, key):
    """Sign a ROM image file with Secure Boot V2; returns the signed path."""
    raw = required_file(raw, "unsigned image")
    key = required_file(key, "SBv2 signing key")
    signed = Path(signed).expanduser().resolve()
    if signed in (raw, key):
        raise ValueError("unsigned, signed, and key paths must differ")
    return hosttools.write_output(
        signed, sbv2.sign_image(raw.read_bytes(), key.read_bytes())
    )


def sign_app(elf, output, g, settings, extra=()):
    """Convert an application ELF and MCUboot-sign it; returns the output path."""
    elf = required_file(elf, "application ELF")
    output = Path(output).expanduser().resolve()
    if elf == output:
        raise ValueError("signed output must not overwrite the ELF")
    key = required_file(settings.mcuboot_key, "MCUboot signing key")
    header = int(settings.header_size, 0) if settings.header_size else g["HEADER_SIZE"]
    with tempfile.TemporaryDirectory(prefix="ferriboot-esp32c3-app-") as temporary:
        esp_bin = Path(temporary) / "app.esp.bin"
        elf_to_esp_image(elf, esp_bin)
        payload = esp_bin.read_bytes()
    signed = sign_image(
        payload,
        key,
        header,
        g["SLOT_SIZE"],
        settings.image_version,
        g["ERASE_SIZE"],
        FLASH_WRITE_SIZE,
        extra,
        board="esp32c3",
    )
    return hosttools.write_output(output, signed)


def inspect(data, slot, g):
    """Print and enforce the slot pre-flight report."""
    report = esp_image.inspect_slot_image(data, slot, g)
    for line in report.lines():
        print(line)
    report.require_valid()


# ------------------------------------------------------------------ boot chain
def build_bootloader(output, profile, prebuilt=None):
    """Produce the unsigned ESP-IDF second stage for ``profile``."""
    g, _ = geometry()
    config = (BOARD / f"bootloader/sdkconfig.defaults.{profile}").read_text()
    match = re.search(r"^CONFIG_PARTITION_TABLE_OFFSET=(.+)$", config, re.MULTILINE)
    if match is None or int(match[1], 0) != g["PARTITION_TABLE_OFFSET"]:
        raise ValueError(
            f"{profile} ESP-IDF partition table offset disagrees with Cargo configuration"
        )
    dest = Path(output).expanduser().resolve()
    dest.parent.mkdir(parents=True, exist_ok=True)
    if prebuilt:
        if profile == "release":
            print(
                "WARNING: supplied ESP_IDF_BOOTLOADER_BIN bypasses release sdkconfig"
                " settings; verify its build configuration",
                file=sys.stderr,
            )
        source = required_file(prebuilt, "ESP_IDF_BOOTLOADER_BIN")
        if source != dest:
            shutil.copyfile(source, dest)
    else:
        docker = find_executable("docker")
        with tempfile.TemporaryDirectory(prefix="ferriboot-esp-idf-") as temporary:
            run(
                docker,
                "build",
                "--file",
                BOARD / "bootloader/Dockerfile.boot",
                "--build-arg",
                f"ESP_BOOTLOADER_PROFILE={profile}",
                "--output",
                f"type=local,dest={temporary}",
                BOARD,
            )
            built = required_file(
                Path(temporary) / "bootloader.bin", "ESP-IDF bootloader"
            )
            shutil.copyfile(built, dest)
    raw = required_file(dest, "ESP-IDF bootloader output").read_bytes()
    if raw[0] != esp_image.IMAGE_MAGIC or len(raw) > IDF_UNSIGNED_MAX:
        raise ValueError(
            f"expected ESP-IDF ROM image <= {IDF_UNSIGNED_MAX // 1024} KiB"
            f" before signing, got {len(raw):#x}"
        )
    return dest


@dataclass(frozen=True)
class BootChain:
    stage2: Path  # signed ESP-IDF second stage, at flash offset 0
    ferriboot: Path  # signed factory Ferriboot
    ferriboot_offset: int
    table: Path
    table_offset: int

    def regions(self):
        # Second stage last: the new chain cannot start until all of it is in.
        return (
            (self.ferriboot_offset, self.ferriboot),
            (self.table_offset, self.table),
            (0, self.stage2),
        )


def build_boot_chain(elf, settings):
    """Convert, sign and size-check both boot stages and the partition table.

    Outputs are written beside the ELF.
    """
    profile = settings.checked_profile()
    key = required_file(settings.sbv2_key, "SBv2 signing key")
    elf = required_file(elf, "Ferriboot ELF")
    out = elf.parent
    g, entries = geometry()
    regions = partition_regions(entries)
    check_ferriboot_elf(elf, g)

    raw = out / "ferriboot.esp.bin"
    if settings.prebuilt_ferriboot:
        if not settings.no_flash:
            raise ValueError("FERRIBOOT_ESP_IMAGE is allowed only with NO_FLASH=1")
        source = required_file(settings.prebuilt_ferriboot, "HIL ESP image")
        if source != raw:
            shutil.copyfile(source, raw)
    else:
        elf_to_esp_image(elf, raw, secure=True)
    page = g["MMU_PAGE_SIZE"]
    if required_file(raw, "Ferriboot ESP image").stat().st_size % page:
        raise ValueError("Ferriboot ESP app must be secure-padded to 64 KiB")
    ferriboot = sign_sbv2(raw, out / "ferriboot.sbv2.bin", key)

    idf_raw = build_bootloader(
        out / "bootloader.idf.bin", profile, settings.idf_bootloader
    )
    stage2 = sign_sbv2(idf_raw, out / "bootloader.idf.sbv2.bin", key)
    table = hosttools.write_output(out / "partitions.bin", table_binary(entries))

    stage2_size, ferriboot_size = stage2.stat().st_size, ferriboot.stat().st_size
    if not 0 < stage2_size <= min(IDF_SIGNED_MAX, g["PARTITION_TABLE_OFFSET"]):
        raise ValueError(
            f"signed IDF second stage exceeds {IDF_SIGNED_MAX // 1024} KiB"
            " or partition-table offset"
        )
    # SBv2 padding: whole 64 KiB pages of image, then one signature sector.
    body = ferriboot_size - sbv2.SECTOR_SIZE
    if ferriboot_size > regions["ferriboot"][1]:
        raise ValueError("signed Ferriboot does not fit its factory partition")
    if body <= 0 or body % page:
        raise ValueError("signed Ferriboot has invalid Secure Boot V2 padding")
    return BootChain(
        stage2, ferriboot, regions["ferriboot"][0], table, g["PARTITION_TABLE_OFFSET"]
    )


# ------------------------------------------------------------------ commands
def cmd_build_bootloader(args, settings):
    profile = settings.checked_profile()
    dest = build_bootloader(args.output, profile, settings.idf_bootloader)
    print(f"ESP-IDF second stage (unsigned, {profile} profile): {dest}")


def cmd_sign(args, settings):
    extra = args.extra[1:] if args.extra[:1] == ["--"] else args.extra
    g, _ = geometry()
    output = sign_app(args.elf, args.output, g, settings, extra)
    print(f"MCUboot-signed application: {output}")


def cmd_sign_sbv2(args, settings):
    signed = sign_sbv2(args.image, args.output, args.key)
    print(f"Secure Boot V2 signed image: {signed}")


def cmd_verify_sbv2(args, settings):
    signed = required_file(args.image, "signed SBv2 image")
    key = required_file(args.key, "SBv2 verification key")
    sbv2.verify_image(signed.read_bytes(), key.read_bytes())
    print(f"Secure Boot V2 verified image: {signed}")


def cmd_inspect(args, settings):
    g, _ = geometry()
    slot = args.slot if args.slot is not None else g["PRIMARY_OFFSET"]
    inspect(args.image.read_bytes(), slot, g)


def cmd_flash_app(args, settings):
    if settings.slot not in APP_SLOTS:
        raise ValueError("SLOT must be image-0 or image-1")
    g, entries = geometry()
    offset, _ = partition_regions(entries)[settings.slot]
    output = Path(f"{required_file(args.elf, 'application ELF')}.signed.bin")
    extra = ("--pad", "--confirm") if settings.confirm else ()
    sign_app(args.elf, output, g, settings, extra)
    inspect(output.read_bytes(), offset, g)
    write_flash(settings.port, (offset, output))
    monitor(settings.port)


def cmd_flash_ferriboot(args, settings):
    chain = build_boot_chain(args.elf, settings)
    print(
        f"IDF second stage {chain.stage2.stat().st_size:#x};"
        f" signed factory Ferriboot {chain.ferriboot.stat().st_size:#x}"
    )
    print(f"stage 2: {chain.stage2} at 0x0")
    print(f"stage 3: {chain.ferriboot} at {chain.ferriboot_offset:#x}")
    print(f"partition table: {chain.table} at {chain.table_offset:#x}")
    if settings.no_flash:
        return
    if not provisioning_acknowledged():
        raise ValueError(
            "refusing to flash: first boot can PERMANENTLY enable Secure Boot V2"
            " and burn ESP32-C3 security eFuses. Use NO_FLASH=1 to build only, or"
            f" {hosttools.acknowledgement_hint()} after reviewing your provisioning plan"
        )
    write_flash(settings.port, *chain.regions())


def parser():
    top = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = top.add_subparsers(dest="command", required=True)

    def command(name, handler, help):
        sub = commands.add_parser(name, help=help)
        sub.set_defaults(handler=handler)
        return sub

    sub = command(
        "build-bootloader",
        cmd_build_bootloader,
        "build/check an unsigned IDF second stage",
    )
    sub.add_argument("output", type=resolved_path)

    sub = command("sign", cmd_sign, "convert and MCUboot-sign an application ELF")
    sub.add_argument("elf", type=resolved_path)
    sub.add_argument("output", type=resolved_path)
    sub.add_argument("extra", nargs=argparse.REMAINDER, help="extra imgtool arguments")

    sub = command(
        "sign-sbv2", cmd_sign_sbv2, "RSA-3072 Secure Boot V2 sign a ROM image"
    )
    sub.add_argument("image", type=resolved_path, help="unsigned ESP image")
    sub.add_argument("output", type=resolved_path, help="signed output image")
    sub.add_argument(
        "--key", type=resolved_path, required=True, help="RSA-3072 private key PEM"
    )

    sub = command("verify-sbv2", cmd_verify_sbv2, "verify a ROM Secure Boot V2 image")
    sub.add_argument("image", type=resolved_path, help="signed ESP image")
    sub.add_argument(
        "--key",
        type=resolved_path,
        required=True,
        help="trusted public or private key PEM",
    )

    sub = command(
        "inspect", cmd_inspect, "check a signed application and its ESP segments"
    )
    sub.add_argument("image", type=resolved_path)
    sub.add_argument(
        "slot",
        nargs="?",
        type=lambda value: int(value, 0),
        help="slot flash offset (default: image-0)",
    )

    sub = command("flash-app", cmd_flash_app, "sign and flash an application slot")
    sub.add_argument("elf", type=resolved_path)

    sub = command(
        "flash-ferriboot",
        cmd_flash_ferriboot,
        "build, sign and optionally flash both boot stages",
    )
    sub.add_argument("elf", type=resolved_path)
    return top


def main(argv=None):
    cli = parser()
    args = cli.parse_args(argv)
    hosttools.run_cli(
        cli, lambda: args.handler(args, Settings.from_environment()), "esp32c3 firmware"
    )


if __name__ == "__main__":
    main()
