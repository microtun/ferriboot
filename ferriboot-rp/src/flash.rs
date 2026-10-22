use core::cell::RefCell;

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

use crate::{LayoutError, Region};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashError<E> {
    Driver(E),
    OutOfBounds,
    NotAligned,
}

impl<E: NorFlashError> NorFlashError for FlashError<E> {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Driver(e) => e.kind(),
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

/// Present four-byte writes to MCUboot's trailer geometry. Embassy's RP
/// driver pads partial pages with 0xff; it never erases a page to update a
/// flag. This is essential to preserve swap progress across power loss.
pub struct WordFlash<F>(F);
impl<F: NorFlash> WordFlash<F> {
    pub fn new(flash: F) -> Self {
        assert!(F::WRITE_SIZE != 0 && F::WRITE_SIZE <= 4 && 4 % F::WRITE_SIZE == 0);
        Self(flash)
    }

    pub fn release(self) -> F {
        self.0
    }
}

impl<F: NorFlash> ErrorType for WordFlash<F> {
    type Error = FlashError<F::Error>;
}

impl<F: NorFlash> ReadNorFlash for WordFlash<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes).map_err(FlashError::Driver)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<F: NorFlash> NorFlash for WordFlash<F> {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        if !offset.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
            return Err(FlashError::NotAligned);
        }
        self.0.write(offset, bytes).map_err(FlashError::Driver)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.0.erase(from, to).map_err(FlashError::Driver)
    }
}

/// Single-core shared partition, with offsets relative to the region.
/// Keep this on core 0; use Embassy's multicore APIs to start core 1 so
/// the RP flash driver can pause it during mutations.
pub struct Partition<'a, F: NorFlash> {
    flash: &'a RefCell<F>,
    region: Region,
}

impl<'a, F: NorFlash> Partition<'a, F> {
    /// A partition over `region`, checked for erase alignment and device bounds.
    pub fn new(flash: &'a RefCell<F>, region: Region) -> Result<Self, LayoutError> {
        region.check::<F>(flash.borrow().capacity())?;
        Ok(Self { flash, region })
    }

    fn address(&self, offset: u32, len: usize) -> Result<u32, FlashError<F::Error>> {
        if offset > self.region.size || len > (self.region.size - offset) as usize {
            return Err(FlashError::OutOfBounds);
        }
        Ok(self.region.offset + offset)
    }
}

impl<F: NorFlash> ErrorType for Partition<'_, F> {
    type Error = FlashError<F::Error>;
}

impl<F: NorFlash> ReadNorFlash for Partition<'_, F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let address = self.address(offset, bytes.len())?;
        self.flash
            .borrow_mut()
            .read(address, bytes)
            .map_err(FlashError::Driver)
    }

    fn capacity(&self) -> usize {
        self.region.size as usize
    }
}

impl<F: NorFlash> NorFlash for Partition<'_, F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let address = self.address(offset, bytes.len())?;
        self.flash
            .borrow_mut()
            .write(address, bytes)
            .map_err(FlashError::Driver)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.checked_sub(from).ok_or(FlashError::OutOfBounds)?;
        let address = self.address(from, len as usize)?;
        self.flash
            .borrow_mut()
            .erase(address, address + len)
            .map_err(FlashError::Driver)
    }
}
