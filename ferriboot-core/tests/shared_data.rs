use ferriboot_core::{ImageVersion, Slot, UpgradeMode, shared_data::*};

const VERSION: ImageVersion = ImageVersion {
    major: 1,
    minor: 2,
    revision: 3,
    build: 4,
};
const MODE: UpgradeMode = UpgradeMode::DirectXip { revert: true };

// Independent upstream-format golden fixture: packed LE header and TLVs,
// one-byte mode/signature/recovery/slot, version, and u32 maximum size.
const STANDARD_PREFIX: [u8; 44] = [
    0x16, 0x20, 44, 0, 0x00, 0x20, 1, 0, 5, 0x01, 0x20, 1, 0, 2, 0x02, 0x20, 1, 0, 0, 0x03, 0x20,
    1, 0, 1, 0x04, 0x20, 8, 0, 1, 2, 3, 0, 4, 0, 0, 0, 0x05, 0x20, 4, 0, 0x00, 0x70, 0x1c, 0x00,
];

fn sample() -> SharedData<BOOT_INFO_SIZE> {
    boot_save_shared_data(MODE, Slot::Secondary, 0x1c7000, Some(VERSION), None).unwrap()
}

#[test]
fn standard_wire_format_and_256_byte_reservation() {
    assert_eq!(BOOT_INFO_SIZE, 256);
    let data = sample();
    assert_eq!(&data.bytes()[..44], &STANDARD_PREFIX);
    assert!(data.bytes()[44..].iter().all(|&b| b == 0));
    assert_eq!(
        BootInfo::from_bytes(&STANDARD_PREFIX),
        BootInfo::from_bytes(data.bytes())
    );
    let info = BootInfo::from_handoff(&STANDARD_PREFIX).unwrap();
    assert_eq!(info.mode(), MODE);
    assert_eq!(info.active_slot(), Slot::Secondary);
    assert_eq!(info.max_image_size(), Some(0x1c7000));
    assert_eq!(info.bootloader_version(), Some(VERSION));
    assert_eq!(info.security_counter(), None);
}

#[test]
fn all_supported_modes_and_optional_version() {
    for (mode, encoded) in [
        (UpgradeMode::SwapScratch, 1),
        (UpgradeMode::Overwrite, 2),
        (UpgradeMode::DirectXip { revert: false }, 4),
        (MODE, 5),
    ] {
        let data =
            boot_save_shared_data::<BOOT_INFO_SIZE>(mode, Slot::Primary, 4096, None, None).unwrap();
        assert_eq!(data.bytes()[8], encoded);
        assert_eq!(&data.bytes()[2..4], &[32, 0]);
        let info = BootInfo::from_bytes(data.bytes()).unwrap();
        assert!(info.supports_layout(4096));
        assert!(!info.supports_layout(8192));
        assert_eq!(info.bootloader_version(), None);
    }
    assert_eq!(
        boot_save_shared_data::<BOOT_INFO_SIZE>(
            UpgradeMode::Overwrite,
            Slot::Secondary,
            4096,
            None,
            None
        ),
        Err(SharedDataError::Invalid)
    );
    assert_eq!(
        boot_save_shared_data::<BOOT_INFO_SIZE>(MODE, Slot::Primary, 0, None, None),
        Err(SharedDataError::Invalid)
    );
}

#[test]
fn optional_hardware_floor_is_upstream_tlv() {
    let data = boot_save_shared_data::<BOOT_INFO_SIZE>(MODE, Slot::Secondary, 4096, None, Some(17))
        .unwrap();
    // mode, signature, recovery, slot => offset 24, max size to 32, counter to 40.
    assert_eq!(&data.bytes()[32..36], &[0x10, 0x20, 4, 0]);
    assert_eq!(&data.bytes()[36..40], &17u32.to_le_bytes());
    assert_eq!(&data.bytes()[2..4], &[40, 0]);
    assert_eq!(
        BootInfo::from_handoff(data.bytes())
            .unwrap()
            .security_counter(),
        Some(17)
    );
}

#[test]
fn legacy_private_extension_is_ignored() {
    let mut data = sample();
    data.add(0xf, 0, &[0xa5; 32]).unwrap();
    // The original 80-byte producer used exactly this TLV payload length.
    assert_eq!(&data.bytes()[2..4], &[80, 0]);
    let info = BootInfo::from_handoff(&data.bytes()[..80]).unwrap();
    assert_eq!(info.security_counter(), None);
    assert_eq!(info.max_image_size(), Some(0x1c7000));
    assert_eq!(BootInfo::from_handoff(data.bytes()), Ok(info));
}

#[test]
fn optional_max_size_and_recovery_do_not_prevent_parsing() {
    let mut data = SharedData::<BOOT_INFO_SIZE>::new().unwrap();
    for (minor, value) in [
        (BLINFO_MODE, &[1u8][..]),
        (BLINFO_SIGNATURE_TYPE, &[2u8][..]),
        (BLINFO_RUNNING_SLOT, &[0u8][..]),
    ] {
        data.add(TLV_MAJOR_BLINFO, minor, value).unwrap();
    }
    let info = BootInfo::from_handoff(data.bytes()).unwrap();
    assert_eq!(info.max_image_size(), None);
    assert_eq!(info.recovery(), None);
    assert!(info.supports_layout(4096));
    let mut data = data;
    data.add(TLV_MAJOR_BLINFO, BLINFO_RECOVERY, &[1]).unwrap();
    let info = BootInfo::from_handoff(data.bytes()).unwrap();
    assert_eq!(info.recovery(), Some(1));
    assert!(info.is_supported());
}

#[test]
fn duplicate_and_overflow_are_transactional() {
    let mut data = sample();
    let old = data;
    assert_eq!(
        data.add(TLV_MAJOR_BLINFO, BLINFO_MODE, &[1]),
        Err(SharedDataError::Duplicate)
    );
    assert_eq!(data.add(1, 0, &[0; 240]), Err(SharedDataError::Overflow));
    assert_eq!(data.add(16, 0, &[]), Err(SharedDataError::Invalid));
    assert_eq!(data, old);
    assert_eq!(SharedData::<3>::new(), Err(SharedDataError::Overflow));
    assert_eq!(
        boot_save_shared_data::<43>(MODE, Slot::Secondary, 4096, Some(VERSION), None),
        Err(SharedDataError::Overflow)
    );
}

#[test]
fn unknown_tlvs_are_ignored_but_known_fields_checked() {
    let mut data = sample();
    data.add(1, 0x3f, &[9, 8, 7]).unwrap();
    data.add(TLV_MAJOR_BLINFO, 0x100, &[6]).unwrap();
    assert_eq!(
        BootInfo::from_bytes(data.bytes()).unwrap().active_slot(),
        Slot::Secondary
    );
    let mut counter = sample();
    counter
        .add(
            TLV_MAJOR_BLINFO,
            BLINFO_SECURITY_COUNTER_IMAGE_0,
            &[1, 2, 3],
        )
        .unwrap();
    assert_eq!(
        BootInfo::from_handoff(counter.bytes()),
        Err(SharedDataError::Invalid)
    );
}

#[test]
fn malformed_lengths_duplicates_and_values_fail_closed() {
    for n in 0..STANDARD_PREFIX.len() {
        assert!(BootInfo::from_bytes(&STANDARD_PREFIX[..n]).is_err());
    }
    for (offset, value) in [
        (0, 0),
        (2, 3),
        (2, 81),
        (2, 255),
        (3, 255),
        (6, 2),
        (6, 255),
        (8, 99),
        (23, 2),
        (26, 7),
        (38, 3),
    ] {
        let mut data = STANDARD_PREFIX;
        data[offset] = value;
        assert!(
            BootInfo::from_bytes(&data).is_err(),
            "offset {offset}, value {value}"
        );
    }
    let mut data = STANDARD_PREFIX.to_vec();
    data[2] = 49;
    data.extend_from_slice(&[3, 0x20, 1, 0, 0]);
    assert_eq!(BootInfo::from_bytes(&data), Err(SharedDataError::Duplicate));
    let mut data = STANDARD_PREFIX;
    data[20] = 9;
    assert_eq!(BootInfo::from_bytes(&data), Err(SharedDataError::Missing));
    data = STANDARD_PREFIX;
    data[40..44].fill(0);
    assert_eq!(BootInfo::from_bytes(&data), Err(SharedDataError::Invalid));
}

#[test]
fn wire_words_round_trip_through_handoff_reader() {
    let data = sample();
    let words = wire_words(&data);
    assert_eq!(words[0], 0x2016 | (44u32 << 16));
    assert_eq!(words.len(), 64);
    assert_eq!(
        BootInfo::from_handoff_words(&words),
        BootInfo::from_bytes(data.bytes())
    );
    assert_eq!(
        BootInfo::from_handoff_words(&[0; BOOT_INFO_WORDS]),
        Err(SharedDataError::Invalid)
    );
}

#[test]
fn parsing_and_application_capability_checks_are_distinct() {
    let valid = sample();
    for (offset, value, supported) in [(13, 1, false), (18, 1, true)] {
        let mut data = *valid.bytes();
        data[offset] = value;
        assert_eq!(
            BootInfo::from_handoff(&data).unwrap().is_supported(),
            supported
        );
    }
}
