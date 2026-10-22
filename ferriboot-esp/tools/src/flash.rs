//! ESP ROM flashing through espflash.
use anyhow::{Result, ensure};
use std::{fs, path::Path, process::Command};

pub fn write_flash(regions: &[(u32, &Path)]) -> Result<()> {
    ensure!(!regions.is_empty(), "no flash regions");
    let port = std::env::var("ESPPORT").ok().filter(|s| !s.is_empty());
    ensure!(
        regions.len() == 1 || port.is_some(),
        "ESPPORT required for multi-region writes"
    );
    // Validate all regions before opening the device.
    for (address, path) in regions {
        ensure!(
            address.is_multiple_of(4096) && fs::metadata(path)?.len() > 0,
            "invalid flash address or empty file"
        );
    }
    for (index, (address, path)) in regions.iter().enumerate() {
        let mut cmd = Command::new(ferriboot_tools::env("ESPFLASH", "espflash"));
        cmd.args(["write-bin", "--chip", "esp32c3", "--no-stub"]);
        if let Some(port) = &port {
            cmd.args(["--port", port]);
        }
        if index != 0 {
            cmd.args(["--before", "no-reset"]);
        }
        cmd.args([
            "--after",
            if index + 1 == regions.len() {
                "hard-reset"
            } else {
                "no-reset"
            },
        ])
        .arg(format!("{address:#x}"))
        .arg(path);
        ferriboot_tools::run(&mut cmd)?;
    }
    Ok(())
}
