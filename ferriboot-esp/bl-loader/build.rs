//! Configure the RAM handoff executable from the FERRIBOOT_BL_LOADER_*
//! variables set by ferriboot-esp's build script. This independently linked
//! executable shares its ABI source file with the platform crate, but it does
//! not depend on that crate or need a separate ABI package.
use std::{env, fs, path::PathBuf};

const VARS: [&str; 5] = [
    "FERRIBOOT_BL_LOADER_CHIP",
    "FERRIBOOT_BL_LOADER_IRAM",
    "FERRIBOOT_BL_LOADER_SIZE",
    "FERRIBOOT_BL_LOADER_BOOT_INFO_ADDRESS",
    "FERRIBOOT_BL_LOADER_BOOT_INFO_SIZE",
];

fn number(name: &str, text: &str) -> u32 {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.unwrap_or_else(|_| panic!("{name}: invalid integer {text:?}"))
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for var in VARS {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=ld");
    let values: Vec<Option<String>> = VARS.iter().map(|var| env::var(var).ok()).collect();
    let [
        Some(chip),
        Some(iram),
        Some(size),
        Some(info),
        Some(info_size),
    ] = <[Option<String>; 5]>::try_from(values).unwrap()
    else {
        fs::write(
            out_dir.join("layout.rs"),
            "compile_error!(\"the bl-loader executable is built by ferriboot-esp's build script \
             (feature `boot-runtime`), which sets FERRIBOOT_BL_LOADER_*\");\n",
        )
        .unwrap();
        return;
    };
    let iram = number(VARS[1], &iram);
    let size = number(VARS[2], &size);
    let info = number(VARS[3], &info);
    let info_size = number(VARS[4], &info_size);

    // The script carries the chip's mask ROM entry points, so one per chip.
    let template = PathBuf::from("ld").join(format!("{chip}.x.in"));
    let source = fs::read_to_string(&template)
        .unwrap_or_else(|error| panic!("bl-loader has no linker script for {chip}: {error}"));
    let output = out_dir.join("loader.x");
    fs::write(
        &output,
        source
            .replace("@LOADER_IRAM@", &format!("{iram:#010x}"))
            .replace("@LOADER_SIZE@", &format!("{size:#x}")),
    )
    .unwrap();
    // The boot-info reservation the payload publishes into, from the same
    // chip description as the bootloader and application linker scripts.
    fs::write(
        out_dir.join("layout.rs"),
        format!(
            "pub const BOOT_INFO_ADDRESS: u32 = {info:#010x};\npub const BOOT_INFO_SIZE: u32 = {info_size:#x};\n",
        ),
    )
    .unwrap();
    println!("cargo:rustc-check-cfg=cfg(chip, values(\"esp32c3\"))");
    println!("cargo:rustc-cfg=chip=\"{chip}\"");
    println!("cargo:rustc-link-arg-bin=bl-loader=-T{}", output.display());
    println!("cargo:rustc-link-arg-bin=bl-loader=--nmagic");
}
