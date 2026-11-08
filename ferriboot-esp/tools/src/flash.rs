//! ESP ROM flashing through espflash.
use anyhow::{Result, ensure};
use ferriboot_build::tools::progress;
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
        // Forward espflash's byte-level progress directly; an activity
        // indicator must not conceal the tool's own measured progress.
        // Each region gets its own real progress bar from espflash.
        eprintln!(
            "Writing ESP flash region {}/{} at {address:#x} ({} bytes)",
            index + 1,
            regions.len(),
            fs::metadata(path)?.len()
        );
        run_write_command(&mut cmd, monitor)?;
        // The exit status, not a fabricated full bar, confirms this region.
        eprintln!("ESP flash region {}/{}: done", index + 1, regions.len());
    }
    Ok(())
}

/// Keep the flashing subprocess attached to the terminal so espflash can
/// display its real bytes-written progress instead of a wrapper animation.
/// Interactive monitoring must also retain stdin after the write completes.
fn run_write_command(cmd: &mut Command, monitor: bool) -> Result<()> {
    if monitor {
        cmd.stdin(Stdio::inherit());
    }
    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    ensure!(status.success(), "espflash write-bin failed ({status})");
    Ok(())
}

/// Erase exactly one application slot before writing a short signed image.
/// `write-bin` only erases sectors covered by its input, so it cannot clear
/// stale MCUboot status/trailer bytes at the far end of a slot on its own.
pub fn erase_region(address: u32, size: u32) -> Result<()> {
    ensure!(
        size != 0
            && address.is_multiple_of(4096)
            && size.is_multiple_of(4096)
            && address.checked_add(size).is_some(),
        "invalid ESP flash erase region"
    );
    let mut cmd = Command::new(ferriboot_build::tools::env("ESPFLASH", "espflash"));
    // espflash's erase-region uses the flash stub; write-bin below retains its
    // existing --no-stub behavior. Never use erase-flash (whole chip) here.
    cmd.args(["erase-region", "--chip", "esp32c3"]);
    if let Some(port) = std::env::var("ESPPORT").ok().filter(|s| !s.is_empty()) {
        cmd.args(["--port", &port]);
    }
    progress::run(
        cmd.arg(format!("{address:#x}")).arg(format!("{size:#x}")),
        format!(
            "Erasing ESP flash {address:#x}..{:#x} ({size} bytes)",
            address + size
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erase_rejects_unaligned_and_overflowing_ranges_before_opening_port() {
        assert!(erase_region(0x1001, 0x2000).is_err());
        assert!(erase_region(0x1000, 0x1fff).is_err());
        assert!(erase_region(0x1000, 0).is_err());
        assert!(erase_region(0xffff_f000, 0x2000).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn live_write_command_propagates_espflash_failure() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'writing 50%%\n' >&2; exit 7"]);
        assert!(run_write_command(&mut cmd, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn live_write_command_accepts_successful_espflash() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'writing 100%%\n' >&2"]);
        assert!(run_write_command(&mut cmd, false).is_ok());
    }
}
