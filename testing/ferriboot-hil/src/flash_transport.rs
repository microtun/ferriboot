//! One-pass compressed fixture transport over RP2350 SWD.
//!
//! Sends all 1024 physical sectors in order, each as an independent LZ4 block
//! (or a raw/erased-sector record). The SRAM algorithm does the comparison,
//! conditional erase/program and full-sector readback verification; no scan or
//! second probe-rs flashing operation is needed.
#[cfg(any(feature = "rp2350-pico2", test))]
use crate::Board;
#[cfg(feature = "rp2350-pico2")]
use crate::Probe;
use crate::flash::{FLASH_SIZE, SECTOR};

pub const PACKET_SIZE: usize = 4096;
pub const SECTORS: usize = FLASH_SIZE / SECTOR as usize;
/// Length of the virtual command window ("flash" in target.yaml).
pub const WINDOW_SIZE: usize = 0x50_0000;
const HEADER_SIZE: usize = 12;

fn packets(records: &[u8]) -> Vec<u8> {
    let payload_size = PACKET_SIZE - HEADER_SIZE;
    let count = records.len().div_ceil(payload_size).max(1);
    let mut output = vec![0xff; count * PACKET_SIZE];
    for (sequence, packet) in output.chunks_exact_mut(PACKET_SIZE).enumerate() {
        let start = sequence * payload_size;
        let len = records.len().saturating_sub(start).min(payload_size);
        for (dst, value) in packet[..HEADER_SIZE].chunks_exact_mut(4).zip([
            sequence as u32,
            len as u32,
            u32::from(sequence + 1 == count),
        ]) {
            dst.copy_from_slice(&value.to_le_bytes());
        }
        packet[HEADER_SIZE..HEADER_SIZE + len].copy_from_slice(&records[start..start + len]);
    }
    output
}

fn check_fixture(desired: &[u8]) -> crate::Result<()> {
    if desired.len() != FLASH_SIZE {
        Err("expected a complete 4 MiB fixture".into())
    } else {
        Ok(())
    }
}

/// Serialize exactly 1024 records in physical order:
///
/// * `0u16` — sector is all 0xFF (no payload).
/// * `4096u16` — incompressible sector, followed by 4096 raw bytes.
/// * `1..4095u16` — LZ4 block, decompressing to exactly 4096 bytes.
///
/// Record headers and bodies can cross command-packet boundaries. Including
/// erased-sector records makes it possible to erase stale contents on target.
pub fn encode_restore(desired: &[u8]) -> crate::Result<Vec<u8>> {
    check_fixture(desired)?;
    let mut records = Vec::new();
    for sector in desired.chunks_exact(SECTOR as usize) {
        if sector.iter().all(|&b| b == 0xff) {
            records.extend_from_slice(&0u16.to_le_bytes());
            continue;
        }
        let compressed = lz4_flex::block::compress(sector);
        if compressed.len() < SECTOR as usize {
            records.extend_from_slice(&(compressed.len() as u16).to_le_bytes());
            records.extend_from_slice(&compressed);
        } else {
            records.extend_from_slice(&(SECTOR as u16).to_le_bytes());
            records.extend_from_slice(sector);
        }
    }
    let output = packets(&records);
    if output.len() > WINDOW_SIZE {
        return Err("compressed restore exceeds the virtual command window".into());
    }
    Ok(output)
}

// The small interface lets host tests exercise stage ordering and failures
// without a probe. Production uses one Session for every operation.
#[cfg(any(feature = "rp2350-pico2", test))]
trait FixtureSession {
    fn commit_packets(&mut self, packets: &[u8]) -> crate::Result<()>;
    fn reset(&mut self) -> crate::Result<()>;
}

/// A probe-rs session attached to the board's fixture target.
#[cfg(feature = "rp2350-pico2")]
struct ProbeSession {
    session: probe_rs::Session,
    probe: &'static Probe,
}

#[cfg(feature = "rp2350-pico2")]
impl ProbeSession {
    fn attach(board: &'static Board) -> crate::Result<Self> {
        let probe = board.probe.as_ref().ok_or("board has no probe")?;
        Ok(Self {
            session: (probe.attach)()?,
            probe,
        })
    }
}

#[cfg(feature = "rp2350-pico2")]
impl FixtureSession for ProbeSession {
    fn commit_packets(&mut self, packets: &[u8]) -> crate::Result<()> {
        let mut loader = self.session.target().flash_loader();
        let window = self
            .probe
            .flash_window
            .as_ref()
            .ok_or("no fixture flash window")?;
        loader
            .add_data(window.window_address, packets)
            .map_err(crate::Error::external)?;
        // This address range is a virtual command window, not physical NOR.
        // Defaults disable preverify, verify, chip erase and preservation reads.
        // The target algorithm does the physical comparison and verification.
        // Keep the normal erase phase (its EraseSector is a no-op).
        loader
            .commit(
                &mut self.session,
                probe_rs::flashing::DownloadOptions::default(),
            )
            .map_err(crate::Error::external)?;
        Ok(())
    }

    fn reset(&mut self) -> crate::Result<()> {
        // reset() resumes execution; reset_and_halt() would prevent booting.
        // The board's debug sequence decides what kind of reset this is.
        self.session
            .core(0)
            .map_err(crate::Error::external)?
            .reset()
            .map_err(crate::Error::external)?;
        Ok(())
    }
}

#[cfg(any(feature = "rp2350-pico2", test))]
fn commit(
    board: &Board,
    session: &mut impl FixtureSession,
    packets: &[u8],
    label: &str,
) -> crate::Result<()> {
    eprintln!(
        "[fixture] {label}: {} KiB {} command upload",
        packets.len().div_ceil(1024),
        board.probe.as_ref().map_or("mock", |probe| probe.link)
    );
    session
        .commit_packets(packets)
        .map_err(|e| format!("{label} failed: {e}").into())
}

#[cfg(any(feature = "rp2350-pico2", test))]
fn restore_with_session(
    board: &Board,
    session: &mut impl FixtureSession,
    desired: &[u8],
    before_reset: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<()> {
    let started = std::time::Instant::now();
    let upload = encode_restore(desired)?;
    commit(board, session, &upload, "restore 1024 physical sectors")?;
    // The harness clears stale console data and starts capture while the
    // target is still halted, immediately before booting it.
    before_reset()?;
    eprintln!("[fixture] resetting target");
    session
        .reset()
        .map_err(|e| format!("fixture reset failed: {e}"))?;
    eprintln!(
        "[fixture] compressed restore+reset {:.2}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Upload the complete compressed 4 MiB fixture in one commit, then boot.
/// The session is dropped on return, before runtime/watchdog assertions.
#[cfg(feature = "rp2350-pico2")]
pub fn restore_and_reset(board: &'static Board, desired: &[u8]) -> crate::Result<()> {
    restore_and_reset_with(board, desired, || Ok(()))
}

/// As above, with a checkpoint while stopped, immediately before final reset.
/// A checkpoint failure prevents booting an unprepared console capture.
#[cfg(feature = "rp2350-pico2")]
pub fn restore_and_reset_with(
    board: &'static Board,
    desired: &[u8],
    before_reset: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<()> {
    check_fixture(desired)?; // Reject malformed input before opening the probe.
    eprintln!(
        "[fixture] attaching {} probe",
        board.probe.as_ref().ok_or("board has no probe")?.link
    );
    let mut session = ProbeSession::attach(board)?;
    restore_with_session(board, &mut session, desired, before_reset)
}

/// Reset-only worker for subsequent scenario resets; no flash scan or upload.
#[cfg(feature = "rp2350-pico2")]
pub fn reset_target(board: &'static Board) -> crate::Result<()> {
    ProbeSession::attach(board)?.reset()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TEST_BOARD;

    fn records(encoded: &[u8]) -> Vec<u8> {
        let mut result = Vec::new();
        for (seq, packet) in encoded.chunks_exact(PACKET_SIZE).enumerate() {
            assert_eq!(
                u32::from_le_bytes(packet[0..4].try_into().unwrap()),
                seq as u32
            );
            let length = u32::from_le_bytes(packet[4..8].try_into().unwrap()) as usize;
            let last = u32::from_le_bytes(packet[8..12].try_into().unwrap());
            assert_eq!(last, u32::from((seq + 1) * PACKET_SIZE == encoded.len()));
            assert!(length <= PACKET_SIZE - HEADER_SIZE);
            if last == 0 {
                assert_eq!(length, PACKET_SIZE - HEADER_SIZE);
            }
            result.extend_from_slice(&packet[HEADER_SIZE..HEADER_SIZE + length]);
        }
        result
    }

    /// Independent reconstruction checks full-image coverage and exact sizes.
    fn unpack(encoded: &[u8]) -> (Vec<u8>, usize, usize, usize) {
        let stream = records(encoded);
        let mut output = Vec::with_capacity(FLASH_SIZE);
        let (mut blank, mut compressed, mut raw) = (0, 0, 0);
        let mut index = 0;
        for _ in 0..SECTORS {
            let length = u16::from_le_bytes(stream[index..index + 2].try_into().unwrap()) as usize;
            index += 2;
            assert!(length <= SECTOR as usize);
            let end = index + length;
            let sector = match length {
                0 => {
                    blank += 1;
                    vec![0xff; SECTOR as usize]
                }
                n if n == SECTOR as usize => {
                    raw += 1;
                    stream[index..end].to_vec()
                }
                _ => {
                    compressed += 1;
                    let mut decoded = vec![0u8; SECTOR as usize];
                    assert_eq!(
                        lz4_flex::block::decompress_into(&stream[index..end], &mut decoded)
                            .unwrap(),
                        SECTOR as usize
                    );
                    decoded
                }
            };
            output.extend_from_slice(&sector);
            index = end;
        }
        assert_eq!(index, stream.len(), "extra records/trailing bytes");
        (output, blank, compressed, raw)
    }

    #[derive(Default)]
    struct FakeSession {
        operations: Vec<&'static str>,
        uploads: Vec<Vec<u8>>,
        fail: Option<&'static str>,
    }
    impl FakeSession {
        fn operation(&mut self, name: &'static str) -> crate::Result<()> {
            self.operations.push(name);
            if self.fail == Some(name) {
                Err(format!("injected {name} failure").into())
            } else {
                Ok(())
            }
        }
    }
    impl FixtureSession for FakeSession {
        fn commit_packets(&mut self, packets: &[u8]) -> crate::Result<()> {
            self.uploads.push(packets.to_vec());
            self.operation("restore")
        }
        fn reset(&mut self) -> crate::Result<()> {
            self.operation("reset")
        }
    }

    #[test]
    fn full_blank_fixture_is_one_pass_and_clears_stale_sectors() {
        let desired = vec![0xff; FLASH_SIZE];
        let encoded = encode_restore(&desired).unwrap();
        assert_eq!(encoded.len(), PACKET_SIZE);
        assert_eq!(records(&encoded), vec![0u8; SECTORS * 2]);
        let (decoded, blank, compressed, raw) = unpack(&encoded);
        assert_eq!(decoded, desired);
        assert_eq!((blank, compressed, raw), (SECTORS, 0, 0));
        let mut session = FakeSession::default();
        restore_with_session(&TEST_BOARD, &mut session, &desired, || Ok(())).unwrap();
        assert_eq!(session.operations, ["restore", "reset"]);
        assert_eq!(session.uploads, [encoded]);
    }

    #[test]
    fn mixed_blank_lz4_and_raw_sectors_roundtrip_across_packets() {
        let mut desired = vec![0xff; FLASH_SIZE];
        desired[SECTOR as usize..2 * SECTOR as usize].fill(0x42);
        let mut x = 0x1234_5678u32;
        for b in &mut desired[2 * SECTOR as usize..3 * SECTOR as usize] {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        desired[(SECTORS - 1) * SECTOR as usize..].fill(0x56);
        let encoded = encode_restore(&desired).unwrap();
        let (decoded, blank, compressed, raw) = unpack(&encoded);
        assert_eq!(decoded, desired);
        assert_eq!((blank, compressed, raw), (SECTORS - 3, 2, 1));
        assert!(encoded.len() < 3 * PACKET_SIZE);
    }

    #[test]
    fn incompressible_whole_image_still_fits_window() {
        let mut image = vec![0u8; FLASH_SIZE];
        let mut x = 0x9876_5432u32;
        for b in &mut image {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        let encoded = encode_restore(&image).unwrap();
        assert!(encoded.len() <= WINDOW_SIZE);
        assert_eq!(unpack(&encoded).0, image);
    }

    #[test]
    fn restore_failure_or_checkpoint_never_resets() {
        let image = vec![0xff; FLASH_SIZE];
        let mut session = FakeSession {
            fail: Some("restore"),
            ..Default::default()
        };
        let error = restore_with_session(&TEST_BOARD, &mut session, &image, || Ok(())).unwrap_err();
        assert!(error.to_string().contains("injected restore"));
        assert_eq!(session.operations, ["restore"]);

        let mut session = FakeSession::default();
        let mut checkpoint_called = false;
        let error = restore_with_session(&TEST_BOARD, &mut session, &image, || {
            checkpoint_called = true;
            Err("console not ready".into())
        })
        .unwrap_err();
        assert!(checkpoint_called);
        assert_eq!(error.to_string(), "console not ready");
        assert_eq!(session.operations, ["restore"]);

        let mut session = FakeSession {
            fail: Some("reset"),
            ..Default::default()
        };
        let error = restore_with_session(&TEST_BOARD, &mut session, &image, || Ok(())).unwrap_err();
        assert!(error.to_string().contains("injected reset"));
        assert_eq!(session.operations, ["restore", "reset"]);
    }

    #[test]
    fn malformed_input_never_commits() {
        assert!(encode_restore(&[]).is_err());
        let mut session = FakeSession::default();
        assert!(restore_with_session(&TEST_BOARD, &mut session, &[], || Ok(())).is_err());
        assert!(session.operations.is_empty());
    }

    #[cfg(feature = "rp2350-pico2")]
    #[test]
    fn command_transport_options_do_not_read_virtual_flash() {
        let options = probe_rs::flashing::DownloadOptions::default();
        assert!(!options.preverify);
        assert!(!options.verify);
        assert!(!options.keep_unwritten_bytes);
        assert!(!options.do_chip_erase);
        assert!(!options.skip_erase);
    }
}
