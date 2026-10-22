use std::{collections::BTreeMap, env, fmt::Write as _, fs, path::PathBuf};

// Only some chip and board constants are needed here.
#[allow(dead_code)]
#[path = "src/board.rs"]
mod board;

const PARTITIONS: &[&str] = &[
    "bootloader",
    "image-0",
    "image-1",
    "image-scratch",
    "storage",
];

// picotool allows a byte count or a decimal size with a K/k suffix.
fn partition_number(value: &serde_json::Value) -> u32 {
    if let Some(n) = value.as_u64() {
        return n.try_into().expect("partition size exceeds u32::MAX");
    }
    let text = value
        .as_str()
        .expect("partition size must be an unsigned integer or K string");
    let digits = match text.as_bytes().last() {
        Some(&b'k') | Some(&b'K') => &text[..text.len() - 1],
        _ => panic!("invalid picotool partition size {text:?}"),
    };
    assert!(
        !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()),
        "invalid picotool partition size {text:?}"
    );
    digits
        .parse::<u32>()
        .expect("partition size too large")
        .checked_mul(1024)
        .expect("partition size too large")
}

fn partitions() -> BTreeMap<String, (u32, u32)> {
    let source = fs::read_to_string("partitions.json").expect("read partitions.json");
    let document: serde_json::Value =
        serde_json::from_str(&source).expect("parse picotool partitions.json");
    let rows = document["partitions"]
        .as_array()
        .expect("missing partitions array");
    let mut regions: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let erase = board::ERASE_SIZE;
    let flash = board::FLASH_SIZE;
    assert!(erase > 0);
    for row in rows {
        let name = row["name"].as_str().expect("partition missing name");
        assert!(PARTITIONS.contains(&name), "unexpected partition {name}");
        let start = partition_number(&row["start"]);
        let size = partition_number(&row["size"]);
        assert!(
            size > 0 && start.is_multiple_of(erase) && size.is_multiple_of(erase),
            "{name}: expected nonempty erase-aligned partition"
        );
        let end = start.checked_add(size).expect("partition address overflow");
        assert!(end <= flash, "{name}: extends past flash");
        for (other, &(offset, length)) in &regions {
            assert!(
                end <= offset || offset + length <= start,
                "{name}: overlaps {other}"
            );
        }
        let expected_family = if name == "bootloader" {
            "rp2350-arm-s"
        } else {
            "data"
        };
        assert!(
            row["families"] == serde_json::json!([expected_family]),
            "{name}: unexpected family"
        );
        assert_eq!(
            row.get("ignored_during_riscv_boot")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "{name}: must be ignored for RISC-V boot"
        );
        if name == "bootloader" {
            assert!(
                row.get("ignored_during_arm_boot").is_none()
                    || row["ignored_during_arm_boot"].as_bool() == Some(false),
                "bootloader cannot be ignored for Arm boot"
            );
        } else {
            assert_eq!(
                row.get("ignored_during_arm_boot")
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "{name}: data partitions must be ignored for Arm boot"
            );
        }
        assert!(
            row.get("link").is_none(),
            "{name}: native A/B links are unsupported"
        );
        assert!(
            regions.insert(name.to_owned(), (start, size)).is_none(),
            "duplicate {name}"
        );
    }
    for name in PARTITIONS {
        assert!(regions.contains_key(*name), "missing partition {name}");
    }
    regions
}

fn main() {
    assert_eq!(board::CHIP, "rp2350");
    println!("cargo:rerun-if-changed=partitions.json");
    println!("cargo:rerun-if-changed=src/board.rs");
    let regions = partitions();
    let bootloader = regions["bootloader"];
    let primary = regions["image-0"];
    let secondary = regions["image-1"];
    let scratch = regions["image-scratch"];
    let storage = regions["storage"];
    assert_eq!(
        bootloader.0, 0,
        "bootloader must begin at flash offset zero"
    );
    assert_eq!(primary.1, secondary.1, "image slots must have equal sizes");
    let erase = board::ERASE_SIZE;
    assert!(scratch.1 >= 2 * erase, "scratch must hold two sectors");
    assert!(
        storage.1 >= 32 * 1024,
        "storage must reserve at least 32 KiB"
    );
    let sectors = primary.1 / erase;
    let trailer = sectors * 3 * board::WRITE_SIZE + 48;
    let max_image = (primary
        .1
        .checked_sub(trailer)
        .expect("slot too small for trailer")
        / erase)
        * erase;
    assert!(
        max_image > board::HEADER_SIZE,
        "slot too small for MCUboot image"
    );

    let mut rust = String::from("// Generated partition geometry.\n");
    for (name, value) in [
        ("BOOTLOADER_SIZE", bootloader.1),
        ("PRIMARY_OFFSET", primary.0),
        ("SECONDARY_OFFSET", secondary.0),
        ("SLOT_SIZE", primary.1),
        ("SCRATCH_OFFSET", scratch.0),
        ("SCRATCH_SIZE", scratch.1),
        ("STORAGE_OFFSET", storage.0),
        ("STORAGE_SIZE", storage.1),
        ("MAX_IMAGE_SIZE", max_image),
    ] {
        writeln!(rust, "pub const {name}: u32 = {value:#010x};").unwrap();
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    fs::write(out.join("layout.rs"), rust).expect("write generated layout.rs");
}
