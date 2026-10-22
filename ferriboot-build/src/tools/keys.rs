//! Stage all development key pairs before publishing; no-clobber by default.
//! Each board crate describes its own key with a [`DevKey`]; the CLI passes
//! the full set so generation stays all-or-nothing.
use anyhow::{Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// One development key pair, written as `{dir}/{stem}.pem` and `{dir}/{stem}.pub.pem`.
#[derive(Clone, Copy)]
pub struct DevKey {
    /// Directory relative to the output root (normally the repository root).
    pub dir: &'static str,
    pub stem: &'static str,
    /// Returns `(private PEM, public PEM)`.
    pub generate: fn() -> Result<(String, String)>,
}
impl DevKey {
    /// Location of the private key below `root`.
    pub fn private_path(&self, root: &Path) -> PathBuf {
        root.join(self.dir).join(format!("{}.pem", self.stem))
    }
}

pub fn generate(root: &Path, keys: &[DevKey], force: bool) -> Result<()> {
    const SUFFIXES: [&str; 2] = [".pem", ".pub.pem"];
    for key in keys {
        for suffix in SUFFIXES {
            let target = root.join(key.dir).join(format!("{}{suffix}", key.stem));
            ensure!(
                force || fs::symlink_metadata(&target).is_err(),
                "{} exists (use --force to replace)",
                target.display()
            );
        }
    }
    fs::create_dir_all(root)?;
    let stage = tempfile::tempdir_in(root)?;
    for key in keys {
        let (private, public) = (key.generate)()?;
        for (suffix, text, private) in [(".pem", private, true), (".pub.pem", public, false)] {
            let rel = Path::new(key.dir).join(format!("{}{suffix}", key.stem));
            let staged = stage.path().join(&rel);
            super::write_output(&staged, text)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &staged,
                    fs::Permissions::from_mode(if private { 0o600 } else { 0o644 }),
                )?;
            }
            #[cfg(not(unix))]
            let _ = private;
        }
    }
    for key in keys {
        fs::create_dir_all(root.join(key.dir))?;
        for suffix in SUFFIXES {
            let rel = Path::new(key.dir).join(format!("{}{suffix}", key.stem));
            let target = root.join(&rel);
            let staged = stage.path().join(&rel);
            if force {
                fs::rename(staged, &target)?;
            } else {
                fs::hard_link(staged, &target)?;
            }
            println!("generated {}", target.display());
        }
    }
    Ok(())
}
