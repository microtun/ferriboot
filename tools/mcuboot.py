#!/usr/bin/env python3
"""Shared P-256 MCUboot signing and signed-image checks for board tools."""

import argparse
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile


# Microtun default identities. Keep these names in sync with each platform's
# policy.rs. `imgtool` derives UUIDv5(DNS, vendor), then UUIDv5(VID, class).
IMAGE_VENDOR = "microtun.dev"
IMAGE_CLASSES = {
    "esp32c3": "esp32c3",
    "rp2350": "rp2350",
    "qemu": "qemu-an385",
}


def max_image_size(slot_size, erase_size=4096, write_size=4):
    """Reserve whole trailer sectors, matching core Trailer::for_slot."""
    sectors = slot_size // erase_size
    trailer = sectors * 3 * write_size + 48
    return (slot_size - trailer) // erase_size * erase_size


def load_signing_key(path):
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import ec

    key = serialization.load_pem_private_key(Path(path).read_bytes(), password=None)
    if not isinstance(key, ec.EllipticCurvePrivateKey) or not isinstance(
        key.curve, ec.SECP256R1
    ):
        raise ValueError("Ferriboot requires an ECDSA P-256 private signing key")
    return key


@dataclass(frozen=True)
class SignedImage:
    load_address: int
    header_size: int
    body_size: int
    flags: int
    size: int


def _tlvs(data, offset, expected_tag, limit):
    if offset + 4 > limit:
        raise ValueError("truncated TLV info")
    tag, length = struct.unpack_from("<HH", data, offset)
    end = offset + length
    if tag != expected_tag or length < 4 or end > limit:
        raise ValueError("invalid TLVs or image overlaps trailer sectors")
    entries = []
    cursor = offset + 4
    while cursor < end:
        if cursor + 4 > end:
            raise ValueError("truncated TLV header")
        kind, size = struct.unpack_from("<HH", data, cursor)
        cursor += 4
        if cursor + size > end:
            raise ValueError("truncated TLV value")
        entries.append((kind, bytes(data[cursor : cursor + size])))
        cursor += size
    return end, entries


def inspect_signed(data, slot_size, erase_size=4096, write_size=4, public_key=None):
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec, utils

    if len(data) < 32:
        raise ValueError("truncated signed header")
    magic, load, hdr, prot, body, flags = struct.unpack_from("<IIHHII", data)
    limit = min(len(data), max_image_size(slot_size, erase_size, write_size))
    if magic != 0x96F3B83D or hdr < 32 or hdr + body > limit:
        raise ValueError("invalid signed header or image overlaps trailer sectors")
    offset = hdr + body
    if prot:
        end, _ = _tlvs(data, offset, 0x6908, limit)
        if end - offset != prot:
            raise ValueError("invalid protected TLV length")
        offset = end
    digest = hashlib.sha256(data[:offset]).digest()
    end, entries = _tlvs(data, offset, 0x6907, limit)
    have_hash = have_signature = have_key = False
    der = (
        None
        if public_key is None
        else public_key.public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
    )
    for kind, value in entries:
        if kind == 0x10:
            have_hash = value == digest
        elif kind == 0x01 and der is not None:
            have_key |= value == hashlib.sha256(der).digest()
        elif kind == 0x02 and der is not None:
            have_key |= value == der
        elif kind == 0x22:
            if not 8 <= len(value) <= 72:
                raise ValueError("invalid P-256 signature length")
            utils.decode_dss_signature(value)
            if public_key is not None:
                try:
                    public_key.verify(
                        value, digest, ec.ECDSA(utils.Prehashed(hashes.SHA256()))
                    )
                except InvalidSignature as error:
                    raise ValueError("invalid P-256 signature") from error
            have_signature = True
        elif kind == 0x25:
            raise ValueError("pure signatures are unsupported")
    if not have_hash or not have_signature or (public_key is not None and not have_key):
        raise ValueError("missing/mismatched SHA256, P-256 signature or trusted key")
    if len(data) > slot_size:
        raise ValueError("signed file exceeds slot")
    return SignedImage(load, hdr, body, flags, end)


def sign_image(
    payload,
    key_path,
    header_size,
    slot_size,
    version,
    erase_size=4096,
    write_size=4,
    extra_args=(),
    board=None,
):
    key_path = Path(key_path).resolve()
    key = load_signing_key(key_path)
    with tempfile.TemporaryDirectory(prefix="ferriboot-sign-") as temp:
        raw, signed = Path(temp) / "payload.bin", Path(temp) / "signed.bin"
        raw.write_bytes(payload)
        command = [
            "imgtool",
            "sign",
            "--key",
            str(key_path),
            "--sha",
            "256",
            "--header-size",
            hex(header_size),
            "--pad-header",
            "--align",
            str(write_size),
            "--max-align",
            "8",
            "--max-sectors",
            str(slot_size // erase_size),
            "--slot-size",
            hex(slot_size),
            "--version",
            version,
        ]
        # Both hardware bootloaders require a signed epoch, including epoch 0.
        # An explicit imgtool option takes precedence over the environment.
        if not any(
            arg == "--security-counter" or arg.startswith("--security-counter=")
            for arg in extra_args
        ):
            command.extend(
                ["--security-counter", os.environ.get("SECURITY_COUNTER", "0")]
            )
        # Image identities are required by each board's Policy. A deliberate
        # extra imgtool argument overrides the corresponding environment or
        # configured default, as for --security-counter.
        for variable, option, default in (
            ("VID", "--vid", IMAGE_VENDOR if board else None),
            ("CID", "--cid", IMAGE_CLASSES[board] if board else None),
        ):
            if not any(
                arg == option or arg.startswith(option + "=") for arg in extra_args
            ):
                value = os.environ.get(variable, default)
                if value:
                    command.extend([option, value])
        subprocess.run(command + list(extra_args) + [str(raw), str(signed)], check=True)
        data = signed.read_bytes()
        inspect_signed(data, slot_size, erase_size, write_size, key.public_key())
        return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check-key").add_argument("key")
    sign = commands.add_parser("sign")
    sign.add_argument("payload", type=Path)
    sign.add_argument("output", type=Path)
    sign.add_argument("--key", required=True)
    sign.add_argument("--header-size", required=True, type=lambda v: int(v, 0))
    sign.add_argument("--slot-size", required=True, type=lambda v: int(v, 0))
    sign.add_argument("--version", default="0.0.0")
    sign.add_argument(
        "--board",
        choices=sorted(IMAGE_CLASSES),
        help="apply board-specific Microtun VID/CID defaults",
    )
    # Arguments after -- belong to imgtool, preserving the ESP wrapper's CLI.
    args_before = sys.argv[1:]
    extra = []
    if "--" in args_before:
        split = args_before.index("--")
        args_before, extra = args_before[:split], args_before[split + 1 :]
    args = parser.parse_args(args_before)
    args.key = str(Path(args.key).resolve())
    if args.command == "sign":
        args.payload = args.payload.resolve()
        args.output = args.output.resolve()
    try:
        if args.command == "check-key":
            load_signing_key(args.key)
        else:
            data = sign_image(
                args.payload.read_bytes(),
                args.key,
                args.header_size,
                args.slot_size,
                args.version,
                extra_args=extra,
                board=args.board,
            )
            args.output.write_bytes(data)
    except (ValueError, OSError, struct.error, subprocess.CalledProcessError) as error:
        parser.exit(1, f"mcuboot: {error}\n")


if __name__ == "__main__":
    main()
