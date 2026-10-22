//! `ferriboot-tools-esp`: ESP32-C3 images, Secure Boot V2 and the IDF boot chain.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use ferriboot_tools::{
    self as tools,
    cli::{self, Signing},
    mcuboot::{self, SignOptions},
};
use ferriboot_tools_esp::{DEV_KEY, chain, flash, geometry, image, layout, sbv2};
use std::{fs, path::PathBuf, process::Command as Process};

#[derive(Parser)]
#[command(
    about = "Ferriboot ESP32-C3 image, Secure Boot V2 and boot-chain tools",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Generate the RSA-3072 Secure Boot V2 development key pair.
    Keys(cli::Keys),
    /// Convert and MCUboot-sign an application.
    Sign {
        elf: PathBuf,
        output: PathBuf,
        #[command(flatten)]
        signing: Signing,
    },
    /// MCUboot-sign a raw payload with this board's geometry.
    SignRaw(cli::SignRaw),
    /// Check signed envelope, ESP segments and MMU mapping.
    Inspect {
        image: PathBuf,
        #[arg(value_parser = tools::number)]
        slot: Option<u32>,
    },
    /// RSA-3072/PSS/SHA256 ROM signing of a padded ESP image.
    SignSbv2 {
        image: PathBuf,
        output: PathBuf,
        #[arg(long)]
        key: PathBuf,
    },
    /// Verify against a trusted private/public PEM.
    VerifySbv2 {
        image: PathBuf,
        #[arg(long)]
        key: PathBuf,
    },
    /// Build or check the IDF second stage.
    BuildBootloader { output: PathBuf },
    /// Sign, preflight, flash and monitor an application.
    FlashApp { elf: PathBuf },
    /// Build/sign both stages, write table and boot chain.
    FlashFerriboot { elf: PathBuf },
    /// Validate partitions; print constants or emit the binary table.
    Layout {
        #[arg(long)]
        partitions: Option<PathBuf>,
        #[arg(long)]
        table: Option<PathBuf>,
        #[arg(long)]
        get: Option<String>,
        #[arg(long)]
        constant: Option<String>,
    },
}

fn main() {
    cli::exit_on_error(env!("CARGO_BIN_NAME"), run(Cli::parse().command));
}
fn run(command: Command) -> Result<()> {
    match command {
        Command::Keys(args) => args.run(&[DEV_KEY]),
        Command::Sign {
            elf,
            output,
            signing,
        } => {
            let geometry = geometry()?;
            let options = signing.options(geometry);
            tools::distinct_output(&output, &[&elf, &options.key])?;
            let data = mcuboot::sign(&image::convert(&elf, false)?, geometry, &options)?;
            image::inspect_slot(
                &data,
                layout::default_layout()?.region("image-0")?.0,
                geometry,
            )?;
            tools::write_output(&output, data)
        }
        Command::SignRaw(args) => args.run(geometry()?),
        Command::Inspect { image: path, slot } => {
            image::inspect_slot(
                &fs::read(path)?,
                slot.unwrap_or(layout::default_layout()?.region("image-0")?.0),
                geometry()?,
            )?;
            println!("OK");
            Ok(())
        }
        Command::SignSbv2 { image, output, key } => {
            tools::distinct_output(&output, &[&image, &key])?;
            tools::write_output(
                &output,
                sbv2::sign(&fs::read(image)?, &fs::read_to_string(key)?)?,
            )
        }
        Command::VerifySbv2 { image, key } => {
            sbv2::verify(&fs::read(image)?, &fs::read_to_string(key)?)
        }
        Command::BuildBootloader { output } => {
            let settings = chain::Settings::from_env();
            if let Some(source) = &settings.idf_bootloader {
                tools::distinct_output(&output, &[source])?;
            }
            tools::write_output(&output, chain::build_idf(&settings)?)
        }
        Command::FlashApp { elf } => {
            let layout = layout::default_layout()?;
            let slot = tools::env("SLOT", "image-0");
            ensure!(
                ["image-0", "image-1"].contains(&slot.as_str()),
                "SLOT must be image-0 or image-1"
            );
            let geometry = geometry()?;
            let mut options = SignOptions::from_env(geometry);
            if tools::flag("CONFIRM") {
                options.extra = vec!["--pad".into(), "--confirm".into()];
            }
            let data = mcuboot::sign(&image::convert(&elf, false)?, geometry, &options)?;
            let offset = layout.region(&slot)?.0;
            image::inspect_slot(&data, offset, geometry)?;
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("app.bin");
            fs::write(&path, data)?;
            flash::write_flash(&[(offset, &path)])?;
            let mut cmd = Process::new(tools::env("ESPFLASH", "espflash"));
            cmd.arg("monitor");
            if let Ok(port) = std::env::var("ESPPORT") {
                cmd.args(["--port", &port]);
            }
            tools::run(&mut cmd)
        }
        Command::FlashFerriboot { elf } => {
            let settings = chain::Settings::from_env();
            let chain = chain::boot_chain(&elf, &settings)?;
            let parent = elf
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .context("ELF path must include a directory")?;
            for name in [
                "ferriboot.sbv2.bin",
                "bootloader.idf.sbv2.bin",
                "partitions.bin",
            ] {
                tools::distinct_output(&parent.join(name), &[&elf, &settings.key])?;
            }
            chain.write(parent)?;
            if settings.no_flash {
                Ok(())
            } else {
                chain.flash(parent)
            }
        }
        Command::Layout {
            partitions,
            table,
            get,
            constant,
        } => {
            let layout = if let Some(path) = partitions {
                layout::EspLayout::load(&path)?
            } else {
                layout::default_layout()?
            };
            if let Some(output) = table {
                tools::write_output(&output, layout.binary()?)?;
            }
            if let Some(name) = get {
                let (offset, size) = layout.region(&name)?;
                println!("{offset:#x} {size:#x}");
                return Ok(());
            }
            cli::print_constants(&layout.constants()?, constant.as_deref())
        }
    }
}
