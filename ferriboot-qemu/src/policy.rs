//! QEMU image-acceptance policy.
//!
//! The VID/CID are Microtun-derived identifiers, generated exactly as
//! `imgtool sign --vid microtun.dev --cid qemu-an385`.
//! Keep both values synchronized with the image-signing defaults.
//! Both the bootloader and the firmware updater must enforce the same policy.

pub use ferriboot_core::Policy;

/// UUIDv5(DNS, "microtun.dev") — shared Microtun vendor ID.
pub const IMAGE_VID: [u8; 16] = [
    0xb3, 0x6b, 0x1d, 0x87, 0x77, 0x70, 0x53, 0xa0, 0xa5, 0x23, 0x9d, 0x81, 0x1b, 0x3b, 0x95, 0x5f,
];
/// UUIDv5(IMAGE_VID, "qemu-an385") — QEMU firmware class.
pub const IMAGE_CID: [u8; 16] = [
    0x0d, 0x9a, 0x7a, 0xf3, 0x7f, 0xce, 0x55, 0x44, 0x97, 0x2c, 0x2f, 0x76, 0x12, 0xf2, 0x99, 0x02,
];

/// QEMU-specific constructors for [`Policy`].
pub trait PolicyExt {
    /// Require the signed image's protected VID/CID TLVs and security counter.
    fn qemu() -> Self;
}

impl PolicyExt for Policy {
    fn qemu() -> Self {
        Self {
            min_security_counter: Some(0),
            vid: Some(IMAGE_VID),
            cid: Some(IMAGE_CID),
            ..Self::default()
        }
    }
}
