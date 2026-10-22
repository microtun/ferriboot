//! Small helpers over `embedded-storage` NOR flash that take care of read and
//! write granularity, and map flash errors into [`crate::Error`].

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};

use crate::{ConfigError, Error};

/// Value of an erased byte. All supported NOR flashes erase to `0xff`.
pub const ERASED: u8 = 0xff;

/// Flash adapter that reports completed I/O to a watchdog or progress monitor.
/// Erases are split into sectors, so erasing a whole slot can take longer than
/// the watchdog period while a stuck sector operation still times out.
pub struct ProgressFlash<F, P> {
    flash: F,
    progress: P,
}

impl<F, P> ProgressFlash<F, P> {
    pub fn new(flash: F, progress: P) -> Self {
        Self { flash, progress }
    }

    pub fn into_inner(self) -> F {
        self.flash
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressFlashError<E> {
    Driver(E),
    OutOfBounds,
    NotAligned,
}

impl<E: embedded_storage::nor_flash::NorFlashError> embedded_storage::nor_flash::NorFlashError
    for ProgressFlashError<E>
{
    fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
        use embedded_storage::nor_flash::NorFlashErrorKind;
        match self {
            Self::Driver(e) => e.kind(),
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

impl<F: ReadNorFlash, P> embedded_storage::nor_flash::ErrorType for ProgressFlash<F, P> {
    type Error = ProgressFlashError<F::Error>;
}

impl<F: ReadNorFlash, P: FnMut()> ReadNorFlash for ProgressFlash<F, P> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash
            .read(offset, bytes)
            .map_err(ProgressFlashError::Driver)?;
        (self.progress)();
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

impl<F: NorFlash, P: FnMut()> NorFlash for ProgressFlash<F, P> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash
            .write(offset, bytes)
            .map_err(ProgressFlashError::Driver)?;
        (self.progress)();
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        // Check the entire request before the first destructive operation.
        if from > to || to as usize > self.flash.capacity() {
            return Err(ProgressFlashError::OutOfBounds);
        }
        let sector = u32::try_from(F::ERASE_SIZE)
            .ok()
            .filter(|size| *size != 0)
            .ok_or(ProgressFlashError::NotAligned)?;
        if !from.is_multiple_of(sector) || !to.is_multiple_of(sector) {
            return Err(ProgressFlashError::NotAligned);
        }
        let mut offset = from;
        while offset < to {
            self.flash
                .erase(offset, offset + sector)
                .map_err(ProgressFlashError::Driver)?;
            (self.progress)();
            offset += sector;
        }
        Ok(())
    }
}

/// Chunk size used for copying and hashing. Kept small because the
/// bootloader runs with a few KiB of stack.
pub(crate) const CHUNK: usize = 512;
const READ_BOUNCE: usize = 64;

/// Validate flash geometry that the bounded helpers rely on. Constructors call
/// this before any I/O so unsupported backends fail as configuration errors.
pub(crate) fn check_geometry<F: NorFlash>() -> Result<(), ConfigError> {
    if F::READ_SIZE == 0 || F::READ_SIZE > READ_BOUNCE {
        return Err(ConfigError::UnsupportedReadSize);
    }
    Ok(())
}

/// A word-aligned byte buffer. Some drivers (esp-storage) fall back to a
/// large on-stack bounce buffer for unaligned destination pointers.
#[repr(C, align(4))]
pub(crate) struct Aligned<const N: usize>(pub [u8; N]);

pub(crate) const fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align) * align
}

pub(crate) const fn align_down(value: u32, align: u32) -> u32 {
    value / align * align
}

pub(crate) fn is_erased(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| *b == ERASED)
}

fn word_aligned(bytes: &[u8]) -> bool {
    (bytes.as_ptr() as usize).is_multiple_of(4)
}

/// Read an arbitrary byte range, independent of the flash read granularity.
pub fn read<F: ReadNorFlash>(
    flash: &mut F,
    offset: u32,
    buf: &mut [u8],
) -> Result<(), Error<F::Error>> {
    if F::READ_SIZE == 0 || F::READ_SIZE > READ_BOUNCE {
        return Err(Error::Config(ConfigError::UnsupportedReadSize));
    }
    let rs = F::READ_SIZE;
    if rs == 1
        || ((offset as usize).is_multiple_of(rs)
            && buf.len().is_multiple_of(rs)
            && word_aligned(buf))
    {
        return flash.read(offset, buf).map_err(Error::Flash);
    }

    let mut tmp = Aligned([0u8; READ_BOUNCE]);
    let chunk = tmp.0.len() / rs * rs;
    if chunk == 0 {
        return Err(Error::Config(ConfigError::UnsupportedReadSize));
    }

    let mut done = 0usize;
    while done < buf.len() {
        let pos = offset as usize + done;
        let aligned = pos - pos % rs;
        let skip = pos - aligned;
        let want = buf.len() - done;
        let len = ((skip + want).div_ceil(rs) * rs).min(chunk);
        flash
            .read(aligned as u32, &mut tmp.0[..len])
            .map_err(Error::Flash)?;
        let n = (len - skip).min(want);
        buf[done..done + n].copy_from_slice(&tmp.0[skip..skip + n]);
        done += n;
    }
    Ok(())
}

/// Read a single byte.
pub fn read_u8<F: ReadNorFlash>(flash: &mut F, offset: u32) -> Result<u8, Error<F::Error>> {
    let mut b = [0u8; 1];
    read(flash, offset, &mut b)?;
    Ok(b[0])
}

/// Write `data` (at most 32 bytes) at a write-aligned offset, padding the
/// tail up to the write granularity with the erased value.
pub fn write_padded<F: NorFlash>(
    flash: &mut F,
    offset: u32,
    data: &[u8],
) -> Result<(), Error<F::Error>> {
    let ws = F::WRITE_SIZE.max(1);
    let len = data.len().div_ceil(ws) * ws;
    let mut tmp = Aligned([ERASED; 32]);
    assert!(len <= tmp.0.len());
    tmp.0[..data.len()].copy_from_slice(data);
    flash.write(offset, &tmp.0[..len]).map_err(Error::Flash)
}

/// Erase `[from, to)`; both must be erase aligned.
pub fn erase<F: NorFlash>(flash: &mut F, from: u32, to: u32) -> Result<(), Error<F::Error>> {
    if from >= to {
        return Ok(());
    }
    flash.erase(from, to).map_err(Error::Flash)
}

/// Erase `[from, to)` one sector at a time starting at the end.
///
/// Trailers are always destroyed back to front so that an interrupted erase
/// never leaves a valid magic in front of stale trailer fields.
pub fn erase_backwards<F: NorFlash>(
    flash: &mut F,
    from: u32,
    to: u32,
) -> Result<(), Error<F::Error>> {
    let es = F::ERASE_SIZE as u32;
    let mut off = to;
    while off > from {
        off -= es;
        flash.erase(off, off + es).map_err(Error::Flash)?;
    }
    Ok(())
}

/// Copy `len` bytes between two flash areas. The destination must already be
/// erased. Chunks that are entirely erased are skipped.
pub fn copy<F: NorFlash>(
    src: &mut F,
    src_off: u32,
    dst: &mut F,
    dst_off: u32,
    len: u32,
) -> Result<(), Error<F::Error>> {
    let gran = F::WRITE_SIZE.max(F::READ_SIZE).max(1) as u32;
    let len = align_up(len, gran);
    let mut buf = Aligned([0u8; CHUNK]);
    let mut done = 0u32;
    while done < len {
        let n = (len - done).min(CHUNK as u32);
        let chunk = &mut buf.0[..n as usize];
        read(src, src_off + done, chunk)?;
        if !is_erased(chunk) {
            dst.write(dst_off + done, chunk).map_err(Error::Flash)?;
        }
        done += n;
    }
    Ok(())
}
