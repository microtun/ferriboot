//! Cache-independent diagnostic output to the ESP32-C3 native USB Serial/JTAG
//! CDC/ACM port. This is the same port used by the board's esp-println logger.
//!
//! No HAL, ROM printf, heap, globals, formatted-string machinery, or calls
//! into Ferriboot are used. The compiled function and its string literals are
//! included in the self-contained, relocated IRAM payload.
//!
//! ESP32-C3 TRM: USB_SERIAL_JTAG_EP1_REG and EP1_CONF_REG at 0x6004_3000/+4.
//! The transmit packet is <=64 bytes, committed with WR_DONE. We never wait
//! indefinitely if the USB host is absent, disconnected, or stops reading.

use core::ptr::{read_volatile, write_volatile};

const USB_EP1_DATA: *mut u32 = 0x6004_3000 as *mut u32;
const USB_EP1_CONF: *mut u32 = 0x6004_3004 as *mut u32;
const TX_FREE: u32 = 1 << 1;
const WR_DONE: u32 = 1;
const MAX_SPINS: usize = 250_000;

/// One compact diagnostic packet: `BL> stage 0x01234567\r\n`.
///
/// Label must be ASCII and fit within 40 bytes. The message is assembled on
/// the protected stack, so there are no references to Ferriboot flash data.
#[inline(never)]
pub fn event(label: &[u8], value: u32) {
    let mut line = [0u8; 64];
    let mut len = 0;

    for &b in b"BL> " {
        line[len] = b;
        len += 1;
    }
    // 4 prefix + 40 label + 3 separator + 8 hex + 2 CRLF = 57 bytes.
    for &b in label.iter().take(40) {
        line[len] = b;
        len += 1;
    }
    for &b in b" 0x" {
        line[len] = b;
        len += 1;
    }
    for shift in (0..8).rev() {
        let nibble = ((value >> (shift * 4)) & 0xf) as u8;
        line[len] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        len += 1;
    }
    line[len] = b'\r';
    line[len + 1] = b'\n';
    len += 2;

    // A previous USB packet may still be in flight. If the host isn't
    // connected, discard the log rather than blocking the boot handoff.
    if !wait_tx_free() {
        return;
    }
    for &byte in &line[..len] {
        // SAFETY: C3 USB_SERIAL_JTAG EP1 data MMIO register.
        unsafe { write_volatile(USB_EP1_DATA, u32::from(byte)) };
    }
    // SAFETY: C3 USB_SERIAL_JTAG EP1_CONF MMIO register.
    unsafe { write_volatile(USB_EP1_CONF, WR_DONE) };

    // Try to drain before ROM reset / MMU handoff. Bounded even if the host
    // disappears, so a debug print cannot *cause* an endless boot loop.
    let _ = wait_tx_free();
}

#[inline(never)]
fn wait_tx_free() -> bool {
    for _ in 0..MAX_SPINS {
        // SAFETY: C3 USB_SERIAL_JTAG EP1_CONF MMIO register.
        if unsafe { read_volatile(USB_EP1_CONF as *const u32) } & TX_FREE != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}
