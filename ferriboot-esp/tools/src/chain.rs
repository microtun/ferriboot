//! ESP-IDF second stage, SBv2 boot-chain signing and provisioning flash.
use crate::{flash::write_flash, image::convert, layout::EspLayout};
use anyhow::{Context, Result, ensure};
use esp32c3_layout as chip;
use ferriboot_build::tools::{number, repo_root, signer::RemoteSigner};
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
    let board = repo_root().join("boards/esp32c3");
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
impl BootChain {
    pub fn write(&self, dir: &Path) -> Result<()> {
        ferriboot_build::tools::write_output(&dir.join("ferriboot.sbv2.bin"), &self.ferriboot)?;
        ferriboot_build::tools::write_output(&dir.join("bootloader.idf.sbv2.bin"), &self.stage2)?;
        ferriboot_build::tools::write_output(&dir.join("partitions.bin"), self.layout.binary()?)
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
