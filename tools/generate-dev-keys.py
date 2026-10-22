#!/usr/bin/env python3
"""Generate the six development PEM files in the repository key layout.

Requires: python3 -m pip install cryptography
Existing keys are never overwritten unless --force is supplied.
"""

import argparse
import os
from pathlib import Path
import tempfile

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec, rsa


KEY_DIRECTORIES = {
    "dev-esp32c3-sbv2-rsa3072": Path("boards/esp32c3/keys"),
    "dev-p256": Path("keys"),
    "dev-rp2350-rom-secp256k1": Path("boards/rp2350/keys"),
}


def key_pairs():
    """Yield (file stem, private key, PEM private-key format)."""
    yield (
        "dev-esp32c3-sbv2-rsa3072",
        rsa.generate_private_key(public_exponent=65537, key_size=3072),
        serialization.PrivateFormat.PKCS8,
    )
    yield (
        "dev-p256",
        ec.generate_private_key(ec.SECP256R1()),
        serialization.PrivateFormat.PKCS8,
    )
    yield (
        "dev-rp2350-rom-secp256k1",
        ec.generate_private_key(ec.SECP256K1()),
        serialization.PrivateFormat.TraditionalOpenSSL,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="root under which to create keys/ and boards/<name>/keys/ (default: repository root)",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="replace existing development keys",
    )
    args = parser.parse_args()
    output = args.output_dir.expanduser().resolve()
    relative_paths = [
        directory / f"{stem}{suffix}"
        for stem, directory in KEY_DIRECTORIES.items()
        for suffix in (".pem", ".pub.pem")
    ]
    targets = [output / relative_path for relative_path in relative_paths]
    if not args.force:
        existing = [path for path in targets if path.exists() or path.is_symlink()]
        if existing:
            parser.error(
                f"key file already exists: {existing[0]} (use --force to replace)"
            )

    output.mkdir(parents=True, exist_ok=True)
    # Generate and stage all six files before publishing any of them.
    with tempfile.TemporaryDirectory(prefix=".dev-keys-", dir=output) as staging:
        stage = Path(staging)
        for stem, key, private_format in key_pairs():
            location = stage / KEY_DIRECTORIES[stem]
            location.mkdir(parents=True, exist_ok=True)
            private = location / f"{stem}.pem"
            public = location / f"{stem}.pub.pem"
            private.write_bytes(
                key.private_bytes(
                    serialization.Encoding.PEM,
                    private_format,
                    serialization.NoEncryption(),
                )
            )
            private.chmod(0o600)
            public.write_bytes(
                key.public_key().public_bytes(
                    serialization.Encoding.PEM,
                    serialization.PublicFormat.SubjectPublicKeyInfo,
                )
            )
            public.chmod(0o644)

        for relative_path, target in zip(relative_paths, targets):
            staged = stage / relative_path
            target.parent.mkdir(parents=True, exist_ok=True)
            if args.force:
                os.replace(staged, target)
            else:
                # Hard links on the same filesystem publish without clobbering
                # even if another process creates the target after our precheck.
                os.link(staged, target)
            print(f"generated {target}")

    print("Development keys only: never use these keys on production devices.")


if __name__ == "__main__":
    main()
