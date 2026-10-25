//! Partition policy for the patched QEMU MPS2-AN385 test board.

pub use ferriboot_chip_qemu::*;

pub const BOOTLOADER_SIZE: u32 = 0x40000;
pub const SLOT_SIZE: u32 = 0x1d0000;
pub const SCRATCH_SIZE: u32 = 0x10000;
