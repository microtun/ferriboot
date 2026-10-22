//! CMSDK APB UART0 at 0x40004000 on the MPS2 AN385 FPGA image.
use crate::regs::uart::{BAUDDIV, CTRL, DATA, STATE};

const TXFULL: u32 = 1;

pub fn init() {
    // 25 MHz PCLK / 217 = ~115207 baud. TX only, polled.
    BAUDDIV.write(217);
    CTRL.write(1);
}

pub fn print(text: &str) {
    for &byte in text.as_bytes() {
        // UART0 STATE.TXFULL must be clear before writing its DATA.
        while STATE.read() & TXFULL != 0 {}
        DATA.write(u32::from(byte));
    }
}
