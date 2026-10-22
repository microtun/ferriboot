#!/usr/bin/env python3
"""RP2350 ELF conversion, MCUboot signing, ROM secure-boot sealing, and SWD loading.

Requires pyelftools, imgtool and cryptography. ROM sealing uses picotool 2.3+.

Environment (Cargo runners pass only the ELF, so these configure them):
  FERRIBOOT_KEY             MCUboot P-256 private key (default keys/dev-p256.pem)
  IMAGE_VERSION             MCUboot image version (default 0.0.0)
  RP2350_SECURE_BOOT_KEY    ROM secp256k1 private key (sign-bootloader --key overrides)
  PICOTOOL                  picotool executable (default: picotool on PATH)
  PROBE_RS_CHIP             probe-rs chip (default RP235x)
  PROBE_RS_PROBE            probe-rs probe selector
  PROBE_RS_SPEED_KHZ        SWD speed
  FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND
                            required by load-bootloader (first boot burns OTP)
SECURITY_COUNTER, VID and CID are read by tools/mcuboot.py.
"""

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
import hosttools  # noqa: E402
from hosttools import find_executable, provisioning_acknowledged, resolved_path, run  # noqa: E402
from mcuboot import inspect_signed, sign_image  # noqa: E402

import elf_image  # noqa: E402
from layout import geometry  # noqa: E402
import seal  # noqa: E402

#: MCUboot image flags required by Ferriboot's RP2350 policy (IMAGE_F_ROM_FIXED).
ROM_FIXED = 0x100


@dataclass(frozen=True)
class Settings:
    mcuboot_key: Path
    image_version: str
    rom_key: str | None
    picotool: str
    probe_chip: str
    probe: str | None
    probe_speed_khz: str | None

    @classmethod
    def from_environment(cls):
        env = os.environ.get
        return cls(
            mcuboot_key=Path(
                env("FERRIBOOT_KEY", hosttools.DEFAULT_MCUBOOT_KEY)
            ).resolve(),
            image_version=env("IMAGE_VERSION", "0.0.0"),
            rom_key=env("RP2350_SECURE_BOOT_KEY"),
            picotool=env("PICOTOOL", "picotool"),
            probe_chip=env("PROBE_RS_CHIP", "RP235x"),
            probe=env("PROBE_RS_PROBE") or None,
            probe_speed_khz=env("PROBE_RS_SPEED_KHZ") or None,
        )

    def probe_options(self):
        options = ["--chip", self.probe_chip, "--protocol", "swd"]
        if self.probe:
            options += ["--probe", self.probe]
        if self.probe_speed_khz:
            options += ["--speed", self.probe_speed_khz]
        return options

    def picotool_runner(self):
        hint = "picotool 2.3+ is required to seal the RP2350 ROM image"
        return seal.Picotool(find_executable(self.picotool, hint=hint))


# ------------------------------------------------------------------ MCUboot
def sign_app(image, g, settings, *, pad=False, confirm=False):
    """MCUboot-sign an application FlashImage for its fixed XIP address.

    Returns the signed file and the size of its authenticated part.
    """
    runtime_base = image.origin - g["HEADER_SIZE"]
    extra = ["--rom-fixed", hex(runtime_base)]
    if pad or confirm:
        extra.append("--pad")
    if confirm:
        extra.append("--confirm")
    signed = sign_image(
        image.payload,
        settings.mcuboot_key,
        g["HEADER_SIZE"],
        g["SLOT_SIZE"],
        settings.image_version,
        g["ERASE_SIZE"],
        g["WRITE_SIZE"],
        extra,
        board="rp2350",
    )
    return signed, check_signed_app(signed, image.origin, g)


def check_signed_app(data, origin, g):
    """The signed header must match the linked runtime address; returns its size."""
    image = inspect_signed(data, g["SLOT_SIZE"], g["ERASE_SIZE"], g["WRITE_SIZE"])
    expected = (origin - g["HEADER_SIZE"], g["HEADER_SIZE"], ROM_FIXED)
    if (image.load_address, image.header_size, image.flags) != expected:
        raise ValueError(
            "signed header does not match the RP2350 runtime/linking contract"
        )
    body = data[image.header_size : image.header_size + image.body_size]
    elf_image.check_vectors(body, origin, g)
    return image.size


# ------------------------------------------------------------------ probe-rs
def download(binary, address, settings):
    options = settings.probe_options()
    run(
        "probe-rs",
        "download",
        *options,
        "--binary-format",
        "bin",
        "--base-address",
        hex(address),
        binary,
    )
    run("probe-rs", "reset", *options)


# ------------------------------------------------------------------ commands
def cmd_check(args, settings, g):
    image = elf_image.load(args.elf, g, bootloader=args.bootloader)
    print(
        f"{args.elf}: checked origin {image.origin:#x}, {len(image.payload):#x} payload bytes"
    )


def cmd_export_bin(args, settings, g):
    image = elf_image.load(args.elf, g, bootloader=args.bootloader)
    args.output.write_bytes(image.payload)
    print(
        f"{args.output}: flash {image.origin:#x}, {len(image.payload):#x} unsigned payload bytes"
    )


def cmd_sign(args, settings, g):
    image = elf_image.load(args.elf, g)
    signed, size = sign_app(image, g, settings, pad=args.pad, confirm=args.confirm)
    args.output.write_bytes(signed)
    print(
        f"{args.output}: runtime base {image.origin - g['HEADER_SIZE']:#x},"
        f" authenticated image {size:#x} bytes"
    )


def cmd_sign_bootloader(args, settings, g):
    key = seal.load_rom_key(args.key or settings.rom_key)
    otp_output = args.otp_output
    if args.output in (args.elf, key, otp_output):
        raise ValueError("signed bootloader output must be a separate file")
    if otp_output is not None and (
        otp_output.exists() or otp_output in (args.elf, key)
    ):
        raise ValueError(
            "OTP output must be a new file; picotool can merge existing OTP JSON"
        )
    picotool = settings.picotool_runner()
    image = elf_image.load(args.elf, g, bootloader=True)
    sealed = seal.seal_bootloader(image, args.output, key, picotool, g, otp_output)
    print(
        f"{args.output}: RP2350 ROM-signed SRAM bootloader,"
        f" flash {image.origin:#x}, {len(sealed):#x} bytes"
    )
    if otp_output is not None:
        print(
            f"{otp_output}: UNAPPLIED one-time OTP plan; review before picotool otp load"
        )


def cmd_load_app(args, settings, g):
    image = elf_image.load(args.elf, g)
    # Factory installations are confirmed; use unpadded `sign` output for OTA.
    signed, _ = sign_app(image, g, settings, confirm=True)
    with tempfile.TemporaryDirectory(prefix="ferriboot-rp2350-") as temp:
        binary = Path(temp) / "app.bin"
        binary.write_bytes(signed)
        # The whole signed slot, not the unsigned payload ELF.
        download(binary, g["XIP_BASE"] + g[f"{args.slot.upper()}_OFFSET"], settings)


def cmd_load_bootloader(args, settings, g):
    image = elf_image.load(args.elf, g, bootloader=True)
    profile = seal.embedded_security_profile(image.payload)
    # The next boot may irreversibly program OTP. Require consent every time.
    if not provisioning_acknowledged():
        raise ValueError(
            f"{profile} secure-boot first boot can irreversibly burn RP2350 OTP,"
            " including permanently arming the glitch detectors"
            " (release also permanently disables SWD and revokes spare key slots);"
            f" review the target and {hosttools.acknowledgement_hint()}"
        )
    key = seal.load_rom_key(settings.rom_key)
    picotool = settings.picotool_runner()
    with tempfile.TemporaryDirectory(prefix="ferriboot-rp2350-") as temp:
        binary = Path(temp) / "bootloader.bin"
        # Never flash an unsealed bootloader.
        seal.seal_bootloader(image, binary, key, picotool, g)
        download(binary, image.origin, settings)


def parser():
    top = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = top.add_subparsers(dest="command", required=True)

    def command(name, handler, help=None):
        sub = commands.add_parser(name, help=help)
        sub.set_defaults(handler=handler)
        sub.add_argument("elf", type=resolved_path)
        return sub

    sub = command("check", cmd_check, "preflight an ELF without flashing")
    sub.add_argument("--bootloader", action="store_true")

    sub = command(
        "export-bin",
        cmd_export_bin,
        "export checked ELF flash bytes as an unsigned binary",
    )
    sub.add_argument("output", type=resolved_path)
    sub.add_argument(
        "--bootloader",
        action="store_true",
        help="export a ROM-loadable RAM bootloader instead of an application",
    )

    sub = command("sign", cmd_sign, "MCUboot-sign an application")
    sub.add_argument("output", type=resolved_path)
    sub.add_argument(
        "--pad", action="store_true", help="factory/test file with pending trailer"
    )
    sub.add_argument(
        "--confirm", action="store_true", help="factory file with confirmed trailer"
    )

    sub = command(
        "sign-bootloader",
        cmd_sign_bootloader,
        "secp256k1 ROM-sign a bootloader; no OTP writes",
    )
    sub.add_argument("output", type=resolved_path)
    sub.add_argument(
        "--key", help="private secp256k1 PEM (default: RP2350_SECURE_BOOT_KEY)"
    )
    sub.add_argument(
        "--otp-output",
        type=resolved_path,
        help="new OTP JSON plan from picotool; does not provision device",
    )

    sub = command("load-app", cmd_load_app, "sign and download an application over SWD")
    sub.add_argument(
        "--slot",
        choices=("primary", "secondary"),
        default="primary",
        help="physical installation slot (default: primary)",
    )

    command(
        "load-bootloader",
        cmd_load_bootloader,
        "seal and download the bootloader over SWD",
    )
    return top


def main(argv=None):
    cli = parser()
    args = cli.parse_args(argv)
    hosttools.run_cli(
        cli,
        lambda: args.handler(args, Settings.from_environment(), geometry()),
        "rp2350",
    )


if __name__ == "__main__":
    main()
