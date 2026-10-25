//! RP2350 board support for the hardware scenarios: CMSIS-DAP/SWD and UART0.
//! Bootloader artifacts use checked ELF conversion and ROM secp256k1 sealing;
//! application artifacts use upstream imgtool (MCUboot P-256).
//! Scenarios compose the entire physical flash using the real updater/trailer.
//!
//! This module describes the board ([`BOARD`]) and holds the RP-specific
//! pieces; everything else is the crate's shared harness:
//! * [`layout`]: board constants from `ferriboot-layout-rp2350` (chip facts plus partitions), also used
//!   by `ferriboot-rp`.
//! * [`artifacts`]: cached builds ROM-sealed with `ferriboot_tools_rp2350::seal`.
//! * [`image`]: checked ELF conversion and Cortex-M vector mutations.
//! * [`flash`]: the Pico 2 layout and policy for [`flash::FlashImage`].
//! * [`sign`]: imgtool with the RP slot geometry and `--rom-fixed`.
//! * [`probe`]: attaching to the fixture target over SWD.

use crate::{Board, Console, FlashWindow, Probe, Signing};

pub mod artifacts;
pub mod flash;
pub mod image;
pub mod probe;
pub mod sign;

/// Constants shared with the target via the board's `no_std` layout crate.
pub use ferriboot_layout_rp2350 as layout;

pub use crate::{Result, dut, hooks};

pub static BOARD: Board = Board {
    name: "rp2350",
    chip: "RP2350",
    env_prefix: "FERRIBOOT_RP2350",
    target: "thumbv8m.main-none-eabihf",
    console: Console {
        port_hint: "the adapter's UART port (/dev/serial/by-id/...)",
        open: crate::dut::open_serial,
        // An external USB-UART adapter: target resets do not affect it.
        reenumerates: false,
    },
    probe: Some(Probe {
        link: "SWD",
        hint: "check SWD wiring and select the adapter using probe-rs list and PROBE_RS_PROBE",
        flash_window: Some(FlashWindow {
            window_address: probe::WINDOW_ADDRESS,
        }),
        attach: probe::attach,
    }),
    signing: Signing {
        header_size: layout::HEADER_SIZE,
        slot_size: layout::SLOT_SIZE,
        sector: layout::ERASE_SIZE,
        // Both physical slots execute through the same ATRANS1 runtime alias.
        vid: "microtun.dev",
        cid: "rp2350",
        rom_fixed: Some(layout::APP_XIP_BASE),
    },
};
