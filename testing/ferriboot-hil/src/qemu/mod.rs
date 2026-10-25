//! Isolated QEMU mps2-an385 guest backend; no serial adapter or probe required.
use crate::{Board, Console, Signing};
pub use crate::{Result, dut, hooks};
pub use ferriboot_layout_qemu as layout;
pub use session::VirtualBoard;

pub mod artifacts;
pub mod flash;
pub mod image;
pub(crate) mod session;
pub mod sign;

pub static BOARD: Board = Board {
    name: "qemu",
    chip: "QEMU Cortex-M3",
    env_prefix: "FERRIBOOT_QEMU",
    target: "thumbv7m-none-eabi",
    // These physical-board callbacks are never used; Dut dispatches to QEMU.
    console: Console {
        port_hint: "QEMU console",
        open: crate::dut::open_serial,
        reenumerates: false,
    },
    probe: None,
    signing: Signing {
        header_size: layout::HEADER_SIZE,
        slot_size: layout::SLOT_SIZE,
        sector: layout::ERASE_SIZE,
        vid: "microtun.dev",
        cid: "qemu-an385",
        rom_fixed: Some(layout::APP_XIP_BASE),
    },
};
