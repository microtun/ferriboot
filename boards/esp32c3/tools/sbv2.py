"""ESP32-C3 ROM Secure Boot V2: RSA-3072 / PSS / SHA-256 signature sectors.

A signed image is the unsigned image, padded with 0xFF to a whole sector,
followed by one 4 KiB signature sector holding a single signature block.

Requires cryptography.
"""

import binascii
import hashlib
import struct

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa

SECTOR_SIZE = 4096
KEY_BITS = 3072
MODULUS_SIZE = KEY_BITS // 8
MAGIC = b"\xe7\x02\x00\x00"  # magic byte 0xE7, block version 2

# Signature block layout. The ROM takes the key in its Montgomery form:
# modulus n, exponent e, R^2 mod n and -n^-1 mod 2^32, all little-endian.
_MAGIC = slice(0, 4)
_DIGEST = slice(4, 36)
_KEY = slice(36, 36 + MODULUS_SIZE + 4 + MODULUS_SIZE + 4)
_SIGNATURE = slice(_KEY.stop, _KEY.stop + MODULUS_SIZE)
_CRC = slice(_SIGNATURE.stop, _SIGNATURE.stop + 4)
_RESERVED = slice(_CRC.stop, _CRC.stop + 16)
BLOCK_SIZE = _RESERVED.stop  # 1216

_PSS = padding.PSS(mgf=padding.MGF1(hashes.SHA256()), salt_length=32)


def _rsa3072(key):
    public = key.public_key() if isinstance(key, rsa.RSAPrivateKey) else key
    if not isinstance(public, rsa.RSAPublicKey) or public.key_size != KEY_BITS:
        raise ValueError("ESP32-C3 Secure Boot V2 requires an RSA-3072 key")
    return public


def _private_key(pem):
    key = serialization.load_pem_private_key(pem, password=None)
    if not isinstance(key, rsa.RSAPrivateKey):
        raise ValueError("ESP32-C3 Secure Boot V2 signing requires an RSA private key")
    _rsa3072(key)
    return key


def _public_key(pem):
    """A public key from a private or public PEM."""
    try:
        key = serialization.load_pem_private_key(pem, password=None)
    except ValueError:
        key = serialization.load_pem_public_key(pem)
    return _rsa3072(key)


def _key_fields(public):
    numbers = public.public_numbers()
    n = numbers.n
    return b"".join(
        (
            n.to_bytes(MODULUS_SIZE, "little"),
            struct.pack("<I", numbers.e),
            pow(2, 2 * KEY_BITS, n).to_bytes(MODULUS_SIZE, "little"),
            struct.pack("<I", -pow(n, -1, 1 << 32) % (1 << 32)),
        )
    )


def sign_image(image: bytes, private_key_pem: bytes) -> bytes:
    """Sector-pad ``image`` and append its signature sector; self-verifies."""
    if not image:
        raise ValueError("empty image")
    key = _private_key(private_key_pem)
    image += b"\xff" * (-len(image) % SECTOR_SIZE)
    block = bytearray(BLOCK_SIZE)
    block[_MAGIC] = MAGIC
    block[_DIGEST] = hashlib.sha256(image).digest()
    block[_KEY] = _key_fields(key.public_key())
    block[_SIGNATURE] = key.sign(image, _PSS, hashes.SHA256())[::-1]
    block[_CRC] = struct.pack("<I", binascii.crc32(block[: _CRC.start]))
    signed = image + bytes(block) + b"\xff" * (SECTOR_SIZE - BLOCK_SIZE)
    verify_image(signed, private_key_pem)
    return signed


def verify_image(signed: bytes, key_pem: bytes) -> None:
    """Check digest, CRC, key fields and signature against a trusted key.

    ``key_pem`` may be private or public. Never trust a key embedded in the
    image: the caller's key must match the block.
    """
    if len(signed) < 2 * SECTOR_SIZE or len(signed) % SECTOR_SIZE:
        raise ValueError("invalid signed image size")
    image, sector = signed[:-SECTOR_SIZE], signed[-SECTOR_SIZE:]
    block = sector[:BLOCK_SIZE]
    if block[_MAGIC] != MAGIC or any(block[_RESERVED]):
        raise ValueError("invalid signature block header or padding")
    if sector[BLOCK_SIZE:] != b"\xff" * (SECTOR_SIZE - BLOCK_SIZE):
        raise ValueError("invalid signature sector padding")
    if struct.unpack("<I", block[_CRC])[0] != binascii.crc32(block[: _CRC.start]):
        raise ValueError("signature block CRC mismatch")
    if block[_DIGEST] != hashlib.sha256(image).digest():
        raise ValueError("image digest mismatch")
    public = _public_key(key_pem)
    if block[_KEY] != _key_fields(public):
        raise ValueError("signature key mismatch")
    try:
        public.verify(block[_SIGNATURE][::-1], image, _PSS, hashes.SHA256())
    except InvalidSignature as error:
        raise ValueError("RSA-PSS signature mismatch") from error
