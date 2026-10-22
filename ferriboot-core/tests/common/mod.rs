#![allow(dead_code)]

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use ferriboot_core::{
    BootConfig, BootDecision, BootLoader, Error, FirmwareUpdater, Policy, PublicKey,
    image::{IMAGE_MAGIC, TLV_INFO_MAGIC, TLV_PROT_INFO_MAGIC, tlv},
    validate::P256_SPKI_PREFIX,
};
use p256::ecdsa::{Signature, SigningKey, signature::hazmat::PrehashSigner};
use sha2::{Digest as _, Sha256};

pub const SECTOR: usize = 4096;
pub const SLOT_SIZE: usize = 32 * SECTOR;
pub const SCRATCH_SIZE: usize = 4 * SECTOR;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    PowerLoss,
    OutOfBounds,
    NotAligned,
    /// Programming a byte that is not erased (a bootloader bug on NOR flash).
    OverWrite,
}

impl NorFlashError for SimError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            SimError::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            SimError::NotAligned => NorFlashErrorKind::NotAligned,
            _ => NorFlashErrorKind::Other,
        }
    }
}

/// Shared "power supply": counts mutating operations across all areas and
/// cuts power after a configured number of them.
#[derive(Clone, Default)]
pub struct Power {
    remaining: Rc<Cell<Option<u64>>>,
    ops: Rc<Cell<u64>>,
}

impl Power {
    pub fn unlimited(&self) {
        self.remaining.set(None);
    }
    pub fn fail_after(&self, ops: u64) {
        self.remaining.set(Some(ops));
    }
    pub fn reset_count(&self) {
        self.ops.set(0);
    }
    pub fn count(&self) -> u64 {
        self.ops.get()
    }
    /// Returns `true` if power is lost during this operation.
    fn tick(&self) -> bool {
        self.ops.set(self.ops.get() + 1);
        match self.remaining.get() {
            None => false,
            Some(0) => true,
            Some(n) => {
                self.remaining.set(Some(n - 1));
                false
            }
        }
    }
}

#[derive(Clone)]
pub struct SimFlash {
    mem: Rc<RefCell<Vec<u8>>>,
    power: Power,
}

impl SimFlash {
    pub fn new(size: usize, power: &Power) -> Self {
        Self {
            mem: Rc::new(RefCell::new(vec![0xff; size])),
            power: power.clone(),
        }
    }
    pub fn snapshot(&self) -> Vec<u8> {
        self.mem.borrow().clone()
    }
    pub fn restore(&self, data: &[u8]) {
        self.mem.borrow_mut().copy_from_slice(data);
    }
    /// Program raw bytes (factory programming, bypasses power simulation).
    pub fn program(&self, offset: usize, data: &[u8]) {
        self.mem.borrow_mut()[offset..offset + data.len()].copy_from_slice(data);
    }
    pub fn get(&self, range: std::ops::Range<usize>) -> Vec<u8> {
        self.mem.borrow()[range].to_vec()
    }
}

impl ErrorType for SimFlash {
    type Error = SimError;
}

impl ReadNorFlash for SimFlash {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SimError> {
        let off = offset as usize;
        if !off.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
            return Err(SimError::NotAligned);
        }
        let mem = self.mem.borrow();
        if off + bytes.len() > mem.len() {
            return Err(SimError::OutOfBounds);
        }
        bytes.copy_from_slice(&mem[off..off + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.mem.borrow().len()
    }
}

impl NorFlash for SimFlash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SimError> {
        let off = offset as usize;
        if !off.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
            return Err(SimError::NotAligned);
        }
        let mut mem = self.mem.borrow_mut();
        if off + bytes.len() > mem.len() {
            return Err(SimError::OutOfBounds);
        }
        for (i, b) in bytes.iter().enumerate() {
            if *b != 0xff && mem[off + i] != 0xff {
                return Err(SimError::OverWrite);
            }
        }
        let cut = self.power.tick();
        let n = if cut { bytes.len() / 2 } else { bytes.len() };
        for i in 0..n {
            mem[off + i] &= bytes[i];
        }
        if cut {
            Err(SimError::PowerLoss)
        } else {
            Ok(())
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), SimError> {
        let (from, to) = (from as usize, to as usize);
        if from % SECTOR != 0 || to % SECTOR != 0 || from > to {
            return Err(SimError::NotAligned);
        }
        let mut mem = self.mem.borrow_mut();
        if to > mem.len() {
            return Err(SimError::OutOfBounds);
        }
        let cut = self.power.tick();
        // On power loss, half of the range (rounded to bytes) is erased.
        let end = if cut { from + (to - from) / 2 + 1 } else { to };
        mem[from..end.min(to)].fill(0xff);
        if cut {
            Err(SimError::PowerLoss)
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Image construction (mirrors imgtool's output)
// ---------------------------------------------------------------------------

pub fn signing_key() -> SigningKey {
    SigningKey::from_bytes((&[7u8; 32]).into()).unwrap()
}

pub fn other_signing_key() -> SigningKey {
    SigningKey::from_bytes((&[9u8; 32]).into()).unwrap()
}

/// A well-formed key that is not in any rig's key ring.
pub fn untrusted_signing_key() -> SigningKey {
    SigningKey::from_bytes((&[11u8; 32]).into()).unwrap()
}

pub fn public_key(k: &SigningKey) -> PublicKey {
    let point = k.verifying_key().to_encoded_point(false);
    PublicKey::from_raw(point.as_bytes().try_into().unwrap()).unwrap()
}

#[derive(Clone)]
pub struct Spec {
    pub version: (u8, u8, u16, u32),
    pub payload: Vec<u8>,
    pub hdr_size: u16,
    pub sec_cnt: Option<u32>,
    pub key: SigningKey,
    pub corrupt_signature: bool,
    /// Put an arbitrary TLV into the unprotected area.
    pub extra_unprotected: Option<u16>,
    /// Header `ih_flags` and `ih_load_addr`.
    pub flags: u32,
    pub load_addr: u32,
    /// Extra protected TLVs, appended after `SEC_CNT`.
    pub extra_protected: Vec<(u16, Vec<u8>)>,
    /// Further `KEYHASH` + `ECDSA_SIG` pairs after the primary one; `true`
    /// corrupts that signature.
    pub extra_signers: Vec<(SigningKey, bool)>,
    /// Emit the signature before its key TLV (upstream then ignores it).
    pub signature_before_key: bool,
}

impl Spec {
    pub fn new(version: (u8, u8, u16, u32), len: usize, seed: u8) -> Self {
        let payload = (0..len)
            .map(|i| {
                (i as u32)
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(seed as u32) as u8
                    ^ seed
            })
            .collect();
        Self {
            version,
            payload,
            hdr_size: 0x20,
            sec_cnt: None,
            key: signing_key(),
            corrupt_signature: false,
            extra_unprotected: None,
            flags: 0,
            load_addr: 0,
            extra_protected: Vec::new(),
            extra_signers: Vec::new(),
            signature_before_key: false,
        }
    }
}

fn push_tlv(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
}

pub fn build_image(spec: &Spec) -> Vec<u8> {
    let mut prot_body = Vec::new();
    if let Some(c) = spec.sec_cnt {
        push_tlv(&mut prot_body, tlv::SEC_CNT, &c.to_le_bytes());
    }
    for (kind, value) in &spec.extra_protected {
        push_tlv(&mut prot_body, *kind, value);
    }
    let prot_len = if prot_body.is_empty() {
        0
    } else {
        4 + prot_body.len()
    };

    let mut img = vec![0u8; spec.hdr_size as usize];
    img[0..4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
    img[4..8].copy_from_slice(&spec.load_addr.to_le_bytes());
    img[16..20].copy_from_slice(&spec.flags.to_le_bytes());
    img[8..10].copy_from_slice(&spec.hdr_size.to_le_bytes());
    img[10..12].copy_from_slice(&(prot_len as u16).to_le_bytes());
    img[12..16].copy_from_slice(&(spec.payload.len() as u32).to_le_bytes());
    img[20] = spec.version.0;
    img[21] = spec.version.1;
    img[22..24].copy_from_slice(&spec.version.2.to_le_bytes());
    img[24..28].copy_from_slice(&spec.version.3.to_le_bytes());
    img.extend_from_slice(&spec.payload);
    if prot_len > 0 {
        img.extend_from_slice(&TLV_PROT_INFO_MAGIC.to_le_bytes());
        img.extend_from_slice(&(prot_len as u16).to_le_bytes());
        img.extend_from_slice(&prot_body);
    }

    let digest: [u8; 32] = Sha256::digest(&img).into();
    let signed = |key: &SigningKey, corrupt: bool| {
        let mut kh = Sha256::new();
        kh.update(P256_SPKI_PREFIX);
        kh.update(key.verifying_key().to_encoded_point(false).as_bytes());
        let key_hash: [u8; 32] = kh.finalize().into();
        let sig: Signature = key.sign_prehash(&digest).unwrap();
        let mut sig = sig.to_der().as_bytes().to_vec();
        if corrupt {
            sig[5] ^= 0x40;
        }
        (key_hash, sig)
    };

    let mut body = Vec::new();
    push_tlv(&mut body, tlv::SHA256, &digest);
    let (key_hash, sig) = signed(&spec.key, spec.corrupt_signature);
    if spec.signature_before_key {
        push_tlv(&mut body, tlv::ECDSA_SIG, &sig);
        push_tlv(&mut body, tlv::KEYHASH, &key_hash);
    } else {
        push_tlv(&mut body, tlv::KEYHASH, &key_hash);
        push_tlv(&mut body, tlv::ECDSA_SIG, &sig);
    }
    for (key, corrupt) in &spec.extra_signers {
        let (key_hash, sig) = signed(key, *corrupt);
        push_tlv(&mut body, tlv::KEYHASH, &key_hash);
        push_tlv(&mut body, tlv::ECDSA_SIG, &sig);
    }
    if let Some(kind) = spec.extra_unprotected {
        push_tlv(&mut body, kind, &[1, 2, 3, 4]);
    }
    img.extend_from_slice(&TLV_INFO_MAGIC.to_le_bytes());
    img.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
    img.extend_from_slice(&body);
    img
}

// ---------------------------------------------------------------------------
// A complete device: three areas sharing one power supply.
// ---------------------------------------------------------------------------

pub struct Rig {
    pub power: Power,
    pub primary: SimFlash,
    pub secondary: SimFlash,
    pub scratch: SimFlash,
    pub keys: Vec<PublicKey>,
    pub config: BootConfig,
}

pub type Snapshot = [Vec<u8>; 3];

impl Rig {
    pub fn new(config: BootConfig) -> Self {
        let power = Power::default();
        Self {
            primary: SimFlash::new(SLOT_SIZE, &power),
            secondary: SimFlash::new(SLOT_SIZE, &power),
            scratch: SimFlash::new(SCRATCH_SIZE, &power),
            keys: vec![public_key(&other_signing_key()), public_key(&signing_key())],
            config,
            power,
        }
    }

    pub fn boot(&self) -> Result<BootDecision, Error<SimError>> {
        BootLoader::new(
            self.primary.clone(),
            self.secondary.clone(),
            Some(self.scratch.clone()),
            &self.keys,
            self.config,
        )
        .unwrap()
        .boot()
    }

    pub fn snapshot(&self) -> Snapshot {
        [
            self.primary.snapshot(),
            self.secondary.snapshot(),
            self.scratch.snapshot(),
        ]
    }

    pub fn restore(&self, s: &Snapshot) {
        self.primary.restore(&s[0]);
        self.secondary.restore(&s[1]);
        self.scratch.restore(&s[2]);
    }

    /// Updater with `active` = primary, `update` = secondary.
    pub fn updater(&self) -> FirmwareUpdater<SimFlash> {
        FirmwareUpdater::new_with_mode(
            self.primary.clone(),
            self.secondary.clone(),
            self.config.mode,
        )
        .unwrap()
    }

    pub fn updater_for(&self, active: &SimFlash, update: &SimFlash) -> FirmwareUpdater<SimFlash> {
        FirmwareUpdater::new_with_mode(active.clone(), update.clone(), self.config.mode).unwrap()
    }

    /// Download an image into `update` in odd-sized chunks and mark it pending.
    pub fn stage(&self, active: &SimFlash, update: &SimFlash, image: &[u8], permanent: bool) {
        let mut up = self.updater_for(active, update);
        {
            let mut w = up.begin_update().unwrap();
            for chunk in image.chunks(333) {
                w.write(chunk).unwrap();
            }
            assert_eq!(w.finish().unwrap() as usize, image.len());
        }
        if permanent {
            up.mark_permanent().unwrap();
        } else {
            up.mark_for_test().unwrap();
        }
    }

    pub fn stage_update(&self, image: &[u8], permanent: bool) {
        self.stage(&self.primary, &self.secondary, image, permanent);
    }

    pub fn version_in(&self, f: &SimFlash) -> Option<(u8, u8, u16)> {
        let mut f = f.clone();
        ferriboot_core::validate::validate_image(&mut f, &self.keys, &Policy::default(), u32::MAX)
            .ok()
            .map(|v| {
                (
                    v.header().version.major,
                    v.header().version.minor,
                    v.header().version.revision,
                )
            })
    }

    /// Run `boot` uninterrupted, then once for every possible single power
    /// loss followed by a recovery boot, and every few ops for a double power
    /// loss. `check` must hold after every completed boot. The rig is left in
    /// the state of the uninterrupted run.
    pub fn exhaustive(&self, check: impl Fn(&Rig, &BootDecision)) {
        let snap = self.snapshot();
        self.power.unlimited();
        self.power.reset_count();
        let d = self.boot().expect("uninterrupted boot");
        let total = self.power.count();
        check(self, &d);
        let done = self.snapshot();

        for n in 0..total {
            self.restore(&snap);
            self.power.fail_after(n);
            let r = self.boot();
            assert_eq!(
                r,
                Err(Error::Flash(SimError::PowerLoss)),
                "power loss at op {n}"
            );
            if self.snapshot() == done {
                // The interrupted operation had already taken full effect
                // (e.g. the final flag write), so the next boot is a regular
                // second boot. For a test upgrade that is indistinguishable
                // from a failure to confirm, and MCUboot reverts.
                continue;
            }
            self.power.unlimited();
            let d = self
                .boot()
                .unwrap_or_else(|e| panic!("recovery after power loss at op {n} failed: {e:?}"));
            check(self, &d);
        }

        for n in (0..total).step_by(5) {
            for m in (0..total).step_by(11) {
                self.restore(&snap);
                self.power.fail_after(n);
                let _ = self.boot();
                if self.snapshot() == done {
                    continue;
                }
                self.power.fail_after(m);
                let _ = self.boot();
                if self.snapshot() == done {
                    continue;
                }
                self.power.unlimited();
                let d = self
                    .boot()
                    .unwrap_or_else(|e| panic!("recovery after power loss at {n}/{m}: {e:?}"));
                check(self, &d);
            }
        }

        self.restore(&done);
    }
}
