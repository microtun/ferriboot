//! secp256k1 keys, signing and verification (k256 / RustCrypto).

use super::error::{Result, err};
use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};

/// Raw public key: X || Y (32 bytes each, big-endian), as stored in SIGNATURE items.
pub type RawPublicKey = [u8; 64];
/// Raw signature: r || s (32 bytes each, big-endian).
pub type RawSignature = [u8; 64];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn raw_public_key(pk: &PublicKey) -> RawPublicKey {
    let point = pk.to_encoded_point(false); // 0x04 || X || Y
    point.as_bytes()[1..].try_into().unwrap()
}

/// Sign a SHA-256 digest (RFC 6979 deterministic nonce, low-S).
pub fn sign_digest(key: &SecretKey, digest: &[u8; 32]) -> Result<RawSignature> {
    let sig: Signature = SigningKey::from(key)
        .sign_prehash(digest)
        .map_err(|e| err!(Unknown, "Signing failed: {e}"))?;
    Ok(sig.to_bytes().into())
}

/// Verify a raw r||s signature over a digest. High-S signatures are accepted, like mbedtls
/// and the bootrom do.
pub fn verify_digest(public_key: &[u8], signature: &[u8], digest: &[u8; 32]) -> bool {
    let point = [&[4u8][..], public_key].concat();
    let (Ok(vk), Ok(sig)) = (
        VerifyingKey::from_sec1_bytes(&point),
        Signature::from_slice(signature),
    ) else {
        return false;
    };
    // k256 0.13 returns Some(normalized) only for a high-S signature.
    let sig = sig.normalize_s().unwrap_or(sig);
    vk.verify_prehash(digest, &sig).is_ok()
}

/// DER-encoded ECDSA signature to raw r||s.
pub fn der_to_raw(der: &[u8]) -> Option<RawSignature> {
    Some(Signature::from_der(der).ok()?.to_bytes().into())
}

/// Raw r||s signature to DER.
pub fn raw_to_der(raw: &RawSignature) -> Vec<u8> {
    Signature::from_slice(raw)
        .map(|s| s.to_der().as_bytes().to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify() {
        let sk = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = raw_public_key(&sk.public_key());
        let digest = sha256(b"hello");
        let sig = sign_digest(&sk, &digest).unwrap();
        assert!(verify_digest(&pk, &sig, &digest));
        assert_eq!(der_to_raw(&raw_to_der(&sig)), Some(sig));
        assert!(!verify_digest(&pk, &sig, &sha256(b"other")));
    }
}
