//! `ferriboot-tools-rp2350`: checked RP2350 ELF conversion, signing, ROM
//! sealing, SWD loading and the fixture flash algorithm.
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use ferriboot_build::tools::{
    self as tools,
    cli::{self, Signing},
    mcuboot::{self, SignOptions},
    signer::RemoteSigner,
};
use ferriboot_tools_rp2350::{
    DEV_KEY, algorithm, check_signed_app, constants, download, elf, geometry, seal,
};
use rp2350_layout as layout;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
};

#[derive(Parser)]
#[command(about = "Ferriboot RP2350 image, ROM sealing and SWD tools", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Generate the secp256k1 ROM development key pair (HIL only).
    Keys(cli::Keys),
    /// Validate ELF, vectors, SRAM reservation and ROM load map.
    Check {
        elf: PathBuf,
        #[arg(long)]
        bootloader: bool,
    },
    /// Export checked physical load bytes.
    ExportBin {
        elf: PathBuf,
        output: PathBuf,
        #[arg(long)]
        bootloader: bool,
    },
    /// MCUboot-sign an application.
    Sign {
        elf: PathBuf,
        output: PathBuf,
        #[command(flatten)]
        signing: Signing,
    },
    /// MCUboot-sign a raw payload with this board's geometry.
    SignRaw(cli::SignRaw),
    /// ROM-seal the bootloader and independently check metadata/key.
    SignBootloader {
        elf: PathBuf,
        output: PathBuf,
        #[arg(long, env = "RP2350_SECURE_BOOT_KEY")]
        key: Option<PathBuf>,
        #[arg(long)]
        otp_output: Option<PathBuf>,
    },
    /// Sign and download over SWD, or save the signed image with NO_FLASH=1.
    LoadApp {
        elf: PathBuf,
        #[arg(long, value_enum, default_value = "primary")]
        slot: Slot,
    },
    /// Seal and download over SWD, or save the image with NO_FLASH=1.
    LoadBootloader {
        elf: PathBuf,
        #[arg(long, env = "RP2350_SECURE_BOOT_KEY")]
        key: Option<PathBuf>,
    },
    /// Build/package the fixture flash algorithm as probe-rs YAML.
    FlashAlgorithm {
        #[arg(long)]
        elf: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, env = "CARGO", default_value = "cargo")]
        cargo: String,
    },
    /// Print the compiled board layout (edit layout/partitions.json and rebuild).
    Layout {
        #[arg(long)]
        constant: Option<String>,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum Slot {
    Primary,
    Secondary,
}

fn main() {
    cli::exit_on_error(env!("CARGO_BIN_NAME"), run(Cli::parse().command));
}
fn run(command: Command) -> Result<()> {
    match command {
        Command::Keys(args) => args.run(&[DEV_KEY]),
        Command::Check {
            elf: path,
            bootloader,
        } => {
            let image = elf::image(&fs::read(path)?, bootloader)?;
            println!(
                "checked origin {:#x}, {:#x} payload bytes",
                image.origin,
                image.payload.len()
            );
            Ok(())
        }
        Command::ExportBin {
            elf: path,
            output,
            bootloader,
        } => {
            tools::distinct_output(&output, &[&path])?;
            tools::write_output(&output, elf::image(&fs::read(path)?, bootloader)?.payload)
        }
        Command::Sign {
            elf: path,
            output,
            signing,
        } => {
            let geometry = geometry();
            let options = signing.options(geometry)?;
            tools::distinct_output(&output, &[&path, &options.key])?;
            let image = elf::image(&fs::read(path)?, false)?;
            let signed = mcuboot::sign(&image.payload, geometry, &options)?;
            check_signed_app(&signed)?;
            tools::write_output(&output, signed)
        }
        Command::SignRaw(args) => args.run(geometry()),
        Command::SignBootloader {
            elf: path,
            output,
            key,
            otp_output,
        } => {
            let key = key.unwrap_or_else(|| DEV_KEY.private_path(&tools::repo_root()));
            let remote = RemoteSigner::from_env()?;
            tools::distinct_output(&output, &[&path, &key])?;
            if let Some(otp) = &otp_output {
                tools::distinct_output(otp, &[&path, &key, &output])?;
            }
            let image = elf::image(&fs::read(path)?, true)?;
            let sealed = if let Some(remote) = remote {
                seal::seal_remote(&image, &remote, otp_output.as_deref())?
            } else {
                seal::seal(&image, &key, otp_output.as_deref())?
            };
            tools::write_output(&output, sealed)
        }
        Command::LoadApp { elf: path, slot } => {
            let image = elf::image(&fs::read(&path)?, false)?;
            let geometry = geometry();
            let mut options = SignOptions::from_env(geometry)?;
            options.extra = vec!["--pad".into(), "--confirm".into()];
            let signed = mcuboot::sign(&image.payload, geometry, &options)?;
            check_signed_app(&signed)?;
            let offset = match slot {
                Slot::Primary => layout::PRIMARY_OFFSET,
                Slot::Secondary => layout::SECONDARY_OFFSET,
            };
            write_or_load(
                &path,
                &signed,
                layout::XIP_BASE + offset,
                tools::flag("NO_FLASH"),
                &[&path, &options.key],
            )
        }
        Command::LoadBootloader { elf: path, key } => {
            let no_flash = tools::flag("NO_FLASH");
            if !no_flash {
                tools::require_provisioning_ack()?;
            }
            let image = elf::image(&fs::read(&path)?, true)?;
            let key = key.unwrap_or_else(|| DEV_KEY.private_path(&tools::repo_root()));
            let sealed = if let Some(remote) = RemoteSigner::from_env()? {
                seal::seal_remote(&image, &remote, None)?
            } else {
                seal::seal(&image, &key, None)?
            };
            write_or_load(&path, &sealed, image.origin, no_flash, &[&path, &key])
        }
        Command::FlashAlgorithm {
            elf: path,
            output,
            cargo,
        } => flash_algorithm(path, output, cargo),
        Command::Layout { constant } => cli::print_constants(&constants(), constant.as_deref()),
    }
}

fn flash_algorithm(elf: Option<PathBuf>, output: Option<PathBuf>, cargo: String) -> Result<()> {
    let dir = tools::repo_root().join("boards/rp2350/flash-algorithm");
    let elf = if let Some(elf) = elf {
        elf
    } else {
        // Use an explicit target and no inherited board linker flags.
        tools::run(
            Process::new(cargo)
                .current_dir(&dir)
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .args([
                    "build",
                    "--release",
                    "--bin",
                    "flash",
                    "--target",
                    algorithm::TARGET,
                ]),
        )?;
        dir.join("target")
            .join(algorithm::TARGET)
            .join("release/flash")
    };
    let output = output.unwrap_or_else(|| dir.join("target.yaml"));
    tools::distinct_output(&output, &[&elf])?;
    tools::write_output(&output, algorithm::generate(&fs::read(elf)?)?)
}

/// `NO_FLASH=1` retains the signed image alongside its ELF instead of touching
/// the device. Ordinary runners still use a temporary file for SWD flashing.
fn write_or_load(
    elf: &Path,
    signed: &[u8],
    address: u32,
    no_flash: bool,
    inputs: &[&Path],
) -> Result<()> {
    if no_flash {
        let output = elf.with_extension("signed.bin");
        tools::distinct_output(&output, inputs)?;
        tools::write_output(&output, signed)?;
        println!("NO_FLASH=1: wrote {}", output.display());
        Ok(())
    } else {
        let dir = tempfile::tempdir()?;
        let bin = dir.path().join("image.bin");
        fs::write(&bin, signed)?;
        download(&bin, address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flash_saves_signed_image_without_loading() {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("rp2350-ferriboot");
        fs::write(&elf, b"ELF").unwrap();

        // No probe-rs executable or connected RP2350 is needed for this path.
        write_or_load(&elf, b"sealed", 0x1000_0000, true, &[&elf]).unwrap();
        assert_eq!(fs::read(elf.with_extension("signed.bin")).unwrap(), b"sealed");
        assert_eq!(fs::read(&elf).unwrap(), b"ELF");
    }

    #[test]
    fn no_flash_does_not_overwrite_signing_key() {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("rp2350-ferriboot");
        let key = elf.with_extension("signed.bin");
        fs::write(&key, b"private key").unwrap();

        assert!(write_or_load(&elf, b"sealed", 0x1000_0000, true, &[&elf, &key]).is_err());
        assert_eq!(fs::read(key).unwrap(), b"private key");
    }
}
