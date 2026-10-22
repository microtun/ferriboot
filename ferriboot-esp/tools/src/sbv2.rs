//! ESP ROM RSA-3072/PSS/SHA256 signature sectors, including Montgomery fields.
use anyhow::{Context, Result, ensure};
use rand_core::OsRng;
use rsa::{
    BigUint, Pss, RsaPrivateKey, RsaPublicKey,
    pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey},
    traits::PublicKeyParts,
};
use sha2::{Digest, Sha256};

const SECTOR: usize = 4096;
const BLOCK: usize = 1216;
const KEY_START: usize = 36;
const SIGNATURE_START: usize = 812;
const CRC: usize = 1196;

pub fn private(pem: &str) -> Result<RsaPrivateKey> {
    let key = RsaPrivateKey::from_pkcs8_pem(pem).or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))?;
    ensure!(key.n().bits() == 3072, "SBv2 requires RSA-3072");
    key.validate()?;
    Ok(key)
}
fn public(pem: &str) -> Result<RsaPublicKey> {
    let key = if let Ok(key) = private(pem) {
        key.to_public_key()
    } else {
        RsaPublicKey::from_public_key_pem(pem).or_else(|_| RsaPublicKey::from_pkcs1_pem(pem))?
    };
    ensure!(key.n().bits() == 3072, "SBv2 requires RSA-3072");
    Ok(key)
}
fn key_fields(key: &RsaPublicKey) -> Result<Vec<u8>> {
    let mut result = key.n().to_bytes_le();
    result.resize(384, 0);
    let e: [u8; 4] = {
        let bytes = key.e().to_bytes_le();
        ensure!(bytes.len() <= 4, "RSA exponent exceeds u32");
        let mut field = [0; 4];
        field[..bytes.len()].copy_from_slice(&bytes);
        field
    };
    result.extend_from_slice(&e);
    let mut rr = ((BigUint::from(1u8) << 6144usize) % key.n()).to_bytes_le();
    rr.resize(384, 0);
    result.extend_from_slice(&rr);
    let n = u32::from_le_bytes(key.n().to_bytes_le()[..4].try_into()?);
    // Newton iteration doubles the number of correct low bits each step.
    let mut inv = 1u32;
    for _ in 0..5 {
        inv = inv.wrapping_mul(2u32.wrapping_sub(n.wrapping_mul(inv)));
    }
    result.extend_from_slice(&inv.wrapping_neg().to_le_bytes());
    Ok(result)
}
pub fn sign(image: &[u8], pem: &str) -> Result<Vec<u8>> {
    ensure!(!image.is_empty(), "empty ROM image");
    let key = private(pem)?;
    let mut signed = image.to_vec();
    signed.resize(image.len().next_multiple_of(SECTOR), 0xff);
    let digest = Sha256::digest(&signed);
    let mut block = vec![0; BLOCK];
    block[..4].copy_from_slice(&[0xe7, 2, 0, 0]);
    block[4..36].copy_from_slice(&digest);
    block[KEY_START..SIGNATURE_START].copy_from_slice(&key_fields(&key.to_public_key())?);
    let mut signature = key.sign_with_rng(&mut OsRng, Pss::new_with_salt::<Sha256>(32), &digest)?;
    signature.reverse();
    block[SIGNATURE_START..CRC].copy_from_slice(&signature);
    let crc = crc32fast::hash(&block[..CRC]);
    block[CRC..CRC + 4].copy_from_slice(&crc.to_le_bytes());
    signed.extend_from_slice(&block);
    signed.resize(signed.len() + SECTOR - BLOCK, 0xff);
    verify(&signed, pem)?;
    Ok(signed)
}
pub fn verify(signed: &[u8], pem: &str) -> Result<()> {
    ensure!(
        signed.len() >= 2 * SECTOR && signed.len().is_multiple_of(SECTOR),
        "invalid SBv2 size"
    );
    let (image, sector) = signed.split_at(signed.len() - SECTOR);
    ensure!(
        sector[..4] == [0xe7, 2, 0, 0]
            && sector[CRC + 4..BLOCK].iter().all(|b| *b == 0)
            && sector[BLOCK..].iter().all(|b| *b == 0xff),
        "invalid SBv2 header/padding"
    );
    ensure!(
        ferriboot_build::tools::word(sector, CRC)? == crc32fast::hash(&sector[..CRC]),
        "SBv2 CRC mismatch"
    );
    let digest = Sha256::digest(image);
    ensure!(sector[4..36] == digest[..], "SBv2 image digest mismatch");
    let key = public(pem)?;
    ensure!(
        sector[KEY_START..SIGNATURE_START] == key_fields(&key)?,
        "SBv2 trusted key mismatch"
    );
    let mut signature = sector[SIGNATURE_START..CRC].to_vec();
    signature.reverse();
    key.verify(Pss::new_with_salt::<Sha256>(32), &digest, &signature)
        .context("SBv2 RSA-PSS mismatch")
}

/// Fresh RSA-3072 `(private PKCS8 PEM, public SPKI PEM)` pair for development.
pub fn generate_pair() -> Result<(String, String)> {
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    let key = RsaPrivateKey::new(&mut OsRng, 3072)?;
    Ok((
        key.to_pkcs8_pem(LineEnding::LF)?.to_string(),
        key.to_public_key().to_public_key_pem(LineEnding::LF)?,
    ))
}
