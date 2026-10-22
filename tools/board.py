#!/usr/bin/env python3
"""Load a board description the way the Rust board layout crates do.

A board's ``layout/board.json`` holds the values the board decides and names
the chip description it builds on, which belongs to the chip's platform crate
(for example ``ferriboot-esp/chips/esp32c3.json``), by a path relative to
``board.json``. Both are flat JSON objects of unsigned integers or
``0x``-prefixed hexadecimal strings. See ``ferriboot_build::read_board``.

Uses only the Python standard library.
"""

import json
from pathlib import Path


def _number(name, value):
    if type(value) is int and value >= 0:
        return value
    if isinstance(value, str) and value.startswith("0x"):
        return int(value, 16)
    raise ValueError(
        f"{name}: expected an integer or a 0x-prefixed string, got {value!r}"
    )


def _values(path, document):
    return {key: _number(f"{path}: {key}", value) for key, value in document.items()}


def chip_path(layout_dir):
    """The resolved path of the chip description the board builds on."""
    board_json = Path(layout_dir) / "board.json"
    document = json.loads(board_json.read_text())
    return (board_json.parent / document["chip"]).resolve()


def chip_name(layout_dir):
    """The chip's name, the chip description's file stem (``esp32c3``)."""
    return chip_path(layout_dir).stem


def load(layout_dir):
    """Return the chip facts and board values of ``layout_dir/board.json``
    as one dictionary of integers."""
    board_json = Path(layout_dir) / "board.json"
    document = json.loads(board_json.read_text())
    chip_json = (board_json.parent / document.pop("chip")).resolve()
    chip = _values(chip_json, json.loads(chip_json.read_text()))
    board = _values(board_json, document)
    shared = sorted(chip.keys() & board.keys())
    if shared:
        raise ValueError(
            f"{board_json}: {', '.join(shared)} are chip facts defined by {chip_json}"
        )
    return {**chip, **board}
