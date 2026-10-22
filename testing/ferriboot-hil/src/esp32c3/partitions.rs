//! Host-side snapshot of the exact partition table programmed into fixtures.
//! No partition offsets or sizes are compiled into the harness.

use std::{process::Command, sync::OnceLock};

use esp_idf_part::PartitionTable;
use ferriboot_core::{FlashBounds, Region, SlotLayout, UpgradeMode};

use super::layout;
use crate::{
    Error, Result,
    artifacts::repo_root,
    flash::{FLASH_SIZE, Partition},
};

pub(super) struct Partitions {
    pub bytes: Vec<u8>,
    pub offset: u32,
    pub slots: SlotLayout,
    pub factory: Region,
    pub storage: Region,
}

impl Partitions {
    fn from_bytes(bytes: Vec<u8>, offset: u32) -> Result<Self> {
        let table = PartitionTable::try_from_bytes(bytes.clone()).map_err(Error::external)?;
        table.validate().map_err(Error::external)?;
        let reserved = offset
            .checked_add(layout::PARTITION_TABLE_SIZE)
            .filter(|end| *end as usize <= FLASH_SIZE)
            .ok_or("partition table extends past flash")?;
        if offset == 0 || !offset.is_multiple_of(layout::ERASE_SIZE) {
            return Err("invalid partition table offset".into());
        }
        for entry in table.partitions() {
            let region = Region::new(entry.offset(), entry.size());
            region
                .check::<Partition>(FLASH_SIZE)
                .map_err(Error::external)?;
            if region.offset < reserved {
                return Err("partition overlaps bootloader or partition table".into());
            }
        }
        let find = |name: &str| -> Result<Region> {
            let entry = table
                .find(name)
                .ok_or_else(|| format!("missing partition {name}"))?;
            Ok(Region::new(entry.offset(), entry.size()))
        };
        let slots = SlotLayout {
            primary: find("image-0")?,
            secondary: find("image-1")?,
            scratch: Some(find("image-scratch")?),
        };
        slots
            .validate::<Partition>(
                FlashBounds {
                    capacity: FLASH_SIZE,
                    reserved,
                },
                UpgradeMode::SwapScratch,
            )
            .map_err(Error::external)?;
        let factory = find("ferriboot")?;
        let storage = find("storage")?;
        Ok(Self {
            bytes,
            offset,
            slots,
            factory,
            storage,
        })
    }
}

/// Cache one table snapshot for signing, composition and flashing. This must
/// not refer to BOARD: its signing configuration is initialized from this table.
pub(super) fn read() -> Result<&'static Partitions> {
    static TABLE: OnceLock<Result<Partitions>> = OnceLock::new();
    TABLE.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Partitions> {
    let tool = repo_root().join("boards/esp32c3/tools/layout.py");
    let dir = tempfile::tempdir()?;
    let bin = dir.path().join("partitions.bin");
    let out = Command::new("python3")
        .arg(&tool)
        .arg("--table")
        .arg(&bin)
        .output()?;
    if !out.status.success() {
        return Err(format!("layout.py: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    let out = Command::new("python3")
        .arg(&tool)
        .args(["--constant", "PARTITION_TABLE_OFFSET"])
        .output()?;
    if !out.status.success() {
        return Err(format!("layout.py: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    let value = std::str::from_utf8(&out.stdout)
        .map_err(Error::external)?
        .trim();
    let offset = u32::from_str_radix(value.strip_prefix("0x").ok_or("invalid table offset")?, 16)?;
    Partitions::from_bytes(std::fs::read(bin)?, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary(csv: &str) -> Vec<u8> {
        PartitionTable::try_from_str(csv).unwrap().to_bin().unwrap()
    }

    #[test]
    fn follows_relocated_and_resized_partitions_in_binary_table() {
        // Deliberately unlike the board's default CSV. Neither factory nor
        // storage placement, slot sizes or slot starts may be assumed.
        let bytes = binary(
            "ferriboot,app,factory,0x30000,0x30000\n\
            image-0,data,undefined,0x7f000,0x100000\n\
            image-1,data,undefined,0x18f000,0x100000\n\
            image-scratch,data,undefined,0x290000,0x8000\n\
            storage,data,undefined,0x2a0000,0x10000\n",
        );
        let table = Partitions::from_bytes(bytes.clone(), 0x10000).unwrap();
        assert_eq!(table.bytes, bytes);
        assert_eq!(table.factory, Region::new(0x30000, 0x30000));
        assert_eq!(table.slots.primary, Region::new(0x7f000, 0x100000));
        assert_eq!(table.slots.secondary, Region::new(0x18f000, 0x100000));
        assert_eq!(table.slots.scratch, Some(Region::new(0x290000, 0x8000)));
        assert_eq!(table.storage, Region::new(0x2a0000, 0x10000));
    }

    #[test]
    fn signing_and_composition_share_the_programmed_table() {
        let table = read().unwrap();
        assert_eq!(
            super::super::BOARD.signing.slot_size,
            table.slots.primary.size
        );
        let chain = super::super::artifacts::Bootloader {
            image: vec![0xa5; 64],
            idf_image: vec![0x5a; 64],
        };
        let flash = super::super::flash::compose(&chain).unwrap();
        let bytes = flash.bytes();
        let offset = table.offset as usize;
        assert_eq!(
            &bytes[offset..offset + table.bytes.len()],
            table.bytes.as_slice()
        );
        let factory = table.factory.offset as usize;
        assert_eq!(
            &bytes[factory..factory + chain.image.len()],
            chain.image.as_slice()
        );
        assert_eq!(flash.layout, table.slots);
        assert_eq!(flash.storage, Some(table.storage));
    }

    #[test]
    fn rejects_missing_slots_and_invalid_geometry() {
        let factory = "ferriboot,app,factory,0x20000,0x40000\n";
        assert!(Partitions::from_bytes(binary(factory), 0x10000).is_err());
        let unequal = format!(
            "{factory}\
            image-0,data,undefined,0x6f000,0x100000\n\
            image-1,data,undefined,0x17f000,0x110000\n\
            image-scratch,data,undefined,0x290000,0x8000\n\
            storage,data,undefined,0x2a0000,0x10000\n"
        );
        assert!(Partitions::from_bytes(binary(&unequal), 0x10000).is_err());
        assert!(Partitions::from_bytes(binary(factory), 0x20000).is_err());
        assert!(Partitions::from_bytes(vec![0; 32], 0x10000).is_err());
    }
}
