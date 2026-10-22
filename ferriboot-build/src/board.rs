//! Board descriptions for the build scripts of board layout crates.
//!
//! A board's `board.json` names the chip it is built on and adds the values
//! the board decides. The chip description belongs to the platform crate
//! (for example `ferriboot-esp/chips/esp32c3.json`); `board.json` refers to
//! it by a path relative to itself:
//!
//! ```json
//! { "chip": "../../../ferriboot-esp/chips/esp32c3.json", "FLASH_SIZE": "0x400000" }
//! ```

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use crate::values::{Values, check_keys, parse_u32, read_object, read_values};

/// A board: the chip description it builds on, plus its own values.
#[derive(Clone, Debug)]
pub struct Board {
    /// Chip name, the chip description's file stem (for example `esp32c3`).
    pub chip: String,
    /// Canonical path of the chip description.
    pub chip_json: PathBuf,
    /// Chip facts, verbatim from the platform crate's chip description.
    pub chip_values: Values,
    /// Values the board decides, exactly the keys the layout crate expects.
    pub board_values: Values,
}

/// Read `board.json` and the chip description it names. `board_keys` lists
/// the board's own keys; the chip description must not repeat any of them.
///
/// Emits `cargo:rerun-if-changed` for both files.
pub fn read_board(path: &Path, board_keys: &[&str]) -> Board {
    let mut object = read_object(path);
    let chip = object
        .remove("chip")
        .unwrap_or_else(|| panic!("{}: missing key chip", path.display()));
    let chip = chip.as_str().unwrap_or_else(|| {
        panic!(
            "{}: chip must be the path of the platform crate's chip description",
            path.display()
        )
    });
    let base = path.parent().unwrap_or(Path::new("."));
    let chip_json = base
        .join(chip)
        .canonicalize()
        .unwrap_or_else(|error| panic!("{}: chip description {chip}: {error}", path.display()));
    check_keys(path, object.keys().map(String::as_str), board_keys);
    let board_values: Values = object
        .iter()
        .map(|(key, value)| (key.clone(), parse_u32(path, key, value)))
        .collect();
    let chip_values = read_values(&chip_json);
    for key in board_values.keys() {
        assert!(
            !chip_values.contains_key(key),
            "{}: {key} is a chip fact defined by {}",
            path.display(),
            chip_json.display()
        );
    }
    let chip = chip_json
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("chip description has a UTF-8 file name")
        .to_owned();
    Board {
        chip,
        chip_json,
        chip_values,
        board_values,
    }
}

impl Board {
    /// Chip and board values together.
    pub fn values(&self) -> Values {
        let mut values = self.chip_values.clone();
        values.extend(self.board_values.clone());
        values
    }

    /// One chip or board value.
    pub fn get(&self, key: &str) -> u32 {
        *self
            .board_values
            .get(key)
            .or_else(|| self.chip_values.get(key))
            .unwrap_or_else(|| panic!("board or chip value {key} missing"))
    }

    /// Rust source declaring `CHIP`, `CHIP_JSON` and a `pub const NAME: u32`
    /// for every chip and board value.
    pub fn rust_consts(&self) -> String {
        self.rust_consts_with_types(&[])
    }

    /// [`Board::rust_consts`], declaring the values named in `types` with
    /// another integer type (for example `("BOOT_INFO_SIZE", "usize")`).
    pub fn rust_consts_with_types(&self, types: &[(&str, &str)]) -> String {
        let ty = |name: &str| {
            types
                .iter()
                .find(|(key, _)| *key == name)
                .map_or("u32", |(_, ty)| ty)
        };
        let mut rust = String::new();
        writeln!(
            rust,
            "/// The chip this board is built on; binaries check it against the platform crate.\n\
             pub const CHIP: &str = {:?};\n\
             /// Canonical path of the chip description, for the same check.\n\
             pub const CHIP_JSON: &str = {:?};",
            self.chip,
            self.chip_json.display().to_string()
        )
        .unwrap();
        rust.push_str("\n// Chip facts, from the platform crate's chip description.\n");
        for (name, value) in &self.chip_values {
            writeln!(rust, "pub const {name}: {} = {value:#010x};", ty(name)).unwrap();
        }
        rust.push_str("\n// Board values, from board.json.\n");
        for (name, value) in &self.board_values {
            writeln!(rust, "pub const {name}: {} = {value:#010x};", ty(name)).unwrap();
        }
        rust
    }
}
