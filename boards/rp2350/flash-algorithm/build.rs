use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=ld/flash-algorithm.x");
    let script =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("ld/flash-algorithm.x");
    println!("cargo:rustc-link-arg-bin=flash=-T{}", script.display());
    println!("cargo:rustc-link-arg-bin=flash=--nmagic");
    for entry in ["Init", "UnInit", "ProgramPage", "EraseSector"] {
        println!("cargo:rustc-link-arg-bin=flash=--undefined={entry}");
    }
}
