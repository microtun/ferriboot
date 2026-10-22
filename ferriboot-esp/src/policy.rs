//! ESP32C3 image-acceptance policy.
//!
//! The VID/CID are Microtun-derived identifiers, generated exactly as
//! `imgtool sign --vid microtun.dev --cid esp32c3`.
//! Keep both values synchronized with the image-signing defaults.
//! Both the bootloader and the firmware updater must enforce the same policy.

pub use ferriboot_core::Policy;

/// UUIDv5(DNS, "microtun.dev") — shared Microtun vendor ID.
pub const IMAGE_VID: [u8; 16] = [
    0xb3, 0x6b, 0x1d, 0x87, 0x77, 0x70, 0x53, 0xa0, 0xa5, 0x23, 0x9d, 0x81, 0x1b, 0x3b, 0x95, 0x5f,
];
/// UUIDv5(IMAGE_VID, "esp32c3") — ESP32C3 firmware class.
pub const IMAGE_CID: [u8; 16] = [
    0xfc, 0xc0, 0x08, 0x85, 0x1b, 0xbf, 0x51, 0xa8, 0xa0, 0x5f, 0x9b, 0x77, 0xe0, 0x3c, 0xe4, 0x73,
];

/// ESP32C3-specific constructors for [`Policy`].
pub trait PolicyExt {
    /// Require the signed image's protected VID/CID TLVs and security counter.
    fn esp32c3() -> Self;
}

impl PolicyExt for Policy {
    fn esp32c3() -> Self {
        Self {
            min_security_counter: Some(0),
            vid: Some(IMAGE_VID),
            cid: Some(IMAGE_CID),
            ..Self::default()
        }
    }
}
