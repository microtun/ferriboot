//! ESP-IDF partition table validation. ESP partitions remain a runtime input
//! shared with ESP-IDF (unlike RP2350, whose layout is compiled in).
use anyhow::{Context, Result, ensure};
use esp_idf_part::{AppType, DataType, PartitionTable, SubType, Type};
use ferriboot_build::tools::{number, repo_root};
use ferriboot_layout_esp32c3 as chip;
use std::{collections::BTreeMap, path::Path};

/// Partition table offset: environment first, then the board Cargo config.
pub fn table_offset() -> Result<u32> {
    const KEY: &str = "ESP_BOOTLOADER_ESP_IDF_CONFIG_PARTITION_TABLE_OFFSET";
    if let Ok(value) = std::env::var(KEY) {
        return number(&value);
    }
    let source = std::fs::read_to_string(repo_root().join("boards/esp32c3/.cargo/config.toml"))?;
    let config: toml::Value = toml::from_str(&source)?;
    number(
        config["env"][KEY]
            .as_str()
            .context("missing ESP partition table offset")?,
    )
}

pub struct EspLayout {
    pub table: PartitionTable,
    pub offset: u32,
}

impl EspLayout {
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)?;
        Self::parse(&source, table_offset()?)
    }
    pub fn parse(source: &str, offset: u32) -> Result<Self> {
        ensure!(
            offset > 0
                && offset.is_multiple_of(chip::ERASE_SIZE)
                && u64::from(offset) + u64::from(chip::PARTITION_TABLE_SIZE)
                    <= u64::from(chip::FLASH_SIZE),
            "invalid partition table offset"
        );
        // Require explicit placement; upstream also accepts implicit offsets.
        for line in source.lines() {
            let line = line.split('#').next().unwrap().trim();
            if line.is_empty() {
                continue;
            }
            let fields: Vec<_> = line.split(',').map(str::trim).collect();
            ensure!(
                fields.len() == 5 && fields.iter().all(|s| !s.is_empty()),
                "expected five explicit partition fields: {line}"
            );
            ensure!(fields[0].len() <= 16, "partition name exceeds 16 bytes");
        }
        let table = PartitionTable::try_from_str(source)?;
        table.validate()?;
        let layout = Self { table, offset };
        let mut occupied = Vec::new();
        for entry in layout.table.partitions() {
            let (start, size) = (entry.offset(), entry.size());
            let end = start.checked_add(size).context("partition overflow")?;
            ensure!(
                size > 0
                    && start.is_multiple_of(chip::ERASE_SIZE)
                    && size.is_multiple_of(chip::ERASE_SIZE),
                "{}: invalid alignment",
                entry.name()
            );
            ensure!(
                start >= offset + chip::PARTITION_TABLE_SIZE && end <= chip::FLASH_SIZE,
                "{}: outside flash or overlaps table",
                entry.name()
            );
            for &(ref name, a, b) in &occupied {
                ensure!(
                    entry.name() != *name && (end <= a || b <= start),
                    "{}: duplicate or overlaps {name}",
                    entry.name()
                );
            }
            occupied.push((entry.name(), start, end));
        }
        let factory = layout
            .table
            .find("ferriboot")
            .context("missing ferriboot partition")?;
        ensure!(
            factory.ty() == Type::App && factory.subtype() == SubType::App(AppType::Factory),
            "ferriboot must be app,factory"
        );
        ensure!(
            factory.offset().is_multiple_of(chip::MMU_PAGE_SIZE)
                && factory.size().is_multiple_of(chip::MMU_PAGE_SIZE),
            "factory partition must be MMU aligned"
        );
        for name in ["image-0", "image-1", "image-scratch", "storage"] {
            let p = layout
                .table
                .find(name)
                .with_context(|| format!("missing {name}"))?;
            ensure!(
                p.ty() == Type::Data && p.subtype() == SubType::Data(DataType::Undefined),
                "{name}: expected data,undefined"
            );
        }
        let primary = layout.region("image-0")?;
        let secondary = layout.region("image-1")?;
        ensure!(
            primary.1 == secondary.1,
            "image slots must have equal sizes"
        );
        ensure!(
            [primary.0, secondary.0]
                .iter()
                .all(|v| (v + chip::HEADER_SIZE).is_multiple_of(chip::MMU_PAGE_SIZE)),
            "slot payloads must be MMU aligned"
        );
        ensure!(
            layout.region("image-scratch")?.1 >= 2 * chip::ERASE_SIZE,
            "scratch must hold two sectors"
        );
        ensure!(layout.region("storage")?.1 >= 32768, "storage needs 32 KiB");
        Ok(layout)
    }
    pub fn region(&self, name: &str) -> Result<(u32, u32)> {
        let p = self
            .table
            .find(name)
            .with_context(|| format!("missing partition {name}"))?;
        Ok((p.offset(), p.size()))
    }
    pub fn binary(&self) -> Result<Vec<u8>> {
        Ok(self.table.to_bin()?)
    }
    /// Chip and board constants plus the regions of this table.
    pub fn constants(&self) -> Result<BTreeMap<String, u32>> {
        let mut values: BTreeMap<String, u32> = BTreeMap::new();
        values.extend(chip::VALUES.iter().map(|&(k, v)| (k.to_owned(), v)));
        values.insert("FLASH_SIZE".into(), chip::FLASH_SIZE);
        values.insert("PARTITION_TABLE_OFFSET".into(), self.offset);
        values.insert("BOOTLOADER_SIZE".into(), self.offset);
        for (name, stem) in [
            ("ferriboot", "FERRIBOOT"),
            ("image-0", "PRIMARY"),
            ("image-1", "SECONDARY"),
            ("image-scratch", "SCRATCH"),
            ("storage", "STORAGE"),
        ] {
            let (offset, size) = self.region(name)?;
            values.insert(format!("{stem}_OFFSET"), offset);
            values.insert(format!("{stem}_SIZE"), size);
        }
        values.insert("SLOT_SIZE".into(), self.region("image-0")?.1);
        Ok(values)
    }
}

/// The board's `boards/esp32c3/layout/partitions.csv`.
pub fn default_layout() -> Result<EspLayout> {
    EspLayout::load(&repo_root().join("boards/esp32c3/layout/partitions.csv"))
}
