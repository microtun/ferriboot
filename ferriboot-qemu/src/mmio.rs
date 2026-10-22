//! A minimal 32-bit MMIO register handle for the AN385 machine and Ferriboot's
//! QEMU test peripherals, which have no PAC.
//!
//! All address-validity reasoning happens once, where a register constant is
//! created with the `unsafe` constructors below; reads and writes are safe.

/// One 32-bit register (or reserved RAM word) accessed with volatile loads
/// and stores.
#[derive(Clone, Copy)]
pub struct Reg(usize);

impl Reg {
    /// # Safety
    /// `address` must be 4-byte aligned and valid for volatile 32-bit reads
    /// and writes for the whole program on the patched mps2-an385 machine: an
    /// MMIO register, or a RAM word that no linker region or Rust reference
    /// covers. Any value written must be acceptable to that register.
    pub const unsafe fn new(address: usize) -> Self {
        Self(address)
    }

    fn ptr(self) -> *mut u32 {
        core::ptr::with_exposed_provenance_mut(self.0)
    }

    pub fn read(self) -> u32 {
        // SAFETY: `Reg::new`'s contract.
        unsafe { self.ptr().read_volatile() }
    }

    pub fn write(self, value: u32) {
        // SAFETY: `Reg::new`'s contract.
        unsafe { self.ptr().write_volatile(value) }
    }
}

/// `len` consecutive 32-bit registers.
#[derive(Clone, Copy)]
pub struct RegArray {
    base: usize,
    len: usize,
}

impl RegArray {
    /// # Safety
    /// Every word `base + 4 * i` for `i < len` satisfies [`Reg::new`]'s
    /// contract.
    pub const unsafe fn new(base: usize, len: usize) -> Self {
        Self { base, len }
    }

    /// The register at `index`. Panics if `index >= len`.
    pub fn at(self, index: usize) -> Reg {
        assert!(index < self.len, "register index out of range");
        // SAFETY: `index < len`, so this is one of the words covered by
        // `RegArray::new`'s contract.
        unsafe { Reg::new(self.base + index * 4) }
    }
}
