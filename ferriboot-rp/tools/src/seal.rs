//! RP2350 ROM image sealing using the BIN-only parts of picoseal, with no
//! `picotool` subprocess. Neither local nor remote signing programs OTP.
use crate::{picobin, romseal};
use anyhow::{Context, Result, ensure};
use ferriboot_build::tools::{elf::Image, signer::RemoteSigner};
use k256::{
    PublicKey, SecretKey,
    ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier},
    elliptic_curve::sec1::ToEncodedPoint,
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePublicKey, LineEnding},
};
use rp2350_layout as chip;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn load_key(path: &Path) -> Result<SecretKey> {
    let pem = std::fs::read_to_string(path)?;
    SecretKey::from_sec1_pem(&pem)
        .or_else(|_| SecretKey::from_pkcs8_pem(&pem))
        .context("RP2350 ROM sealing needs a secp256k1 private PEM key (not MCUboot P-256)")
}

/// Fresh secp256k1 `(private SEC1 PEM, public SPKI PEM)` pair for development.
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
    match ferriboot_build::tools::bytes(payload, positions[0] + MARKER.len(), 4)? {
        b"DEV\0" => Ok("dev"),
        b"REL\0" => Ok("release"),
        _ => anyhow::bail!("unknown OTP profile"),
    }
}

/// Check unchanged executable bytes, the boot key, and a requested OTP plan.
pub fn check_sealed(
    unsigned: &[u8],
    sealed: &[u8],
    key: &PublicKey,
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
    let point = key.to_encoded_point(false);
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

/// Independently verify both signed ROM hash and the exact Ferriboot flash-to-SRAM
/// load map. This replaces parsing the human-readable `picotool info` output.
fn check_rom(image: &Image, sealed: &[u8]) -> Result<()> {
    let report = romseal::verify(sealed, image.origin).context("parse sealed RP2350 image")?;
    ensure!(
        report.hash == romseal::Verified::Passed,
        "RP2350 ROM hash did not verify"
    );
    ensure!(
        report.signature == romseal::Verified::Passed,
        "RP2350 ROM signature did not verify"
    );
    ensure!(
        report.image_type().as_deref() == Some("ARM Secure"),
        "seal is not ARM Secure"
    );
    let block = report.block.context("missing sealed PICOBIN block")?;
    let entries = block.load_map().context("missing sealed ROM load map")?;
    let expected = image
        .payload
        .len()
        .checked_sub(picobin::ROM_HEADER_SIZE)
        .context("invalid bootloader ROM header")?;
    ensure!(
        matches!(entries, [e] if e.storage_address == image.origin + picobin::ROM_HEADER_SIZE as u32
            && e.runtime_address == chip::RAM_BASE
            && usize::try_from(e.size) == Ok(expected)),
        "seal lacks exact flash-to-SRAM ROM load map"
    );
    Ok(())
}

fn save_otp(otp_output: Option<&Path>, key: &PublicKey) -> Result<Option<serde_json::Value>> {
    let Some(path) = otp_output else {
        return Ok(None);
    };
    // Always create-new; never overwrite or silently merge provisioning plans.
    let point = key.to_encoded_point(false);
    let raw_public_key: [u8; 64] = point.as_bytes()[1..].try_into()?;
    let otp_json = romseal::update_otp_json(None, &raw_public_key)?;
    let document: serde_json::Value = serde_json::from_str(&otp_json)?;
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(otp_json.as_bytes())?;
    Ok(Some(document))
}

fn finish(
    image: &Image,
    sealed: Vec<u8>,
    key: &PublicKey,
    otp_output: Option<&Path>,
) -> Result<Vec<u8>> {
    // Check *before* creating an OTP plan, so failed signing never emits one.
    check_rom(image, &sealed)?;
    check_sealed(&image.payload, &sealed, key, None)?;
    if let Some(path) = otp_output {
        ensure!(!path.try_exists()?, "OTP output must be a new file");
        let document = save_otp(Some(path), key)?.context("missing generated OTP plan")?;
        check_sealed(&image.payload, &sealed, key, Some(&document))?;
    }
    Ok(sealed)
}

/// Seal and sign the RP2350 ROM bootloader with a local secp256k1 PEM key.
pub fn seal(image: &Image, key_path: &Path, otp_output: Option<&Path>) -> Result<Vec<u8>> {
    profile(&image.payload)?;
    let key = load_key(key_path)?;
    let options = romseal::SealOptions {
        sign: true,
        hash: true,
        bin_offset: Some(image.origin),
        ..Default::default()
    };
    let sealed = romseal::seal(&image.payload, &options, Some(&key))?.data;
    finish(image, sealed, &key.public_key(), otp_output)
}

/// Seal with a remote microtun-signer secp256k1 key, using the in-process
/// external-sign/reseal implementation. No private key leaves the signer.
pub fn seal_remote(
    image: &Image,
    remote: &RemoteSigner,
    otp_output: Option<&Path>,
) -> Result<Vec<u8>> {
    profile(&image.payload)?;
    let pem = remote.public_key()?;
    let key = PublicKey::from_public_key_pem(&pem)
        .context("RP2350 remote signing requires a secp256k1 public key")?;
    let options = romseal::SealOptions {
        external_sign: true,
        bin_offset: Some(image.origin),
        ..Default::default()
    };
    let prepared = romseal::seal(&image.payload, &options, None)?;
    let digest = prepared
        .hash
        .context("RP2350 external seal did not supply a SHA-256 hash")?;
    let signature = remote.sign_digest(&digest)?;
    ensure!(
        signature.len() == 64,
        "RP2350 requires a 64-byte secp256k1 signature"
    );
    let signature =
        Signature::from_slice(&signature).context("invalid secp256k1 r||s signature")?;
    let verifier = VerifyingKey::from_sec1_bytes(key.to_encoded_point(false).as_bytes())?;
    verifier
        .verify_prehash(&digest, &signature)
        .context("remote RP2350 signature mismatch")?;
    let mut sealed = prepared.data;
    romseal::reseal(
        &mut sealed,
        image.origin,
        signature.to_der().as_bytes(),
        &key,
    )?;
    finish(image, sealed, &key, otp_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner};

    // An unsealed bootloader with precisely the 512-byte ROM header produced
    // by boards/rp2350/ferriboot/ld/bootloader.x and a V3 OTP profile.
    fn image() -> Image {
        let payload_size = 0x800usize;
        let mut payload = vec![0u8; payload_size];
        let image_words: [u32; 14] = [
            0xffff_ded3,
            0x1021_0142,
            0x0000_0203,
            chip::RAM_BASE,
            0x0000_0344,
            chip::RAM_BASE + 0x101,
            chip::BOOT_INFO_ADDRESS,
            0x0100_0406,
            picobin::ROM_HEADER_SIZE as u32 - 28,
            chip::RAM_BASE,
            (payload_size - picobin::ROM_HEADER_SIZE) as u32,
            0x0000_0aff,
            0,
            0xab12_3579,
        ];
        for (i, word) in image_words.iter().enumerate() {
            payload[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        payload[512..516].copy_from_slice(&chip::BOOT_INFO_ADDRESS.to_le_bytes());
        payload[516..520].copy_from_slice(&(chip::RAM_BASE + 0x101).to_le_bytes());
        payload[0x280..0x290].copy_from_slice(b"FERRIBOOT_OTP_V3");
        payload[0x290..0x294].copy_from_slice(b"DEV\0");
        Image {
            origin: chip::XIP_BASE,
            payload,
        }
    }

    #[test]
    fn local_pem_seals_bootloader_without_picotool() {
        let bootloader = image();
        let temp = tempfile::tempdir().unwrap();
        let key = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let path = temp.path().join("rom-key.pem");
        std::fs::write(&path, key.to_sec1_pem(LineEnding::LF).unwrap().as_bytes()).unwrap();
        let otp_path = temp.path().join("otp.json");
        let signed = seal(&bootloader, &path, Some(&otp_path)).unwrap();
        check_rom(&bootloader, &signed).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&otp_path).unwrap()).unwrap();
        check_sealed(
            &bootloader.payload,
            &signed,
            &key.public_key(),
            Some(&document),
        )
        .unwrap();
        assert!(seal(&bootloader, &path, Some(&otp_path)).is_err());
    }

    #[test]
    fn external_signature_reseals_ferriboot_image() {
        let bootloader = image();
        let key = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let opts = romseal::SealOptions {
            external_sign: true,
            bin_offset: Some(bootloader.origin),
            ..Default::default()
        };
        let prepared = romseal::seal(&bootloader.payload, &opts, None).unwrap();
        let digest = prepared.hash.unwrap();
        let sig: Signature = SigningKey::from(&key).sign_prehash(&digest).unwrap();
        let mut sealed = prepared.data;
        romseal::reseal(
            &mut sealed,
            bootloader.origin,
            sig.to_der().as_bytes(),
            &key.public_key(),
        )
        .unwrap();
        check_rom(&bootloader, &sealed).unwrap();
        check_sealed(&bootloader.payload, &sealed, &key.public_key(), None).unwrap();
        // Signature bytes alone are not sufficient when the payload is changed.
        sealed[0x400] ^= 1;
        assert!(check_rom(&bootloader, &sealed).is_err());
    }
}
