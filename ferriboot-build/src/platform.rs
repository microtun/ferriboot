use std::env;

/// The single chip feature enabled on the package being built, out of
/// `chips`. Panics unless exactly one is enabled.
pub fn selected_chip<'a>(package: &str, chips: &[&'a str]) -> &'a str {
    let enabled: Vec<&str> = chips
        .iter()
        .copied()
        .filter(|chip| {
            let feature = format!("CARGO_FEATURE_{}", chip.to_uppercase().replace('-', "_"));
            env::var_os(feature).is_some()
        })
        .collect();
    match enabled.as_slice() {
        [chip] => chip,
        [] => panic!(
            "{package}: enable exactly one chip feature ({})",
            chips.join(", ")
        ),
        _ => panic!(
            "{package}: chip features {} are mutually exclusive",
            enabled.join(" and ")
        ),
    }
}

/// Require the target triple `expected` for `package` built for `chip`.
pub fn require_target(package: &str, chip: &str, expected: &str) {
    let target = env::var("TARGET").expect("Cargo must set TARGET");
    assert_eq!(
        target, expected,
        "{package} for {chip} must be built for {expected}; build it from the board workspace"
    );
}

/// Publish the selected chip through the platform crate's `links` metadata,
/// for the build scripts of the binaries that depend on it directly.
pub fn publish_chip(chip: &str) {
    println!("cargo::metadata=CHIP={chip}");
}

/// The chip a platform crate was built for, from its `links` metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformChip {
    pub name: String,
}

/// In the build script of a binary: read the chip that the platform crate
/// with `links = "<links>"` was built for and require that it is the chip
/// the board's layout crate was generated from (its `CHIP`).
///
/// The binary must depend on the platform crate directly, otherwise Cargo
/// does not pass the metadata.
pub fn check_platform_chip(links: &str, chip: &str) -> PlatformChip {
    let prefix = format!("DEP_{}", links.to_uppercase().replace('-', "_"));
    let read = |key: &str| {
        let name = format!("{prefix}_{key}");
        env::var(&name).unwrap_or_else(|_| {
            panic!("{name} is not set: depend on {links} directly, with a chip feature")
        })
    };
    let platform = PlatformChip { name: read("CHIP") };
    assert_eq!(
        platform.name, chip,
        "{links} is built for chip {}, but the board layout is for {chip}: \
         enable the `{chip}` feature of {links}",
        platform.name
    );
    platform
}
