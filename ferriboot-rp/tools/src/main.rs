//! `ferriboot-tools-rp2350`: checked RP2350 ELF conversion, signing, ROM
//! sealing, SWD loading and the fixture flash algorithm.
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use ferriboot_build::tools::{
    self as tools,
    cli::{self, Signing},
    mcuboot::{self, SignOptions},
};
use ferriboot_tools_rp2350::{
    DEV_KEY, algorithm, check_signed_app, constants, download, elf, geometry, seal,
};
use rp2350_layout as layout;
use std::{fs, path::PathBuf, process::Command as Process};

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
        key: PathBuf,
        #[arg(long)]
        otp_output: Option<PathBuf>,
    },
    /// Sign, download over SWD, reset.
    LoadApp {
        elf: PathBuf,
        #[arg(long, value_enum, default_value = "primary")]
        slot: Slot,
    },
    /// Seal, download over SWD, reset.
    LoadBootloader {
        elf: PathBuf,
        #[arg(long, env = "RP2350_SECURE_BOOT_KEY")]
        key: PathBuf,
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
            let options = signing.options(geometry);
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
            tools::distinct_output(&output, &[&path, &key])?;
            if let Some(otp) = &otp_output {
                tools::distinct_output(otp, &[&path, &key, &output])?;
            }
            let image = elf::image(&fs::read(path)?, true)?;
            let sealed = seal::seal(&image, &key, otp_output.as_deref())?;
            tools::write_output(&output, sealed)
        }
        Command::LoadApp { elf: path, slot } => {
            let image = elf::image(&fs::read(path)?, false)?;
            let geometry = geometry();
            let mut options = SignOptions::from_env(geometry);
            options.extra = vec!["--pad".into(), "--confirm".into()];
            let signed = mcuboot::sign(&image.payload, geometry, &options)?;
            check_signed_app(&signed)?;
            let dir = tempfile::tempdir()?;
            let bin = dir.path().join("app.bin");
            fs::write(&bin, signed)?;
            let offset = match slot {
                Slot::Primary => layout::PRIMARY_OFFSET,
                Slot::Secondary => layout::SECONDARY_OFFSET,
            };
            download(&bin, layout::XIP_BASE + offset)
        }
        Command::LoadBootloader { elf: path, key } => {
            tools::require_provisioning_ack()?;
            let image = elf::image(&fs::read(path)?, true)?;
            let sealed = seal::seal(&image, &key, None)?;
            let dir = tempfile::tempdir()?;
            let bin = dir.path().join("bootloader.bin");
            fs::write(&bin, sealed)?;
            download(&bin, image.origin)
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
