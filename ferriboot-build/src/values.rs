//! Flat JSON descriptions of unsigned 32-bit hardware and layout values.

use std::{collections::BTreeMap, fs, path::Path};

/// Named values, in key order.
pub type Values = BTreeMap<String, u32>;

/// Read a flat JSON object whose values are unsigned integers or
/// `0x`-prefixed hexadecimal strings, each at most `u32::MAX`.
///
/// Emits `cargo:rerun-if-changed` for `path`.
pub fn read_values(path: &Path) -> Values {
    let object = read_object(path);
    object
        .iter()
        .map(|(key, value)| (key.clone(), parse_u32(path, key, value)))
        .collect()
}

/// [`read_values`], requiring exactly the keys in `keys`.
pub fn read_values_exact(path: &Path, keys: &[&str]) -> Values {
    let values = read_values(path);
    check_keys(path, values.keys().map(String::as_str), keys);
    values
}

/// Parse `path` as a JSON object and register it with Cargo.
pub(crate) fn read_object(path: &Path) -> BTreeMap<String, serde_json::Value> {
    println!("cargo:rerun-if-changed={}", path.display());
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{}: expected a flat JSON object: {error}", path.display()))
}

/// Require that `found` is exactly the set `expected`.
pub(crate) fn check_keys<'a>(path: &Path, found: impl Iterator<Item = &'a str>, expected: &[&str]) {
    let found: Vec<&str> = found.collect();
    for key in &found {
        assert!(
            expected.contains(key),
            "{}: unknown key {key}",
            path.display()
        );
    }
    for key in expected {
        assert!(found.contains(key), "{}: missing key {key}", path.display());
    }
}

pub(crate) fn parse_u32(path: &Path, name: &str, value: &serde_json::Value) -> u32 {
    let number = match value {
        serde_json::Value::String(text) => {
            let hex = text.strip_prefix("0x").unwrap_or_else(|| {
                panic!(
                    "{}: {name}: expected a 0x-prefixed hexadecimal string",
                    path.display()
                )
            });
            u64::from_str_radix(hex, 16).unwrap_or_else(|_| {
                panic!("{}: {name}: invalid hexadecimal integer", path.display())
            })
        }
        serde_json::Value::Number(number) => number.as_u64().unwrap_or_else(|| {
            panic!(
                "{}: {name}: expected a non-negative integer",
                path.display()
            )
        }),
        _ => panic!(
            "{}: {name}: expected an integer or a 0x-prefixed string",
            path.display()
        ),
    };
    number
        .try_into()
        .unwrap_or_else(|_| panic!("{}: {name}: exceeds u32::MAX", path.display()))
}
