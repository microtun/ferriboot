//! Publish the machine to dependent binaries. Machine constants come from
//! `chips/qemu` as a normal dependency; the board passes its flash map as a
//! value.
use ferriboot_build::{publish_chip, require_target, selected_chip};

/// Supported machines, the Cargo feature selecting each and its target triple.
const MACHINES: &[(&str, &str)] = &[("mps2-an385", "thumbv7m-none-eabi")];

fn main() {
    let names: Vec<&str> = MACHINES.iter().map(|(name, _)| *name).collect();
    let machine = selected_chip("ferriboot-qemu", &names);
    let target = MACHINES
        .iter()
        .find(|(name, _)| *name == machine)
        .unwrap()
        .1;
    require_target("ferriboot-qemu", machine, target);
    publish_chip(machine);
}
