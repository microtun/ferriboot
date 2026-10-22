#!/usr/bin/env python3
"""Generate MCUboot-compatible P-256/SHA-256 fixtures (requires cryptography).

The fixed private scalar is public test material, never a production key.
Run from any directory: python3 tools/generate-test-images.py
"""

import hashlib
from pathlib import Path
import struct
import uuid

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, utils

DATA = Path(__file__).resolve().parents[1] / "ferriboot-core/tests/data"


def tlv(kind, value):
    return struct.pack("<HH", kind, len(value)) + value


def generate():
    key = ec.derive_private_key(int.from_bytes(bytes([13]) * 32, "big"), ec.SECP256R1())
    public_der = key.public_key().public_bytes(
        serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
    )
    (DATA / "test-p256.pem").write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    (DATA / "test-p256.pub.der").write_bytes(public_der)
    payload = (DATA / "payload.bin").read_bytes()
    vid = uuid.uuid5(uuid.NAMESPACE_DNS, "example.com")
    cid = uuid.uuid5(vid, "esp32c3-app")
    protected = (
        tlv(0x50, struct.pack("<I", 7)) + tlv(0x74, vid.bytes) + tlv(0x75, cid.bytes)
    )
    protected = struct.pack("<HH", 0x6908, len(protected) + 4) + protected
    for filename, hdr_size, version, prot, full_key, pad in [
        ("signed.bin", 0x1000, (1, 2, 3, 4), protected, False, False),
        ("signed-pubkey.bin", 32, (0, 9, 0, 0), b"", True, False),
        ("signed-padded-confirmed.bin", 32, (2, 0, 0, 0), b"", False, True),
    ]:
        header = struct.pack(
            "<IIHHIIBBHI4x",
            0x96F3B83D,
            0,
            hdr_size,
            len(prot),
            len(payload),
            0,
            *version,
        )
        image = header + bytes(hdr_size - len(header)) + payload + prot
        digest = hashlib.sha256(image).digest()
        # Prehashed signs the image hash exactly once, matching imgtool ECDSA.
        signature = key.sign(
            digest,
            ec.ECDSA(utils.Prehashed(hashes.SHA256()), deterministic_signing=True),
        )
        key_tlv = (
            tlv(0x02, public_der)
            if full_key
            else tlv(0x01, hashlib.sha256(public_der).digest())
        )
        body = tlv(0x10, digest) + key_tlv + tlv(0x22, signature)
        image += struct.pack("<HH", 0x6907, len(body) + 4) + body
        if pad:
            image += b"\xff" * (131072 - len(image))
            image = bytearray(image)
            image[-24] = 1
            image[-16:] = bytes.fromhex("77c295f360d2ef7f3552500f2cb67980")
        (DATA / filename).write_bytes(image)


if __name__ == "__main__":
    generate()
