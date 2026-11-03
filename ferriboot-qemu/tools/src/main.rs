//! `ferriboot-tools-qemu`: checked AN385 images, MCUboot signing, persistent
//! virtual SPI NOR flashing and patched QEMU execution.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use ferriboot_build::tools::{
    self as tools,
    cli::{self, Signing},
    mcuboot::{self, SignOptions},
    progress::Progress,
};
use ferriboot_layout_qemu as board;
use ferriboot_tools_qemu::{constants, elf, flash, geometry};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
};

#[derive(Parser)]
#[command(
    about = "Ferriboot QEMU (MPS2 AN385) image, virtual flash and emulator tools",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Validate ELF load addresses and vectors without signing.
    Check {
        elf: PathBuf,
        #[arg(long)]
        bootloader: bool,
    },
    /// Export checked AN385 physical load bytes.
    ExportBin {
        elf: PathBuf,
        output: PathBuf,
        #[arg(long)]
        bootloader: bool,
    },
    /// Convert and MCUboot-sign an application ELF.
    Sign {
        elf: PathBuf,
        output: PathBuf,
        #[command(flatten)]
        signing: Signing,
    },
    /// MCUboot-sign a raw payload with this board's geometry.
    SignRaw(cli::SignRaw),
    /// Erase an entire virtual app slot, then sign and program only image bytes.
    FlashApp {
        elf: PathBuf,
        #[arg(long, value_enum, default_value = "primary")]
        slot: Slot,
        /// The 4 MiB persistent QEMU SPI NOR backing file.
        #[arg(long, env = "FERRIBOOT_QEMU_FLASH")]
        flash: Option<PathBuf>,
    },
    /// Export and flash the checked bootloader to the virtual NOR's first partition.
    FlashBootloader {
        elf: PathBuf,
        #[arg(long, env = "FERRIBOOT_QEMU_FLASH")]
        flash: Option<PathBuf>,
    },
    /// Boot the persistent virtual NOR with patched qemu-system-arm.
    Run {
        #[arg(long, env = "FERRIBOOT_QEMU_FLASH")]
        flash: Option<PathBuf>,
    },
    /// Print the compiled board layout.
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

fn flash_path(path: Option<PathBuf>) -> PathBuf {
    path.unwrap_or_else(flash::default_path)
}

fn run(command: Command) -> Result<()> {
    match command {
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
            let options = signing.options(geometry())?;
            tools::distinct_output(&output, &[&path, &options.key])?;
            let image = elf::image(&fs::read(path)?, false)?;
            let signed = mcuboot::sign(&image.payload, geometry(), &options)?;
            flash::check_signed_app(&signed)?;
            tools::write_output(&output, signed)
        }
        Command::SignRaw(args) => args.run(geometry()),
        Command::FlashApp {
            elf: path,
            slot,
            flash: backing,
        } => {
            let image = elf::image(&fs::read(&path)?, false)?;
            let options = SignOptions::from_env(geometry())?;
            let signed = mcuboot::sign(&image.payload, geometry(), &options)?;
            flash::check_signed_app(&signed)?;
            let offset = match slot {
                Slot::Primary => board::PRIMARY_OFFSET,
                Slot::Secondary => board::SECONDARY_OFFSET,
            };
            let backing = flash_path(backing);
            tools::distinct_output(&backing, &[&path, &options.key])?;
            flash_or_save_app(
                &path,
                &signed,
                &backing,
                offset,
                tools::flag("NO_FLASH"),
                &[&path, &options.key],
            )
        }
        Command::FlashBootloader {
            elf: path,
            flash: backing,
        } => {
            // QEMU has no ROM signature stage: its bootloader is a checked,
            // unsigned ARM load image, unlike ESP SBv2 or RP2350 PICOBIN.
            let image = elf::image(&fs::read(&path)?, true)?;
            let backing = flash_path(backing);
            tools::distinct_output(&backing, &[&path])?;
            flash_or_save_bootloader(&path, &image.payload, &backing, tools::flag("NO_FLASH"))
        }
        Command::Run { flash: backing } => run_qemu(&flash_path(backing)),
        Command::Layout { constant } => cli::print_constants(&constants(), constant.as_deref()),
    }
}

fn flash_or_save_app(
    elf: &Path,
    signed: &[u8],
    backing: &Path,
    offset: u32,
    no_flash: bool,
    inputs: &[&Path],
) -> Result<()> {
    if no_flash {
        let output = elf.with_extension("signed.bin");
        tools::distinct_output(&output, inputs)?;
        tools::distinct_output(&output, &[backing])?;
        tools::write_output(&output, signed)?;
        println!("NO_FLASH=1: wrote {}", output.display());
        Ok(())
    } else {
        flash::flash_app(backing, signed, offset)
    }
}

fn flash_or_save_bootloader(
    elf: &Path,
    image: &[u8],
    backing: &Path,
    no_flash: bool,
) -> Result<()> {
    if no_flash {
        let output = elf.with_extension("bootloader.bin");
        tools::distinct_output(&output, &[elf, backing])?;
        tools::write_output(&output, image)?;
        println!("NO_FLASH=1: wrote {}", output.display());
        Ok(())
    } else {
        flash::flash_bootloader(backing, image)
    }
}

/// QEMU's -kernel mapping must contain exactly the bootloader currently in
/// the SPI NOR. Keep the temporary copy alive for the whole QEMU session;
/// preserve flash.bin and efuses.bin between runs and never reset OTP.
fn run_qemu(backing: &Path) -> Result<()> {
    let bootloader = flash::read_bootloader(backing)?;
    let efuses = flash::ensure_efuses(backing)?;
    let dir = tempfile::tempdir()?;
    let kernel = dir.path().join("bootloader.bin");
    fs::write(&kernel, bootloader)?;
    let flash = backing.canonicalize()?;
    let efuses = efuses.canonicalize()?;
    let executable = tools::env("FERRIBOOT_QEMU_BIN", "qemu-system-arm");
    let mut command = Process::new(executable);
    command.args([
        "-M",
        "mps2-an385",
        "-cpu",
        "cortex-m3",
        "-accel",
        "tcg",
        "-display",
        "none",
        "-monitor",
        "none",
        "-serial",
        "stdio",
        "-watchdog-action",
        "reset",
    ]);
    command
        .arg("-drive")
        .arg(format!("if=mtd,format=raw,file={}", flash.display()));
    command.arg("-drive").arg(format!(
        "if=pflash,index=1,format=raw,file={}",
        efuses.display()
    ));
    command.arg("-kernel").arg(&kernel);
    let mut progress = Progress::new("Starting patched QEMU", None);
    let mut child = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context(
            "cannot start patched QEMU; set FERRIBOOT_QEMU_BIN (see boards/qemu/qemu/README.md)",
        )?;
    progress.finish();
    let status = child.wait()?;
    ensure!(status.success(), "QEMU exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_parses_matching_flash_commands() {
        for command in ["flash-app", "flash-bootloader"] {
            let parsed = Cli::try_parse_from(["qemu-tools", command, "image.elf"]).unwrap();
            assert!(matches!(
                parsed.command,
                Command::FlashApp { .. } | Command::FlashBootloader { .. }
            ));
        }
        assert!(matches!(
            Cli::try_parse_from(["qemu-tools", "run"]).unwrap().command,
            Command::Run { .. }
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "qemu-tools",
                "flash-app",
                "image.elf",
                "--slot",
                "secondary"
            ])
            .unwrap()
            .command,
            Command::FlashApp {
                slot: Slot::Secondary,
                ..
            }
        ));
    }

    #[test]
    fn qemu_no_flash_saves_files_without_touching_flash() {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("boot.elf");
        let backing = dir.path().join("flash.bin");
        fs::write(&elf, b"ELF").unwrap();
        flash_or_save_bootloader(&elf, b"boot", &backing, true).unwrap();
        assert_eq!(
            fs::read(elf.with_extension("bootloader.bin")).unwrap(),
            b"boot"
        );
        flash_or_save_app(
            &elf,
            b"signed",
            &backing,
            board::PRIMARY_OFFSET,
            true,
            &[&elf],
        )
        .unwrap();
        assert_eq!(
            fs::read(elf.with_extension("signed.bin")).unwrap(),
            b"signed"
        );
        assert!(!backing.exists());
        assert!(!dir.path().join("efuses.bin").exists());
    }

    #[test]
    fn no_flash_rejects_overwriting_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("boot.elf");
        let key = elf.with_extension("signed.bin");
        fs::write(&elf, b"ELF").unwrap();
        fs::write(&key, b"private key").unwrap();
        assert!(
            flash_or_save_app(
                &elf,
                b"signed",
                &dir.path().join("flash.bin"),
                board::PRIMARY_OFFSET,
                true,
                &[&elf, &key]
            )
            .is_err()
        );
        assert_eq!(fs::read(key).unwrap(), b"private key");
        assert!(
            flash_or_save_bootloader(&elf, b"boot", &elf.with_extension("bootloader.bin"), true)
                .is_err()
        );
    }
}
