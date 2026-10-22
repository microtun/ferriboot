mod common;

use std::cell::RefCell;

use common::*;
use ferriboot_core::{
    BootConfig, Slot, UpgradeMode,
    layout::{FlashBounds, LayoutError, Region, SlotLayout},
    shared_data::{
        BLINFO_MAX_APPLICATION_SIZE, BLINFO_MODE, BLINFO_RECOVERY, BLINFO_RUNNING_SLOT,
        BLINFO_SIGNATURE_TYPE, BOOT_INFO_SIZE, BootInfo, SharedData, TLV_MAJOR_BLINFO,
        boot_save_shared_data,
    },
};

const S: u32 = SECTOR as u32;
const BOUNDS: FlashBounds = FlashBounds {
    capacity: 128 * SECTOR,
    reserved: 8 * S,
};

fn layout() -> SlotLayout {
    SlotLayout {
        primary: Region::new(8 * S, 32 * S),
        secondary: Region::new(40 * S, 32 * S),
        scratch: Some(Region::new(72 * S, 4 * S)),
    }
}

fn max() -> u32 {
    layout().max_image_size::<SimFlash>().unwrap()
}

fn info(mode: UpgradeMode, slot: Slot, max_image_size: u32) -> BootInfo {
    let data =
        boot_save_shared_data::<BOOT_INFO_SIZE>(mode, slot, max_image_size, None, None).unwrap();
    BootInfo::from_bytes(data.bytes()).unwrap()
}

/// A handoff assembled entry by entry, including ones the bootloader's own
/// writer refuses to produce.
fn raw_info(mode: u8, signature: u8, recovery: u8, slot: u8, max_image_size: u32) -> BootInfo {
    let mut data = SharedData::<BOOT_INFO_SIZE>::new().unwrap();
    for (minor, value) in [
        (BLINFO_MODE, &[mode][..]),
        (BLINFO_SIGNATURE_TYPE, &[signature]),
        (BLINFO_RECOVERY, &[recovery]),
        (BLINFO_RUNNING_SLOT, &[slot]),
        (BLINFO_MAX_APPLICATION_SIZE, &max_image_size.to_le_bytes()),
    ] {
        data.add(TLV_MAJOR_BLINFO, minor, value).unwrap();
    }
    BootInfo::from_bytes(data.bytes()).unwrap()
}

fn validate(l: SlotLayout, mode: UpgradeMode) -> Result<(), LayoutError> {
    l.validate::<SimFlash>(BOUNDS, mode)
}

#[test]
fn accepts_disjoint_aligned_slots_in_every_mode() {
    for mode in [
        UpgradeMode::SwapScratch,
        UpgradeMode::Overwrite,
        UpgradeMode::DirectXip { revert: false },
        UpgradeMode::DirectXip { revert: true },
    ] {
        assert_eq!(validate(layout(), mode), Ok(()), "{mode:?}");
    }
    assert_eq!(
        validate(
            SlotLayout {
                scratch: None,
                ..layout()
            },
            UpgradeMode::Overwrite
        ),
        Ok(())
    );
}

#[test]
fn rejects_bad_geometry() {
    let swap = UpgradeMode::SwapScratch;
    let with = |f: fn(&mut SlotLayout)| {
        let mut l = layout();
        f(&mut l);
        validate(l, swap)
    };
    assert_eq!(
        with(|l| l.secondary.size -= S),
        Err(LayoutError::SlotSizeMismatch)
    );
    assert_eq!(
        with(|l| l.primary.offset += 1),
        Err(LayoutError::Misaligned)
    );
    assert_eq!(
        with(|l| l.scratch = Some(Region::new(72 * S, 0))),
        Err(LayoutError::Misaligned)
    );
    assert_eq!(
        with(|l| l.scratch = Some(Region::new(127 * S, 4 * S))),
        Err(LayoutError::OutOfBounds)
    );
    assert_eq!(
        with(|l| l.scratch = Some(Region::new(u32::MAX - S + 1, 4 * S))),
        Err(LayoutError::OutOfBounds),
        "end overflows"
    );
    assert_eq!(
        with(|l| l.primary.offset = 4 * S),
        Err(LayoutError::Overlap),
        "inside the reserved bootloader area"
    );
    assert_eq!(
        with(|l| l.secondary.offset = 39 * S),
        Err(LayoutError::Overlap),
        "slots overlap"
    );
    assert_eq!(
        with(|l| l.scratch = Some(Region::new(70 * S, 4 * S))),
        Err(LayoutError::Overlap),
        "scratch overlaps a slot"
    );
    assert_eq!(with(|l| l.scratch = None), Err(LayoutError::MissingScratch));
    assert_eq!(
        with(|l| l.scratch = Some(Region::new(72 * S, S))),
        Err(LayoutError::ScratchTooSmall)
    );
}

#[test]
fn handoff_must_match_size_verifier_and_slot_for_reported_mode() {
    let l = layout();
    let swap = UpgradeMode::SwapScratch;
    let xip = UpgradeMode::DirectXip { revert: true };
    let active = |i: &BootInfo| l.active_slot::<SimFlash>(i);

    assert_eq!(active(&info(swap, Slot::Primary, max())), Ok(Slot::Primary));
    assert_eq!(
        active(&info(xip, Slot::Secondary, max())),
        Ok(Slot::Secondary)
    );

    let invalid = Err(LayoutError::InvalidHandoff);
    assert_eq!(active(&info(swap, Slot::Primary, max() - S)), invalid);
    // Mode 1 is scratch swap, signature type 2 is ECDSA P-256.
    assert_eq!(active(&raw_info(1, 2, 0, 0, max())), Ok(Slot::Primary));
    assert_eq!(
        active(&raw_info(1, 2, 0, 1, max())),
        invalid,
        "swap mode runs from the primary slot"
    );
    assert_eq!(active(&raw_info(1, 1, 0, 0, max())), invalid, "verifier");
    assert_eq!(
        active(&raw_info(1, 2, 1, 0, max())),
        Ok(Slot::Primary),
        "recovery does not affect verifier"
    );
}

#[test]
fn updater_writes_the_slot_the_bootloader_did_not_start() {
    let power = Power::default();
    let requested = RefCell::new(Vec::new());
    let partition = |r: Region| {
        requested.borrow_mut().push(r);
        Ok(SimFlash::new(r.size as usize, &power))
    };
    let l = layout();
    let xip = UpgradeMode::DirectXip { revert: false };

    let i = info(xip, Slot::Secondary, max());
    let mut up = l.updater(BOUNDS, &i, partition).unwrap();
    assert_eq!(up.slot_size(), 32 * S);
    assert!(
        !up.needs_confirmation().unwrap(),
        "DirectXip without revert comes from the boot handoff"
    );
    assert_eq!(*requested.borrow(), [l.secondary, l.primary]);

    // The application updater requires neither signing keys nor policy.
    // Those are enforced by the bootloader at reboot.
    requested.borrow_mut().clear();
    l.updater(BOUNDS, &i, partition).unwrap();

    // Geometry validation also follows the mode published in the handoff.
    requested.borrow_mut().clear();
    let swap_info = info(UpgradeMode::SwapScratch, Slot::Primary, max());
    let no_scratch = SlotLayout { scratch: None, ..l };
    assert!(matches!(
        no_scratch.updater(BOUNDS, &swap_info, partition,),
        Err(LayoutError::MissingScratch)
    ));
    assert!(requested.borrow().is_empty());

    // An invalid handoff or layout fails before any partition is created.
    requested.borrow_mut().clear();
    let invalid = raw_info(1, 2, 0, 1, max());
    assert!(matches!(
        l.updater(BOUNDS, &invalid, partition),
        Err(LayoutError::InvalidHandoff)
    ));
    let bad = SlotLayout {
        secondary: l.primary,
        ..l
    };
    assert!(matches!(
        bad.updater(BOUNDS, &i, partition),
        Err(LayoutError::Overlap)
    ));
    assert!(requested.borrow().is_empty());
}

#[test]
fn bootloader_only_opens_scratch_for_scratch_swap() {
    let power = Power::default();
    let rig = Rig::new(BootConfig::default());
    let requested = RefCell::new(Vec::new());
    let partition = |r: Region| {
        requested.borrow_mut().push(r);
        Ok(SimFlash::new(r.size as usize, &power))
    };
    let l = layout();

    let overwrite = BootConfig {
        mode: UpgradeMode::Overwrite,
        ..BootConfig::default()
    };
    let b = l
        .bootloader(BOUNDS, &rig.keys, overwrite, partition)
        .unwrap();
    assert_eq!(b.max_image_size(), max());
    assert_eq!(*requested.borrow(), [l.primary, l.secondary]);

    requested.borrow_mut().clear();
    l.bootloader(BOUNDS, &rig.keys, BootConfig::default(), partition)
        .unwrap();
    assert_eq!(requested.borrow().len(), 3);

    requested.borrow_mut().clear();
    let no_scratch = SlotLayout { scratch: None, ..l };
    assert!(matches!(
        no_scratch.bootloader(BOUNDS, &rig.keys, BootConfig::default(), partition),
        Err(LayoutError::MissingScratch)
    ));
    assert!(requested.borrow().is_empty());
}
