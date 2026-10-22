//! Attaching to the RP2350 fixture target over SWD. The single-pass LZ4 restore protocol
//! itself is shared (`crate::flash_transport`); see
//! `boards/rp2350/flash-algorithm/README.md` for the target side.

/// Start of the virtual command window in target.yaml.
pub const WINDOW_ADDRESS: u64 = 0x1000_0000;
pub const TARGET_NAME: &str = "RP235x-ferriboot-fixture";
const TARGET_YAML: &str = include_str!("../../../../boards/rp2350/flash-algorithm/target.yaml");

/// Attach to the single-core fixture target once. The shared transport keeps
/// the resulting session alive through one compressed commit and the
/// final reset.
pub fn attach() -> crate::Result<probe_rs::Session> {
    use probe_rs::probe::{DebugProbeSelector, WireProtocol, list::Lister};
    use probe_rs::{Permissions, config::Registry};
    let mut registry = Registry::new();
    registry
        .add_target_family_from_yaml(TARGET_YAML)
        .map_err(crate::Error::external)?;
    let lister = Lister::new();
    let mut probe = match std::env::var("PROBE_RS_PROBE") {
        Ok(spec) => lister
            .open(
                spec.parse::<DebugProbeSelector>()
                    .map_err(crate::Error::external)?,
            )
            .map_err(crate::Error::external)?,
        Err(std::env::VarError::NotPresent) => {
            let probes = lister.list_all();
            if probes.len() != 1 {
                return Err("expected exactly one SWD probe; set PROBE_RS_PROBE".into());
            }
            probes[0].open().map_err(crate::Error::external)?
        }
        Err(e) => return Err(e.into()),
    };
    probe
        .select_protocol(WireProtocol::Swd)
        .map_err(crate::Error::external)?;
    match std::env::var("PROBE_RS_SPEED_KHZ") {
        Ok(speed) => {
            probe
                .set_speed(speed.parse::<u32>()?)
                .map_err(crate::Error::external)?;
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(e) => return Err(e.into()),
    }
    probe
        .attach_with_registry(TARGET_NAME, Permissions::default(), &registry)
        .map_err(crate::Error::external)
}

/// RP2350 raw OTP page 32 occupies rows 0x800..0x83f. The first 63
/// rows hold 21 triplicated counter words; the last must remain blank.
/// The raw aperture maps each 24-bit OTP row into one 32-bit word. These
/// constants match ferriboot-rp/src/security_counter.rs and the RP2350 TRM.
const COUNTER_ROW: u32 = 0x800;
const COUNTER_ROWS: usize = 64;
const OTP_DATA_RAW_GUARDED_BASE: u64 = 0x4013_c000;

fn decode_counter_rows(rows: &[u32; COUNTER_ROWS]) -> crate::Result<u32> {
    if rows[63] != 0 {
        return Err("RP2350 OTP page 32 spare row 0x83f is not blank".into());
    }
    ferriboot_core::security_counter::majority_thermometer_rows_value(&rows[..63], 24)
        .map_err(|error| format!("invalid RP2350 OTP security counter: {error:?}").into())
}

/// Read the physical security-counter page through the existing SWD probe.
/// Does not flash, run a diagnostic, write OTP, or enter USB BOOTSEL mode.
/// Requires secure SWD access to OTP page 32; fail if access is blocked.
pub fn read_security_counter() -> crate::Result<u32> {
    use probe_rs::MemoryInterface;

    let mut session = attach()?;
    let mut core = session.core(0).map_err(crate::Error::external)?;
    let mut rows = [0u32; COUNTER_ROWS];
    core.read_32(
        OTP_DATA_RAW_GUARDED_BASE + u64::from(COUNTER_ROW) * 4,
        &mut rows,
    )
    .map_err(|error| format!("cannot read RP2350 OTP page 32 via secure SWD: {error}"))?;
    let floor = decode_counter_rows(&rows)?;
    eprintln!("[security-counter] RP2350 raw OTP page 32 reports current floor {floor}");
    Ok(floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_otp_page_decoder_matches_triplicated_encoding() {
        for value in [0, 1, 23, 24, 25, 503, 504] {
            let mut rows = [0u32; COUNTER_ROWS];
            for n in 0..value {
                let first = n as usize / 24 * 3;
                for row in &mut rows[first..first + 3] {
                    *row |= 1 << (n % 24);
                }
            }
            assert_eq!(decode_counter_rows(&rows).unwrap(), value);
        }
        let mut rows = [0u32; COUNTER_ROWS];
        assert_eq!(decode_counter_rows(&rows).unwrap(), 0);
        rows[0] = 1; // a single programmed copy does not raise the majority floor
        assert_eq!(decode_counter_rows(&rows).unwrap(), 0);
        rows[1] = 0b101;
        assert_eq!(decode_counter_rows(&rows).unwrap(), 1); // one copy is noisy
        rows[2] = 0b101; // corrupt majority
        assert!(decode_counter_rows(&rows).is_err());
        rows = [0; COUNTER_ROWS];
        rows[63] = 1; // unused row must remain blank
        assert!(decode_counter_rows(&rows).is_err());
    }

    /// The embedded target.yaml loads, and its command window is where the
    /// shared transport writes and large enough for a worst-case restore.
    #[test]
    fn embedded_target_matches_the_shared_transport() {
        use crate::flash_transport::{PACKET_SIZE, WINDOW_SIZE};
        let mut registry = probe_rs::config::Registry::new();
        registry.add_target_family_from_yaml(TARGET_YAML).unwrap();
        let target = registry.get_target_by_name(TARGET_NAME).unwrap();
        let raw = &target.flash_algorithms[0];
        let window = &raw.flash_properties.address_range;
        assert_eq!(window.start, WINDOW_ADDRESS);
        assert_eq!((window.end - window.start) as usize, WINDOW_SIZE);
        assert_eq!(raw.flash_properties.page_size as usize, PACKET_SIZE);
    }
}
