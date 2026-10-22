//! Checked ELF conversion and deliberate Cortex-M vector mutations. Mutations
//! are rejected in the guest by `ferriboot_qemu::vectors`.

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

/// Export checked ELF32 physical load segments for AN385.
pub fn from_elf(elf: &[u8], bootloader: bool) -> crate::Result<ArmImage> {
    let image =
        ferriboot_tools_qemu::elf::image(elf, bootloader).map_err(crate::Error::external)?;
    Ok(ArmImage(image.payload))
}
