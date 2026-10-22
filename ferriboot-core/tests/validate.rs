mod common;

use common::*;
use ferriboot_core::{
    Error, ImageError, ImageVersion, Policy, PublicKey,
    image::{flags, tlv},
    security_config_fingerprint,
    trailer::{Flag, MagicState, Trailer},
    validate::validate_image,
};

fn slot_with(image: &[u8]) -> SimFlash {
    let f = SimFlash::new(SLOT_SIZE, &Power::default());
    f.program(0, image);
    f
}

fn max() -> u32 {
    Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32)
        .unwrap()
        .max_image_size()
}

fn imgtool_key() -> PublicKey {
    PublicKey::from_spki_der(include_bytes!("data/test-p256.pub.der")).unwrap()
}

fn replace_signature(image: &mut Vec<u8>, replacement: &[u8], kind: u16) {
    let hdr = u16::from_le_bytes(image[8..10].try_into().unwrap()) as usize;
    let prot = u16::from_le_bytes(image[10..12].try_into().unwrap()) as usize;
    let payload = u32::from_le_bytes(image[12..16].try_into().unwrap()) as usize;
    let start = hdr + payload + prot;
    let mut off = start + 4;
    while off < image.len() {
        let tag = u16::from_le_bytes(image[off..off + 2].try_into().unwrap());
        let len = u16::from_le_bytes(image[off + 2..off + 4].try_into().unwrap()) as usize;
        if tag == tlv::ECDSA_SIG {
            image.splice(off + 4..off + 4 + len, replacement.iter().copied());
            image[off..off + 2].copy_from_slice(&kind.to_le_bytes());
            image[off + 2..off + 4].copy_from_slice(&(replacement.len() as u16).to_le_bytes());
            let total = (image.len() - start) as u16;
            image[start + 2..start + 4].copy_from_slice(&total.to_le_bytes());
            return;
        }
        off += 4 + len;
    }
    panic!("missing signature");
}

#[test]
fn p256_public_key_encoding_and_validation() {
    use sha2::{Digest as _, Sha256};
    let der = include_bytes!("data/test-p256.pub.der");
    let key = imgtool_key();
    assert_eq!(key.key_hash().as_slice(), &Sha256::digest(der)[..]);
    assert_eq!(key.as_bytes().as_slice(), &der[26..]);
    assert_eq!(
        PublicKey::from_raw(key.as_bytes()).unwrap().key_hash(),
        key.key_hash()
    );
    for len in [0, 26, 90, 92] {
        let mut truncated = der.to_vec();
        truncated.resize(len, 0);
        assert!(PublicKey::from_spki_der(&truncated).is_err());
    }
    let mut wrong_curve = *der;
    wrong_curve[22] ^= 1;
    assert!(PublicKey::from_spki_der(&wrong_curve).is_err());
    assert!(PublicKey::from_raw(&[0u8; 65]).is_err());
    let mut off_curve = [0u8; 65];
    off_curve[0] = 4;
    assert!(PublicKey::from_raw(&off_curve).is_err());
}

#[test]
fn security_config_fingerprint_tracks_effective_authentication_config() {
    let keys = [public_key(&other_signing_key()), public_key(&signing_key())];
    let base = Policy::default();
    let fingerprint = security_config_fingerprint(&keys, &base);

    let reversed = [keys[1].clone(), keys[0].clone()];
    assert_ne!(fingerprint, security_config_fingerprint(&reversed, &base));

    for changed in [
        Policy {
            min_version: Some(ImageVersion::new(1, 2, 3, 0)),
            ..base
        },
        Policy {
            min_security_counter: Some(7),
            ..base
        },
        Policy {
            vid: Some([1; 16]),
            ..base
        },
        Policy {
            cid: Some([2; 16]),
            ..base
        },
    ] {
        assert_ne!(fingerprint, security_config_fingerprint(&keys, &changed));
    }

    // The build component is intentionally not part of min-version semantics.
    let min_a = Policy {
        min_version: Some(ImageVersion::new(1, 2, 3, 4)),
        ..base
    };
    let min_b = Policy {
        min_version: Some(ImageVersion::new(1, 2, 3, 99)),
        ..base
    };
    assert_eq!(
        security_config_fingerprint(&keys, &min_a),
        security_config_fingerprint(&keys, &min_b)
    );

    // Lock the wire-handoff fingerprint encoding across bootloader/application
    // version skew. This key is the checked-in imgtool SPKI fixture.
    let vector_policy = Policy {
        min_version: Some(ImageVersion::new(1, 2, 3, 99)),
        min_security_counter: Some(7),
        vid: Some([1; 16]),
        cid: Some([2; 16]),
    };
    let vector = security_config_fingerprint(&[imgtool_key()], &vector_policy);
    assert_eq!(
        *vector.as_bytes(),
        [
            0x02, 0x3c, 0x51, 0xdc, 0x7a, 0xec, 0xf7, 0x34, 0xbc, 0xf2, 0xc1, 0xee, 0xce, 0x5d,
            0x4d, 0xa1, 0xc0, 0xcf, 0x58, 0x9b, 0xd7, 0x26, 0x8f, 0xff, 0x0c, 0xa4, 0x45, 0x06,
            0x77, 0x01, 0xcc, 0xf5,
        ]
    );
}

#[test]
fn ecdsa_requires_der_and_a_single_image_hash() {
    use p256::ecdsa::{
        Signature,
        signature::{Signer, hazmat::PrehashSigner},
    };
    use sha2::{Digest as _, Sha256};
    let base = build_image(&Spec::new((1, 0, 0, 0), 5000, 1));
    let keys = [public_key(&signing_key())];
    let digest = Sha256::digest(&base[..32 + 5000]);
    let check =
        |image: &[u8]| validate_image(&mut slot_with(image), &keys, &Policy::default(), max());
    let signature: Signature = signing_key().sign_prehash(&digest).unwrap();
    let mut image = base.clone();
    replace_signature(&mut image, signature.to_der().as_bytes(), tlv::ECDSA_SIG);
    check(&image).unwrap();

    // Signer hashes the already computed digest again, which must not authenticate.
    let double_hash: Signature = signing_key().sign(&digest);
    replace_signature(&mut image, double_hash.to_der().as_bytes(), tlv::ECDSA_SIG);
    assert_eq!(
        check(&image),
        Err(Error::Image(ImageError::SignatureInvalid))
    );
    for bad in [vec![], vec![0; 73]] {
        let mut image = base.clone();
        replace_signature(&mut image, &bad, tlv::ECDSA_SIG);
        assert_eq!(
            check(&image),
            Err(Error::Image(ImageError::InvalidTlvLength(tlv::ECDSA_SIG)))
        );
    }
    for bad in [vec![0; 70], signature.to_bytes().to_vec()] {
        let mut image = base.clone();
        replace_signature(&mut image, &bad, tlv::ECDSA_SIG);
        assert_eq!(
            check(&image),
            Err(Error::Image(ImageError::SignatureInvalid))
        );
    }
    // A legacy Ed25519 TLV is recognised by the parser but cannot satisfy P-256 validation.
    replace_signature(&mut image, &[0; 64], tlv::ED25519);
    assert_eq!(
        check(&image),
        Err(Error::Image(ImageError::MissingSignature))
    );
}

#[test]
fn trailer_layout_matches_upstream() {
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    let s = SLOT_SIZE as u32;
    assert_eq!(t.magic_off(), s - 16);
    assert_eq!(t.image_ok_off(), s - 24);
    assert_eq!(t.copy_done_off(), s - 32);
    assert_eq!(t.swap_info_off(), s - 40);
    assert_eq!(t.swap_size_off(), s - 48);
    // 32 sectors * 3 states * 4 bytes + 48 bytes of fields.
    assert_eq!(t.total_size(), 32 * 3 * 4 + 48);
    assert_eq!(t.max_image_size(), s - SECTOR as u32);
}

#[test]
fn accepts_imgtool_image_with_policy() {
    let mut f = slot_with(include_bytes!("data/signed.bin"));
    let policy = Policy {
        min_version: Some(ImageVersion::new(1, 2, 0, 0)),
        min_security_counter: Some(7),
        ..Policy::default()
    }
    .with_imgtool_ids("example.com", "esp32c3-app")
    .unwrap();
    let keys = [public_key(&other_signing_key()), imgtool_key()];
    let v = validate_image(&mut f, &keys, &policy, max()).unwrap();
    assert_eq!(v.key_index(), 1);
    assert_eq!(v.header().hdr_size, 0x1000);
    assert_eq!(v.header().version, ImageVersion::new(1, 2, 3, 4));
    assert_eq!(v.security_counter(), Some(7));
    assert_eq!(v.size() as usize, include_bytes!("data/signed.bin").len());

    let wrong = Policy::default()
        .with_imgtool_ids("example.com", "other")
        .unwrap();
    assert_eq!(
        validate_image(&mut f, &keys, &wrong, max()),
        Err(Error::Image(ImageError::WrongCid))
    );
    let too_new = Policy {
        min_version: Some(ImageVersion::new(1, 3, 0, 0)),
        ..Policy::default()
    };
    assert_eq!(
        validate_image(&mut f, &keys, &too_new, max()),
        Err(Error::Image(ImageError::VersionTooLow))
    );
    let counter = Policy {
        min_security_counter: Some(8),
        ..Policy::default()
    };
    assert_eq!(
        validate_image(&mut f, &keys, &counter, max()),
        Err(Error::Image(ImageError::SecurityCounterTooLow))
    );
    let only_other = [public_key(&other_signing_key())];
    assert_eq!(
        validate_image(&mut f, &only_other, &Policy::default(), max()),
        Err(Error::Image(ImageError::UnknownKey))
    );
}

#[test]
fn accepts_imgtool_full_public_key_tlv() {
    let mut f = slot_with(include_bytes!("data/signed-pubkey.bin"));
    let v = validate_image(&mut f, &[imgtool_key()], &Policy::default(), max()).unwrap();
    assert_eq!(v.header().version, ImageVersion::new(0, 9, 0, 0));
}

#[test]
fn reads_imgtool_confirmed_trailer() {
    let mut f = slot_with(include_bytes!("data/signed-padded-confirmed.bin"));
    let t = Trailer::for_slot::<SimFlash>(SLOT_SIZE as u32).unwrap();
    let s = t.read_state(&mut f).unwrap();
    assert_eq!(s.magic, MagicState::Good);
    assert_eq!(s.image_ok, Flag::Set);
    assert_eq!(s.copy_done, Flag::Unset);
    validate_image(&mut f, &[imgtool_key()], &Policy::default(), max()).unwrap();
}

#[test]
fn detects_tampering() {
    let keys = [public_key(&signing_key())];
    let spec = Spec::new((1, 0, 0, 0), 5000, 1);

    let mut img = build_image(&spec);
    validate_image(&mut slot_with(&img), &keys, &Policy::default(), max()).unwrap();

    img[0x20 + 100] ^= 1;
    assert_eq!(
        validate_image(&mut slot_with(&img), &keys, &Policy::default(), max()),
        Err(Error::Image(ImageError::HashMismatch))
    );

    let mut bad_sig = spec.clone();
    bad_sig.corrupt_signature = true;
    assert_eq!(
        validate_image(
            &mut slot_with(&build_image(&bad_sig)),
            &keys,
            &Policy::default(),
            max()
        ),
        Err(Error::Image(ImageError::SignatureInvalid))
    );

    let mut sneaky = spec.clone();
    sneaky.extra_unprotected = Some(tlv::SEC_CNT);
    assert_eq!(
        validate_image(
            &mut slot_with(&build_image(&sneaky)),
            &keys,
            &Policy::default(),
            max()
        ),
        Err(Error::Image(ImageError::UnprotectedTlvNotAllowed(
            tlv::SEC_CNT
        )))
    );

    // Upstream never allows the decompression TLVs outside the protected
    // area (imgtool puts them in the protected area).
    let mut unprot_decomp = spec.clone();
    unprot_decomp.extra_unprotected = Some(tlv::DECOMP_SIZE);
    assert_eq!(
        validate_image(
            &mut slot_with(&build_image(&unprot_decomp)),
            &keys,
            &Policy::default(),
            max()
        ),
        Err(Error::Image(ImageError::UnprotectedTlvNotAllowed(
            tlv::DECOMP_SIZE
        )))
    );

    // imgtool `--pure` puts SIG_PURE in the unprotected area.
    let mut pure = spec.clone();
    pure.extra_unprotected = Some(tlv::SIG_PURE);
    assert_eq!(
        validate_image(
            &mut slot_with(&build_image(&pure)),
            &keys,
            &Policy::default(),
            max()
        ),
        Err(Error::Image(ImageError::UnsupportedPureSignature))
    );

    let mut odd_hdr = spec.clone();
    odd_hdr.hdr_size = 0x200;
    odd_hdr.payload.truncate(4093); // unaligned TLV area
    validate_image(
        &mut slot_with(&build_image(&odd_hdr)),
        &keys,
        &Policy::default(),
        max(),
    )
    .unwrap();

    let big = Spec::new((1, 0, 0, 0), SLOT_SIZE - SECTOR, 1);
    assert_eq!(
        validate_image(
            &mut slot_with(&build_image(&big)),
            &keys,
            &Policy::default(),
            max()
        ),
        Err(Error::Image(ImageError::ImageTooLarge))
    );

    assert_eq!(
        validate_image(
            &mut SimFlash::new(SLOT_SIZE, &Power::default()),
            &keys,
            &Policy::default(),
            max()
        ),
        Err(Error::Image(ImageError::NoImage))
    );
}

#[test]
fn follows_upstream_tlv_semantics() {
    let keys = [public_key(&signing_key()), public_key(&other_signing_key())];
    let base = Spec::new((1, 0, 0, 0), 3000, 9);
    let check = |spec: &Spec| {
        validate_image(
            &mut slot_with(&build_image(spec)),
            &keys,
            &Policy::default(),
            max(),
        )
    };

    // A signature by an unknown key is skipped; a later one by a trusted
    // key is used (`bootutil_find_key` returns -1, the signature is ignored).
    let mut s = base.clone();
    s.key = untrusted_signing_key();
    assert_eq!(check(&s), Err(Error::Image(ImageError::UnknownKey)));
    s.extra_signers.push((other_signing_key(), false));
    assert_eq!(check(&s).unwrap().key_index(), 1);

    // The last checked signature decides, as upstream overwrites
    // `valid_signature` for every signature it verifies.
    let mut s = base.clone();
    s.extra_signers.push((other_signing_key(), true));
    assert_eq!(check(&s), Err(Error::Image(ImageError::SignatureInvalid)));
    let mut s = base.clone();
    s.corrupt_signature = true;
    s.extra_signers.push((other_signing_key(), false));
    assert_eq!(check(&s).unwrap().key_index(), 1);

    // A signature is paired with the key TLV in front of it.
    let mut s = base.clone();
    s.signature_before_key = true;
    assert_eq!(check(&s), Err(Error::Image(ImageError::MissingSignature)));

    // Single-image MCUboot ignores dependency TLVs.
    let mut s = base.clone();
    s.extra_protected
        .push((tlv::DEPENDENCY, vec![1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]));
    check(&s).unwrap();

    // Only encryption and compression flags make an image unusable;
    // RAM_LOAD (imgtool --load-addr) and unknown flags are ignored.
    let mut s = base.clone();
    s.flags = flags::RAM_LOAD | 0x8000_0000;
    s.load_addr = 0x3fc8_0000;
    check(&s).unwrap();
    for f in [flags::ENCRYPTED_AES128, flags::COMPRESSED_LZMA2] {
        let mut s = base.clone();
        s.flags = f;
        assert_eq!(
            check(&s),
            Err(Error::Image(ImageError::UnsupportedFlags(f)))
        );
    }

    // An erased header means "no image"; anything else that is not a valid
    // header is a corrupt image.
    let mut img = build_image(&base);
    img[0] = 0;
    assert_eq!(
        validate_image(&mut slot_with(&img), &keys, &Policy::default(), max()),
        Err(Error::Image(ImageError::InvalidHeader))
    );
}
