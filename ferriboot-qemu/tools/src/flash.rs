//! Virtual AN385 NOR flashing. Unlike the hardware tools this updates a
//! persistent 4 MiB backing file; it never invokes or modifies a real device.
use anyhow::{Context, Result, ensure};
use ferriboot_build::tools::{mcuboot, progress::Progress};
use ferriboot_layout_qemu as board;
use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const CHUNK: usize = 64 * 1024;
const ERASED: u8 = 0xff;

/// Default location shared by `flash-app`, `flash-bootloader`, and `run`.
pub fn default_path() -> PathBuf {
    ferriboot_build::tools::repo_root().join("target/qemu/flash.bin")
}

/// Check an MCUboot-signed application for this board, including its virtual
/// execution address and vector table, before opening a flash backing file.
pub fn check_signed_app(data: &[u8]) -> Result<usize> {
    let signed = mcuboot::inspect(data, crate::geometry(), None)?;
    ensure!(
        signed.load_address == board::APP_XIP_BASE
            && signed.header_size == board::HEADER_SIZE as usize
            && signed.flags == 0x100,
        "signed application differs from QEMU linking contract"
    );
    ferriboot_build::tools::elf::check_vectors(
        &data[signed.header_size..signed.header_size + signed.body_size],
        board::APP_XIP_BASE + board::HEADER_SIZE,
        board::RAM_BASE,
        board::BOOT_INFO_ADDRESS,
    )?;
    Ok(signed.size)
}

/// Only one of the two application slots can be erased, never scratch/storage.
fn checked_slot(offset: u32, length: usize) -> Result<u32> {
    ensure!(
        offset == board::PRIMARY_OFFSET || offset == board::SECONDARY_OFFSET,
        "QEMU flash-app offset is not an application slot"
    );
    ensure!(
        length > 0 && length <= board::SLOT_SIZE as usize,
        "signed application is empty or exceeds the slot"
    );
    Ok(board::SLOT_SIZE)
}

/// Erase the *whole* chosen slot, then write just the signed TLV envelope.
pub fn flash_app(path: &Path, signed: &[u8], offset: u32) -> Result<()> {
    let real_len = check_signed_app(signed)?;
    let size = checked_slot(offset, real_len)?;
    write_region(path, offset, size, &signed[..real_len], "application slot")
}

/// Erase only the virtual bootloader partition; leave app slots and OTP alone.
/// The caller must first convert and validate the bootloader ELF.
pub fn flash_bootloader(path: &Path, image: &[u8]) -> Result<()> {
    ensure!(
        !image.is_empty() && image.len() <= board::BOOTLOADER_SIZE as usize,
        "QEMU bootloader is empty or exceeds its partition"
    );
    write_region(path, 0, board::BOOTLOADER_SIZE, image, "bootloader")
}

/// Open a complete, existing NOR image or create a truly erased 0xFF one.
/// Reject short files rather than silently extending them with 0x00 bytes.
fn open_flash(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let result = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path);
    let mut file = match result {
        Ok(mut file) => {
            let mut progress =
                Progress::new("Creating erased QEMU NOR", Some(board::FLASH_SIZE as u64));
            let block = [ERASED; CHUNK];
            let mut remaining = board::FLASH_SIZE as usize;
            while remaining != 0 {
                let count = remaining.min(block.len());
                file.write_all(&block[..count])?;
                progress.inc(count as u64);
                remaining -= count;
            }
            file.sync_all()?;
            progress.finish();
            file
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            OpenOptions::new().read(true).write(true).open(path)?
        }
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    };
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() == board::FLASH_SIZE as u64,
        "QEMU NOR backing file {} must be exactly {} bytes",
        path.display(),
        board::FLASH_SIZE
    );
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

/// Erase/program/verify only a sector-aligned partition, preserving all other
/// NOR sectors. Bytes written/verified drive real percentage progress bars.
fn write_region(path: &Path, offset: u32, size: u32, image: &[u8], name: &str) -> Result<()> {
    ensure!(
        size > 0
            && offset.is_multiple_of(board::ERASE_SIZE)
            && size.is_multiple_of(board::ERASE_SIZE)
            && offset
                .checked_add(size)
                .is_some_and(|end| end <= board::FLASH_SIZE)
            && !image.is_empty()
            && image.len() <= size as usize,
        "invalid QEMU NOR partition or image"
    );
    let mut file = open_flash(path)?;
    let block = [ERASED; CHUNK];
    file.seek(SeekFrom::Start(u64::from(offset)))?;
    let mut erase = Progress::new(format!("Erasing QEMU {name}"), Some(u64::from(size)));
    let mut remaining = size as usize;
    while remaining > 0 {
        let count = remaining.min(block.len());
        file.write_all(&block[..count])?;
        erase.inc(count as u64);
        remaining -= count;
    }
    erase.finish();

    file.seek(SeekFrom::Start(u64::from(offset)))?;
    let mut program = Progress::new(format!("Programming QEMU {name}"), Some(image.len() as u64));
    for chunk in image.chunks(CHUNK) {
        file.write_all(chunk)?;
        program.inc(chunk.len() as u64);
    }
    file.sync_all()?;
    program.finish();

    // Verify both the programmed image and the remainder of the erased slot,
    // so a stale MCUboot trailer cannot survive a short application update.
    file.seek(SeekFrom::Start(u64::from(offset)))?;
    let mut verify = Progress::new(format!("Verifying QEMU {name}"), Some(u64::from(size)));
    let mut buffer = [0u8; CHUNK];
    let mut pos = 0usize;
    while pos < size as usize {
        let count = (size as usize - pos).min(CHUNK);
        file.read_exact(&mut buffer[..count])?;
        for (i, &byte) in buffer[..count].iter().enumerate() {
            let index = pos + i;
            let expected = image.get(index).copied().unwrap_or(ERASED);
            ensure!(
                byte == expected,
                "QEMU {name} verification failed at {:#x}",
                offset as usize + index
            );
        }
        pos += count;
        verify.inc(count as u64);
    }
    verify.finish();
    Ok(())
}

/// Read the QEMU -kernel image from the same flash snapshot as the SPI NOR.
pub fn read_bootloader(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    ensure!(
        file.metadata()?.len() == board::FLASH_SIZE as u64,
        "invalid QEMU NOR size"
    );
    let mut bytes = vec![0; board::BOOTLOADER_SIZE as usize];
    file.read_exact(&mut bytes)?;
    ensure!(
        bytes[..8].iter().any(|&b| b != ERASED),
        "QEMU bootloader is blank: flash it first"
    );
    Ok(bytes)
}

/// Persist a virtual OTP backing file between QEMU runs. Only initialize it
/// once; flashing NOR must never clear an already-burned security counter.
pub fn ensure_efuses(flash: &Path) -> Result<PathBuf> {
    let path = flash.with_file_name("efuses.bin");
    ferriboot_build::tools::distinct_output(&path, &[flash])?;
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(&[0; board::SECURITY_COUNTER_BYTES])?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    ensure!(
        fs::metadata(&path)?.len() == board::SECURITY_COUNTER_BYTES as u64,
        "invalid QEMU eFuse backing file size (refusing to reinitialize it)"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_slot_range_rejects_bootloader_and_out_of_bounds() {
        assert_eq!(
            checked_slot(board::PRIMARY_OFFSET, 1).unwrap(),
            board::SLOT_SIZE
        );
        assert_eq!(
            checked_slot(board::SECONDARY_OFFSET, 1).unwrap(),
            board::SLOT_SIZE
        );
        assert!(checked_slot(0, 1).is_err());
        assert!(checked_slot(board::PRIMARY_OFFSET + 1, 1).is_err());
        assert!(checked_slot(board::PRIMARY_OFFSET, 0).is_err());
        assert!(checked_slot(board::PRIMARY_OFFSET, board::SLOT_SIZE as usize + 1).is_err());
    }

    #[test]
    fn flashing_preserves_unselected_regions_and_clears_old_trailers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flash.bin");
        write_region(
            &path,
            0,
            board::BOOTLOADER_SIZE,
            b"bootloader",
            "bootloader",
        )
        .unwrap();
        let slot = board::PRIMARY_OFFSET;
        write_region(&path, slot, board::SLOT_SIZE, b"old-image", "app").unwrap();
        write_region(&path, slot, board::SLOT_SIZE, b"new", "app").unwrap();
        let image = fs::read(&path).unwrap();
        assert_eq!(image.len(), board::FLASH_SIZE as usize);
        assert_eq!(&image[..10], b"bootloader");
        assert_eq!(&image[slot as usize..slot as usize + 3], b"new");
        assert_eq!(&image[slot as usize + 3..slot as usize + 9], &[ERASED; 6]);
        assert_eq!(image[board::SECONDARY_OFFSET as usize], ERASED);
        assert_eq!(image[board::STORAGE_OFFSET as usize], ERASED);
    }

    #[test]
    fn invalid_images_never_create_or_touch_nor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flash.bin");
        assert!(flash_app(&path, b"not signed", board::PRIMARY_OFFSET).is_err());
        assert!(!path.exists());
        assert!(flash_bootloader(&path, &[]).is_err());
        assert!(flash_bootloader(&path, &vec![0x55; board::BOOTLOADER_SIZE as usize + 1]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn invalid_flash_and_otp_are_rejected_not_reinitialized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flash.bin");
        fs::write(&path, b"do not overwrite").unwrap();
        assert!(flash_bootloader(&path, b"valid-size").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"do not overwrite");

        fs::remove_file(&path).unwrap();
        flash_bootloader(&path, b"boot").unwrap();
        let otp = ensure_efuses(&path).unwrap();
        let mut data = fs::read(&otp).unwrap();
        data[0] = 0x0f;
        fs::write(&otp, &data).unwrap();
        flash_bootloader(&path, b"new-boot").unwrap();
        assert_eq!(fs::read(ensure_efuses(&path).unwrap()).unwrap(), data);
        fs::write(&otp, [0]).unwrap();
        assert!(ensure_efuses(&path).is_err());
    }
}
