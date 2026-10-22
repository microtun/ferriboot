//! Clap pieces shared by `ferriboot-tools` and every board binary.
use crate::{
    keys::{self, DevKey},
    mcuboot::{self, Geometry, SignOptions},
};
use anyhow::{Context, Result};
use clap::Args;
use std::{collections::BTreeMap, fs, path::PathBuf};

/// MCUboot signing options.
#[derive(Args)]
pub struct Signing {
    #[arg(long, env = "FERRIBOOT_KEY")]
    pub key: Option<PathBuf>,
    #[arg(long, env = "IMAGE_VERSION", default_value = "0.0.0")]
    pub version: String,
    #[arg(long)]
    pub pad: bool,
    #[arg(long)]
    pub confirm: bool,
    /// Extra imgtool arguments after -- take precedence over defaults.
    #[arg(last = true)]
    pub extra: Vec<String>,
}
impl Signing {
    pub fn options(self, geometry: Geometry) -> SignOptions {
        let mut options = SignOptions::from_env(geometry);
        if let Some(key) = self.key {
            options.key = key;
        }
        options.version = self.version;
        if self.pad || self.confirm {
            options.extra.push("--pad".into());
        }
        if self.confirm {
            options.extra.push("--confirm".into());
        }
        options.extra.extend(self.extra);
        options
    }
}

/// `keys`: generate development key pairs; never overwrite by default.
#[derive(Args)]
pub struct Keys {
    #[arg(long)]
    pub output_dir: Option<PathBuf>,
    #[arg(long)]
    pub force: bool,
}
impl Keys {
    pub fn run(self, pairs: &[DevKey]) -> Result<()> {
        let root = self.output_dir.unwrap_or_else(crate::repo_root);
        keys::generate(&root, pairs, self.force)
    }
}

/// `sign-raw`: MCUboot-sign a raw payload with the board's geometry.
#[derive(Args)]
pub struct SignRaw {
    pub payload: PathBuf,
    pub output: PathBuf,
    #[command(flatten)]
    pub signing: Signing,
}
impl SignRaw {
    pub fn run(self, geometry: Geometry) -> Result<()> {
        let options = self.signing.options(geometry);
        crate::distinct_output(&self.output, &[&self.payload, &options.key])?;
        let signed = mcuboot::sign(&fs::read(&self.payload)?, geometry, &options)?;
        crate::write_output(&self.output, signed)
    }
}

/// Print every layout constant as JSON, or one as hex.
pub fn print_constants(values: &BTreeMap<String, u32>, constant: Option<&str>) -> Result<()> {
    if let Some(name) = constant {
        println!("{:#x}", values.get(name).context("unknown constant")?);
    } else {
        println!("{}", serde_json::to_string_pretty(values)?);
    }
    Ok(())
}

/// Print `program: error` and exit with status 1 on failure.
pub fn exit_on_error(program: &str, result: Result<()>) {
    if let Err(error) = result {
        eprintln!("{program}: {error:#}");
        std::process::exit(1);
    }
}
