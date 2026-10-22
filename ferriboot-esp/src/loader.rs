//! Loading an ESP-IDF format application image (as produced by
//! `espflash save-image` / `esptool elf2image`) and jumping to it.
//!
//! Equivalent of ESP-IDF's `bootloader_utility_load_boot_image` +
//! `set_cache_and_start_app` for the selected chip, in two phases:
//!
//! 1. [`plan`] (ordinary code) parses the segment table and checks it:
//!    RAM segments (IRAM, DRAM, RTC fast) must lie in application RAM and may
//!    not reach the relocated handoff and its separate high-DRAM stack; flash
//!    segments (IROM at 0x4200_0000, DROM at 0x3C00_0000 on C3; I/D shared on C6) need
//!    `flash offset % 64 KiB == vaddr % 64 KiB`, which holds when the ESP
//!    image starts on a 64 KiB boundary (slot offset + image header size),
//!    and must not need conflicting pages in the shared MMU table.
//! 2. [`confirm`] authenticates the image a second time and re-derives the
//!    plan from flash, as the fault-hardened point of no return.
//! 3. [`execute`] installs an embedded payload and jumps to its dedicated stack; the payload only calls
//!    mask ROM functions, because copying the RAM segments overwrites the rest
//!    of the bootloader. It copies, maps the flash segments into the cache and
//!    jumps to the entry point.
//!
//! The image signature already covers the whole ESP image, so the ESP image
//! checksum and appended SHA-256 are not checked again.

use core::{
    arch::asm,
    ptr::{read_volatile, write_volatile},
};

use crate::boot_info::{RESERVED_SIZE, SharedData, WORDS, wire_words};
use crate::{SharedStorage, chip as layout};
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use ferriboot_core::{
    Policy, PublicKey, Region, SecurityCounterStore, ValidatedImage, counter_permits, fih,
    reauthenticate,
};
use log::info;

use crate::loader_plan;
pub use crate::loader_plan::LoadError;
pub type Plan = loader_plan::Plan<WORDS>;
const _: () = assert!(WORDS == bl_loader::BOOT_INFO_WORDS);

// Linked to run at this address but EMBEDDED AS RODATA, not as an ESP RAM segment.
// build.rs compiles and validates the independent bl-loader executable.
static LOADER_IMAGE: &[u8] = include_bytes!(env!("BL_LOADER_BIN"));
const LOADER_IRAM: u32 = layout::IDF_IRAM_LOADER_START;
const LOADER_DRAM: u32 = layout::PROTECTED_START;
const PLAN_ADDRESS: u32 = layout::LOADER_PLAN_ADDRESS;
const STACK_TOP: u32 = layout::LOADER_STACK_TOP;

/// Parse the ESP image inside the authenticated MCUboot payload range.
/// `image_size` excludes the MCUboot header and TLVs.
pub fn plan<F: ReadNorFlash>(
    flash: &mut F,
    image: u32,
    image_size: u32,
    boot_info: SharedData<{ RESERVED_SIZE as usize }>,
) -> Result<Plan, LoadError<F::Error>> {
    let mut plan =
        loader_plan::plan_from(flash, image, image, image_size, layout::PROTECTED_START)?;
    plan.set_boot_info(wire_words(&boot_info));
    for c in plan.copies() {
        info!(
            "load  {:#010x} <- flash {:#x} ({:#x} bytes)",
            c.dest, c.flash, c.len
        );
    }
    for m in plan.maps() {
        info!(
            "map   {:#010x} -> flash {:#x} ({} x 64 KiB)",
            m.vaddr, m.paddr, m.pages
        );
    }
    Ok(plan)
}

/// Check an image still stored in an update partition against its eventual
/// execution address. The parser reads `source`, while MMU and copy operations
/// are planned for `image`; swap/overwrite must use the primary destination.
pub fn validate<F: ReadNorFlash>(
    flash: &mut F,
    source: u32,
    image: u32,
    image_size: u32,
) -> Result<(), LoadError<F::Error>> {
    loader_plan::plan_from::<_, WORDS>(flash, source, image, image_size, layout::PROTECTED_START)
        .map(|_| ())
}

/// A plan that passed [`confirm`]. [`execute`] accepts nothing else.
pub struct ConfirmedPlan(Plan);

/// What the confirmation re-checks the plan against.
pub struct Authenticated<'a> {
    /// The slot the plan was built from.
    pub region: Region,
    pub image: &'a ValidatedImage,
    pub keys: &'a [PublicKey],
    pub policy: &'a Policy,
    pub max_image_size: u32,
}

/// The point of no return, hardened against fault injection.
///
/// Authenticates the slot a second time, independently of the boot decision,
/// re-derives the load plan from flash and requires it to match `plan`
/// exactly, and re-checks the eFuse rollback floor. Then, after a random
/// delay, the combined verdict is consumed with
/// [`require`](ferriboot_core::Fih::require). A disagreement panics
/// (resetting); there is no error path to skip.
///
/// Call it after the last UART output. `random` should come from the TRNG.
#[inline(never)]
pub fn confirm<F: NorFlash, S: SecurityCounterStore, R: rand::CryptoRng>(
    plan: Plan,
    flash: &SharedStorage<F>,
    checked: Authenticated<'_>,
    counter: &mut S,
    random: &mut R,
) -> ConfirmedPlan {
    let Authenticated {
        region,
        image,
        keys,
        policy,
        max_image_size,
    } = checked;
    let authentic = reauthenticate(
        &mut crate::partition(flash, region),
        keys,
        policy,
        max_image_size,
        image,
    );
    // The plan must be exactly what this authenticated slot produces. Read
    // it through the same absolute flash addresses `plan` used.
    let start = region
        .offset
        .checked_add(u32::from(image.header().hdr_size));
    let again = start.ok_or(()).and_then(|start| {
        flash.lock(|storage| {
            loader_plan::plan_from::<_, WORDS>(
                &mut *storage.borrow_mut(),
                start,
                start,
                image.header().img_size,
                layout::PROTECTED_START,
            )
            .map_err(|_| ())
        })
    });
    let plan_matches = ferriboot_core::Fih::ok(&again)
        & again.map_or(fih::FAIL, |again| loader_plan::plans_agree(&plan, &again));
    let verdict = authentic & plan_matches & counter_permits(counter, image);
    fih::random_delay(random.next_u32());
    verdict.require();
    ConfirmedPlan(plan)
}

/// Install the signed payload into the SRAM formerly occupied by IDF stage 2,
/// copy the plan into a non-overlapping protected slot, then hand over on a
/// dedicated stack. No Rust instruction in Ferriboot executes after the jump.
#[inline(never)]
pub fn execute(confirmed: ConfirmedPlan) -> ! {
    let ConfirmedPlan(plan) = confirmed;
    const PLAN_CAPACITY: usize = (layout::LOADER_STACK_BOTTOM - PLAN_ADDRESS) as usize;
    assert!(core::mem::size_of::<Plan>() <= PLAN_CAPACITY);
    assert!(!LOADER_IMAGE.is_empty());
    assert_eq!(LOADER_IMAGE.len() % 4, 0);
    assert!(LOADER_IMAGE.len() <= layout::IRAM_HANDOFF_SIZE as usize);
    assert_eq!(LOADER_IRAM - layout::IRAM_DRAM_OFFSET, LOADER_DRAM);

    // NOTE: IDF stage 2 has returned before this code executes. The regular
    // Ferriboot stack and code are below this memory and remain live until jr.
    // Copy through DRAM alias, as instruction memory is shared physical SRAM.
    // The chip layout fixes these addresses; nothing in Rust allocates there.
    let loader_dram = core::ptr::with_exposed_provenance_mut::<u32>(LOADER_DRAM as usize);
    let plan_dram = core::ptr::with_exposed_provenance_mut::<u32>(PLAN_ADDRESS as usize);
    let (words, _) = LOADER_IMAGE.as_chunks::<4>();
    for (index, word) in words.iter().enumerate() {
        let expected = u32::from_le_bytes(*word);
        // SAFETY: the abandoned IDF stage-2 IRAM handoff area, word aligned;
        // `index` < IRAM_HANDOFF_SIZE / 4 by the length assertion above.
        unsafe { write_volatile(loader_dram.wrapping_add(index), expected) };
    }
    for (index, word) in words.iter().enumerate() {
        let expected = u32::from_le_bytes(*word);
        // SAFETY: the same words just written, read back for verification.
        if unsafe { read_volatile(loader_dram.wrapping_add(index)) } != expected {
            log::error!("embedded bl-loader installation byte mismatch at word {index}");
            crate::boot_runtime::reset_after_delay();
        }
    }

    // Copy the complete C-ABI plan into the protected region BEFORE any app
    // RAM writes. The copy is word-aligned and finishes before the stack switch.
    // SAFETY: `Plan` is `repr(C)` and consists only of `u32`s; bl_loader
    // asserts at compile time that it has no padding, a size that is a
    // multiple of 4 and 4-byte alignment, so reading it as words is valid.
    // The destination is the protected plan slot, which fits `Plan`
    // (assertion above) and does not overlap this stack frame.
    unsafe {
        core::ptr::copy_nonoverlapping(
            core::ptr::addr_of!(plan).cast::<u32>(),
            plan_dram,
            core::mem::size_of::<Plan>() / 4,
        );
    }
    info!(
        "bl-loader installed: {} bytes at {LOADER_IRAM:#010x}; switching to protected stack",
        LOADER_IMAGE.len()
    );
    // Disable interrupts before abandoning the runtime/exception stack.
    // A shared physical SRAM alias is written; synchronize instruction fetch.
    // SAFETY: masks machine interrupts and orders the stores above; touches
    // no Rust-visible state.
    unsafe {
        asm!(
            "csrci mstatus, 8",
            "fence rw, rw",
            "fence.i",
            options(nostack)
        );
    }
    // Keep the stack-switch and jump contiguous. No Rust code may run
    // after SP is replaced. a0 holds the protected Plan pointer.
    // SAFETY: the loader image was installed and verified at LOADER_IRAM,
    // the confirmed plan is at PLAN_ADDRESS, STACK_TOP is the dedicated
    // handoff stack, and interrupts are masked. The asm never returns.
    unsafe {
        asm!(
            "mv sp, {new_sp}",
            "jr {target}",
            new_sp = in(reg) STACK_TOP,
            target = in(reg) LOADER_IRAM,
            in("a0") PLAN_ADDRESS,
            options(noreturn),
        );
    }
}
