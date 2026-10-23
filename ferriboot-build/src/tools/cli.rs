//! Clap pieces shared by the board tool binaries.
use super::{
    keys::{self, DevKey},
    mcuboot::{self, Geometry, SignOptions},
    signer::RemoteSigner,
};
use anyhow::{Context, Result};
use clap::Args;
use std::{collections::BTreeMap, fs, path::PathBuf};

/// MCUboot signing options.
#[derive(Args)]
pub struct Signing {
    #[arg(long, env = "FERRIBOOT_KEY")]
    pub key: Option<PathBuf>,
    /// Remote signer endpoint; requires --signer-key-id.
    #[arg(long, env = "MICROTUN_SIGNER_URL")]
    pub signer_url: Option<String>,
    /// Remote signer key identifier (firmware requires a P-256 key).
    #[arg(long, env = "MICROTUN_SIGNER_KEY_ID")]
    pub signer_key_id: Option<String>,
    /// Authenticate interactively through GitHub device login.
    #[arg(long, env = "MICROTUN_SIGNER_GITHUB_LOGIN")]
    pub signer_github_login: bool,
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
    pub fn options(self, geometry: Geometry) -> Result<SignOptions> {
        let mut options = SignOptions::from_env(geometry)?;
        if let Some(key) = self.key {
            options.key = key;
        }
        options.remote = match (self.signer_url, self.signer_key_id) {
            (None, None) => options.remote,
            (Some(url), Some(key_id)) => Some(RemoteSigner {
                url,
                key_id,
                github_login: self.signer_github_login,
            }),
            _ => anyhow::bail!("--signer-url and --signer-key-id must be specified together"),
        };
        options.version = self.version;
        if self.pad || self.confirm {
            options.extra.push("--pad".into());
        }
        if self.confirm {
            options.extra.push("--confirm".into());
        }
        options.extra.extend(self.extra);
        Ok(options)
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
        let root = self.output_dir.unwrap_or_else(super::repo_root);
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
        let options = self.signing.options(geometry)?;
        super::distinct_output(&self.output, &[&self.payload, &options.key])?;
        let signed = mcuboot::sign(&fs::read(&self.payload)?, geometry, &options)?;
        super::write_output(&self.output, signed)
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
