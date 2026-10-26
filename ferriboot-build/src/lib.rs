//! Build-script helpers shared by the Ferriboot platform crates and board
//! binaries, plus optional host image tooling. Nothing here runs on a target.
//! Build scripts use a `[build-dependencies]` entry with no features; board
//! tools and HIL enable `tools`, and CLI binaries also enable `cli`.
//!
//! * [`emit_public_keys`] embeds the trusted ECDSA P-256 keys as
//!   `$OUT_DIR/keys.rs`.
//! * [`selected_chip`] and [`publish_chip`] (platform crates) and
//!   [`check_platform_chip`] / [`platform_chip`] (binaries) make sure a binary
//!   links the platform crate built for its board's chip.
//! * [`configure_esp_application`] and [`configure_rp2350_application`] pass
//!   an application's linker arguments. The chip-only linker contract is
//!   exported by `ferriboot-esp` / `ferriboot-rp`. RP2350 MEMORY regions are
//!   generated here, in the application's own build script, from
//!   `chips/rp235x` and the board's maximum image size; ESP applications take
//!   theirs from `esp-hal` and need no board input.

#![forbid(unsafe_code)]

#[cfg(feature = "tools")]
pub mod tools;

mod platform;

pub use platform::{
    PlatformChip, check_platform_chip, platform_chip, publish_chip, require_target, selected_chip,
};

use std::{env, fs, path::PathBuf};

/// Cargo's `OUT_DIR` for the running build script.
pub fn out_dir() -> PathBuf {
    PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"))
}

/// Write `contents` to `$OUT_DIR/name` and return the path.
pub fn write_out(name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
    let path = out_dir().join(name);
    fs::write(&path, contents).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    path
}

/// Read the ECDSA P-256 public keys listed (comma separated) in
/// `FERRIBOOT_PUBKEYS`, or `default` when it is unset, and generate
/// `$OUT_DIR/keys.rs` with `pub const PUBLIC_KEYS: [[u8; 65]; N]`.
///
/// Accepted formats are the output of `imgtool getpub -k <key> -e pem` and
/// `imgtool getpub -k <key> -e raw`. Relative paths are relative to the
/// package being built (Cargo runs build scripts there).
pub fn emit_public_keys(default: &str) {
    const SPKI_PREFIX: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];

    println!("cargo:rerun-if-env-changed=FERRIBOOT_PUBKEYS");
    let spec = env::var("FERRIBOOT_PUBKEYS").unwrap_or_else(|_| default.to_string());

    let mut keys = Vec::new();
    for path in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        println!("cargo:rerun-if-changed={path}");
        let data = fs::read(path).unwrap_or_else(|e| {
            panic!("cannot read public key {path}: {e} (set FERRIBOOT_PUBKEYS)")
        });
        let der = if data.starts_with(b"-----BEGIN") {
            let pem = pem::parse(&data).unwrap_or_else(|e| panic!("{path}: invalid PEM: {e}"));
            if pem.tag() != "PUBLIC KEY" {
                panic!(
                    "{path}: expected a PUBLIC KEY, found {}; export it with `imgtool getpub -k <key> -e pem`",
                    pem.tag()
                );
            }
            pem.contents().to_vec()
        } else {
            data
        };
        let raw: [u8; 65] = match der.len() {
            65 if der[0] == 0x04 => der.try_into().unwrap(),
            91 if der.starts_with(&SPKI_PREFIX) && der[26] == 0x04 => der[26..].try_into().unwrap(),
            n => panic!("{path}: not an ECDSA P-256 public key ({n} bytes)"),
        };
        keys.push(raw);
    }
    if keys.is_empty() {
        panic!("FERRIBOOT_PUBKEYS lists no keys");
    }

    let mut out = format!("pub const PUBLIC_KEYS: [[u8; 65]; {}] = [\n", keys.len());
    for k in &keys {
        out += &format!("    {k:?},\n");
    }
    out += "];\n";
    write_out("keys.rs", out);
}

/// Configure a Ferriboot ESP *application* (or the third-stage bootloader)
/// using `esp-hal`: link `esp-hal`'s `linkall.x`, then the `ferriboot.x`
/// contract exported by `ferriboot-esp` (boot-info reservation). The order
/// matters: the contract checks memory regions defined by `linkall.x`.
///
/// No board input is needed: ESP slots come from the partition table at
/// runtime, and the chip is fixed by `ferriboot-esp`'s chip feature, which
/// also selects `esp-hal`'s (a conflicting `esp-hal` chip fails to build).
///
/// Link arguments only apply to the package that emits them, so this MUST run
/// in the final application's `build.rs`. Requires `ferriboot-esp` as a
/// direct dependency (checked through its `links` metadata) and
/// `ferriboot-build` as a build dependency.
///
/// ```ignore
/// // application build.rs
/// fn main() {
///     ferriboot_build::configure_esp_application();
///     ferriboot_build::emit_public_keys("path/to/key.pub.pem");
/// }
/// ```
pub fn configure_esp_application() {
    platform_chip("ferriboot-esp");
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    println!("cargo:rustc-link-arg=-Tferriboot.x");
}

/// Configure a Ferriboot RP2350/RP2354 *application* using `cortex-m-rt`:
/// write the `memory.x` that `cortex-m-rt`'s `link.x` includes and link
/// `link.x`.
///
/// `memory.x` holds the MEMORY regions: the application's execution window
/// (`chips/rp235x`), limited to the board's `max_image_size` (the slot size
/// less the MCUboot trailer), and SRAM below the boot-info reservation. It then
/// includes `ferriboot-rp.x`, the chip's application contract exported by
/// `ferriboot-rp`.
///
/// Link arguments only apply to the package that emits them, so this MUST run
/// in the final application's `build.rs`. Requires `ferriboot-rp` as a direct
/// dependency, `cortex-m-rt`, and the `embassy-rp/imagedef-none` feature. The
/// application must not provide its own `memory.x`.
///
/// ```ignore
/// // application build.rs
/// fn main() {
///     ferriboot_build::configure_rp2350_application(
///         ferriboot_layout_rp2350_pico2::MAX_IMAGE_SIZE,
///     );
///     ferriboot_build::emit_public_keys("path/to/key.pub.pem");
/// }
/// ```
pub fn configure_rp2350_application(max_image_size: u32) {
    use ferriboot_chip_rp235x::{
        APP_XIP_BASE, APP_XIP_WINDOW_SIZE, BOOT_INFO_ADDRESS, BOOT_INFO_SIZE, CHIP, HEADER_SIZE,
        RAM_BASE,
    };

    check_platform_chip("ferriboot-rp", CHIP);
    assert!(
        max_image_size > HEADER_SIZE && max_image_size <= APP_XIP_WINDOW_SIZE,
        "max_image_size {max_image_size:#x} must exceed the header and fit the XIP window"
    );
    let memory = format!(
        "/* @generated by ferriboot_build::configure_rp2350_application. */
MEMORY
{{
    FLASH     : ORIGIN = {:#010x}, LENGTH = {:#x}
    RAM       : ORIGIN = {RAM_BASE:#010x}, LENGTH = {:#x}
    BOOT_INFO : ORIGIN = {BOOT_INFO_ADDRESS:#010x}, LENGTH = {BOOT_INFO_SIZE:#x}
}}

INCLUDE ferriboot-rp.x
",
        APP_XIP_BASE + HEADER_SIZE,
        max_image_size - HEADER_SIZE,
        BOOT_INFO_ADDRESS - RAM_BASE,
    );
    write_out("memory.x", memory);
    println!("cargo:rustc-link-search={}", out_dir().display());
    println!("cargo:rustc-link-arg=-Tlink.x");
}
