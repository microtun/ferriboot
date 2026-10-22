//! Read ESP32-C3 BLOCK0.SECURE_VERSION directly over USB-JTAG / an external
//! JTAG probe, without flashing or relying on firmware output.

// probe-rs 0.32 moved Espressif targets and the native USB-JTAG driver into
// a separate plugin. Registration MUST precede both Lister::new() and attach,
// and only happen once in a process (multiple smoke tests share one binary).
fn register_espressif_plugin() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(probe_rs_espressif::register_plugin);
}

const CAPACITY: u32 = 16;
const EFUSE_BASE: u64 = 0x6000_8800;
// ESP32-C3 TRM: EFUSE_RD_REPEAT_DATA3_REG is BLOCK0 read word 4. Its
// SECURE_VERSION field is 16 bits wide at bits 14..29 (BLOCK0 bits 142..157).
const SECURE_VERSION_REG: u64 = EFUSE_BASE + 0x3c;
// Read-only repeat-programming-error bits for the same SECURE_VERSION field.
const SECURE_VERSION_ERROR_REG: u64 = EFUSE_BASE + 0x188;
const VERSION_SHIFT: u32 = 14;
const VERSION_MASK: u32 = (1 << CAPACITY) - 1;

fn decode_registers(data: u32, errors: u32) -> crate::Result<u32> {
    if (errors >> VERSION_SHIFT) & VERSION_MASK != 0 {
        return Err("ESP32-C3 SECURE_VERSION eFuse reports programming errors".into());
    }
    // Ferriboot counts burned bits, not the numeric value of the field.
    Ok(((data >> VERSION_SHIFT) & VERSION_MASK).count_ones())
}

/// Read the eFuse controller's read-only, reset-latched BLOCK0 registers via
/// JTAG. The hardware refreshes these at boot; the HIL board must have booted
/// since any out-of-band programming. JTAG must be enabled on the DUT.
pub fn read() -> crate::Result<u32> {
    use probe_rs::probe::{DebugProbeSelector, WireProtocol, list::Lister};
    use probe_rs::{MemoryInterface, Permissions};

    register_espressif_plugin();
    let lister = Lister::new();
    let mut probe = match std::env::var("PROBE_RS_PROBE") {
        Ok(selector) => lister
            .open(
                selector
                    .parse::<DebugProbeSelector>()
                    .map_err(crate::Error::external)?,
            )
            .map_err(crate::Error::external)?,
        Err(std::env::VarError::NotPresent) => {
            let probes = lister.list_all();
            if probes.len() != 1 {
                if probes.is_empty() {
                    return Err(concat!(
                        "no JTAG probe detected for ESP32-C3 after registering probe-rs-espressif; ",
                        "check USB connection to GPIO18 (D-) / GPIO19 (D+), ",
                        "`lsusb -d 303a:1001`, and USB permissions/udev rules; ",
                        "PROBE_RS_PROBE cannot select a disconnected probe"
                    ).into());
                }
                return Err(format!(
                    "expected exactly one ESP32-C3 JTAG probe (found {}); set PROBE_RS_PROBE",
                    probes.len()
                )
                .into());
            }
            probes[0].open().map_err(crate::Error::external)?
        }
        Err(error) => return Err(error.into()),
    };
    probe
        .select_protocol(WireProtocol::Jtag)
        .map_err(crate::Error::external)?;
    match std::env::var("PROBE_RS_SPEED_KHZ") {
        Ok(speed) => {
            probe
                .set_speed(speed.parse::<u32>()?)
                .map_err(crate::Error::external)?;
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => return Err(error.into()),
    }
    let mut session = probe
        .attach("esp32c3", Permissions::default())
        .map_err(|error| {
            crate::Error::new(format!(
                "cannot attach to ESP32-C3 JTAG: {error}; JTAG may be disabled by Secure Boot"
            ))
        })?;
    let mut core = session.core(0).map_err(crate::Error::external)?;
    let before = core
        .read_word_32(SECURE_VERSION_REG)
        .map_err(crate::Error::external)?;
    let errors = core
        .read_word_32(SECURE_VERSION_ERROR_REG)
        .map_err(crate::Error::external)?;
    let after = core
        .read_word_32(SECURE_VERSION_REG)
        .map_err(crate::Error::external)?;
    if before != after {
        return Err("ESP32-C3 SECURE_VERSION changed while reading over JTAG".into());
    }
    let floor = decode_registers(after, errors)?;
    eprintln!("[security-counter] ESP32-C3 JTAG eFuses report current floor {floor}");
    Ok(floor)
}
