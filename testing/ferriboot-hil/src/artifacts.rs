//! Build the bootloader variants and the firmware with cargo. Each variant
//! uses its own target directory under `target/hil-<board>/` so switching
//! features does not rebuild others. The board modules cache the results per
//! test process and convert them to their flash format.
//!
//! Each build runs inside the board crate's directory, so the board
//! directory's `.cargo/config.toml` (target, profiles, `[env]`) and its own
//! (runner, bootloader-only `[env]`) apply exactly as for a manual build.
//!
//! `FERRIBOOT_TEST_CARGO_ARGS` adds arguments to every cargo invocation (for
//! example `--offline`). `FERRIBOOT_TEST_PROFILE` selects `release` (default)
//! or `dev`.
//!
//! Cargo's output goes through `eprintln!` like the console echo in
//! [`dut`](crate::dut), so libtest shows it only for a failing test (or live
//! with `--nocapture`) instead of in the middle of the test list.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use ferriboot_core::UpgradeMode;

use crate::Board;

/// The repository root (this crate lives in `testing/ferriboot-hil`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// `boards/<board>/` at the repository root.
pub fn board_dir(board: &Board) -> PathBuf {
    repo_root().join("boards").join(board.name)
}

/// `target/hil-<board>/` at the repository root.
pub fn target_root(board: &Board) -> PathBuf {
    repo_root().join(format!("target/hil-{}", board.name))
}

/// Bootloader build configurations under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// Scratch swap, downgrade prevention, bootstrap (RP2350 uses dev OTP policy).
    SwapScratch,
    Overwrite,
    DirectXip,
    DirectXipRevert,
}

impl Variant {
    pub const COUNT: usize = 4;

    pub fn name(self) -> &'static str {
        match self {
            Variant::SwapScratch => "swap-scratch",
            Variant::Overwrite => "overwrite-only",
            Variant::DirectXip => "direct-xip",
            Variant::DirectXipRevert => "direct-xip-revert",
        }
    }

    pub fn features(self, _board_name: &str) -> String {
        // RP2350's development OTP policy is selected by absence of the
        // `production` feature, not an explicit feature. All boards share
        // the same non-production policy features for these HIL variants.
        let base = "downgrade-prevention,bootstrap";
        match self {
            Variant::SwapScratch => base.into(),
            other => format!("{base},{}", other.name()),
        }
    }

    pub fn mode(self) -> UpgradeMode {
        match self {
            Variant::SwapScratch => UpgradeMode::SwapScratch,
            Variant::Overwrite => UpgradeMode::Overwrite,
            Variant::DirectXip => UpgradeMode::DirectXip { revert: false },
            Variant::DirectXipRevert => UpgradeMode::DirectXip { revert: true },
        }
    }
}

pub struct Bootloader {
    pub elf: Vec<u8>,
    /// ROM-loadable image for flash offset 0x0.
    pub image: Vec<u8>,
}

/// ELF of the board's Ferriboot (or QEMU bootloader) built with `variant`'s features.
pub fn build_bootloader(board: &Board, variant: Variant) -> Vec<u8> {
    let binary = "ferriboot";
    let path = cargo_build(
        board,
        &board_dir(board).join(binary),
        &target_root(board).join(variant.name()),
        &variant.features(board.name),
        &format!("{}-ferriboot", board.name),
    );
    std::fs::read(path).unwrap()
}

/// ELF of the board's example firmware with built-in test hooks.
pub fn build_firmware(board: &Board) -> Vec<u8> {
    let path = cargo_build(
        board,
        &board_dir(board).join("firmware"),
        &target_root(board).join("firmware"),
        "",
        &format!("{}-firmware", board.name),
    );
    std::fs::read(path).unwrap()
}

fn profile() -> String {
    std::env::var("FERRIBOOT_TEST_PROFILE").unwrap_or_else(|_| "release".into())
}

fn cargo_build(
    board: &Board,
    crate_dir: &Path,
    target_dir: &Path,
    features: &str,
    bin: &str,
) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(crate_dir)
        .args(["build", "--profile", &profile()])
        .arg("--no-default-features")
        .env("CARGO_TARGET_DIR", target_dir)
        // The host test build's settings must not leak into the target build.
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        // Signing fixtures always use the development key, regardless of shell settings.
        .env(
            "FERRIBOOT_PUBKEYS",
            repo_root().join("keys/dev-p256.pub.pem"),
        );
    if !features.is_empty() {
        cmd.args(["--features", features]);
    }
    if let Ok(extra) = std::env::var("FERRIBOOT_TEST_CARGO_ARGS") {
        cmd.args(extra.split_whitespace());
    }
    eprintln!("[build] {} (features: {features:?})", crate_dir.display());
    let status = run_captured(cmd, "cargo")
        .unwrap_or_else(|e| panic!("cannot run cargo in {}: {e}", crate_dir.display()));
    assert!(status.success(), "building {} failed", crate_dir.display());
    let dir = if profile() == "dev" {
        "debug".into()
    } else {
        profile()
    };
    target_dir.join(board.target).join(dir).join(bin)
}

/// Run `cmd` with its stdout and stderr echoed line by line through
/// `eprintln!`, which libtest captures. A child with inherited stdio writes
/// straight to the terminal, past the capture.
/// Used by both Cargo builds and the ESP32-C3 signing script.
pub(crate) fn run_captured(mut cmd: Command, label: &str) -> std::io::Result<ExitStatus> {
    let (reader, writer) = std::io::pipe()?;
    cmd.stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    let mut child = cmd.spawn()?;
    // `cmd` still holds the write ends; the reader sees EOF only once the
    // child exits and these copies are closed too.
    drop(cmd);
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line)? > 0 {
        eprintln!("[{label}] {}", String::from_utf8_lossy(&line).trim_end());
        line.clear();
    }
    child.wait()
}

#[cfg(test)]
mod tests {
    use super::Variant;

    #[test]
    fn rp2350_hil_uses_dev_otp_policy_without_production_feature() {
        for variant in [
            Variant::SwapScratch,
            Variant::Overwrite,
            Variant::DirectXip,
            Variant::DirectXipRevert,
        ] {
            let features = variant.features("rp2350-pico2");
            assert!(!features.split(',').any(|feature| feature == "production"));
            assert!(features.split(',').any(|feature| feature == "bootstrap"));
            assert!(
                features
                    .split(',')
                    .any(|feature| feature == "downgrade-prevention")
            );
            if variant != Variant::SwapScratch {
                assert!(features.split(',').any(|feature| feature == variant.name()));
            }
            for other in ["qemu", "esp32c3-supermini"] {
                assert!(!variant.features(other).contains("production"));
            }
        }
    }
}
