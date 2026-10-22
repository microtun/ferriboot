//! `ferriboot-tools-qemu`: AN385 ELF export, signing and layout.
use anyhow::Result;
use clap::{Parser, Subcommand};
use ferriboot_build::tools::{self as tools, cli};
use ferriboot_tools_qemu::{constants, elf, geometry};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(about = "Ferriboot QEMU (MPS2 AN385) image tools", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Export checked AN385 physical load bytes.
    ExportBin {
        elf: PathBuf,
        output: PathBuf,
        #[arg(long)]
        bootloader: bool,
    },
    /// MCUboot-sign a raw payload with this board's geometry.
    SignRaw(cli::SignRaw),
    /// Print the compiled board layout.
    Layout {
        #[arg(long)]
        constant: Option<String>,
    },
}

fn main() {
    cli::exit_on_error(env!("CARGO_BIN_NAME"), run(Cli::parse().command));
}
fn run(command: Command) -> Result<()> {
    match command {
        Command::ExportBin {
            elf: path,
            output,
            bootloader,
        } => {
            tools::distinct_output(&output, &[&path])?;
            tools::write_output(&output, elf::image(&fs::read(path)?, bootloader)?.payload)
        }
        Command::SignRaw(args) => args.run(geometry()),
        Command::Layout { constant } => cli::print_constants(&constants(), constant.as_deref()),
    }
}
