//! `ferriboot-tools`: board-independent commands. Board commands are in
//! `ferriboot-tools-esp`, `ferriboot-tools-rp2350` and `ferriboot-tools-qemu`.
use anyhow::Result;
use clap::{Parser, Subcommand};
use ferriboot_tools::{cli, keys, mcuboot};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Ferriboot shared MCUboot key and signing tools", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Generate the MCUboot P-256 development key pair (all boards).
    Keys(cli::Keys),
    /// Check an MCUboot P-256 private key.
    CheckKey { key: PathBuf },
}

fn main() {
    cli::exit_on_error(env!("CARGO_BIN_NAME"), run(Cli::parse().command));
}
fn run(command: Command) -> Result<()> {
    match command {
        Command::Keys(args) => args.run(&[keys::MCUBOOT]),
        Command::CheckKey { key } => {
            mcuboot::load_key(&key)?;
            Ok(())
        }
    }
}
