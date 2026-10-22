//! Detailed QEMU scenarios (see `common` for the virtual-board fixtures).
#![cfg(all(feature = "regression", feature = "qemu"))]

mod common;

use common::*;

const VARIANTS: [Variant; 4] = [
    Variant::SwapScratch,
    Variant::Overwrite,
    Variant::DirectXip,
    Variant::DirectXipRevert,
];

fn expect_idle_in(dut: &Dut, from: usize, version: &str, slot: &str) -> usize {
    let i = expect_running(dut, from, version);
    let (_, info) = dut.expect_from(i, "@fbt bootinfo", BOOT).unwrap();
    assert_eq!(field(&info, "slot"), Some(slot));
    dut.expect_from(i, "@fbt idle", BOOT).unwrap();
    i
}

/// Permanent trailer flags must never bypass authentication in any strategy.
#[test]
fn unauthentic_permanent_candidates_are_erased_in_every_mode() {
    let good = signed("2.0.0");
    let untrusted = sign::sign(
        payload().bytes(),
        &SignOptions::new("9.0.0").key(Key::Untrusted),
    )
    .unwrap();
    let mut truncated = good.clone();
    // The header still advertises the full payload, but the TLVs are missing.
    truncated.truncate(sign::HEADER_SIZE as usize + 32);
    let cases = [
        ("signature", sign::corrupt_signature(good.clone())),
        ("payload", sign::flip_payload_bit(good.clone(), 0x200)),
        ("hash", sign::corrupt_hash_tlv(good)),
        ("untrusted key", untrusted),
        ("truncated download", truncated),
    ];
    for v in VARIANTS {
        for (name, image) in &cases {
            let flash = factory(v);
            flash.stage_raw(Slot::Secondary, image, Request::Permanent);
            let dut = Dut::boot(&flash).unwrap();
            let (i, _) = dut
                .expect_from(0, "is not valid", BOOT)
                .unwrap_or_else(|e| panic!("{v:?} / {name}: {e}"));
            let (i, _) = dut.expect_from(i, "erasing Secondary slot", BOOT).unwrap();
            expect_idle_in(&dut, i, "1.0.0", "Primary");
            let m = dut.mark();
            dut.reset().unwrap();
            expect_idle_in(&dut, m, "1.0.0", "Primary");
            dut.expect_quiet_from(m, QUIET, "repeated candidate rejection", |l| {
                l.contains("is not valid") || l.contains("erasing Secondary slot")
            })
            .unwrap();
        }
    }
}

/// A bootable secondary recovers an erased or corrupted primary without an
/// update request. Copy modes bootstrap; XIP modes run the secondary in place.
#[test]
fn valid_secondary_recovers_missing_or_corrupted_primary_in_every_mode() {
    for v in VARIANTS {
        for corrupt_primary in [false, true] {
            let flash = blank(v);
            if corrupt_primary {
                // Force XIP to try the bad primary before the valid secondary.
                // Copy modes recover to a newer image to respect overwrite's
                // downgrade policy even when primary authentication fails.
                let primary_version = match v {
                    Variant::DirectXip | Variant::DirectXipRevert => "3.0.0",
                    _ => "1.0.0",
                };
                flash
                    .install(
                        Slot::Primary,
                        &sign::flip_payload_bit(signed(primary_version), 0x200),
                    )
                    .confirm_raw(Slot::Primary);
            }
            flash.install(Slot::Secondary, &signed("2.0.0"));
            // Reverting XIP requires an eligible secondary. Copy modes must
            // recover even an ordinary download without pending trailer magic.
            if v == Variant::DirectXipRevert {
                flash.confirm_raw(Slot::Secondary);
            }
            let expected_slot = match v {
                Variant::SwapScratch | Variant::Overwrite => "Primary",
                _ => "Secondary",
            };
            let dut = Dut::boot(&flash).unwrap();
            expect_idle_in(&dut, 0, "2.0.0", expected_slot);
            let m = dut.mark();
            dut.reset().unwrap();
            expect_idle_in(&dut, m, "2.0.0", expected_slot);
        }
    }
}

/// Downloading bytes does not commit an update in swap/overwrite modes.
#[test]
fn uncommitted_or_partial_download_does_not_replace_healthy_primary() {
    for v in [Variant::SwapScratch, Variant::Overwrite] {
        for complete in [false, true] {
            let flash = factory(v);
            let mut image = signed("2.0.0");
            if !complete {
                image.truncate(sign::HEADER_SIZE as usize + 32);
            }
            flash.install(Slot::Secondary, &image);
            let dut = Dut::boot(&flash).unwrap();
            expect_idle_in(&dut, 0, "1.0.0", "Primary");
            let m = dut.mark();
            dut.reset().unwrap();
            expect_idle_in(&dut, m, "1.0.0", "Primary");
        }
    }
}

/// A pending trailer with an erased header can occur during interrupted
/// cleanup. It must not block the valid primary or cause a reboot loop.
#[test]
fn orphaned_pending_trailer_does_not_block_primary() {
    for v in [Variant::SwapScratch, Variant::Overwrite] {
        for request in [Request::Test, Request::Permanent] {
            let flash = factory(v);
            flash.stage_raw(Slot::Secondary, &[], request);
            let dut = Dut::boot(&flash).unwrap();
            expect_idle_in(&dut, 0, "1.0.0", "Primary");
            let m = dut.mark();
            dut.reset().unwrap();
            expect_idle_in(&dut, m, "1.0.0", "Primary");
        }
    }
}

/// XIP chooses the highest semantic version; an equal or older secondary
/// must not displace primary, even when both slots are confirmed.
#[test]
fn direct_xip_prefers_primary_for_equal_or_older_secondary() {
    for v in [Variant::DirectXip, Variant::DirectXipRevert] {
        for secondary_version in ["0.9.0", "1.0.0", "1.0.0+99"] {
            let flash = factory(v);
            flash
                .install(Slot::Secondary, &signed(secondary_version))
                .confirm_raw(Slot::Secondary);
            let dut = Dut::boot(&flash).unwrap();
            expect_idle_in(&dut, 0, "1.0.0", "Primary");
            let m = dut.mark();
            dut.reset().unwrap();
            expect_idle_in(&dut, m, "1.0.0", "Primary");
        }
    }
}

#[test]
fn no_valid_image_halts_in_every_mode() {
    for v in VARIANTS {
        for invalid_images in [false, true] {
            let flash = blank(v);
            if invalid_images {
                flash
                    .install(Slot::Primary, &sign::corrupt_signature(signed("1.0.0")))
                    .confirm_raw(Slot::Primary);
                flash.stage_raw(
                    Slot::Secondary,
                    &sign::corrupt_signature(signed("2.0.0")),
                    Request::Permanent,
                );
            }
            let dut = Dut::boot(&flash).unwrap();
            dut.expect_from(0, NO_EXECUTABLE, BOOT)
                .unwrap_or_else(|e| panic!("{v:?} / invalid_images={invalid_images}: {e}"));
            dut.expect_quiet_from(0, QUIET, "application handoff", |l| {
                l.contains("@fbt") || l.contains(STARTING_IMAGE)
            })
            .unwrap();
        }
    }
}
