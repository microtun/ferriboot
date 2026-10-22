# Shared host tools

Python scripts used across boards. Run them from the repository root with
Python 3.11+. Board-specific tools live in `boards/<board>/tools/`.

| Script | Purpose |
|---|---|
| `generate-dev-keys.py` | Create the development signing keys ([details](../keys/README.md)). Never overwrites without `--force`. |
| `generate-test-images.py` | Regenerate the signed fixtures in `ferriboot-core/tests/data/` with a fixed, public test key |
| `mcuboot.py` | MCUboot P-256 signing and signed-image checks, shared by the board tools and mirrored by `ferriboot-hil` |
| `board.py` | Load `layout/board.json` and its chip description, as the Rust layout crates do |
| `hosttools.py` | Common plumbing for the board tools: environment flags, the provisioning acknowledgement, process and file helpers |

```sh
python3 -m pip install cryptography imgtool
python3 tools/generate-dev-keys.py
python3 tools/generate-test-images.py
```

`mcuboot.py` can also be run directly:

```sh
python3 tools/mcuboot.py check-key keys/dev-p256.pem
python3 tools/mcuboot.py sign PAYLOAD OUTPUT --key keys/dev-p256.pem \
  --header-size 0x200 --slot-size 0x1E0000 --board rp2350 [-- IMGTOOL_ARGS]
```

`PAYLOAD` is a raw binary; the board tools handle ELF conversion. `--board` applies the default image identity (`--vid microtun.dev` and the
board's `--cid`). The `SECURITY_COUNTER`, `VID` and `CID` environment variables
override the defaults, and imgtool arguments after `--` override both.

`board.py` and `hosttools.py` use only the standard library; the rest need
`cryptography`, and signing needs `imgtool`.
