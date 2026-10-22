# Security Policy

## Project status

Ferriboot is an **experimental prototype**. It can be used in a
product only at your own risk, and only after you have read
[THREAT-MODEL.md](THREAT-MODEL.md) and completed every item in its
*Integrator responsibilities* checklist. On-flash formats, features and
security behaviour may still change without notice between commits.

Ferriboot is maintained on a best-effort basis. There are no guaranteed
response times, fix timelines or long-term support commitments.

## Supported versions

Only the latest commit on the default branch is supported. Fixes are not
backported to earlier commits or tags.

| Version | Supported |
| --- | --- |
| Default branch (latest commit) | Yes |
| Anything older | No |

## Reporting a vulnerability

**Please do not report security issues in public issues, pull requests or
discussions.**

Email `owner@microtun.dev` instead. Where possible, include:

- the affected chip and board (`esp32c3`, `rp2350`, `qemu`) and the commit hash;
- the build configuration: Cargo features (for example `production`,
  `downgrade-prevention`, `bootstrap`), the ESP-IDF profile (`dev` or
  `release`), and whether `FERRIBOOT_PUBKEYS` was set;
- which security objective (O1–O7 in the threat model) is broken and which
  attacker (A1–A5) can trigger it;
- reproduction steps, ideally as a failing test in `ferriboot-core/tests/` or a
  scenario on the QEMU board, plus any crafted images or trailers involved.

Sign test images with development or freshly generated keys only. **Never send
private keys** that are, or may become, production keys.

### What to expect

Reports are handled on a best-effort basis, without fixed timelines. You can
expect the maintainer to confirm receipt, assess the report against the threat
model, and coordinate with you on a fix and on when details are made public.
Please allow reasonable time for a fix before disclosing. You will be credited
in the fix or advisory unless you ask not to be.

## Scope

Ferriboot's security claims are defined in [THREAT-MODEL.md](THREAT-MODEL.md).
A vulnerability is anything that breaks one of the objectives O1–O7 under the
attacker model described there.

**In scope**, for example:

- executing an image that is unsigned, signed by an untrusted key, bound to the
  wrong VID/CID, or below the hardware security counter floor;
- accepting security-relevant TLVs (`SEC_CNT`, VID, CID) from outside the
  protected area, or any unlisted unprotected TLV;
- crafted images, segment plans or vector tables that make Ferriboot load
  memory, program the MMU or jump outside the bounds it should enforce;
- crashes, hangs, boot loops or corrupted upgrades caused by crafted image
  bytes or trailers delivered through the update path (attacker A1), or by
  power loss during an upgrade;
- bypasses of the fault-injection hardening at the handoff or a counter burn
  that need fewer or less precisely placed faults than the threat model
  describes;
- RP2350 OTP provisioning or ESP32 eFuse handling that writes, locks or leaves
  open something other than what is documented;
- memory-safety bugs in the platform crates (`ferriboot-esp`, `ferriboot-rp`,
  `ferriboot-qemu`), which use `unsafe`;
- host tools under `ferriboot-tools/`, `ferriboot-*/tools/` or `boards/*/tools/` that produce images which
  verify when they should not, or embed the wrong key;
- statements in the threat model or board READMEs that turn out to be false.

**Not considered vulnerabilities**, unless you show impact beyond what is
already documented:

- the known limitations L1–L11 listed in the threat model;
- anything listed under *Out of scope* in the threat model: confidentiality,
  key management, transport security, runtime protection of the application,
  flash-bus attacks (A4), side channels, and holders of trusted signing keys (A6);
- the development keys being public, or development builds trusting them;
- debug interfaces, ROM download mode or BOOTSEL/PICOBOOT left enabled in
  development profiles;
- the example firmware's unauthenticated test-hook record (see
  [testing/ferriboot-test-hooks/README.md](testing/ferriboot-test-hooks/README.md)),
  which is not meant for production;
- denial of service by an attacker who already runs code on the device (A2) or
  has physical access, such as erasing flash or bricking the board.