//! Fault-injection hardening, modelled on the host.
//!
//! A glitch that makes the first validation accept an image is modelled by
//! changing the flash between the first validation and the re-authentication
//! that guards an irreversible step: the first run "saw" an authentic image,
//! the truth is different. The guard must then fault, not proceed.

use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use ferriboot_core::{
    BootConfig, BootLoader, Fih, Policy, SecurityCounterError, SecurityCounterStore, Slot,
    UpgradeMode, ValidatedImage, counter_permits, reauthenticate, validate::validate_image,
};
use rand::{CryptoRng, RngCore};

mod common;
use common::*;

fn passes(verdict: Fih) -> bool {
    catch_unwind(|| verdict.require()).is_ok()
}

fn panicked_with_fault(result: std::thread::Result<impl Sized>) -> bool {
    match result {
        Ok(_) => false,
        Err(payload) => payload
            .downcast_ref::<&str>()
            .is_some_and(|msg| *msg == "fault injection detected"),
    }
}

/// Flips one byte after a given number of reads, once.
#[derive(Clone)]
struct Glitchy {
    inner: SimFlash,
    reads: Rc<Cell<u32>>,
    flip_after: Rc<Cell<Option<u32>>>,
    offset: usize,
}

impl Glitchy {
    fn new(inner: &SimFlash, offset: usize) -> Self {
        Self {
            inner: inner.clone(),
            reads: Rc::default(),
            flip_after: Rc::default(),
            offset,
        }
    }
}

impl ErrorType for Glitchy {
    type Error = SimError;
}

impl ReadNorFlash for Glitchy {
    const READ_SIZE: usize = SimFlash::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SimError> {
        if self.flip_after.get() == Some(self.reads.get()) {
            self.flip_after.set(None);
            let old = self.inner.get(self.offset..self.offset + 1)[0];
            self.inner.program(self.offset, &[old ^ 0x01]);
        }
        self.reads.set(self.reads.get() + 1);
        self.inner.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}

impl NorFlash for Glitchy {
    const WRITE_SIZE: usize = SimFlash::WRITE_SIZE;
    const ERASE_SIZE: usize = SimFlash::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SimError> {
        self.inner.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), SimError> {
        self.inner.erase(from, to)
    }
}

struct Counter {
    value: u32,
    writes: u32,
    /// Values returned by successive `get` calls before falling back to
    /// `value`, to model corrupted reads.
    reads: Vec<Result<u32, SecurityCounterError>>,
}

impl Counter {
    fn at(value: u32) -> Self {
        Self {
            value,
            writes: 0,
            reads: Vec::new(),
        }
    }
}

impl SecurityCounterStore for Counter {
    fn get(&mut self) -> Result<u32, SecurityCounterError> {
        if self.reads.is_empty() {
            Ok(self.value)
        } else {
            self.reads.remove(0)
        }
    }

    fn advance(&mut self, value: u32) -> Result<(), SecurityCounterError> {
        self.writes += 1;
        self.value = self.value.max(value);
        Ok(())
    }

    fn capacity(&self) -> u32 {
        16
    }
}

fn image(seed: u8, sec_cnt: Option<u32>) -> Vec<u8> {
    let mut spec = Spec::new((1, 0, 0, seed.into()), 6000, seed);
    spec.sec_cnt = sec_cnt;
    build_image(&spec)
}

fn validated(flash: &SimFlash, keys: &[ferriboot_core::PublicKey]) -> ValidatedImage {
    validate_image(&mut flash.clone(), keys, &Policy::default(), u32::MAX).unwrap()
}

/// A payload byte, inside the region covered by the hash and signature.
const PAYLOAD_BYTE: usize = 0x20 + 100;

#[test]
fn reauthentication_agrees_only_with_the_same_authentic_image() {
    let rig = Rig::new(BootConfig::default());
    rig.primary.program(0, &image(1, Some(2)));
    let first = validated(&rig.primary, &rig.keys);
    let again = || {
        reauthenticate(
            &mut rig.primary.clone(),
            &rig.keys,
            &Policy::default(),
            u32::MAX,
            &first,
        )
    };
    assert!(passes(again()));

    // The flash no longer holds what the first run accepted.
    let byte = rig.primary.get(PAYLOAD_BYTE..PAYLOAD_BYTE + 1)[0];
    rig.primary.program(PAYLOAD_BYTE, &[byte ^ 0x80]);
    assert!(!passes(again()));

    // Another authentic image is not the one that was accepted either.
    rig.primary.program(0, &image(2, Some(2)));
    assert!(
        validate_image(
            &mut rig.primary.clone(),
            &rig.keys,
            &Policy::default(),
            u32::MAX
        )
        .is_ok()
    );
    assert!(!passes(again()));
}

#[test]
fn counter_check_reads_the_floor_twice_and_checks_both_bounds() {
    let rig = Rig::new(BootConfig::default());
    rig.primary.program(0, &image(1, Some(5)));
    let five = validated(&rig.primary, &rig.keys);
    rig.primary.program(0, &image(2, Some(17)));
    let beyond_capacity = validated(&rig.primary, &rig.keys);
    rig.primary.program(0, &image(3, None));
    let without_counter = validated(&rig.primary, &rig.keys);

    assert!(passes(counter_permits(&mut Counter::at(5), &five)));
    assert!(passes(counter_permits(&mut Counter::at(0), &five)));
    assert!(!passes(counter_permits(&mut Counter::at(6), &five)));
    assert!(!passes(counter_permits(
        &mut Counter::at(0),
        &beyond_capacity
    )));
    assert!(!passes(counter_permits(
        &mut Counter::at(0),
        &without_counter
    )));

    // One corrupted read that lowers the floor is caught by the second one.
    let mut store = Counter::at(9);
    store.reads = vec![Ok(0), Ok(9)];
    assert!(!passes(counter_permits(&mut store, &five)));
    let mut store = Counter::at(0);
    store.reads = vec![Err(SecurityCounterError::Read)];
    assert!(!passes(counter_permits(&mut store, &five)));
}

/// Overwrite mode treats the primary image as confirmed, so its counter is
/// burned on boot: validation, then re-authentication, then the burn.
fn burn_rig() -> (Rig, Glitchy, u32) {
    let rig = Rig::new(BootConfig {
        mode: UpgradeMode::Overwrite,
        ..BootConfig::default()
    });
    rig.primary.program(0, &image(1, Some(3)));
    let primary = Glitchy::new(&rig.primary, PAYLOAD_BYTE);
    let max = BootLoader::new(
        rig.primary.clone(),
        rig.secondary.clone(),
        None,
        &rig.keys,
        rig.config,
    )
    .unwrap()
    .max_image_size();
    // Reads taken by one complete validation of the primary slot.
    validate_image(&mut primary.clone(), &rig.keys, &rig.config.policy, max).unwrap();
    let per_validation = primary.reads.replace(0);
    (rig, primary, per_validation)
}

// Deterministic test-only RNG: the production API requires the CryptoRng marker.
struct FixedEntropy;

impl RngCore for FixedEntropy {
    fn next_u32(&mut self) -> u32 {
        1234
    }

    fn next_u64(&mut self) -> u64 {
        u64::from(self.next_u32()) | (u64::from(self.next_u32()) << 32)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let bytes = self.next_u32().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

impl CryptoRng for FixedEntropy {}

fn boot_with(rig: &Rig, primary: &Glitchy, counter: &mut Counter) -> std::thread::Result<()> {
    let secondary = Glitchy::new(&rig.secondary, 0);
    catch_unwind(AssertUnwindSafe(|| {
        let mut entropy = FixedEntropy;
        let mut loader = BootLoader::new(primary.clone(), secondary, None, &rig.keys, rig.config)
            .unwrap()
            .with_security_counter_store(counter)
            .with_entropy(&mut entropy);
        let decision = loader.boot().unwrap();
        assert_eq!(decision.slot, Slot::Primary);
    }))
}

#[test]
fn generic_counter_and_entropy_can_be_attached_in_either_order() {
    // The type-level handles work independently of builder method order.
    let (rig, primary, _) = burn_rig();
    let mut counter = Counter::at(0);
    let mut random = FixedEntropy;
    let mut loader = BootLoader::new(
        primary,
        Glitchy::new(&rig.secondary, 0),
        None,
        &rig.keys,
        rig.config,
    )
    .unwrap()
    .with_entropy(&mut random)
    .with_security_counter_store(&mut counter);
    assert_eq!(loader.boot().unwrap().slot, Slot::Primary);
    drop(loader);
    assert_eq!(counter.value, 3);
}

#[test]
fn counter_is_burned_after_two_agreeing_authentications() {
    let (rig, primary, _) = burn_rig();
    let mut counter = Counter::at(0);
    assert!(boot_with(&rig, &primary, &mut counter).is_ok());
    assert_eq!(counter.value, 3);
}

#[test]
fn counter_burn_faults_when_the_first_authentication_was_wrong() {
    let (rig, primary, per_validation) = burn_rig();
    // The first validation accepted the image; by the time the burn is
    // re-authenticated, flash disagrees with what was accepted.
    primary.flip_after.set(Some(per_validation));
    let mut counter = Counter::at(0);
    assert!(panicked_with_fault(boot_with(&rig, &primary, &mut counter)));
    assert_eq!(counter.writes, 0, "nothing may be burned");
    assert_eq!(counter.value, 0);
}
