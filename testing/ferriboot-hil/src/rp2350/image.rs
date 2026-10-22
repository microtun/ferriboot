//! Checked ELF conversion, ROM secure-boot sealing and deliberate Cortex-M
//! vector mutations. Whether a mutation is rejected is decided on the board
//! by `ferriboot_rp::vector`.
use std::{ffi::OsString, path::PathBuf};

#[derive(Clone, Debug)]
pub struct ArmImage(Vec<u8>);
impl ArmImage {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn stack(&self) -> u32 {
        u32::from_le_bytes(self.0[..4].try_into().unwrap())
    }
    pub fn reset(&self) -> u32 {
        u32::from_le_bytes(self.0[4..8].try_into().unwrap())
    }
    pub fn set_stack(mut self, value: u32) -> Self {
        self.0[..4].copy_from_slice(&value.to_le_bytes());
        self
    }
    pub fn set_reset(mut self, value: u32) -> Self {
        self.0[4..8].copy_from_slice(&value.to_le_bytes());
        self
    }
    pub fn truncate(mut self, len: usize) -> Self {
        self.0.truncate(len);
        self
    }
}

/// Reuse ferriboot-tools-rp2350's flash LMA export, ROM load-map checks and boot-info checks.
/// In particular the bootloader ELF has SRAM VMAs; objcopy by VMA is incorrect.
pub fn from_elf(elf: &[u8], bootloader: bool) -> crate::Result<ArmImage> {
    let image =
        ferriboot_tools_rp2350::elf::image(elf, bootloader).map_err(crate::Error::external)?;
    Ok(ArmImage(image.payload))
}

/// The bundled, intentionally public development key is the default for RP2350
/// HIL, just as the ESP32-C3 HIL harness uses a bundled ROM-signing key.
/// Already-provisioned boards may require RP2350_SECURE_BOOT_KEY instead.
fn key_from_override(override_key: Option<OsString>) -> crate::Result<PathBuf> {
    match override_key {
        Some(key) if key.is_empty() => {
            Err("RP2350_SECURE_BOOT_KEY is empty; unset it to use the development key".into())
        }
        Some(key) => Ok(PathBuf::from(key)),
        None => Ok(
            crate::artifacts::board_dir(&super::BOARD).join("keys/dev-rp2350-rom-secp256k1.pem")
        ),
    }
}

pub fn rom_signing_key() -> crate::Result<PathBuf> {
    let key = key_from_override(std::env::var_os("RP2350_SECURE_BOOT_KEY"))?;
    if !key.is_file() {
        return Err(format!(
            "RP2350 ROM signing key not found: {}; set RP2350_SECURE_BOOT_KEY for a previously provisioned board",
            key.display()
        ).into());
    }
    Ok(key)
}

/// Convert and ROM-sign the bootloader using the board's production seal
/// pipeline. `ferriboot-tools-rp2350` verifies the secp256k1 signature, embedded key,
/// Arm Secure metadata and SRAM load map before publishing the BIN.
/// No unsigned fallback is permitted for RP2350 HIL.
pub fn seal_bootloader(elf: &[u8]) -> crate::Result<Vec<u8>> {
    let key = rom_signing_key()?;
    let image = ferriboot_tools_rp2350::elf::image(elf, true).map_err(crate::Error::external)?;
    ferriboot_tools_rp2350::seal::seal(&image, &key, None).map_err(crate::Error::external)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_rom_key_is_default() {
        let key = key_from_override(None).unwrap();
        assert_eq!(
            key,
            crate::artifacts::board_dir(&super::super::BOARD)
                .join("keys/dev-rp2350-rom-secp256k1.pem")
        );
        assert!(key.is_file());
    }

    #[test]
    fn explicitly_selected_rom_key_takes_precedence() {
        assert_eq!(
            key_from_override(Some(OsString::from("/tmp/board-rom-key.pem"))).unwrap(),
            PathBuf::from("/tmp/board-rom-key.pem")
        );
        assert!(key_from_override(Some(OsString::new())).is_err());
    }
}
