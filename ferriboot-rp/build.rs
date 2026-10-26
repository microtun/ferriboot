//! Export the application linker contract and publish the chip to dependent
//! binaries. Chip constants come from `chips/rp235x` as a normal dependency;
//! boards pass their flash map as a value.
use ferriboot_build::{out_dir, publish_chip, require_target, selected_chip, write_out};

/// Package features. Every variant is the same RP2350 die (`chips/rp235x`);
/// the feature only selects embassy-rp's pin count.
const FEATURES: &[&str] = &["rp235xa", "rp235xb"];
const CHIP: &str = "rp2350";
const TARGET: &str = "thumbv8m.main-none-eabihf";

fn main() {
    selected_chip("ferriboot-rp", FEATURES);
    require_target("ferriboot-rp", CHIP, TARGET);
    publish_chip(CHIP);

    // Included by the memory.x of every application, after its regions.
    write_out("ferriboot-rp.x", include_str!("ld/ferriboot-rp.x"));
    println!("cargo:rustc-link-search={}", out_dir().display());
    println!("cargo:rerun-if-changed=ld/ferriboot-rp.x");
}
