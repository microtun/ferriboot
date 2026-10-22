"""Plumbing shared by the board host tools (``boards/*/tools/firmware.py``).

Library code in the board tools takes explicit arguments. Environment
variables are read in one place per tool (its ``Settings``), using the
helpers here, so every knob a tool honours is listed together.

Uses only the Python standard library.
"""

import os
from pathlib import Path
import shutil
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[1]

#: MCUboot P-256 key used when ``FERRIBOOT_KEY`` is not set.
DEFAULT_MCUBOOT_KEY = ROOT / "keys/dev-p256.pem"

#: Consent gate for flashing a bootloader whose first boot burns fuses/OTP.
#: Deliberately an environment variable with an exact value, not a flag.
PROVISIONING_ACK_VARIABLE = "FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING"
PROVISIONING_ACK_VALUE = "I_UNDERSTAND"

#: Errors reported as a one-line message instead of a traceback.
CLI_ERRORS = (ValueError, OSError, struct.error, subprocess.CalledProcessError)


def env_flag(name):
    """``NAME=1`` enables a flag; anything else (or unset) leaves it off."""
    return os.environ.get(name, "0") == "1"


def provisioning_acknowledged():
    return os.environ.get(PROVISIONING_ACK_VARIABLE) == PROVISIONING_ACK_VALUE


def acknowledgement_hint():
    return f"set {PROVISIONING_ACK_VARIABLE}={PROVISIONING_ACK_VALUE}"


def required_file(value, what):
    """Resolve ``value`` to an existing, non-empty file or raise ValueError."""
    path = Path(value).expanduser().resolve()
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f"missing {what}: {path}")
    return path


def find_executable(name, *fallbacks, hint=None):
    """Return the path of the first of ``name, *fallbacks`` found on PATH."""
    for candidate in (name, *fallbacks):
        path = shutil.which(candidate)
        if path:
            return path
    names = " or ".join((name, *fallbacks))
    raise ValueError(hint or f"missing executable: {names}")


def run(*command):
    """Run an external command (never through a shell); fail on non-zero exit."""
    subprocess.run([str(arg) for arg in command], check=True)


def write_output(path, data):
    """Write ``data`` to ``path``, creating parent directories."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


def resolved_path(value):
    """argparse ``type`` for file arguments."""
    return Path(value).expanduser().resolve()


def run_cli(parser, action, label):
    """Run ``action()``, reporting expected failures as one line and exit 1."""
    try:
        action()
    except CLI_ERRORS as error:
        parser.exit(1, f"{label}: {error}\n")
