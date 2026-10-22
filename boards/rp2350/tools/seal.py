"""RP2350 ROM secure boot: seal a checked bootloader with picotool 2.x.

The ROM key is a secp256k1 key, separate from the MCUboot P-256 key. The
bootloader embeds only its OTP profile (dev or release), never the key: on
its first boot it enrolls SHA-256 of the public key found in its own seal.

Requires cryptography and picotool (2.3 or later sets the EXTRA_SECURITY
image-type bit that both this tool and the bootloader require).
"""

from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec

from hosttools import write_output
import picobin

# The bootloader's enrollment marker: a tag followed by a 4-byte profile.
ENROLLMENT_MARKER = b"FERRIBOOT_OTP_V3"
# Builds that embedded the ROM key or its digest; reject rather than seal.
LEGACY_ENROLLMENT_MARKERS = (b"FERRIBOOT_OTP_V1", b"FERRIBOOT_OTP_V2")
PROVISION_PROFILES = {b"DEV\x00": "dev", b"REL\x00": "release"}


def load_rom_key(path):
    """Resolve and check a secp256k1 private key; never the MCUboot key."""
    if not path:
        raise ValueError("set RP2350_SECURE_BOOT_KEY to a private secp256k1 PEM key")
    key_path = Path(path).expanduser().resolve()
    key = serialization.load_pem_private_key(key_path.read_bytes(), password=None)
    if not isinstance(key, ec.EllipticCurvePrivateKey) or not isinstance(
        key.curve, ec.SECP256K1
    ):
        raise ValueError(
            "RP2350 ROM secure boot needs an ECDSA secp256k1 PEM private key (not MCUboot P-256)"
        )
    return key_path


def public_key_bytes(key_path):
    """Raw X || Y of the key, as PICOBIN SIGNATURE items store it."""
    key = serialization.load_pem_private_key(Path(key_path).read_bytes(), password=None)
    numbers = key.public_key().public_numbers()
    return numbers.x.to_bytes(32, "big") + numbers.y.to_bytes(32, "big")


def embedded_security_profile(payload):
    """The bootloader's single dev/release OTP profile marker."""
    if any(old in payload for old in LEGACY_ENROLLMENT_MARKERS):
        raise ValueError(
            "legacy key-embedded OTP build: rebuild the key-agnostic V3 profile"
        )
    count = payload.count(ENROLLMENT_MARKER)
    if count > 1:
        raise ValueError("bootloader contains multiple secure-boot OTP profiles")
    if not count:
        raise ValueError("missing mandatory RP2350 secure-boot OTP profile")
    start = payload.index(ENROLLMENT_MARKER) + len(ENROLLMENT_MARKER)
    if start < picobin.ROM_HEADER_SIZE or start + 4 > len(payload):
        raise ValueError(
            "secure-boot OTP profile is not inside ROM-loaded bootloader SRAM"
        )
    profile = PROVISION_PROFILES.get(payload[start : start + 4])
    if profile is None:
        raise ValueError("unknown RP2350 OTP secure-boot profile")
    return profile


def check_otp_plan(plan):
    """Refuse an incomplete picotool OTP plan (BOOTKEY0, KEY_VALID, SECURE_BOOT_ENABLE)."""
    if not isinstance(plan, dict):
        raise ValueError("picotool OTP configuration must be a JSON object")
    key = plan.get("bootkey0")
    if (
        not isinstance(key, list)
        or len(key) != 32
        or any(type(value) is not int or not 0 <= value <= 255 for value in key)
        or not any(key)
    ):
        raise ValueError("picotool OTP plan lacks a 32-byte BOOTKEY0 digest")
    flags = plan.get("boot_flags1")
    if (
        not isinstance(flags, dict)
        or type(flags.get("key_valid")) is not int
        or not flags["key_valid"] & 1
    ):
        raise ValueError("picotool OTP plan does not mark BOOTKEY0 valid")
    critical = plan.get("crit1")
    if not isinstance(critical, dict) or critical.get("secure_boot_enable") != 1:
        raise ValueError("picotool OTP plan does not enable secure boot")


def verify_seal_key(sealed, key_path, otp_plan=None):
    """The sealed image's key must be the ROM key's; return its SHA-256.

    With ``otp_plan`` (picotool's JSON), its BOOTKEY0 must be that digest too.
    """
    embedded = picobin.signing_public_key(sealed)
    if embedded != public_key_bytes(key_path):
        raise ValueError("PICOBIN public key does not match ROM signing private key")
    digest = hashlib.sha256(embedded).digest()
    if otp_plan is not None:
        check_otp_plan(otp_plan)
        if bytes(otp_plan["bootkey0"]) != digest:
            raise ValueError(
                "OTP plan digest differs from the embedded PICOBIN signing key"
            )
    return digest


# ------------------------------------------------------------------ picotool
@dataclass(frozen=True)
class Picotool:
    executable: str

    def seal(self, unsigned, origin, sealed, key, otp_plan=None):
        # BIN in and out: the ELF has SRAM VMAs and flash LMAs, and picotool
        # must keep the custom SRAM load map.
        command = [
            self.executable,
            "seal",
            "--sign",
            str(unsigned),
            "-t",
            "bin",
            "-o",
            hex(origin),
            str(sealed),
            "-t",
            "bin",
            str(key),
        ]
        if otp_plan is not None:
            command.append(str(otp_plan))
        subprocess.run(command, check=True)

    def info(self, image):
        result = subprocess.run(
            [self.executable, "info", "-a", str(image), "-t", "bin"],
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout + result.stderr


def check_picotool_report(report, unsigned_size, g):
    """picotool's own verdict on the sealed image, from ``picotool info -a``.

    Only picotool verifies the secp256k1 signature over its hashed ranges,
    so this parses its human-readable report. The signed block must still
    copy exactly the bootloader into SRAM rather than XIP-execute flash.
    """
    if not re.search(r"signature\s*:\s*verified\b", report, re.I):
        raise ValueError("picotool did not verify the ROM secp256k1 signature")
    if not re.search(r"image type\s*:\s*ARM Secure\b", report, re.I):
        raise ValueError("signed image is not RP2350 ARM Secure")
    source = g["XIP_BASE"] + picobin.ROM_HEADER_SIZE
    source_end = g["XIP_BASE"] + unsigned_size
    destination = g["RAM_BASE"]
    destination_end = destination + unsigned_size - picobin.ROM_HEADER_SIZE
    expected_copy = (
        rf"load map entry\s*\d+\s*:\s*Copy\s+"
        rf"{source:#x}->{source_end:#x}\s+to\s+"
        rf"{destination:#x}->{destination_end:#x}\b"
    )
    if not re.search(expected_copy, report, re.I):
        raise ValueError("signed image has no exact ROM flash-to-SRAM load map")


def check_sealed_bootloader(unsigned, signed, g):
    """Sealing may append and re-link metadata, never change the SRAM image."""
    if not picobin.ROM_HEADER_SIZE <= len(unsigned) < len(signed):
        raise ValueError("sealed bootloader is missing a signature block")
    if len(signed) > g["BOOTLOADER_SIZE"]:
        raise ValueError("sealed bootloader exceeds its flash partition")
    # The linker block up to its LAST item is fixed; picotool may only
    # rewrite the link word after it.
    fixed = picobin.LINKER_LAST_OFFSET
    if signed[:fixed] != unsigned[:fixed]:
        raise ValueError("sealing changed the bootloader's ROM metadata items")
    if (
        signed[picobin.ROM_HEADER_SIZE : len(unsigned)]
        != unsigned[picobin.ROM_HEADER_SIZE :]
    ):
        raise ValueError("sealing changed the bootloader SRAM executable")


def seal_bootloader(image, output, key_path, picotool, g, otp_output=None):
    """ROM-sign a checked bootloader FlashImage; write the sealed BIN to ``output``.

    With ``otp_output``, also write picotool's OTP plan there. Nothing is
    written unless every check passes. Nothing is programmed into OTP.
    """
    embedded_security_profile(image.payload)
    with tempfile.TemporaryDirectory(prefix="ferriboot-rp2350-seal-") as temp:
        unsigned_path = Path(temp) / "unsigned.bin"
        sealed_path = Path(temp) / "signed.bin"
        plan_path = Path(temp) / "otp.json" if otp_output is not None else None
        unsigned_path.write_bytes(image.payload)
        picotool.seal(unsigned_path, image.origin, sealed_path, key_path, plan_path)
        sealed = sealed_path.read_bytes()
        check_sealed_bootloader(image.payload, sealed, g)
        check_picotool_report(picotool.info(sealed_path), len(image.payload), g)
        plan = json.loads(plan_path.read_text()) if plan_path is not None else None
        verify_seal_key(sealed, key_path, plan)
        write_output(output, sealed)
        if otp_output is not None:
            write_output(otp_output, plan_path.read_bytes())
    return sealed
