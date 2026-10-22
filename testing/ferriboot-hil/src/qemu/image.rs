//! Checked ELF conversion and deliberate Cortex-M vector mutations. Mutations
//! are rejected in the guest by `ferriboot_qemu::vectors`.
use std::{process::Command, time::Duration};

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
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("image.elf");
    let output = dir.path().join("image.bin");
    std::fs::write(&input, elf)?;
    let mut cmd = Command::new(std::env::var("PYTHON").unwrap_or_else(|_| "python3".into()));
    cmd.arg(crate::artifacts::repo_root().join("boards/qemu/tools/image.py"))
        .arg(input)
        .arg(&output);
    if bootloader {
        cmd.arg("--bootloader");
    }
    crate::process::run(&mut cmd, "QEMU ELF conversion", Duration::from_secs(60))?;
    Ok(ArmImage(std::fs::read(output)?))
}
