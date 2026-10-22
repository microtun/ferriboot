//! RP2350 ROM sealing through picotool, independently re-checked. Sealing
//! itself never writes OTP.
use crate::picobin;
use anyhow::{Context, Result, ensure};
use ferriboot_tools::elf::Image;
use k256::{
    SecretKey,
    elliptic_curve::sec1::ToEncodedPoint,
    pkcs8::{DecodePrivateKey, EncodePublicKey, LineEnding},
};
use rp2350_layout as chip;
use sha2::{Digest, Sha256};
use std::{path::Path, process::Command};

pub fn load_key(path: &Path) -> Result<SecretKey> {
    let pem = std::fs::read_to_string(path)?;
    SecretKey::from_sec1_pem(&pem)
        .or_else(|_| SecretKey::from_pkcs8_pem(&pem))
        .context("RP2350 ROM sealing needs a secp256k1 private PEM key (not MCUboot P-256)")
}
/// Fresh secp256k1 `(private SEC1 PEM, public SPKI PEM)` pair for development.
/// picotool accepts SEC1 (EC PRIVATE KEY), not PKCS8, for its ROM key.
pub fn generate_pair() -> Result<(String, String)> {
    let key = SecretKey::random(&mut rand_core::OsRng);
    Ok((
        key.to_sec1_pem(LineEnding::LF)?.to_string(),
        key.public_key().to_public_key_pem(LineEnding::LF)?,
    ))
}
pub fn profile(payload: &[u8]) -> Result<&'static str> {
    const MARKER: &[u8] = b"FERRIBOOT_OTP_V3";
    for old in [b"FERRIBOOT_OTP_V1", b"FERRIBOOT_OTP_V2"] {
        ensure!(
            !payload.windows(old.len()).any(|w| w == old),
            "legacy key-embedded OTP build; rebuild V3 profile"
        );
    }
    let positions: Vec<_> = payload
        .windows(MARKER.len())
        .enumerate()
        .filter_map(|(i, w)| (w == MARKER).then_some(i))
        .collect();
    ensure!(
        positions.len() == 1 && positions[0] >= picobin::ROM_HEADER_SIZE,
        "bootloader needs exactly one OTP profile in ROM-loaded SRAM"
    );
    match ferriboot_tools::bytes(payload, positions[0] + MARKER.len(), 4)? {
        b"DEV\0" => Ok("dev"),
        b"REL\0" => Ok("release"),
        _ => anyhow::bail!("unknown OTP profile"),
    }
}
pub fn check_sealed(
    unsigned: &[u8],
    sealed: &[u8],
    key: &SecretKey,
    plan: Option<&serde_json::Value>,
) -> Result<()> {
    ensure!(
        unsigned.len() >= picobin::ROM_HEADER_SIZE
            && sealed.len() > unsigned.len()
            && sealed.len() <= rp2350_layout::BOOTLOADER_SIZE as usize,
        "invalid sealed bootloader size"
    );
    ensure!(
        unsigned[..picobin::LINKER_LAST_OFFSET] == sealed[..picobin::LINKER_LAST_OFFSET]
            && unsigned[picobin::ROM_HEADER_SIZE..]
                == sealed[picobin::ROM_HEADER_SIZE..unsigned.len()],
        "seal changed executable or ROM metadata"
    );
    let embedded = picobin::signing_public_key(sealed)?;
    let point = key.public_key().to_encoded_point(false);
    ensure!(
        embedded == &point.as_bytes()[1..],
        "seal key differs from signing key"
    );
    if let Some(plan) = plan {
        let key = plan["bootkey0"]
            .as_array()
            .context("OTP plan needs BOOTKEY0")?;
        let digest: Vec<u8> = key
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .context("invalid BOOTKEY0 byte")
            })
            .collect::<Result<_>>()?;
        ensure!(
            digest == Sha256::digest(embedded)[..],
            "OTP plan BOOTKEY0 differs from seal digest"
        );
        ensure!(
            plan["boot_flags1"]["key_valid"]
                .as_u64()
                .is_some_and(|v| v & 1 == 1)
                && plan["crit1"]["secure_boot_enable"].as_u64() == Some(1),
            "OTP plan must mark BOOTKEY0 valid and enable secure boot"
        );
    }
    Ok(())
}
pub fn check_report(report: &str, unsigned_size: usize) -> Result<()> {
    use regex::Regex;
    ensure!(
        Regex::new(r"(?i)signature\s*:\s*verified\b")?.is_match(report),
        "picotool did not verify secp256k1 signature"
    );
    ensure!(
        Regex::new(r"(?i)image type\s*:\s*ARM Secure\b")?.is_match(report),
        "seal is not ARM Secure"
    );
    let source = chip::XIP_BASE + picobin::ROM_HEADER_SIZE as u32;
    let source_end = u64::from(chip::XIP_BASE) + unsigned_size as u64;
    let dest = chip::RAM_BASE;
    let dest_end = u64::from(dest) + unsigned_size as u64 - picobin::ROM_HEADER_SIZE as u64;
    let pattern = format!(
        r"(?i)load map entry\s*\d+\s*:\s*Copy\s+{source:#x}->{source_end:#x}\s+to\s+{dest:#x}->{dest_end:#x}\b"
    );
    ensure!(
        Regex::new(&pattern)?.is_match(report),
        "seal lacks exact flash-to-SRAM ROM load map"
    );
    Ok(())
}
pub fn seal(image: &Image, key_path: &Path, otp_output: Option<&Path>) -> Result<Vec<u8>> {
    profile(&image.payload)?;
    let key = load_key(key_path)?;
    if let Some(path) = otp_output {
        ensure!(
            !path.try_exists()?,
            "OTP output must be new; picotool merges existing JSON"
        );
    }
    let dir = tempfile::tempdir()?;
    let raw = dir.path().join("unsigned.bin");
    let output = dir.path().join("signed.bin");
    let plan = dir.path().join("otp.json");
    std::fs::write(&raw, &image.payload)?;
    let picotool = ferriboot_tools::env("PICOTOOL", "picotool");
    let mut cmd = Command::new(&picotool);
    cmd.args(["seal", "--sign"])
        .arg(&raw)
        .args(["-t", "bin", "-o"])
        .arg(format!("{:#x}", image.origin))
        .arg(&output)
        .args(["-t", "bin"])
        .arg(key_path);
    if otp_output.is_some() {
        cmd.arg(&plan);
    }
    ferriboot_tools::run(&mut cmd)?;
    let sealed = std::fs::read(&output)?;
    let report = Command::new(&picotool)
        .args(["info", "-a"])
        .arg(&output)
        .args(["-t", "bin"])
        .output()?;
    ensure!(
        report.status.success(),
        "picotool info failed: {}",
        String::from_utf8_lossy(&report.stderr)
    );
    check_report(
        &format!(
            "{}{}",
            String::from_utf8_lossy(&report.stdout),
            String::from_utf8_lossy(&report.stderr)
        ),
        image.payload.len(),
    )?;
    let plan_data = if otp_output.is_some() {
        Some(std::fs::read(&plan)?)
    } else {
        None
    };
    let document = plan_data
        .as_ref()
        .map(|data| serde_json::from_slice(data))
        .transpose()?;
    check_sealed(&image.payload, &sealed, &key, document.as_ref())?;
    if let (Some(path), Some(data)) = (otp_output, plan_data) {
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        output.write_all(&data)?;
    }
    Ok(sealed)
}
