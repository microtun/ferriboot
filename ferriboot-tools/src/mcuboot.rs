//! Upstream imgtool signing, followed by independent P-256 envelope validation.
use crate::{bytes, half, word};
use anyhow::{Context, Result, ensure};
use p256::{
    SecretKey,
    ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier},
    pkcs8::{DecodePrivateKey, EncodePublicKey},
};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// MCUboot slot geometry. Board crates provide constructors
/// (`ferriboot_tools_esp::geometry()`, etc.).
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub header: u32,
    pub slot: u32,
    pub erase: u32,
    pub write: u32,
    pub rom_fixed: Option<u32>,
    pub class: &'static str,
}
impl Geometry {
    pub fn max_image(&self) -> Result<usize> {
        ensure!(
            self.erase > 0 && self.write > 0 && self.slot.is_multiple_of(self.erase),
            "invalid slot geometry"
        );
        let trailer = u64::from(self.slot / self.erase) * 3 * u64::from(self.write) + 48;
        let available = u64::from(self.slot)
            .checked_sub(trailer)
            .context("slot too small for trailer")?;
        Ok((available / u64::from(self.erase) * u64::from(self.erase)) as usize)
    }
}

pub fn load_key(path: &Path) -> Result<SecretKey> {
    SecretKey::from_pkcs8_pem(&std::fs::read_to_string(path)?)
        .context("expected unencrypted PKCS8 ECDSA P-256 private key")
}
#[derive(Debug)]
pub struct SignedImage {
    pub load_address: u32,
    pub header_size: usize,
    pub body_size: usize,
    pub flags: u32,
    pub size: usize,
}
type Tlvs<'a> = Vec<(u16, &'a [u8])>;
fn tlvs(data: &[u8], offset: usize, magic: u16, limit: usize) -> Result<(usize, Tlvs<'_>)> {
    ensure!(
        offset + 4 <= limit && half(data, offset)? == magic,
        "invalid TLV info"
    );
    let len = usize::from(half(data, offset + 2)?);
    let end = offset.checked_add(len).context("TLV offset overflow")?;
    ensure!(
        len >= 4 && end <= limit,
        "TLVs overlap trailer or exceed image"
    );
    let mut cursor = offset + 4;
    let mut entries = Vec::new();
    while cursor < end {
        ensure!(cursor + 4 <= end, "truncated TLV header");
        let kind = half(data, cursor)?;
        let len = usize::from(half(data, cursor + 2)?);
        cursor += 4;
        ensure!(cursor + len <= end, "truncated TLV value");
        entries.push((kind, bytes(data, cursor, len)?));
        cursor += len;
    }
    Ok((end, entries))
}

pub fn inspect(
    data: &[u8],
    geometry: Geometry,
    public: Option<&VerifyingKey>,
) -> Result<SignedImage> {
    ensure!(
        data.len() >= 32 && data.len() <= geometry.slot as usize && word(data, 0)? == 0x96f3b83d,
        "invalid signed image/header"
    );
    let header = usize::from(half(data, 8)?);
    let protected = usize::from(half(data, 10)?);
    let body = word(data, 12)? as usize;
    let limit = data.len().min(geometry.max_image()?);
    ensure!(
        header >= 32 && header.checked_add(body).is_some_and(|end| end <= limit),
        "image overlaps trailer sectors"
    );
    let mut offset = header + body;
    if protected != 0 {
        let (end, _) = tlvs(data, offset, 0x6908, limit)?;
        ensure!(end - offset == protected, "protected TLV size mismatch");
        offset = end;
    }
    let digest = Sha256::digest(&data[..offset]);
    let (end, entries) = tlvs(data, offset, 0x6907, limit)?;
    let der = public.map(|p| p.to_public_key_der()).transpose()?;
    let (mut hash, mut signature, mut key) = (false, false, false);
    for (kind, value) in entries {
        match kind {
            0x10 => {
                ensure!(!hash && value == &digest[..], "SHA256 mismatch/duplicate");
                hash = true;
            }
            0x01 if der.is_some() => {
                key |= value == &Sha256::digest(der.as_ref().unwrap().as_bytes())[..];
            }
            0x02 if der.is_some() => {
                key |= value == der.as_ref().unwrap().as_bytes();
            }
            0x22 => {
                ensure!(!signature, "duplicate P-256 signature");
                let sig = Signature::from_der(value).context("invalid P-256 DER signature")?;
                if let Some(public) = public {
                    public
                        .verify_prehash(&digest, &sig)
                        .context("P-256 signature mismatch")?;
                }
                signature = true;
            }
            0x25 => anyhow::bail!("pure signatures are unsupported"),
            _ => {}
        }
    }
    ensure!(
        hash && signature && (public.is_none() || key),
        "missing SHA256, signature or trusted key"
    );
    Ok(SignedImage {
        load_address: word(data, 4)?,
        header_size: header,
        body_size: body,
        flags: word(data, 16)?,
        size: end,
    })
}

pub struct SignOptions {
    pub key: PathBuf,
    pub version: String,
    pub counter: String,
    pub vendor: String,
    pub class: String,
    pub extra: Vec<String>,
}
impl SignOptions {
    pub fn from_env(geometry: Geometry) -> Self {
        Self {
            key: std::env::var_os("FERRIBOOT_KEY")
                .map(PathBuf::from)
                .unwrap_or_else(|| crate::keys::MCUBOOT.private_path(&crate::repo_root())),
            version: crate::env("IMAGE_VERSION", "0.0.0"),
            counter: crate::env("SECURITY_COUNTER", "0"),
            vendor: crate::env("VID", "microtun.dev"),
            class: crate::env("CID", geometry.class),
            extra: Vec::new(),
        }
    }
}
pub fn sign(payload: &[u8], geometry: Geometry, options: &SignOptions) -> Result<Vec<u8>> {
    let secret = load_key(&options.key)?;
    ensure!(
        geometry.header as usize + payload.len() < geometry.max_image()?,
        "payload exceeds usable slot"
    );
    let dir = tempfile::tempdir()?;
    let raw = dir.path().join("payload.bin");
    let signed = dir.path().join("signed.bin");
    std::fs::write(&raw, payload)?;
    let mut cmd = Command::new(crate::env("IMGTOOL", "imgtool"));
    cmd.args(["sign", "--key"])
        .arg(&options.key)
        .args(["--sha", "256", "--header-size"])
        .arg(format!("{:#x}", geometry.header))
        .args(["--pad-header", "--align"])
        .arg(geometry.write.to_string())
        .args(["--max-align", "8", "--max-sectors"])
        .arg((geometry.slot / geometry.erase).to_string())
        .arg("--slot-size")
        .arg(format!("{:#x}", geometry.slot))
        .arg("--version")
        .arg(&options.version);
    let overridden = |name: &str| {
        options
            .extra
            .iter()
            .any(|arg| arg == name || arg.starts_with(&format!("{name}=")))
    };
    for (name, value) in [
        ("--security-counter", &options.counter),
        ("--vid", &options.vendor),
        ("--cid", &options.class),
    ] {
        if !overridden(name) {
            cmd.arg(name).arg(value);
        }
    }
    if let Some(address) = geometry.rom_fixed
        && !overridden("--rom-fixed")
    {
        cmd.arg("--rom-fixed").arg(format!("{address:#x}"));
    }
    crate::run(cmd.args(&options.extra).arg(&raw).arg(&signed))?;
    let data = std::fs::read(signed)?;
    inspect(
        &data,
        geometry,
        Some(&VerifyingKey::from(secret.public_key())),
    )?;
    Ok(data)
}
