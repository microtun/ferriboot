//! Exercise the actual MMIO counter algorithm with a host-side eFuse shim.
//! QEMU persists the words to efuses.bin in end-to-end tests.
use super::layout;
use ferriboot_core::{SecurityCounterError as E, SecurityCounterStore};

#[path = "../../../../ferriboot-qemu/src/security_counter.rs"]
mod guest;

// The guest's machine facts, from the board layout crate (same JSON source).
mod chip {
    pub use super::layout::SECURITY_COUNTER_CAPACITY;
}

// Same callable interface as the target MMIO frontend, with one-way set bits.
mod efuse {
    use super::layout;
    use std::cell::RefCell;

    struct State {
        present: bool,
        words: Vec<u32>,
    }
    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State {
            present: true,
            words: vec![0; layout::SECURITY_COUNTER_BYTES / 4],
        });
    }
    pub fn setup(bytes: &[u8]) {
        assert_eq!(bytes.len(), layout::SECURITY_COUNTER_BYTES);
        STATE.with(|state| {
            *state.borrow_mut() = State {
                present: true,
                words: bytes
                    .chunks_exact(4)
                    .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
                    .collect(),
            };
        });
    }
    pub fn present() -> bool {
        STATE.with(|state| state.borrow().present)
    }
    pub fn set_present(present: bool) {
        STATE.with(|state| state.borrow_mut().present = present);
    }
    pub fn read_word(index: usize) -> u32 {
        STATE.with(|state| state.borrow().words[index])
    }
    pub fn program_bit(bit: u32) {
        STATE.with(|state| {
            state.borrow_mut().words[(bit / 32) as usize] |= 1 << (bit % 32);
        });
    }
}

#[test]
fn guest_counter_survives_recreation_and_cannot_decrease() {
    efuse::setup(&[0; layout::SECURITY_COUNTER_BYTES]);
    let mut counter = guest::EfuseSecurityCounter::new();
    counter.advance(9).unwrap();
    let mut after_reset = guest::EfuseSecurityCounter::new();
    assert_eq!(after_reset.get(), Ok(9));
    after_reset.advance(3).unwrap();
    assert_eq!(after_reset.get(), Ok(9));
    after_reset.advance(65).unwrap(); // crosses 32-bit row boundaries
    assert_eq!(after_reset.get(), Ok(65));
    assert_eq!(efuse::read_word(0), u32::MAX);
    assert_eq!(efuse::read_word(1), u32::MAX);
    assert_eq!(efuse::read_word(2), 1);
}

#[test]
fn missing_peripheral_or_malformed_fuses_fail_closed() {
    efuse::setup(&[0; layout::SECURITY_COUNTER_BYTES]);
    efuse::set_present(false);
    assert_eq!(guest::EfuseSecurityCounter::new().get(), Err(E::Read));
    for words in [[2u32, 0], [1, 1], [0, 1]] {
        let mut bytes = [0; layout::SECURITY_COUNTER_BYTES];
        bytes[..4].copy_from_slice(&words[0].to_le_bytes());
        bytes[4..8].copy_from_slice(&words[1].to_le_bytes());
        efuse::setup(&bytes);
        assert_eq!(
            guest::EfuseSecurityCounter::new().get(),
            Err(E::InvalidState)
        );
    }
}

#[test]
fn interrupted_burn_resumes_from_the_burned_floor() {
    efuse::setup(&[0; layout::SECURITY_COUNTER_BYTES]);
    for bit in 0..3 {
        efuse::program_bit(bit);
    }
    let mut after_reset = guest::EfuseSecurityCounter::new();
    assert_eq!(after_reset.get(), Ok(3));
    after_reset.advance(10).unwrap();
    assert_eq!(after_reset.get(), Ok(10));
}

#[test]
fn exhausted_counter_still_accepts_its_current_epoch() {
    efuse::setup(&[0xff; layout::SECURITY_COUNTER_BYTES]);
    let mut counter = guest::EfuseSecurityCounter::new();
    assert_eq!(counter.get(), Ok(counter.capacity()));
    counter.advance(counter.capacity()).unwrap();
    assert_eq!(counter.advance(counter.capacity() + 1), Err(E::Exhausted));
}
