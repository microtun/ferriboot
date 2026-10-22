//! PL022 (ARM PrimeCell SSP) driver for the test-only W25Q32 SPI NOR.
//! This is real SPI command I/O: no guest reads or writes to a PSRAM mirror.
//! A Ferriboot QEMU patch connects APB SSP0 to a W25Q32 and exposes its
//! active-low chip select through the test-only 0x40030000 register.
use super::FlashError;
use crate::regs::spi::{CPSR, CR0, CR1, CS, DR, SR};

const SR_TNF: u32 = 0x02;
const SR_RNE: u32 = 0x04;
const MAX_POLLS: usize = 100_000;
const PAGE: usize = 256;
const SECTOR: usize = 4096;

fn selected(on: bool) {
    CS.write(u32::from(on));
}

/// Each transaction's CS rising edge ends the flash command, even on error.
struct Transaction;
impl Transaction {
    fn begin() -> Self {
        selected(true);
        Self
    }
    fn byte(&mut self, value: u8) -> Result<u8, FlashError> {
        // The QEMU PL022 has an eight-word TX and RX FIFO. Keeping one byte
        // outstanding prevents RX overflow and makes every byte full duplex.
        for _ in 0..MAX_POLLS {
            if SR.read() & SR_TNF != 0 {
                DR.write(u32::from(value));
                for _ in 0..MAX_POLLS {
                    if SR.read() & SR_RNE != 0 {
                        return Ok(DR.read() as u8);
                    }
                }
                return Err(FlashError::Io);
            }
        }
        Err(FlashError::Io)
    }
    fn addr(&mut self, addr: u32) -> Result<(), FlashError> {
        self.byte((addr >> 16) as u8)?;
        self.byte((addr >> 8) as u8)?;
        self.byte(addr as u8)?;
        Ok(())
    }
}
impl Drop for Transaction {
    fn drop(&mut self) {
        selected(false);
    }
}

pub fn init() -> Result<(), FlashError> {
    // 8-bit, Motorola mode 0. Clock rate is ignored by QEMU's PL022 model.
    selected(false);
    CR1.write(0);
    CR0.write(7);
    CPSR.write(2);
    CR1.write(2); // SSE, controller/master
    let mut t = Transaction::begin();
    t.byte(0x9f)?; // JEDEC ID
    let found = [t.byte(0)?, t.byte(0)?, t.byte(0)?];
    if found != [0xef, 0x40, 0x16] {
        return Err(FlashError::Protocol);
    }
    Ok(())
}

pub fn read(addr: u32, bytes: &mut [u8]) -> Result<(), FlashError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut t = Transaction::begin();
    t.byte(0x03)?; // READ, 24-bit address, no dummy cycles
    t.addr(addr)?;
    for b in bytes {
        *b = t.byte(0)?;
    }
    Ok(())
}

fn write_enable() -> Result<(), FlashError> {
    let mut t = Transaction::begin();
    t.byte(0x06)?; // WREN
    Ok(())
}

fn wait_ready() -> Result<(), FlashError> {
    let mut t = Transaction::begin();
    t.byte(0x05)?; // RDSR1; keep CS low throughout polling
    for _ in 0..MAX_POLLS {
        if t.byte(0)? & 1 == 0 {
            return Ok(());
        }
    }
    Err(FlashError::Io)
}

pub fn program(mut addr: u32, mut bytes: &[u8]) -> Result<(), FlashError> {
    while !bytes.is_empty() {
        // W25Q32 page program wraps *within* a page; never cross it.
        let n = bytes.len().min(PAGE - addr as usize % PAGE);
        write_enable()?;
        {
            let mut t = Transaction::begin();
            t.byte(0x02)?; // Page Program
            t.addr(addr)?;
            for &byte in &bytes[..n] {
                t.byte(byte)?;
            }
        } // CS high commits the page
        wait_ready()?;
        addr += n as u32;
        bytes = &bytes[n..];
    }
    Ok(())
}

pub fn erase(mut addr: u32, mut len: usize) -> Result<(), FlashError> {
    while len != 0 {
        write_enable()?;
        {
            let mut t = Transaction::begin();
            t.byte(0x20)?; // Sector Erase 4 KiB
            t.addr(addr)?;
        }
        wait_ready()?;
        addr += SECTOR as u32;
        len -= SECTOR;
    }
    Ok(())
}
