//! Ferriboot's test-only MMIO frontend to the QEMU Xilinx eFUSE model.
//! The *model* owns the one-way bits and persistent block backend; this
//! peripheral exposes the first 1536 bits as 48 little-endian words.
use crate::chip::SECURITY_COUNTER_CAPACITY;
use crate::regs::efuse::{DEVICE_ID, PROGRAM_BIT, WORDS};

const MAGIC: u32 = 0x4655_5345; // "FUSE"

pub fn present() -> bool {
    DEVICE_ID.read() == MAGIC
}

pub fn read_word(index: usize) -> u32 {
    WORDS.at(index).read()
}

pub fn program_bit(bit: u32) {
    assert!(bit < SECURITY_COUNTER_CAPACITY);
    PROGRAM_BIT.write(bit);
}
