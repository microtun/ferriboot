//! Dependency-free C ABI between Ferriboot and its relocated IRAM loader.
//! Any change to these types requires rebuilding both executables together.

pub const MAX_SEGMENTS: usize = 16;
pub const BOOT_INFO_WORDS: usize = 256 / core::mem::size_of::<u32>();

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CopyOp {
    pub dest: u32,
    pub flash: u32,
    pub len: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapOp {
    pub vaddr: u32,
    pub paddr: u32,
    pub pages: u32,
    pub ibus: u32,
}

#[repr(C)]
pub struct Plan<const N: usize> {
    pub entry: u32,
    pub boot_info: [u32; N],
    pub copies: u32,
    pub maps: u32,
    pub copy: [CopyOp; MAX_SEGMENTS],
    pub map: [MapOp; MAX_SEGMENTS],
}

impl<const N: usize> Plan<N> {
    #[inline(always)]
    pub fn entry(&self) -> u32 {
        self.entry
    }
    #[inline(always)]
    pub fn set_boot_info(&mut self, info: [u32; N]) {
        self.boot_info = info;
    }
    #[inline(always)]
    pub fn boot_info(&self) -> &[u32; N] {
        &self.boot_info
    }
    #[inline(always)]
    pub fn copies(&self) -> impl Iterator<Item = &CopyOp> {
        self.copy.iter().take(self.copies as usize)
    }
    #[inline(always)]
    pub fn maps(&self) -> impl Iterator<Item = &MapOp> {
        self.map.iter().take(self.maps as usize)
    }
}

// Ferriboot copies a `Plan` into the protected slot by reading it as `u32`
// words, which is only valid if no field leaves padding (padding bytes are
// uninitialized). Every type here is made of `u32`s, so "no padding" means
// "size equals the sum of the fields' sizes"; these assertions check that
// independently of the field count, so adding e.g. a `u8` field fails here.
const _: () = {
    use core::mem::{align_of, size_of};
    const W: usize = size_of::<u32>();
    assert!(size_of::<CopyOp>() == 3 * W && align_of::<CopyOp>() == W);
    assert!(size_of::<MapOp>() == 4 * W && align_of::<MapOp>() == W);
    const N: usize = BOOT_INFO_WORDS;
    assert!(
        size_of::<Plan<N>>()
            == W * (1 + N + 2)
                + MAX_SEGMENTS * size_of::<CopyOp>()
                + MAX_SEGMENTS * size_of::<MapOp>()
    );
    assert!(size_of::<Plan<BOOT_INFO_WORDS>>() == 716);
    assert!(align_of::<Plan<BOOT_INFO_WORDS>>() == 4);
};
