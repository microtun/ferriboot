//! MCUboot signing with upstream `imgtool`, plus envelope mutations.
//!
//! Arguments follow `ferriboot_build::tools::mcuboot`, which the boards' Rust signing tools use;
//! the slot geometry comes from the board's [`Signing`]. Set `IMGTOOL` to
//! override the executable. HIL signs ordinary images at the actual DUT
//! security-counter floor; explicit signing arguments take precedence.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::{Signing, artifacts::repo_root};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// `keys/dev-p256.pem`, trusted by the test builds.
    Dev,
    /// A freshly generated P-256 key the bootloader does not trust.
    Untrusted,
}

impl Key {
    pub fn path(self) -> PathBuf {
        match self {
            Key::Dev => repo_root().join("keys/dev-p256.pem"),
            Key::Untrusted => untrusted_key().to_path_buf(),
        }
    }
}

fn imgtool() -> String {
    std::env::var("IMGTOOL").unwrap_or_else(|_| "imgtool".into())
}

/// Read the current security epoch from the device under test, not an
/// independently configured value. Cache it for ordinary fixtures so every
/// image in this test process uses the same floor. An incrementing test reads
/// its dedicated QEMU board separately.
pub fn initial_security_counter() -> crate::Result<u32> {
    static FLOOR: OnceLock<Result<u32, String>> = OnceLock::new();
    match FLOOR.get_or_init(|| read_initial_security_counter().map_err(|e| e.to_string())) {
        Ok(value) => Ok(*value),
        Err(error) => Err(error.clone().into()),
    }
}

fn read_initial_security_counter() -> crate::Result<u32> {
    #[cfg(feature = "qemu")]
    {
        // Ordinary QEMU boards start with freshly initialized eFuse storage.
        crate::qemu::VirtualBoard::new(0)?.security_counter()
    }
    #[cfg(any(feature = "esp32c3", feature = "rp2350"))]
    {
        // No firmware compilation, image composition, flashing, or bootloader
        // output: both physical backends read the chip's fuses directly.
        #[cfg(feature = "esp32c3")]
        let board = &crate::esp32c3::BOARD;
        #[cfg(feature = "rp2350")]
        let board = &crate::rp2350::BOARD;
        let port_var = board.env("PORT");
        let port = std::env::var(&port_var)
            .map_err(|_| format!("set {port_var} to {}", board.console.port_hint))?;
        let _lock = crate::board_lock::BoardLock::acquire(board, &port)?;
        #[cfg(feature = "esp32c3")]
        return crate::esp32c3::security_counter::read();
        #[cfg(feature = "rp2350")]
        return crate::rp2350::probe::read_security_counter();
    }
    #[cfg(not(any(feature = "esp32c3", feature = "rp2350", feature = "qemu")))]
    Err("select a HIL board feature to read the security counter".into())
}

fn untrusted_key() -> &'static Path {
    static KEY: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &KEY.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("untrusted-p256.pem");
        run(Command::new(imgtool())
            .args(["keygen", "-t", "ecdsa-p256", "-k"])
            .arg(&path))
        .expect("imgtool keygen");
        (dir, path)
    })
    .1
}

fn run(cmd: &mut Command) -> crate::Result<()> {
    crate::process::run(cmd, "imgtool", Duration::from_secs(60))
        .map_err(|e| format!("{e} (install imgtool: pip install imgtool, or set IMGTOOL)").into())
}

#[derive(Clone, Debug)]
pub struct SignOptions {
    pub version: String,
    pub key: Key,
    /// `--pad --confirm`: a pre-confirmed image padded to the slot size.
    pub confirm: bool,
    /// Further imgtool arguments, e.g. `--security-counter 3` or `--rom-fixed`.
    pub extra: Vec<String>,
}

impl SignOptions {
    pub fn new(version: &str) -> Self {
        Self {
            version: version.into(),
            key: Key::Dev,
            confirm: false,
            extra: Vec::new(),
        }
    }
    pub fn key(mut self, key: Key) -> Self {
        self.key = key;
        self
    }
    pub fn confirmed(mut self) -> Self {
        self.confirm = true;
        self
    }
    /// Override the default HIL epoch for this image.
    pub fn security_counter(self, value: u32) -> Self {
        self.arg("--security-counter").arg(value.to_string())
    }
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.extra.push(arg.into());
        self
    }
}

/// `imgtool sign` a payload for the board's slots.
pub fn sign(signing: &Signing, payload: &[u8], opts: &SignOptions) -> crate::Result<Vec<u8>> {
    let counter = if opts
        .extra
        .iter()
        .any(|arg| arg == "--security-counter" || arg.starts_with("--security-counter="))
    {
        None
    } else {
        Some(initial_security_counter()?)
    };
    let Signing {
        header_size,
        slot_size,
        sector,
        rom_fixed,
        vid,
        cid,
    } = *signing;
    // Reuse ordinary signed fixtures: randomized ECDSA signatures would
    // otherwise force an identical image's last sector to be reprogrammed.
    // Extra arguments can refer to files; leave those calls uncached. The
    // Geometry and the default epoch are part of the key so cached images
    // cannot cross boards or security-counter settings.
    type CacheKey = (
        [u8; 32],
        [u8; 32],
        String,
        bool,
        String,
        [u32; 3],
        Option<u32>,
        Option<u32>,
        &'static str,
        &'static str,
    );
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Vec<u8>>>> = OnceLock::new();
    let key_path = opts.key.path();
    let key = (
        Sha256::digest(payload).into(),
        Sha256::digest(std::fs::read(&key_path)?).into(),
        opts.version.clone(),
        opts.confirm,
        imgtool(),
        [header_size, slot_size, sector],
        rom_fixed,
        counter,
        vid,
        cid,
    );
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if opts.extra.is_empty()
        && let Some(image) = cache.get(&key)
    {
        return Ok(image.clone());
    }
    let geometry = ferriboot_build::tools::mcuboot::Geometry {
        header: header_size,
        slot: slot_size,
        erase: sector,
        write: 4,
        rom_fixed,
        class: cid,
    };
    let mut options = ferriboot_build::tools::mcuboot::SignOptions {
        remote: None,
        key: key_path,
        version: opts.version.clone(),
        counter: counter.unwrap_or(0).to_string(),
        vendor: vid.into(),
        class: cid.into(),
        extra: opts.extra.clone(),
    };
    if opts.confirm {
        options.extra.extend(["--pad".into(), "--confirm".into()]);
    }
    let image = ferriboot_build::tools::mcuboot::sign(payload, geometry, &options)
        .map_err(crate::Error::external)?;
    if opts.extra.is_empty() {
        cache.insert(key, image.clone());
    }
    Ok(image)
}

// ----- envelope mutations ----------------------------------------------------

fn u16_at(b: &[u8], off: usize) -> usize {
    u16::from_le_bytes([b[off], b[off + 1]]) as usize
}

/// Offsets of every unprotected TLV as (kind, value offset, length).
pub fn unprotected_tlvs(image: &[u8]) -> Vec<(u16, usize, usize)> {
    let hdr = u16_at(image, 8);
    let prot = u16_at(image, 10);
    let body = u32::from_le_bytes(image[12..16].try_into().unwrap()) as usize;
    let start = hdr + body + prot;
    assert_eq!(u16_at(image, start), 0x6907, "unprotected TLV info magic");
    let end = start + u16_at(image, start + 2);
    let mut out = Vec::new();
    let mut at = start + 4;
    while at + 4 <= end {
        let kind = u16_at(image, at) as u16;
        let len = u16_at(image, at + 2);
        out.push((kind, at + 4, len));
        at += 4 + len;
    }
    out
}

/// Flip a bit inside the ECDSA signature's `r` integer (DER stays valid).
pub fn corrupt_signature(mut image: Vec<u8>) -> Vec<u8> {
    let (_, at, _) = *unprotected_tlvs(&image)
        .iter()
        .find(|(k, _, _)| *k == 0x22)
        .expect("ECDSA_SIG TLV");
    image[at + 6] ^= 0x01;
    image
}

/// Flip one bit of the signed payload `offset` bytes after the MCUboot header
/// (whose size the image declares).
pub fn flip_payload_bit(mut image: Vec<u8>, offset: usize) -> Vec<u8> {
    let header = u16_at(&image, 8);
    image[header + offset] ^= 0x01;
    image
}

/// Replace the signed hash TLV with zeros (signature then fails too).
pub fn corrupt_hash_tlv(mut image: Vec<u8>) -> Vec<u8> {
    let (_, at, len) = *unprotected_tlvs(&image)
        .iter()
        .find(|(k, _, _)| *k == 0x10)
        .expect("SHA256 TLV");
    image[at..at + len].fill(0);
    image
}
