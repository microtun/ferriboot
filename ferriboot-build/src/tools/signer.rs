//! Optional microtun-signer CLI adapter. The private key never leaves the service.
//! The service signs a base64-encoded 32-byte SHA-256 digest and returns a
//! base64 signature; all results are authenticated locally before use.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Debug)]
pub struct RemoteSigner {
    pub url: String,
    pub key_id: String,
    pub github_login: bool,
}

impl RemoteSigner {
    /// Remote signing is opt-in. A partially configured signer is an error,
    /// never an invitation to silently use a development private key.
    pub fn from_env() -> Result<Option<Self>> {
        let url = std::env::var("MICROTUN_SIGNER_URL").ok();
        let key_id = std::env::var("MICROTUN_SIGNER_KEY_ID").ok();
        match (url, key_id) {
            (None, None) => Ok(None),
            (Some(url), Some(key_id)) => Ok(Some(Self {
                url,
                key_id,
                github_login: super::flag("MICROTUN_SIGNER_GITHUB_LOGIN"),
            })),
            _ => anyhow::bail!("set both MICROTUN_SIGNER_URL and MICROTUN_SIGNER_KEY_ID"),
        }
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut cmd = Command::new(super::env("MICROTUN_SIGNER", "microtun-signer"));
        cmd.arg(subcommand)
            .args(["--url", &self.url, "--key-id", &self.key_id]);
        cmd
    }

    /// Captures stdout (the PEM or signature) but passes stderr straight to
    /// the terminal, so interactive prompts such as GitHub's device-login URL
    /// and code are visible while the signer waits for them.
    fn output(&self, mut cmd: Command) -> Result<Vec<u8>> {
        let output = cmd
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .with_context(|| format!("cannot execute {cmd:?}"))?;
        ensure!(
            output.status.success(),
            "microtun-signer failed ({}); see its messages above",
            output.status
        );
        Ok(output.stdout)
    }

    pub fn public_key(&self) -> Result<String> {
        let out = self.output(self.command("public-key"))?;
        let pem = String::from_utf8(out)?;
        ensure!(
            pem.trim_start().starts_with("-----BEGIN PUBLIC KEY-----"),
            "signer did not return an SPKI PEM public key"
        );
        if let Some(path) = std::env::var_os("MICROTUN_SIGNER_EXPECTED_PUBLIC_KEY") {
            let path = PathBuf::from(path);
            let expected = std::fs::read(&path)
                .with_context(|| format!("read trusted signer key {}", path.display()))?;
            let actual = pem::parse(pem.as_bytes())?;
            let expected = pem::parse(&expected)?;
            ensure!(
                actual.tag() == "PUBLIC KEY"
                    && expected.tag() == "PUBLIC KEY"
                    && actual.contents() == expected.contents(),
                "microtun-signer public key differs from MICROTUN_SIGNER_EXPECTED_PUBLIC_KEY"
            );
        }
        Ok(pem)
    }

    pub fn sign_digest(&self, digest: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            digest.len() == 32,
            "microtun-signer requires a SHA-256 digest"
        );
        let mut cmd = self.command("sign");
        cmd.arg("--digest").arg(STANDARD.encode(digest));
        if self.github_login {
            cmd.arg("--github-login");
        }
        let out = self.output(cmd)?;
        STANDARD
            .decode(String::from_utf8(out)?.trim())
            .context("signer returned invalid base64 signature")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_digest_is_rejected_before_invoking_signer() {
        let signer = RemoteSigner {
            url: "https://example.invalid".into(),
            key_id: "test".into(),
            github_login: false,
        };
        assert!(signer.sign_digest(&[0; 31]).is_err());
    }

    /// stdout is still returned to the caller while stderr (where the signer
    /// prints its GitHub device-login prompt) is left on the terminal.
    #[cfg(unix)]
    #[test]
    fn signer_stderr_is_not_captured() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-signer");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'GitHub authentication required (test prompt)' >&2\necho c2lnbmF0dXJl\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let signer = RemoteSigner {
            url: "https://example.invalid".into(),
            key_id: "test".into(),
            github_login: false,
        };
        let mut cmd = Command::new(&fake);
        cmd.arg("sign");
        let stdout = signer.output(cmd).unwrap();
        assert_eq!(String::from_utf8(stdout).unwrap().trim(), "c2lnbmF0dXJl");
    }
}
