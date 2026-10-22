//! Ferriboot shared boot information with upstream-compatible TLVs in reserved SRAM.
//!
//! Applications call `ferriboot_build::configure_esp_application()` in
//! their build script to link `ferriboot.x` (see `memory.x.in`) after
//! `linkall.x`. The NOLOAD reservation is outside BSS clearing and the
//! reclaimed `.dram2_uninit` heap.

pub use ferriboot_core::shared_data::{SharedData, boot_save_shared_data, wire_words};

pub const RESERVED_SIZE: u32 = ferriboot_core::shared_data::BOOT_INFO_SIZE as u32;
pub const WORDS: usize = ferriboot_core::shared_data::BOOT_INFO_WORDS;
// The address and size come from the same chip description that generates
// the BOOT_INFO region of the application linker contract.
pub const ADDRESS: u32 = crate::chip::BOOT_INFO_ADDRESS;
const _: () = assert!(
    crate::chip::BOOT_INFO_SIZE == RESERVED_SIZE,
    "chip description and ferriboot-core disagree on the boot-info size"
);

// An allocated input section keeps the NOLOAD reservation unambiguous to
// GNU ld and LLD. EXTERN/KEEP preserves it through archive extraction and GC.
#[used]
#[unsafe(export_name = "_ferriboot_boot_info_storage")]
#[unsafe(link_section = ".ferriboot_boot_info")]
static mut BOOT_INFO_STORAGE: [u32; WORDS] = [0; WORDS];

// `mut` because the bootloader rewrites this RAM: mutating an immutable
// static, extern or not, is undefined behaviour even through volatile stores.
// Its type spans the whole reservation, so word pointers stay in bounds.
unsafe extern "C" {
    static mut _ferriboot_boot_info_start: [u32; WORDS];
}

/// The first word of the boot-info reservation.
fn reservation() -> *mut u32 {
    (&raw mut _ferriboot_boot_info_start).cast()
}

/// Checked current boot context. No references into shared RAM are retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootInfo(ferriboot_core::shared_data::BootInfo);

impl BootInfo {
    /// Read the upstream TLV header and entries. An absent, malformed or
    /// unsupported handoff fails closed; the updater must not guess a slot.
    pub fn read() -> Option<Self> {
        let area = reservation();
        let mut words = [0; WORDS];
        for (i, word) in words.iter_mut().enumerate() {
            // SAFETY: both linkers reserve these aligned words at this
            // symbol, and `i` stays within its `WORDS` words.
            *word = unsafe { area.wrapping_add(i).read_volatile() };
        }
        if let Ok(info) = ferriboot_core::shared_data::BootInfo::from_handoff_words(&words) {
            return Some(Self(info));
        }
        // Migration: old bootloaders published an 80-byte handoff at the
        // tail of this now 256-byte RAM region. Only the application reads it.
        const LEGACY_BYTES: usize = 80;
        const LEGACY_START: usize = (WORDS * 4 - LEGACY_BYTES) / 4;
        let mut legacy = [0u8; LEGACY_BYTES];
        for (chunk, word) in legacy
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&words[LEGACY_START..])
        {
            *chunk = word.to_le_bytes();
        }
        ferriboot_core::shared_data::BootInfo::from_handoff(&legacy)
            .ok()
            .map(Self)
    }

    /// Bootloader only: invalidate the old header before selecting an image.
    /// The protected loader publishes a fresh header immediately before jump.
    pub fn invalidate() {
        // SAFETY: the first aligned word belongs exclusively to this region
        // and no Rust reference to the reservation exists.
        unsafe { reservation().write_volatile(0) };
    }
}

impl core::ops::Deref for BootInfo {
    type Target = ferriboot_core::shared_data::BootInfo;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
