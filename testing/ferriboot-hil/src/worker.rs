//! Shared `ferriboot-flash` worker. The HIL parent supervises this process so
//! stalled USB/SWD operations can be killed before the next test.
//!
//! ```text
//! ferriboot-flash esp32c3-supermini PORT FLASH.bin  flash 4 MiB via the ESP ROM loader
//! ferriboot-flash rp2350-pico2 FLASH.bin       restore 4 MiB over SWD, then reset
//! ferriboot-flash rp2350-pico2 --reset         reset only
//! ```

use std::ffi::OsString;
#[cfg(feature = "rp2350-pico2")]
use std::{path::PathBuf, time::Duration};

#[cfg(feature = "rp2350-pico2")]
use crate::{Board, flash_transport};

const USAGE: &str = "usage: ferriboot-flash esp32c3-supermini PORT FLASH.bin | ferriboot-flash rp2350-pico2 <FLASH.bin|--reset>";

/// Run the worker for the board named on the command line.
/// Successful completion returns normally; errors produce a failure exit code.
pub fn main() {
    if let Err(error) = run_cli(std::env::args_os().skip(1)) {
        eprintln!("flash worker failed: {error}");
        std::process::exit(1);
    }
}

fn run_cli(mut args: impl Iterator<Item = OsString>) -> crate::Result<()> {
    let board = args.next().ok_or(USAGE)?;
    match board.to_str() {
        #[cfg(feature = "esp32c3-supermini")]
        Some("esp32c3-supermini") => run_esp32c3(args),
        #[cfg(feature = "rp2350-pico2")]
        Some("rp2350-pico2") => run_rp2350(&crate::rp2350::BOARD, args),
        _ => Err(USAGE.into()),
    }
}

/// Own the USB port until flashing finishes; the parent reopens CDC afterward.
#[cfg(feature = "esp32c3-supermini")]
fn run_esp32c3(mut args: impl Iterator<Item = OsString>) -> crate::Result<()> {
    let port = args.next().ok_or(USAGE)?;
    let image = args.next().ok_or(USAGE)?;
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    crate::esp32c3::flash::write_full_image_in_worker(
        port.to_str().ok_or("PORT must be valid UTF-8")?,
        std::path::Path::new(&image),
    )
}

#[cfg(feature = "rp2350-pico2")]
fn run_rp2350(
    board: &'static Board,
    mut args: impl Iterator<Item = OsString>,
) -> crate::Result<()> {
    let fixture = args.next().ok_or(USAGE)?;
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    if fixture == "--reset" {
        return flash_transport::reset_target(board);
    }
    let desired = std::fs::read(&fixture)?;
    // The checkpoint starts console capture before reset without releasing
    // the SWD session. A hung operation is bounded by the parent timeout.
    match std::env::var_os(board.env("RESET_GATE")) {
        Some(directory) => {
            let directory = PathBuf::from(directory);
            flash_transport::restore_and_reset_with(board, &desired, || {
                await_reset_gate(&directory)
            })
        }
        None => flash_transport::restore_and_reset(board, &desired),
    }
}

/// Let the parent open/clear the serial console before releasing the reset.
#[cfg(feature = "rp2350-pico2")]
fn await_reset_gate(directory: &std::path::Path) -> crate::Result<()> {
    std::fs::write(directory.join("ready"), b"")?;
    let release = directory.join("release");
    loop {
        match std::fs::metadata(&release) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = OsString> {
        list.iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn missing_or_unknown_board_reports_usage() {
        for bad in [
            &[][..],
            &["unknown", "--reset"][..],
            &["qemu", "fixture.bin"][..],
        ] {
            assert_eq!(run_cli(args(bad)).unwrap_err().to_string(), USAGE);
        }
    }

    #[cfg(feature = "esp32c3-supermini")]
    #[test]
    fn esp32c3_requires_port_and_image() {
        for bad in [
            &["esp32c3-supermini"][..],
            &["esp32c3-supermini", "port"][..],
            &["esp32c3-supermini", "port", "image", "extra"][..],
        ] {
            assert_eq!(run_cli(args(bad)).unwrap_err().to_string(), USAGE);
        }
        // Valid arguments must validate fixture size before opening USB.
        let image = tempfile::NamedTempFile::new().unwrap();
        let error = run_cli(args(&[
            "esp32c3-supermini",
            "unused",
            image.path().to_str().unwrap(),
        ]))
        .unwrap_err();
        assert!(
            error.to_string().contains("expected complete 4 MiB"),
            "{error}"
        );
    }

    #[cfg(feature = "rp2350-pico2")]
    #[test]
    fn rp2350_rejects_invalid_arguments() {
        use crate::TEST_BOARD;
        for bad in [
            &["rp2350-pico2"][..],
            &["rp2350-pico2", "--reset", "extra"][..],
        ] {
            assert_eq!(run_cli(args(bad)).unwrap_err().to_string(), USAGE);
        }
        for bad in [&[][..], &["a", "b"][..]] {
            let error = run_rp2350(&TEST_BOARD, args(bad)).unwrap_err();
            assert_eq!(error.to_string(), USAGE);
        }
    }

    #[cfg(feature = "rp2350-pico2")]
    #[test]
    fn rp2350_reset_and_restore_go_through_board_probe() {
        use crate::TEST_BOARD;
        let error = run_rp2350(&TEST_BOARD, args(&["--reset"])).unwrap_err();
        assert_eq!(error.to_string(), "board has no probe");
        // A malformed fixture is rejected before the probe is touched.
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = run_rp2350(&TEST_BOARD, args(&[file.path().to_str().unwrap()])).unwrap_err();
        assert!(error.to_string().contains("4 MiB"), "{error}");
    }
}
