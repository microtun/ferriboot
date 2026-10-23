//! OTP JSON output for secure boot (port of picotool's `output_otp_secure_boot`).

use super::crypto::{RawPublicKey, sha256};
use super::error::{Result, bail, err};
use serde_json::{Map, Value, json};

/// Update (or create) an OTP JSON document so that the given public key is registered as a
/// boot key and secure boot is enabled. Returns the new JSON text (4-space indented, sorted
/// keys, trailing newline - matching picotool's nlohmann::json output).
pub fn update_otp_json(existing: Option<&str>, public_key: &RawPublicKey) -> Result<String> {
    let key_hash = sha256(public_key);
    let mut otp: Value = match existing {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| err!(Format, "Failed to parse OTP JSON: {}", e))?,
        None => Value::Object(Map::new()),
    };
    if otp.is_null() {
        otp = Value::Object(Map::new());
    }
    let Some(obj) = otp.as_object_mut() else {
        bail!(Format, "OTP JSON must be an object");
    };

    // Find a matching or unused boot key slot
    let mut key_idx = None;
    for i in 0..4 {
        let name = format!("bootkey{}", i);
        match obj.get(&name) {
            Some(v) => {
                let matches =
                    (0..32).all(|j| v.get(j).and_then(Value::as_u64) == Some(key_hash[j] as u64));
                if matches {
                    key_idx = Some(i);
                    break;
                }
            }
            None => {
                key_idx = Some(i);
                break;
            }
        }
    }
    let Some(key_idx) = key_idx else {
        bail!(
            NotPossible,
            "All 4 boot keys in the OTP JSON are already used by other keys"
        );
    };

    obj.insert(
        format!("bootkey{}", key_idx),
        Value::Array(key_hash.iter().map(|b| json!(*b)).collect()),
    );

    let crit1 = obj
        .entry("crit1")
        .or_insert_with(|| Value::Object(Map::new()));
    match crit1.as_object_mut() {
        Some(c) => {
            c.insert("secure_boot_enable".into(), json!(1));
        }
        None => bail!(Format, "OTP JSON field crit1 must be an object"),
    }

    let mut key_valid: u8 = 1 << key_idx;
    let flags = obj
        .entry("boot_flags1")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(flags) = flags.as_object_mut() else {
        bail!(Format, "OTP JSON field boot_flags1 must be an object");
    };
    if let Some(v) = flags.get("key_valid") {
        let existing = v
            .as_u64()
            .or_else(|| v.as_i64().map(|x| x as u64))
            .ok_or_else(|| {
                err!(
                    Format,
                    "OTP JSON field boot_flags1.key_valid must be a number"
                )
            })?;
        key_valid |= existing as u8;
    }
    flags.insert("key_valid".into(), json!(key_valid));

    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(&otp, &mut ser).map_err(|e| err!(Unknown, "{}", e))?;
    buf.push(b'\n');
    Ok(String::from_utf8(buf).expect("serde_json produces UTF-8"))
}
