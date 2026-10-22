//! Image validation: a port of MCUboot's `bootutil_img_validate` for
//! SHA-256 + ECDSA P-256 images, reading the image back from flash.

use embedded_storage::nor_flash::ReadNorFlash;
use p256::ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier};
use sha2::{Digest as _, Sha256};

use crate::{
    Error, ImageError,
    fih::{self, Fih},
    flash,
    image::{IMAGE_HEADER_SIZE, ImageHeader, ImageVersion, TlvIter, tlv},
};

/// DER `SubjectPublicKeyInfo` prefix for ECDSA P-256. imgtool hashes
/// `prefix || raw_key` to produce the `KEYHASH` TLV, and embeds the same
/// encoding in a `PUBKEY` TLV.
pub const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// A trusted ECDSA P-256 public key together with its MCUboot key hash.
#[derive(Clone, Debug)]
pub struct PublicKey {
    key: VerifyingKey,
    raw: [u8; 65],
    hash: [u8; 32],
}

impl PublicKey {
    /// From a 65-byte uncompressed SEC1 point (`0x04 || X || Y`).
    pub fn from_raw(raw: &[u8; 65]) -> Result<Self, ImageError> {
        let key = VerifyingKey::from_sec1_bytes(raw).map_err(|_| ImageError::InvalidPublicKey)?;
        let mut h = Sha256::new();
        h.update(P256_SPKI_PREFIX);
        h.update(raw);
        Ok(Self {
            key,
            raw: *raw,
            hash: h.finalize().into(),
        })
    }

    /// From the 91-byte uncompressed DER `SubjectPublicKeyInfo` emitted by imgtool.
    pub fn from_spki_der(der: &[u8]) -> Result<Self, ImageError> {
        let raw = der
            .strip_prefix(&P256_SPKI_PREFIX)
            .and_then(|r| <&[u8; 65]>::try_from(r).ok())
            .ok_or(ImageError::InvalidPublicKey)?;
        Self::from_raw(raw)
    }

    /// SHA-256 of the DER encoding, as stored in the `KEYHASH` TLV.
    pub fn key_hash(&self) -> &[u8; 32] {
        &self.hash
    }

    pub fn as_bytes(&self) -> &[u8; 65] {
        &self.raw
    }
}

/// Policy applied to otherwise authentic images.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Reject images whose `major.minor.revision` is lower than this.
    pub min_version: Option<ImageVersion>,
    /// Require a protected `SEC_CNT` TLV of at least this value.
    pub min_security_counter: Option<u32>,
    /// Require a protected `UUID_VID` TLV with this value.
    pub vid: Option<[u8; 16]>,
    /// Require a protected `UUID_CID` TLV with this value.
    pub cid: Option<[u8; 16]>,
}

impl Policy {
    /// Pin the vendor/class UUIDs to the values imgtool derives for
    /// `imgtool sign --vid <vid> --cid <cid>`
    /// Literal canonical/compact UUIDs are preserved. Other names use UUIDv5.
    #[cfg(feature = "uuid")]
    pub fn with_imgtool_ids(mut self, vid: &str, cid: &str) -> Result<Self, uuid::Error> {
        let (v, c) = imgtool_uuids(vid, cid)?;
        self.vid = Some(v);
        self.cid = Some(c);
        Ok(self)
    }
}

/// Stable digest of the image-authentication configuration. This is not a
/// signature or trust anchor. Ferriboot uses it for the RP2350 bootloader
/// internal prepared-plan recheck, **not** for the shared BootInfo ABI.
///
/// Key order is intentionally part of the configuration. Release tooling
/// should provision bootloader and application keys consistently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityConfigFingerprint([u8; 32]);

impl SecurityConfigFingerprint {
    pub const LEN: usize = 32;

    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

/// Fingerprint the trusted P-256 key ring and every policy field that affects
/// [`validate_image`]. The encoding is domain-separated and versioned so its
/// meaning can evolve without silently equating different configurations.
///
/// `ImageVersion::build` is omitted because minimum-version enforcement
/// compares only `major.minor.revision`.
pub fn security_config_fingerprint(
    keys: &[PublicKey],
    policy: &Policy,
) -> SecurityConfigFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(b"ferriboot/security-config/p256/v1\0");

    hasher.update(b"keys\0");
    hasher.update((keys.len() as u64).to_le_bytes());
    for key in keys {
        hasher.update(key.key_hash());
    }
    hasher.update(b"policy\0");

    match policy.min_version {
        Some(version) => {
            hasher.update([1, version.major, version.minor]);
            hasher.update(version.revision.to_le_bytes());
        }
        None => hasher.update([0]),
    }
    match policy.min_security_counter {
        Some(counter) => {
            hasher.update([1]);
            hasher.update(counter.to_le_bytes());
        }
        None => hasher.update([0]),
    }
    match policy.vid {
        Some(vid) => {
            hasher.update([1]);
            hasher.update(vid);
        }
        None => hasher.update([0]),
    }
    match policy.cid {
        Some(cid) => {
            hasher.update([1]);
            hasher.update(cid);
        }
        None => hasher.update([0]),
    }

    SecurityConfigFingerprint(hasher.finalize().into())
}

/// Derive the UUIDs that `imgtool sign --vid <vid> --cid <cid>` embeds.
#[cfg(feature = "uuid")]
pub fn imgtool_uuids(vid: &str, cid: &str) -> Result<([u8; 16], [u8; 16]), uuid::Error> {
    use uuid::Uuid;
    fn parse(namespace: &Uuid, value: &str) -> Result<Uuid, uuid::Error> {
        match value.len() {
            32 | 36 => Uuid::parse_str(value),
            _ => Ok(Uuid::new_v5(namespace, value.as_bytes())),
        }
    }
    let v = parse(&Uuid::NAMESPACE_DNS, vid)?;
    let c = parse(&v, cid)?;
    Ok((v.into_bytes(), c.into_bytes()))
}

/// Proof that the image in a slot was authenticated.
///
/// Only [`validate_image`] (and the updater and bootloader built on it) can
/// construct this value, so holding one means the digest, key, signature and
/// policy were checked. It describes the slot contents at the moment of
/// validation: the bootloader refuses to boot
/// an image whose slot has been modified since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedImage {
    header: ImageHeader,
    size: u32,
    digest: [u8; 32],
    key_index: usize,
    security_counter: Option<u32>,
    vid: Option<[u8; 16]>,
    cid: Option<[u8; 16]>,
    /// The updater's update-slot revision when this image was read from it;
    /// `None` for images authenticated anywhere else.
    update_revision: Option<u64>,
}

impl ValidatedImage {
    /// The authenticated image header.
    pub fn header(&self) -> &ImageHeader {
        &self.header
    }

    /// Total image size including TLVs.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// SHA-256 covered by the signature.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Index into the key ring of the key that signed the image.
    pub fn key_index(&self) -> usize {
        self.key_index
    }

    /// Value of the protected `SEC_CNT` TLV, if present.
    pub fn security_counter(&self) -> Option<u32> {
        self.security_counter
    }

    /// Value of the protected `UUID_VID` TLV, if present.
    pub fn vid(&self) -> Option<[u8; 16]> {
        self.vid
    }

    /// Value of the protected `UUID_CID` TLV, if present.
    pub fn cid(&self) -> Option<[u8; 16]> {
        self.cid
    }

    /// Whether an independent second authentication produced the same
    /// result, compared field by field without data-dependent branches.
    ///
    /// The digest covers the header bytes on flash, but the *parsed* header
    /// is compared too: it is what the boards use to locate vectors and size
    /// the image, and a fault while parsing it the first time would otherwise
    /// go unnoticed.
    pub fn agrees_with(&self, again: &ValidatedImage) -> Fih {
        let (a, b) = (&self.header, &again.header);
        Fih::eq(&self.digest, &again.digest)
            & Fih::eq_u32(self.size, again.size)
            & Fih::eq_u32(self.key_index as u32, again.key_index as u32)
            & fih_opt_u32(self.security_counter, again.security_counter)
            & fih_opt_id(&self.vid, &again.vid)
            & fih_opt_id(&self.cid, &again.cid)
            & Fih::eq_u32(a.load_addr, b.load_addr)
            & Fih::eq_u32(u32::from(a.hdr_size), u32::from(b.hdr_size))
            & Fih::eq_u32(u32::from(a.protect_tlv_size), u32::from(b.protect_tlv_size))
            & Fih::eq_u32(a.img_size, b.img_size)
            & Fih::eq_u32(a.flags, b.flags)
            & Fih::eq_u32(version_word(&a.version), version_word(&b.version))
            & Fih::eq_u32(a.version.build, b.version.build)
    }
}

fn version_word(v: &ImageVersion) -> u32 {
    u32::from(v.major) << 24 | u32::from(v.minor) << 16 | u32::from(v.revision)
}

fn fih_opt_u32(a: Option<u32>, b: Option<u32>) -> Fih {
    Fih::eq_u32(u32::from(a.is_some()), u32::from(b.is_some()))
        & Fih::eq_u32(a.unwrap_or(0), b.unwrap_or(0))
}

fn fih_opt_id(a: &Option<[u8; 16]>, b: &Option<[u8; 16]>) -> Fih {
    Fih::eq_u32(u32::from(a.is_some()), u32::from(b.is_some()))
        & Fih::eq(&a.unwrap_or_default(), &b.unwrap_or_default())
}

/// Authenticate the image in `flash` a second time and compare the result
/// with `first`, for use immediately before an irreversible step.
///
/// The two runs share no state and happen at different times, so a single
/// fault that made the first one accept an image cannot also make this one
/// agree. Any validation error, including a flash error, is a failure.
/// Consume the verdict with [`Fih::require`].
pub fn reauthenticate<F: ReadNorFlash>(
    flash: &mut F,
    keys: &[PublicKey],
    policy: &Policy,
    max_size: u32,
    first: &ValidatedImage,
) -> Fih {
    let again = validate_image(flash, keys, policy, max_size);
    Fih::ok(&again) & again.map_or(fih::FAIL, |again| first.agrees_with(&again))
}

fn set_once<const N: usize, E>(
    slot: &mut Option<[u8; N]>,
    kind: u16,
    value: [u8; N],
) -> Result<(), Error<E>> {
    if slot.is_some() {
        return Err(ImageError::DuplicateTlv(kind).into());
    }
    *slot = Some(value);
    Ok(())
}

fn read_value<F: ReadNorFlash, const N: usize>(
    flash: &mut F,
    entry: &crate::image::TlvEntry,
) -> Result<[u8; N], Error<F::Error>> {
    if usize::from(entry.len) != N {
        return Err(ImageError::InvalidTlvLength(entry.kind).into());
    }
    let mut out = [0u8; N];
    flash::read(flash, entry.value_offset, &mut out)?;
    Ok(out)
}

fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Hash `[0, len)` of a slot.
fn hash_region<F: ReadNorFlash>(flash: &mut F, len: u32) -> Result<[u8; 32], Error<F::Error>> {
    let rs = F::READ_SIZE.max(1) as u32;
    let mut hasher = Sha256::new();
    let mut buf = flash::Aligned([0u8; flash::CHUNK]);
    let chunk = (flash::CHUNK as u32) / rs * rs;
    let mut off = 0u32;
    while off < len {
        let take = (len - off).min(chunk);
        let aligned = take / rs * rs;
        if aligned > 0 {
            flash::read(flash, off, &mut buf.0[..aligned as usize])?;
            hasher.update(&buf.0[..aligned as usize]);
            off += aligned;
        } else {
            flash::read(flash, off, &mut buf.0[..take as usize])?;
            hasher.update(&buf.0[..take as usize]);
            off += take;
        }
    }
    Ok(hasher.finalize().into())
}

/// Read the header, distinguishing an erased slot from a corrupt header
/// (`boot_check_header_erased` versus `boot_check_header_valid`). Upstream
/// erases a secondary slot whose header is neither.
fn read_header<F: ReadNorFlash>(flash: &mut F) -> Result<ImageHeader, Error<F::Error>> {
    let mut raw = flash::Aligned([0u8; IMAGE_HEADER_SIZE]);
    flash::read(flash, 0, &mut raw.0)?;
    match ImageHeader::parse(&raw.0) {
        Some(header) => Ok(header),
        None if flash::is_erased(&raw.0[..4]) => Err(ImageError::NoImage.into()),
        None => Err(ImageError::InvalidHeader.into()),
    }
}

/// Validate the image at offset 0 of `flash`.
///
/// `max_size` is the largest permitted total image size (the slot size minus
/// its trailer sectors). The returned [`ValidatedImage`] is only produced if the
/// digest, key and signature check out and `policy` is satisfied.
pub fn validate_image<F: ReadNorFlash>(
    flash: &mut F,
    keys: &[PublicKey],
    policy: &Policy,
    max_size: u32,
) -> Result<ValidatedImage, Error<F::Error>> {
    let header = read_header(flash)?;
    header.check()?;
    let mut it = TlvIter::begin(flash, &header, max_size)?;
    let size = it.total_size();

    let hashed_len = header.hashed_len().ok_or(ImageError::InvalidHeader)?;
    let digest = hash_region(flash, hashed_len)?;

    // Like `bootutil_img_validate`, every SHA-256 TLV must match, and each
    // signature is checked with the key named by the key TLV immediately
    // preceding it. A signature whose key is unknown (or that has no key TLV
    // in front of it) is skipped. The outcome of the last checked signature
    // decides, as upstream overwrites `valid_signature` for each one.
    let mut hash_ok: Option<bool> = None;
    let mut saw_key_tlv = false;
    let mut saw_known_key = false;
    let mut current_key: Option<usize> = None;
    let mut verdict: Option<Result<usize, ImageError>> = None;
    let mut sec_cnt: Option<[u8; 4]> = None;
    let mut vid: Option<[u8; 16]> = None;
    let mut cid: Option<[u8; 16]> = None;

    while let Some(e) = it.next(flash)? {
        // imgtool `--pure` puts SIG_PURE in the unprotected area. Report the
        // unsupported signature mode rather than a generic allow-list error.
        if e.kind == tlv::SIG_PURE {
            return Err(ImageError::UnsupportedPureSignature.into());
        }
        if !e.protected && !tlv::ALLOWED_UNPROTECTED.contains(&e.kind) {
            return Err(ImageError::UnprotectedTlvNotAllowed(e.kind).into());
        }
        // Upstream recognises the hash, key and signature TLVs in either
        // area; imgtool always places them in the unprotected one.
        match e.kind {
            tlv::SHA256 => {
                let v: [u8; 32] = read_value(flash, &e)?;
                let ok = ct_eq(&v, &digest);
                hash_ok = Some(hash_ok.unwrap_or(true) && ok);
            }
            tlv::KEYHASH => {
                // Upstream compares a prefix of up to 32 bytes; requiring the
                // full SHA-256 is deliberately stricter.
                let v: [u8; 32] = read_value(flash, &e)?;
                saw_key_tlv = true;
                current_key = keys.iter().position(|k| ct_eq(k.key_hash(), &v));
                saw_known_key |= current_key.is_some();
            }
            tlv::PUBKEY => {
                // The full DER key, matched by its hash (`MCUBOOT_HW_KEY`).
                let mut der = [0u8; 91];
                let len = usize::from(e.len);
                if len > der.len() {
                    return Err(ImageError::InvalidTlvLength(e.kind).into());
                }
                flash::read(flash, e.value_offset, &mut der[..len])?;
                let h: [u8; 32] = Sha256::digest(&der[..len]).into();
                saw_key_tlv = true;
                current_key = keys.iter().position(|k| ct_eq(k.key_hash(), &h));
                saw_known_key |= current_key.is_some();
            }
            tlv::ECDSA_SIG => {
                let Some(index) = current_key.take() else {
                    continue;
                };
                // imgtool emits ASN.1 DER, not a fixed-size r || s signature.
                // Bound the read before parsing and verify the existing image
                // digest directly: Verifier::verify would hash it a second time.
                let len = usize::from(e.len);
                if !(8..=72).contains(&len) {
                    return Err(ImageError::InvalidTlvLength(e.kind).into());
                }
                let mut der = [0u8; 72];
                flash::read(flash, e.value_offset, &mut der[..len])?;
                verdict = Some(
                    Signature::from_der(&der[..len])
                        .and_then(|sig| keys[index].key.verify_prehash(&digest, &sig))
                        .map(|()| index)
                        .map_err(|_| ImageError::SignatureInvalid),
                );
            }
            // The allow-list above guarantees these three are protected.
            tlv::SEC_CNT => {
                let v = read_value(flash, &e)?;
                set_once(&mut sec_cnt, e.kind, v)?;
            }
            tlv::UUID_VID => {
                let v = read_value(flash, &e)?;
                set_once(&mut vid, e.kind, v)?;
            }
            tlv::UUID_CID => {
                let v = read_value(flash, &e)?;
                set_once(&mut cid, e.kind, v)?;
            }
            // Dependencies only matter to multi-image builds, which ignore
            // them for a single image, as do other signature algorithms,
            // alternative hashes and custom protected TLVs.
            _ => {}
        }
    }

    match hash_ok {
        None => return Err(ImageError::MissingSha256.into()),
        Some(false) => return Err(ImageError::HashMismatch.into()),
        Some(true) => {}
    }

    let key_index = match verdict {
        Some(result) => result?,
        None if !saw_key_tlv => return Err(ImageError::MissingKeyHash.into()),
        None if !saw_known_key => return Err(ImageError::UnknownKey.into()),
        None => return Err(ImageError::MissingSignature.into()),
    };

    // Everything below is policy about an image that is now known to be
    // authentic.
    let security_counter = sec_cnt.map(u32::from_le_bytes);
    if let Some(min) = policy.min_security_counter {
        let have = security_counter.ok_or(ImageError::MissingSecurityCounter)?;
        if have < min {
            return Err(ImageError::SecurityCounterTooLow.into());
        }
    }
    if let Some(expected_vid) = policy.vid
        && vid.ok_or(ImageError::MissingVid)? != expected_vid
    {
        return Err(ImageError::WrongVid.into());
    }
    if let Some(expected_cid) = policy.cid
        && cid.ok_or(ImageError::MissingCid)? != expected_cid
    {
        return Err(ImageError::WrongCid.into());
    }
    if let Some(min) = policy.min_version
        && header.version.semver_cmp(&min).is_lt()
    {
        return Err(ImageError::VersionTooLow.into());
    }

    Ok(ValidatedImage {
        header,
        size,
        digest,
        key_index,
        security_counter,
        vid,
        cid,
        update_revision: None,
    })
}
