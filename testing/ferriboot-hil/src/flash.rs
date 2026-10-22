//! A complete 4 MiB flash image: bootloader, slots, scratch and storage.
//!
//! The board suites lay out the bootloader (and, on ESP, the partition table)
//! and hand over the slot layout; everything after that is shared.
//! Slots are staged with ferriboot-core's real [`FirmwareUpdater`] (the code
//! the application runs for OTA), so the suite exercises the same on-flash
//! encoding as a device.
//! The updater deliberately does not verify signatures; even a corrupt
//! candidate may be staged. Boot-time authentication must reject it.

use std::{cell::RefCell, rc::Rc};

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use ferriboot_core::{
    FirmwareUpdater, ImageVersion, Policy, PublicKey, Region, Slot, SlotLayout, SwapType,
    UpgradeMode, trailer::Trailer, validate::validate_image,
};

use crate::{Board, artifacts::repo_root, hooks::HookConfig};

pub const FLASH_SIZE: usize = 4 * 1024 * 1024;
pub const SECTOR: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashError {
    OutOfBounds,
    NotAligned,
}

impl NorFlashError for FlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            FlashError::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            FlashError::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

/// One region of the image as NOR flash with the geometry both boards' flash
/// drivers use (4-byte words, 4 KiB sectors); trailer offsets depend on it.
#[derive(Clone)]
pub struct Partition {
    mem: Rc<RefCell<Vec<u8>>>,
    region: Region,
}

impl Partition {
    fn range(
        &self,
        offset: u32,
        len: usize,
        align: u32,
    ) -> Result<std::ops::Range<usize>, FlashError> {
        if !offset.is_multiple_of(align) || !(len as u32).is_multiple_of(align) {
            return Err(FlashError::NotAligned);
        }
        if offset as usize + len > self.region.size as usize {
            return Err(FlashError::OutOfBounds);
        }
        let start = (self.region.offset + offset) as usize;
        Ok(start..start + len)
    }
}

impl ErrorType for Partition {
    type Error = FlashError;
}

impl ReadNorFlash for Partition {
    const READ_SIZE: usize = 4;
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), FlashError> {
        let r = self.range(offset, bytes.len(), 4)?;
        bytes.copy_from_slice(&self.mem.borrow()[r]);
        Ok(())
    }
    fn capacity(&self) -> usize {
        self.region.size as usize
    }
}

impl NorFlash for Partition {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR as usize;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), FlashError> {
        let r = self.range(offset, bytes.len(), 4)?;
        // NOR programming can only clear bits.
        for (dst, src) in self.mem.borrow_mut()[r].iter_mut().zip(bytes) {
            *dst &= src;
        }
        Ok(())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), FlashError> {
        if from > to {
            return Err(FlashError::OutOfBounds);
        }
        let r = self.range(from, (to - from) as usize, SECTOR)?;
        self.mem.borrow_mut()[r].fill(0xFF);
        Ok(())
    }
}

/// How a staged image is requested (`boot_set_pending`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Test,
    Permanent,
}

/// The trusted key ring of the test bootloader builds (the development key;
/// [`artifacts`](crate::artifacts) builds with it whatever the shell sets).
pub fn trusted_keys() -> Vec<PublicKey> {
    let pem = std::fs::read(repo_root().join("keys/dev-p256.pub.pem")).unwrap();
    let der = pem::parse(pem).unwrap();
    vec![PublicKey::from_spki_der(der.contents()).unwrap()]
}

#[derive(Clone)]
pub struct FlashImage {
    mem: Rc<RefCell<Vec<u8>>>,
    board: &'static Board,
    policy: Policy,
    pub layout: SlotLayout,
    /// The application-owned `storage` partition, if the layout has one.
    pub storage: Option<Region>,
}

impl FlashImage {
    /// Erased flash for `board`; the board suite then places its bootloader
    /// (and anything else outside the slots) with [`FlashImage::program`].
    /// `policy` is the image policy shared by the board's bootloader and
    /// application.
    pub fn erased(
        board: &'static Board,
        layout: SlotLayout,
        storage: Option<Region>,
        policy: Policy,
    ) -> Self {
        Self {
            mem: Rc::new(RefCell::new(vec![0xFF; FLASH_SIZE])),
            board,
            policy,
            layout,
            storage,
        }
    }

    /// The board this image is for; [`Dut::boot`](crate::dut::Dut::boot) uses it.
    pub fn board(&self) -> &'static Board {
        self.board
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.mem.borrow().clone()
    }

    /// Copy raw `bytes` to `offset` (factory programming outside the slots).
    pub fn program(&self, offset: u32, bytes: &[u8]) -> &Self {
        let at = offset as usize;
        self.mem.borrow_mut()[at..at + bytes.len()].copy_from_slice(bytes);
        self
    }

    pub fn partition(&self, region: Region) -> Partition {
        Partition {
            mem: self.mem.clone(),
            region,
        }
    }

    pub fn slot(&self, slot: Slot) -> Partition {
        self.partition(self.layout.region(slot))
    }

    fn trailer(&self) -> Trailer {
        Trailer::for_slot::<Partition>(self.layout.primary.size).unwrap()
    }

    // ----- writing -----------------------------------------------------------

    /// Erase a slot and program `image` at its start (factory programming).
    pub fn install(&self, slot: Slot, image: &[u8]) -> &Self {
        let mut p = self.slot(slot);
        let size = p.capacity() as u32;
        assert!(image.len() <= size as usize, "image exceeds slot");
        p.erase(0, size).unwrap();
        let mut padded = image.to_vec();
        padded.resize(image.len().next_multiple_of(4), 0xFF);
        p.write(0, &padded).unwrap();
        self
    }

    /// Replace the firmware's test-hook record in the `storage` partition.
    pub fn configure(&self, hooks: &HookConfig) -> &Self {
        let region = self.storage.expect("layout has no storage partition");
        let mut p = self.partition(region);
        p.erase(0, SECTOR).unwrap();
        let mut record = hooks.to_bytes();
        record.resize(record.len().next_multiple_of(4), 0xFF);
        p.write(0, &record).unwrap();
        self
    }

    /// Erase the `storage` partition: the firmware then runs normally.
    pub fn clear_hooks(&self) -> &Self {
        let region = self.storage.expect("layout has no storage partition");
        self.partition(region).erase(0, region.size).unwrap();
        self
    }

    /// Mark `slot`'s current contents confirmed (what `imgtool --confirm` writes).
    pub fn confirm_raw(&self, slot: Slot) -> &Self {
        let mut p = self.slot(slot);
        let t = self.trailer();
        t.write_magic(&mut p).unwrap();
        t.write_image_ok(&mut p).unwrap();
        self
    }

    /// Download `image` into `update` with the application-side updater,
    /// and request the upgrade. The bootloader authenticates on reboot.
    pub fn stage(
        &self,
        mode: UpgradeMode,
        update: Slot,
        image: &[u8],
        request: Request,
    ) -> crate::Result<()> {
        let mut updater =
            FirmwareUpdater::new_with_mode(self.slot(update.other()), self.slot(update), mode)
                .map_err(|e| format!("updater: {e:?}"))?;
        {
            let mut w = updater.begin_update().map_err(|e| format!("{e:?}"))?;
            for chunk in image.chunks(1000) {
                w.write(chunk).map_err(|e| format!("{e:?}"))?;
            }
            w.finish().map_err(|e| format!("{e:?}"))?;
        }
        match request {
            Request::Test => updater.mark_for_test(),
            Request::Permanent => updater.mark_permanent(),
        }
        .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }

    /// Install arbitrary raw bytes and write a scratch-swap pending trailer,
    /// bypassing the updater's ordinary flash-writing and bounds logic.
    pub fn stage_raw(&self, update: Slot, image: &[u8], request: Request) -> &Self {
        self.install(update, image);
        let mut p = self.slot(update);
        let t = self.trailer();
        t.write_magic(&mut p).unwrap();
        let swap_type = match request {
            Request::Test => SwapType::Test,
            Request::Permanent => {
                t.write_image_ok(&mut p).unwrap();
                SwapType::Perm
            }
        };
        t.write_swap_info(&mut p, swap_type, 0).unwrap();
        self
    }

    // ----- reading -----------------------------------------------------------

    /// Version of the authenticated image in `slot`, if any.
    pub fn version(&self, slot: Slot) -> Option<ImageVersion> {
        let keys = trusted_keys();
        let max = self.trailer().max_image_size();
        validate_image(&mut self.slot(slot), &keys, &self.policy, max)
            .ok()
            .map(|v| v.header().version)
    }

    pub fn read(&self, offset: u32, len: usize) -> Vec<u8> {
        self.mem.borrow()[offset as usize..offset as usize + len].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn erased() -> FlashImage {
        let slot = |n: u32| Region::new(n * 32 * SECTOR, 32 * SECTOR);
        let layout = SlotLayout {
            primary: slot(1),
            secondary: slot(2),
            scratch: None,
        };
        FlashImage::erased(&crate::TEST_BOARD, layout, Some(slot(3)), Policy::default())
    }

    #[test]
    fn partition_preserves_nor_write_and_erase_semantics() {
        let image = erased();
        let mut part = image.partition(Region::new(0, SECTOR * 2));
        part.write(SECTOR, &[0xf0, 0, 0xaa, 0x55]).unwrap();
        part.write(SECTOR, &[0x0f, 0xff, 0x55, 0xff]).unwrap();
        assert_eq!(image.read(SECTOR, 4), vec![0, 0, 0, 0x55]);
        part.erase(SECTOR, SECTOR * 2).unwrap();
        assert_eq!(image.read(SECTOR, 4), vec![0xff; 4]);
    }

    #[test]
    fn partition_rejects_unaligned_and_out_of_bounds_access() {
        let image = erased();
        let mut part = image.partition(Region::new(SECTOR, SECTOR));
        assert_eq!(part.write(2, &[0; 4]), Err(FlashError::NotAligned));
        assert_eq!(part.write(SECTOR, &[0; 4]), Err(FlashError::OutOfBounds));
        assert_eq!(part.erase(0, 4), Err(FlashError::NotAligned));
        assert_eq!(part.erase(SECTOR, 0), Err(FlashError::OutOfBounds));
    }

    #[test]
    fn program_places_raw_bytes_outside_the_slots() {
        let image = erased();
        image.program(0x10, &[1, 2, 3]);
        assert_eq!(image.read(0x0f, 5), vec![0xff, 1, 2, 3, 0xff]);
        assert_eq!(image.bytes().len(), FLASH_SIZE);
    }
}
