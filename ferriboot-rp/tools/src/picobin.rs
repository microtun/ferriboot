//! Bounded PICOBIN parsing. Follow the two-block loop; never scan for keys.
use anyhow::{Context, Result, ensure};
use ferriboot_build::tools::{bytes, word};

pub const ROM_HEADER_SIZE: usize = 512;
pub const LINKER_LAST_OFFSET: usize = 44;
const LINKER_LAST: u32 = 0xaff;
const ARM_SECURE: u32 = 0x10210142;
const ARM_SECURE_SIGNED: u32 = ARM_SECURE | (0x0800 << 16);
const LINKER_HEADERS: [u32; 4] = [ARM_SECURE, 0x203, 0x344, 0x01000406];

#[derive(Debug)]
pub struct Item {
    pub offset: usize,
    pub header: u32,
}
impl Item {
    pub fn kind(&self) -> u8 {
        self.header as u8
    }
    pub fn words(&self) -> usize {
        ((self.header >> 8)
            & if self.kind() & 0x80 != 0 {
                0xffff
            } else {
                0xff
            }) as usize
    }
}
#[derive(Debug)]
pub struct Block {
    pub items: Vec<Item>,
    pub last: Item,
    pub link: i32,
}

pub fn parse_block(data: &[u8], offset: usize) -> Result<Block> {
    ensure!(offset.is_multiple_of(4), "unaligned PICOBIN block");
    ensure!(
        word(data, offset)? == 0xffffded3,
        "missing PICOBIN block header"
    );
    let end = offset
        .checked_add(0x180)
        .context("block overflow")?
        .min(data.len());
    let mut at = offset + 4;
    let mut items = Vec::new();
    while at + 12 <= end {
        let item = Item {
            offset: at,
            header: word(data, at)?,
        };
        if item.kind() == 0xff {
            ensure!(
                (item.header >> 8) & 0xffff == ((at - offset - 4) / 4) as u32,
                "PICOBIN LAST length mismatch"
            );
            ensure!(word(data, at + 8)? == 0xab123579, "missing PICOBIN footer");
            return Ok(Block {
                items,
                last: item,
                link: word(data, at + 4)? as i32,
            });
        }
        ensure!(
            item.words() > 0 && at + 4 * item.words() + 12 <= end,
            "invalid PICOBIN item size"
        );
        at += 4 * item.words();
        items.push(item);
    }
    anyhow::bail!("unterminated PICOBIN block")
}

pub struct ImageDef {
    pub vector: u32,
    pub entry: u32,
    pub stack: u32,
    pub source: u32,
    pub destination: u32,
    pub size: u32,
    pub link: i32,
}

pub fn linker_image_def(data: &[u8], origin: u32) -> Result<ImageDef> {
    let block = parse_block(data, 0)?;
    ensure!(
        block.items.iter().map(|i| i.header).eq(LINKER_HEADERS) && block.last.header == LINKER_LAST,
        "ROM metadata must be exactly Arm Secure IMAGE_TYPE, VECTOR_TABLE, ENTRY_POINT and one relative LOAD_MAP"
    );
    let load = block.items[3].offset;
    let source = i64::from(origin) + load as i64 + i64::from(word(data, load + 4)? as i32);
    Ok(ImageDef {
        vector: word(data, 12)?,
        entry: word(data, 20)?,
        stack: word(data, 24)?,
        source: source.try_into().context("load source overflow")?,
        destination: word(data, load + 8)?,
        size: word(data, load + 12)?,
        link: block.link,
    })
}

fn security(data: &[u8], offset: usize, signed: bool) -> Result<(Block, Option<&[u8]>)> {
    let block = parse_block(data, offset)?;
    let mut key = None;
    let (mut arm, mut sha) = (false, false);
    for item in &block.items {
        match item.kind() {
            0x42 => {
                ensure!(
                    !arm && item.header
                        == if signed {
                            ARM_SECURE_SIGNED
                        } else {
                            ARM_SECURE
                        },
                    "wrong/duplicate PICOBIN image type"
                );
                arm = true;
            }
            0x47 => {
                ensure!(
                    !sha && item.words() == 2 && item.header >> 24 == 1,
                    "wrong PICOBIN hash definition"
                );
                sha = true;
            }
            0x09 => {
                ensure!(
                    key.is_none() && item.words() == 33 && item.header >> 24 == 1,
                    "invalid PICOBIN signature item"
                );
                let raw = bytes(data, item.offset + 4, 64)?;
                ensure!(raw.iter().any(|b| *b != 0), "empty PICOBIN key");
                key = Some(raw);
            }
            _ => {}
        }
    }
    ensure!(arm && (!signed || sha), "missing Arm Secure type or SHA256");
    Ok((block, key))
}

pub fn signing_public_key(data: &[u8]) -> Result<&[u8]> {
    ensure!(data.len() >= ROM_HEADER_SIZE, "truncated ROM metadata");
    let first = linker_image_def(data, 0)?;
    ensure!(
        first.link >= ROM_HEADER_SIZE as i32,
        "first block must link to sealed metadata"
    );
    let offset = first.link as usize;
    let (second, key) = security(data, offset, true)?;
    ensure!(
        second.link == -first.link,
        "sealed block must link back to first block"
    );
    key.context("sealed block has no signing key")
}
