//! ESP ROM flashing through espflash.
use anyhow::{Result, ensure};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

pub fn write_flash(regions: &[(u32, &Path)], monitor: bool) -> Result<()> {
    ensure!(!regions.is_empty(), "no flash regions");
    ensure!(
        !monitor || regions.len() == 1,
        "monitor requires a single flash region"
    );
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
        let mut cmd = Command::new(ferriboot_build::tools::env("ESPFLASH", "espflash"));
        cmd.args(["write-bin", "--chip", "esp32c3", "--no-stub"]);
        if let Some(port) = &port {
            cmd.args(["--port", port]);
        }
        if index != 0 {
            cmd.args(["--before", "no-reset"]);
        }
        if monitor {
            cmd.arg("--monitor");
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
        if monitor {
            // Keep the same espflash session for monitoring, with live terminal I/O.
            // tools::run() captures output until the command exits, so it cannot
            // be used for an interactive monitor.
            let status = cmd
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()?;
            ensure!(status.success(), "espflash write-bin failed ({status})");
        } else {
            ferriboot_build::tools::run(&mut cmd)?;
        }
    }
    Ok(())
}
