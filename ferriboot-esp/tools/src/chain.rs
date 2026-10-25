//! ESP-IDF second stage, SBv2 boot-chain signing and provisioning flash.
use crate::{flash::write_flash, image::convert, layout::EspLayout};
use anyhow::{Context, Result, ensure};
use ferriboot_build::tools::{number, repo_root, signer::RemoteSigner};
use ferriboot_layout_esp32c3_supermini as chip;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct Settings {
    pub key: PathBuf,
    pub profile: String,
    pub idf_bootloader: Option<PathBuf>,
    pub prebuilt_image: Option<PathBuf>,
    pub no_flash: bool,
}
impl Settings {
    pub fn from_env() -> Self {
        Self {
            key: std::env::var_os("ESP_SBV2_KEY")
                .map(PathBuf::from)
                .unwrap_or_else(|| crate::DEV_KEY.private_path(&repo_root())),
            profile: ferriboot_build::tools::env("ESP_BOOTLOADER_PROFILE", "dev"),
            idf_bootloader: std::env::var_os("ESP_IDF_BOOTLOADER_BIN").map(PathBuf::from),
            prebuilt_image: std::env::var_os("FERRIBOOT_ESP_IMAGE").map(PathBuf::from),
            no_flash: ferriboot_build::tools::flag("NO_FLASH"),
        }
    }
}
pub fn build_idf(settings: &Settings) -> Result<Vec<u8>> {
    ensure!(
        ["dev", "release"].contains(&settings.profile.as_str()),
        "ESP_BOOTLOADER_PROFILE must be dev or release"
    );
    let board = repo_root().join("boards/esp32c3-supermini");
    let config = fs::read_to_string(board.join(format!(
        "bootloader/sdkconfig.defaults.{}",
        settings.profile
    )))?;
    let offset = config
        .lines()
        .find_map(|s| s.strip_prefix("CONFIG_PARTITION_TABLE_OFFSET="))
        .context("missing IDF table offset")?;
    ensure!(
        number(offset)? == crate::layout::table_offset()?,
        "IDF table offset disagrees with Cargo config"
    );
    let raw = if let Some(path) = &settings.idf_bootloader {
        fs::read(path)?
    } else {
        let dir = tempfile::tempdir()?;
        ferriboot_build::tools::run(
            Command::new("docker")
                .arg("build")
                .arg("--file")
                .arg(board.join("bootloader/Dockerfile.boot"))
                .arg("--build-arg")
                .arg(format!("ESP_BOOTLOADER_PROFILE={}", settings.profile))
                .arg("--output")
                .arg(format!("type=local,dest={}", dir.path().display()))
                .arg(&board),
        )?;
        fs::read(dir.path().join("bootloader.bin"))?
    };
    ensure!(
        raw.first() == Some(&0xe9) && raw.len() <= 0xb000,
        "expected unsigned IDF ESP image <= 44 KiB"
    );
    Ok(raw)
}
pub struct BootChain {
    pub stage2: Vec<u8>,
    pub ferriboot: Vec<u8>,
    pub layout: EspLayout,
}
pub fn boot_chain(elf: &Path, settings: &Settings) -> Result<BootChain> {
    crate::elf::check_bootloader(&fs::read(elf)?)?;
    let layout = crate::layout::default_layout()?;
    let raw = if let Some(path) = &settings.prebuilt_image {
        ensure!(settings.no_flash, "FERRIBOOT_ESP_IMAGE requires NO_FLASH=1");
        fs::read(path)?
    } else {
        convert(elf, true)?
    };
    ensure!(
        !raw.is_empty() && raw.len().is_multiple_of(chip::MMU_PAGE_SIZE as usize),
        "Ferriboot must be secure-padded to 64 KiB"
    );
    let idf = build_idf(settings)?;
    // The same RSA key signs the ROM's IDF stage and the Ferriboot stage.
    // If remote signing was explicitly configured, failure is fatal: never
    // silently downgrade to a local development private key.
    let (ferriboot, stage2) = if let Some(remote) = RemoteSigner::from_env()? {
        (
            crate::sbv2::sign_remote(&raw, &remote)?,
            crate::sbv2::sign_remote(&idf, &remote)?,
        )
    } else {
        let key = fs::read_to_string(&settings.key)?;
        (
            crate::sbv2::sign(&raw, &key)?,
            crate::sbv2::sign(&idf, &key)?,
        )
    };
    crate::sbv2::verify_same_key(&ferriboot, &stage2)?;
    ensure!(
        stage2.len() <= 0xc000 && stage2.len() <= layout.offset as usize,
        "signed IDF exceeds loader/table limit"
    );
    ensure!(
        ferriboot.len() <= layout.region("ferriboot")?.1 as usize,
        "signed Ferriboot exceeds factory partition"
    );
    Ok(BootChain {
        stage2,
        ferriboot,
        layout,
    })
}
/// Output files written by [`BootChain::write`].
pub const OUTPUTS: [&str; 4] = [
    "ferriboot.sbv2.bin",
    "bootloader.idf.sbv2.bin",
    "partitions.bin",
    MERGED,
];
/// Single image for flash 0x0 up to the end of the factory partition.
pub const MERGED: &str = "boot-chain.sbv2.bin";
impl BootChain {
    pub fn write(&self, dir: &Path) -> Result<()> {
        ferriboot_build::tools::write_output(&dir.join("ferriboot.sbv2.bin"), &self.ferriboot)?;
        ferriboot_build::tools::write_output(&dir.join("bootloader.idf.sbv2.bin"), &self.stage2)?;
        ferriboot_build::tools::write_output(&dir.join("partitions.bin"), self.layout.binary()?)?;
        ferriboot_build::tools::write_output(&dir.join(MERGED), self.merged()?)
    }
    /// The IDF second stage, partition table and factory Ferriboot as one
    /// raw image for flash `0x0..end of the ferriboot partition`. Unused
    /// bytes are erased (0xFF), including any partition that lies between
    /// the table and Ferriboot (nvs, phy_init on this board).
    pub fn merged(&self) -> Result<Vec<u8>> {
        let (factory, size) = self.layout.region("ferriboot")?;
        let end = factory
            .checked_add(size)
            .context("factory partition overflow")?;
        let table = self.layout.binary()?;
        crate::merge::merge(
            end,
            &[
                (0, &self.stage2),
                (self.layout.offset, &table),
                (factory, &self.ferriboot),
            ],
        )
    }
    pub fn flash(&self, dir: &Path) -> Result<()> {
        ferriboot_build::tools::require_provisioning_ack()?;
        let factory = dir.join("ferriboot.sbv2.bin");
        let table = dir.join("partitions.bin");
        let stage2 = dir.join("bootloader.idf.sbv2.bin");
        write_flash(&[
            (self.layout.region("ferriboot")?.0, &factory),
            (self.layout.offset, &table),
            (0, &stage2),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_image_spans_stage2_table_and_factory_partition() {
        let layout = crate::layout::default_layout().unwrap();
        let table = layout.binary().unwrap();
        let (factory, size) = layout.region("ferriboot").unwrap();
        let chain = BootChain {
            stage2: vec![0xe9; 0x9000],
            ferriboot: vec![0x5a; 0x30000],
            layout,
        };
        let image = chain.merged().unwrap();
        assert_eq!(image.len(), (factory + size) as usize);
        assert_eq!(image.len(), 0x60000, "board layout: 0x0..0x60000");
        let at = |offset: u32, len: usize| &image[offset as usize..][..len];
        assert_eq!(at(0, chain.stage2.len()), &chain.stage2[..]);
        assert_eq!(at(chain.layout.offset, table.len()), &table[..]);
        assert_eq!(at(factory, chain.ferriboot.len()), &chain.ferriboot[..]);
        // Everything else is erased flash.
        let used = [
            (0, chain.stage2.len()),
            (chain.layout.offset as usize, table.len()),
            (factory as usize, chain.ferriboot.len()),
        ];
        for (i, &b) in image.iter().enumerate() {
            if !used.iter().any(|&(s, l)| (s..s + l).contains(&i)) {
                assert_eq!(b, crate::merge::ERASED, "byte {i:#x}");
            }
        }
    }
}
