use common::*;
use ferriboot_core::{
    BootConfig, ImageHeader, ImageSlots, Slot, UpgradeMode,
    upload::{FirmwareUpload, UPLOAD_TIMEOUT_MS, UploadError, UploadReply, UploadRequest},
};
mod common;
use sha2::{Digest, Sha256};

fn setup() -> (Rig, ImageHeader, Vec<u8>) {
    let r = Rig::new(BootConfig::default());
    r.primary
        .program(0, &build_image(&Spec::new((1, 0, 0, 0), 1000, 1)));
    let active = *r.boot().unwrap().image.header();
    let image = build_image(&Spec::new((2, 0, 0, 0), 9000, 2));
    (r, active, image)
}
fn req(off: u32, total: u32, data: &[u8]) -> UploadRequest<'_> {
    UploadRequest {
        off,
        len: if off == 0 { Some(total) } else { None },
        sha: None,
        upgrade: false,
        data,
    }
}

#[test]
fn split_header_and_odd_chunks_inspect_only_after_completion() {
    let (r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    let before = r.snapshot();
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(0, image.len() as u32, &image[..11]),
                0
            )
            .unwrap(),
        UploadReply::Offset(11)
    );
    assert_eq!(r.snapshot(), before, "no erase before complete header");
    let mut off = 11;
    for chunk in image[11..].chunks(333) {
        let reply = upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(off as u32, image.len() as u32, chunk),
                off as u64,
            )
            .unwrap();
        off += chunk.len();
        if off == image.len() {
            let UploadReply::Complete {
                off: received,
                image: validated,
            } = reply
            else {
                panic!("completion");
            };
            assert_eq!(received, off as u32);
            assert_eq!(validated.version.major, 2);
        } else {
            assert_eq!(reply, UploadReply::Offset(off as u32));
        }
    }
    assert!(!upload.is_active());
    assert!(!upload.cleanup_required());
    assert!(!upload.has_written());
    u.inspect_update().unwrap();
}

#[test]
fn upload_cannot_complete_with_stale_unreceived_signed_tail() {
    let (r, active, image) = setup();
    r.secondary.program(0, &image);
    let before = r.snapshot();
    let mut upload = FirmwareUpload::new(false);
    assert_eq!(
        upload.receive(
            &mut r.updater(),
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, SECTOR as u32, &image[..SECTOR]),
            0
        ),
        Err(UploadError::Header)
    );
    assert_eq!(r.snapshot(), before);
    assert!(!upload.is_active());
}

#[test]
fn pending_slot_is_preserved_on_upload() {
    let (r, active, image) = setup();
    r.stage_update(&image, false);
    let old = r.updater().inspect_update().unwrap();
    let before = r.snapshot();
    let replacement = build_image(&Spec::new((3, 0, 0, 0), 9000, 3));
    assert_eq!(
        FirmwareUpload::new(false).receive(
            &mut r.updater(),
            &ImageSlots::new(active, Some(old), Slot::Primary),
            req(0, replacement.len() as u32, &replacement[..128]),
            0
        ),
        Err(UploadError::Pending)
    );
    assert_eq!(r.snapshot(), before);
}

#[test]
fn upgrade_only_rejection_precedes_any_flash_write_even_with_split_header() {
    for first_len in [11, 128] {
        let (r, active, _) = setup();
        let image = build_image(&Spec::new((0, 9, 0, 0), 9000, 3));
        let before = r.snapshot();
        let mut upload = FirmwareUpload::new(false);
        let mut first = req(0, image.len() as u32, &image[..first_len]);
        first.upgrade = true;
        let mut u = r.updater();
        let result = upload.receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            first,
            0,
        );
        if first_len < 32 {
            assert_eq!(result, Ok(UploadReply::Offset(first_len as u32)));
            assert_eq!(
                upload.receive(
                    &mut u,
                    &ImageSlots::new(active, None, Slot::Primary),
                    req(first_len as u32, image.len() as u32, &image[first_len..128]),
                    1
                ),
                Err(UploadError::Version)
            );
        } else {
            assert_eq!(result, Err(UploadError::Version));
        }
        assert_eq!(r.snapshot(), before);
        assert!(!upload.is_active());
        assert!(u.inspect_update().is_err());
    }
}

#[test]
fn duplicate_and_out_of_order_chunks_are_acknowledged_without_writes() {
    let (r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    let hash: [u8; 32] = Sha256::digest(&image).into();
    let mut first = req(0, image.len() as u32, &image[..333]);
    first.sha = Some(&hash);
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                first,
                0
            )
            .unwrap(),
        UploadReply::Offset(333)
    );
    let before = r.snapshot();
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                first,
                1
            )
            .unwrap(),
        UploadReply::Offset(333)
    );
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(999, image.len() as u32, &image[333..666]),
                2
            )
            .unwrap(),
        UploadReply::Offset(333)
    );
    assert_eq!(r.snapshot(), before);
}

#[test]
fn hashless_offset_zero_starts_new_same_length_same_header_session() {
    let (r, active, image) = setup();
    let replacement = build_image(&Spec::new((2, 0, 0, 0), 9000, 9));
    assert_eq!(image.len(), replacement.len());
    assert_eq!(&image[..32], &replacement[..32]);

    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(0, image.len() as u32, &image[..333]),
                0,
            )
            .unwrap(),
        UploadReply::Offset(333)
    );
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(0, replacement.len() as u32, &replacement[..333]),
                1,
            )
            .unwrap(),
        UploadReply::Offset(333)
    );

    let reply = upload
        .receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            req(333, replacement.len() as u32, &replacement[333..]),
            2,
        )
        .unwrap();
    let UploadReply::Complete {
        off,
        image: validated,
    } = reply
    else {
        panic!("replacement upload should complete");
    };
    assert_eq!(off, replacement.len() as u32);
    assert_eq!(validated.version.major, 2);
    assert_eq!(r.secondary.get(0..replacement.len()), replacement);
}

#[test]
fn different_new_session_restarts_and_abort_removes_candidate() {
    let (r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    let mut first = req(0, image.len() as u32, &image[..333]);
    first.sha = Some(&[1; 32]);
    upload
        .receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            first,
            0,
        )
        .unwrap();
    first.sha = Some(&[2; 32]);
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                first,
                1
            )
            .unwrap(),
        UploadReply::Offset(333)
    );
    upload.abort(&mut u).unwrap();
    assert!(!upload.is_active());
    assert!(r.secondary.snapshot().iter().all(|b| *b == 0xff));
    assert_eq!(
        upload
            .receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(333, image.len() as u32, &image[333..666]),
                2
            )
            .unwrap(),
        UploadReply::Offset(0)
    );
}

#[test]
fn inactivity_expiration_invalidates_written_data() {
    let (r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    upload
        .receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, image.len() as u32, &image[..333]),
            7,
        )
        .unwrap();
    assert!(!upload.expire(&mut u, UPLOAD_TIMEOUT_MS + 6).unwrap());
    assert!(upload.expire(&mut u, UPLOAD_TIMEOUT_MS + 7).unwrap());
    assert!(!upload.is_active());
    assert!(u.inspect_update().is_err());
}

#[test]
fn optional_file_hash_is_distinct_from_signature_and_failure_is_durable() {
    for check in [false, true] {
        let (r, active, image) = setup();
        let mut u = r.updater();
        let mut request = req(0, image.len() as u32, &image);
        request.sha = Some(&[0; 32]);
        let result = FirmwareUpload::new(check).receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            request,
            0,
        );
        if check {
            assert_eq!(result, Err(UploadError::Hash));
            assert!(
                u.inspect_update().is_err(),
                "startup cannot rediscover a rejected image"
            );
        } else {
            assert!(matches!(result, Ok(UploadReply::Complete { .. })));
        }
    }
    let (r, active, mut image) = setup();
    image.extend_from_slice(&[0xff; 127]);
    let hash: [u8; 32] = Sha256::digest(&image).into();
    let mut request = req(0, image.len() as u32, &image);
    request.sha = Some(&hash);
    let result = FirmwareUpload::new(true)
        .receive(
            &mut r.updater(),
            &ImageSlots::new(active, None, Slot::Primary),
            request,
            0,
        )
        .unwrap();
    let UploadReply::Complete {
        image: validated, ..
    } = result
    else {
        panic!("complete");
    };
    assert_eq!(
        validated.version.major, 2,
        "defined trailing padding is allowed"
    );
}

#[test]
fn invalid_signature_is_accepted_by_upload_but_rejected_by_bootloader() {
    let (r, active, mut image) = setup();
    image[40] ^= 1; // Tamper with body but keep structural TLVs intact.
    let mut updater = r.updater();
    let reply = FirmwareUpload::new(false)
        .receive(
            &mut updater,
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, image.len() as u32, &image),
            0,
        )
        .unwrap();
    assert!(matches!(reply, UploadReply::Complete { .. }));
    updater.mark_for_test().unwrap();
    drop(updater);
    // Only the already signed, bootable primary image can execute.
    assert_eq!(r.boot().unwrap().image.header().version.major, 1);
}

#[test]
fn no_revert_direct_xip_protects_automatically_selected_newer_image() {
    let (mut r, active, image) = setup();
    r.config.mode = UpgradeMode::DirectXip { revert: false };
    r.secondary.program(0, &image);
    let inactive = r.updater().inspect_update().unwrap();
    assert_eq!(
        FirmwareUpload::new(false).receive(
            &mut r.updater(),
            &ImageSlots::new(active, Some(inactive), Slot::Primary),
            req(0, image.len() as u32, &image[..333]),
            0
        ),
        Err(UploadError::Pending)
    );
}

#[test]
fn running_swap_trial_protects_the_rollback_image() {
    let (r, _, image) = setup();
    r.stage_update(&image, false);
    let active = *r.boot().unwrap().image.header();
    let mut u = r.updater();
    let rollback = u.inspect_update().unwrap();
    let replacement = build_image(&Spec::new((3, 0, 0, 0), 1000, 3));
    let before = r.snapshot();
    assert_eq!(
        FirmwareUpload::new(false).receive(
            &mut u,
            &ImageSlots::new(active, Some(rollback), Slot::Primary),
            req(0, replacement.len() as u32, &replacement[..128]),
            0
        ),
        Err(UploadError::Pending)
    );
    assert_eq!(r.snapshot(), before);
}

#[test]
fn failed_abort_keeps_cleanup_until_a_successful_retry() {
    let (r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    upload
        .receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, image.len() as u32, &image[..333]),
            0,
        )
        .unwrap();
    // Leave dirty data beyond the partially erased range too.
    r.secondary.program(SLOT_SIZE / 2, &[0; 16]);
    r.power.fail_after(0);
    for _ in 0..2 {
        assert!(upload.abort(&mut u).is_err());
        assert!(upload.cleanup_required());
        assert!(upload.is_active());
        assert!(upload.has_written());
    }
    r.power.unlimited();
    upload.abort(&mut u).unwrap();
    assert!(!upload.cleanup_required());
    assert!(!upload.is_active());
    assert!(!upload.has_written());
    assert!(r.secondary.snapshot().iter().all(|b| *b == 0xff));
    let ops = r.power.count();
    upload.abort(&mut u).unwrap();
    assert_eq!(r.power.count(), ops, "successful cleanup is idempotent");
}

#[test]
fn failed_expiration_retries_without_another_timeout() {
    let (r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    upload
        .receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, image.len() as u32, &image[..333]),
            7,
        )
        .unwrap();
    r.power.fail_after(0);
    assert!(upload.expire(&mut u, UPLOAD_TIMEOUT_MS + 7).is_err());
    assert!(upload.cleanup_required());
    r.power.unlimited();
    assert!(upload.expire(&mut u, UPLOAD_TIMEOUT_MS + 8).unwrap());
    assert!(!upload.is_active());
    assert!(r.secondary.snapshot().iter().all(|b| *b == 0xff));
}

#[test]
fn failed_append_cleanup_blocks_continuation_and_session_replacement() {
    // Fail begin_progress, first data-sector erase, and first data write.
    for successful_ops in 0..3 {
        let (r, active, image) = setup();
        let mut u = r.updater();
        let mut upload = FirmwareUpload::new(false);
        r.power.fail_after(successful_ops);
        assert!(matches!(
            upload.receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(0, image.len() as u32, &image[..333]),
                0
            ),
            Err(UploadError::Storage(_))
        ));
        assert!(upload.cleanup_required());
        for off in [0, 333] {
            assert!(matches!(
                upload.receive(
                    &mut u,
                    &ImageSlots::new(active, None, Slot::Primary),
                    req(off, image.len() as u32, &image[..333]),
                    1
                ),
                Err(UploadError::Storage(_))
            ));
            assert!(upload.cleanup_required());
        }
        r.power.unlimited();
        assert_eq!(
            upload
                .receive(
                    &mut u,
                    &ImageSlots::new(active, None, Slot::Primary),
                    req(333, image.len() as u32, &image[333..666]),
                    2
                )
                .unwrap(),
            UploadReply::Offset(0),
            "a cancelled session cannot resume"
        );
        assert!(!upload.is_active());
        assert!(r.secondary.snapshot().iter().all(|b| *b == 0xff));
        assert!(matches!(
            upload.receive(
                &mut u,
                &ImageSlots::new(active, None, Slot::Primary),
                req(0, image.len() as u32, &image),
                3
            ),
            Ok(UploadReply::Complete { .. })
        ));
    }
}

fn completion_ops(image: &[u8]) -> u64 {
    let (r, active, _) = setup();
    r.power.reset_count();
    FirmwareUpload::new(false)
        .receive(
            &mut r.updater(),
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, image.len() as u32, image),
            0,
        )
        .unwrap();
    r.power.count()
}

#[test]
fn rejected_completion_retains_cleanup_even_if_signed_image_is_still_valid() {
    let (mut r, active, image) = setup();
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(true);
    let mut request = req(0, image.len() as u32, &image);
    request.sha = Some(&[0; 32]);
    // Upload/finish succeed; the first cleanup erase fails in the trailer.
    r.power.fail_after(completion_ops(&image));
    assert!(matches!(
        upload.receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            request,
            0
        ),
        Err(UploadError::Storage(_))
    ));
    assert!(upload.cleanup_required());
    assert!(upload.is_active());
    assert!(upload.has_written());
    let stale = u.inspect_update().unwrap();
    assert!(upload.abort(&mut u).is_err());
    r.power.unlimited();
    // Retrying the same offset-zero request cannot acknowledge the rejected
    // file or treat the stale cache as a pending no-revert XIP upgrade.
    r.config.mode = UpgradeMode::DirectXip { revert: false };
    let mut u = r.updater();
    let result = upload.receive(
        &mut u,
        &ImageSlots::new(active, Some(stale), Slot::Primary),
        request,
        1,
    );
    assert_eq!(result, Err(UploadError::Hash));
    assert!(!upload.is_active());
    assert!(!upload.cleanup_required());
    assert!(u.inspect_update().is_err());
}

#[test]
fn failed_flash_finish_keeps_cleanup_retryable() {
    let (_, _, image) = setup();
    let ops = completion_ops(&image);
    let (r, active, _) = setup();
    r.power.fail_after(ops - 1);
    let mut u = r.updater();
    let mut upload = FirmwareUpload::new(false);
    assert!(matches!(
        upload.receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, image.len() as u32, &image),
            0,
        ),
        Err(UploadError::Storage(_))
    ));
    assert!(upload.cleanup_required());
    assert!(upload.abort(&mut u).is_err());
    r.power.unlimited();
    upload.abort(&mut u).unwrap();
    assert!(!upload.is_active());
    assert!(r.secondary.snapshot().iter().all(|b| *b == 0xff));
}

#[test]
fn bootloader_alone_enforces_the_rollback_counter() {
    use ferriboot_core::Policy;

    let config = BootConfig {
        policy: Policy {
            min_security_counter: Some(5),
            ..Policy::default()
        },
        ..BootConfig::default()
    };
    let r = Rig::new(config);
    let signed = |version, seed, sec_cnt| {
        let mut spec = Spec::new(version, 9000, seed);
        spec.sec_cnt = Some(sec_cnt);
        build_image(&spec)
    };
    r.primary.program(0, &signed((1, 0, 0, 0), 1, 5));
    let active = *r.boot().unwrap().image.header();
    let mut u = r.updater();
    let rejected = signed((2, 0, 0, 0), 2, 4);
    let reply = FirmwareUpload::new(false)
        .receive(
            &mut u,
            &ImageSlots::new(active, None, Slot::Primary),
            req(0, rejected.len() as u32, &rejected),
            0,
        )
        .unwrap();
    assert!(matches!(reply, UploadReply::Complete { .. }));
    u.mark_for_test().unwrap();
    drop(u);
    assert_eq!(
        r.boot().unwrap().image.header().version.major,
        1,
        "bootloader must reject a below-floor signed candidate"
    );

    let mut u = r.updater();
    let accepted = signed((2, 0, 0, 0), 3, 5);
    let mut w = u.begin_update().unwrap();
    w.write(&accepted).unwrap();
    w.finish().unwrap();
    u.mark_for_test().unwrap();
    drop(u);
    assert_eq!(r.boot().unwrap().image.header().version.major, 2);
}
