//! RP2350 flat-BIN ROM seal/reseal implementation adapted from picotool.
#![allow(dead_code, clippy::all)]

mod crypto;
mod error;
mod memory;
mod model;
mod otp;
mod picobin;
mod reseal;
mod seal;
mod verify;

pub use otp::update_otp_json;
pub use reseal::reseal;
pub use seal::{SealOptions, seal};
pub use verify::{Verified, verify};

#[cfg(test)]
mod tests {
    use super::*;
    use k256::SecretKey;
    use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner};

    #[test]
    fn bin_hash_matches_picotool() {
        let opts = SealOptions {
            hash: true,
            major: 3,
            minor: 1,
            ..Default::default()
        };
        let input = include_bytes!("../../tests/data/flash.bin");
        let expected = include_bytes!("../../tests/data/flash.hash.picotool.bin");
        assert_eq!(
            seal(input, &opts, None).unwrap().data.as_slice(),
            expected.as_slice()
        );
    }

    #[test]
    fn external_sign_matches_picotool_and_reseals() {
        let input = include_bytes!("../../tests/data/no_flash_ram.bin");
        let expected = include_bytes!("../../tests/data/no_flash_ram.ext.picotool.bin");
        let expected_digest = include_bytes!("../../tests/data/no_flash_ram.ext.hash.bin");
        let opts = SealOptions {
            external_sign: true,
            bin_offset: Some(0x2000_0000),
            ..Default::default()
        };
        let output = seal(input, &opts, None).unwrap();
        assert_eq!(output.data.as_slice(), expected.as_slice());
        let digest = output.hash.unwrap();
        assert_eq!(digest.as_slice(), expected_digest.as_slice());
        let key = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let sig: k256::ecdsa::Signature = SigningKey::from(&key).sign_prehash(&digest).unwrap();
        let mut signed = output.data;
        reseal(
            &mut signed,
            0x2000_0000,
            sig.to_der().as_bytes(),
            &key.public_key(),
        )
        .unwrap();
        let result = verify(&signed, 0x2000_0000).unwrap();
        assert_eq!(result.hash, Verified::Passed);
        assert_eq!(result.signature, Verified::Passed);
        // Mutating any hashed byte must invalidate at least the digest.
        signed[0x200] ^= 1;
        assert_ne!(verify(&signed, 0x2000_0000).unwrap().hash, Verified::Passed);
    }

    #[test]
    fn incorrect_signature_does_not_modify_image() {
        let input = include_bytes!("../../tests/data/no_flash_ram.bin");
        let opts = SealOptions {
            external_sign: true,
            bin_offset: Some(0x2000_0000),
            ..Default::default()
        };
        let mut output = seal(input, &opts, None).unwrap().data;
        let original = output.clone();
        let key = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let sig: k256::ecdsa::Signature = SigningKey::from(&key).sign_prehash(&[1u8; 32]).unwrap();
        assert!(
            reseal(
                &mut output,
                0x2000_0000,
                sig.to_der().as_bytes(),
                &key.public_key()
            )
            .is_err()
        );
        assert_eq!(output, original);
    }
}
