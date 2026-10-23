//! Architecture-independent host tooling shared by the per-arch crates
//! (`ferriboot-tools-esp`, `ferriboot-tools-rp2350`, `ferriboot-tools-qemu`).
//! External tools are invoked directly, with argument arrays; no command runs
//! through a shell.

#[cfg(feature = "cli")]
pub mod cli;
pub mod elf;
pub mod keys;
pub mod mcuboot;
pub mod signer;

use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Repository root, derived from this crate's location (`ferriboot-build/`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

/// Replay child diagnostics on the calling thread so libtest can capture them.
/// Output is buffered until the child exits, including on failure.
pub fn run(command: &mut Command) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("cannot start {command:?}"))?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    ensure!(
        output.status.success(),
        "command failed ({}): {command:?}",
        output.status
    );
    Ok(())
}

pub fn write_output(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

pub fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

pub fn flag(name: &str) -> bool {
    env(name, "0") == "1"
}

/// Keep the existing exact-value acknowledgement before any provisioning boot.
pub fn require_provisioning_ack() -> Result<()> {
    ensure!(
        env("FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING", "") == "I_UNDERSTAND",
        "first boot can irreversibly program OTP/eFuses; review the target and set FERRIBOOT_ACKNOWLEDGE_FIRST_BOOT_PROVISIONING=I_UNDERSTAND"
    );
    Ok(())
}

/// Check the destination's resolved parent too, including symlink aliases.
pub fn distinct_output(output: &Path, inputs: &[&Path]) -> Result<()> {
    fn resolved(path: &Path) -> Result<PathBuf> {
        if path.exists() {
            return Ok(path.canonicalize()?);
        }
        let absolute = std::path::absolute(path)?;
        let mut parent = absolute.parent().context("output has no parent")?;
        let mut tail = vec![
            absolute
                .file_name()
                .context("output has no filename")?
                .to_owned(),
        ];
        while !parent.exists() {
            tail.push(
                parent
                    .file_name()
                    .context("invalid output parent")?
                    .to_owned(),
            );
            parent = parent.parent().context("invalid output path")?;
        }
        let mut value = parent.canonicalize()?;
        for part in tail.into_iter().rev() {
            value.push(part);
        }
        Ok(value)
    }
    let destination = resolved(output)?;
    for input in inputs {
        ensure!(
            destination != resolved(input)?,
            "output must differ from {}",
            input.display()
        );
    }
    Ok(())
}

/// Parse a size/offset: decimal or `0x` hex, with optional `K`/`M` suffix.
pub fn number(text: &str) -> Result<u32> {
    let text = text.trim();
    let (digits, factor) = match text.as_bytes().last() {
        Some(b'K' | b'k') => (&text[..text.len() - 1], 1024),
        Some(b'M' | b'm') => (&text[..text.len() - 1], 1024 * 1024),
        _ => (text, 1),
    };
    let value = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)?
    } else {
        ensure!(
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
            "invalid number {text:?}"
        );
        digits.parse::<u32>()?
    };
    value.checked_mul(factor).context("number exceeds u32")
}

/// Bounds-checked slice of binary data.
pub fn bytes(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    data.get(offset..offset.checked_add(len).context("binary offset overflow")?)
        .context("truncated binary data")
}
/// Bounds-checked little-endian `u32`.
pub fn word(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, offset, 4)?.try_into()?))
}
/// Bounds-checked little-endian `u16`.
pub fn half(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(data, offset, 2)?.try_into()?))
}
