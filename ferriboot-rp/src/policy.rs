//! RP2350 image-acceptance policy.
//!
//! The VID/CID are Microtun-derived identifiers, generated exactly as
//! `imgtool sign --vid microtun.dev --cid rp2350`.
//! Keep both values synchronized with the image-signing defaults.
//! Both the bootloader and the firmware updater must enforce the same policy.

pub use ferriboot_core::Policy;

/// UUIDv5(DNS, "microtun.dev") — shared Microtun vendor ID.
pub const IMAGE_VID: [u8; 16] = [
    0xb3, 0x6b, 0x1d, 0x87, 0x77, 0x70, 0x53, 0xa0, 0xa5, 0x23, 0x9d, 0x81, 0x1b, 0x3b, 0x95, 0x5f,
];
/// UUIDv5(IMAGE_VID, "rp2350") — RP2350 firmware class.
pub const IMAGE_CID: [u8; 16] = [
    0xcb, 0x34, 0x71, 0x58, 0x28, 0xf2, 0x54, 0xdb, 0x96, 0x7e, 0x3e, 0x0a, 0x61, 0xc9, 0x66, 0x73,
];

/// RP2350-specific constructors for [`Policy`].
pub trait PolicyExt {
    /// Require the signed image's protected VID/CID TLVs and security counter.
    fn rp2350() -> Self;
}

impl PolicyExt for Policy {
    fn rp2350() -> Self {
        Self {
            min_security_counter: Some(0),
            vid: Some(IMAGE_VID),
            cid: Some(IMAGE_CID),
            ..Self::default()
        }
    }
}
