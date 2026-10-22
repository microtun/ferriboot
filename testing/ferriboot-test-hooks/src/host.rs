//! Host-only construction of the test-hook record used by board test suites.

use std::vec::Vec;

use ferriboot_core::ImageVersion;

use crate::{Behaviour, MAGIC, MAX_ENTRIES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub version: ImageVersion,
    pub behaviour: Behaviour,
    /// Watchdog timeout in seconds; 0 preserves the firmware's default.
    pub watchdog_secs: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookConfig {
    /// Behaviour for versions without an entry.
    pub default: Behaviour,
    pub default_watchdog_secs: u8,
    pub entries: Vec<Entry>,
}

impl Default for HookConfig {
    /// Every image confirms itself and idles: a healthy test device.
    fn default() -> Self {
        Self {
            default: Behaviour::Confirm,
            default_watchdog_secs: 0,
            entries: Vec::new(),
        }
    }
}

/// Parse `major.minor.revision[+build]` (as used by `imgtool --version`).
pub fn parse_version(s: &str) -> ImageVersion {
    let (main, build) = s.split_once('+').unwrap_or((s, "0"));
    let mut it = main.split('.');
    let mut next = || it.next().unwrap_or("0");
    let (major, minor, revision) = (next(), next(), next());
    ImageVersion::new(
        major.parse().expect("major"),
        minor.parse().expect("minor"),
        revision.parse().expect("revision"),
        build.parse().expect("build"),
    )
}

impl HookConfig {
    /// Run `behaviour` for `version` rather than the default.
    pub fn with(mut self, version: &str, behaviour: Behaviour) -> Self {
        self.set(version, behaviour, 0);
        self
    }

    pub fn set(&mut self, version: &str, behaviour: Behaviour, watchdog_secs: u8) {
        let version = parse_version(version);
        self.entries.retain(|e| e.version != version);
        assert!(self.entries.len() < MAX_ENTRIES, "too many hook entries");
        self.entries.push(Entry {
            version,
            behaviour,
            watchdog_secs,
        });
    }

    /// Serialize the used portion of the on-flash record. Remaining storage
    /// bytes may be left erased; the decoder ignores unused entries.
    pub fn to_bytes(&self) -> Vec<u8> {
        assert!(self.entries.len() <= MAX_ENTRIES, "too many hook entries");
        let mut out = MAGIC.to_vec();
        out.extend([
            self.entries.len() as u8,
            self.default as u8,
            self.default_watchdog_secs,
            0,
        ]);
        for e in &self.entries {
            let v = e.version;
            out.extend([v.major, v.minor]);
            out.extend(v.revision.to_le_bytes());
            out.extend(v.build.to_le_bytes());
            out.extend([e.behaviour as u8, e.watchdog_secs, 0, 0]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Hook, RECORD_LEN};

    fn decode(config: &HookConfig) -> Config {
        let bytes = config.to_bytes();
        let mut record = [0xff; RECORD_LEN];
        record[..bytes.len()].copy_from_slice(&bytes);
        Config::from_bytes(record).unwrap()
    }

    #[test]
    fn host_encoder_roundtrips_decoder() {
        let mut host = HookConfig::default();
        host.set("2.1.3+42", Behaviour::HangNoFeed, 3);
        host.set("1.0.0", Behaviour::Normal, 0);
        let record = decode(&host);
        assert_eq!(
            record.select(&parse_version("2.1.3+42")),
            Some(Hook {
                raw: Behaviour::HangNoFeed as u8,
                behaviour: Some(Behaviour::HangNoFeed),
                watchdog_secs: 3,
            })
        );
        assert_eq!(record.select(&parse_version("1.0.0")), None);
        assert_eq!(
            record.select(&parse_version("9.0.0")),
            Some(Hook {
                raw: Behaviour::Confirm as u8,
                behaviour: Some(Behaviour::Confirm),
                watchdog_secs: 0,
            })
        );
    }

    #[test]
    fn later_set_replaces_same_version() {
        let mut host = HookConfig::default();
        host.set("2.0.0", Behaviour::Idle, 0);
        host.set("2.0.0+0", Behaviour::Panic, 2);
        assert_eq!(host.entries.len(), 1);
        assert_eq!(
            decode(&host)
                .select(&parse_version("2.0.0"))
                .unwrap()
                .behaviour,
            Some(Behaviour::Panic)
        );
    }

    #[test]
    fn permits_exact_maximum_entries() {
        let mut host = HookConfig::default();
        for build in 0..MAX_ENTRIES {
            host.set(&std::format!("1.2.3+{build}"), Behaviour::Idle, 0);
        }
        assert_eq!(host.to_bytes().len(), RECORD_LEN);
        assert_eq!(
            decode(&host)
                .select(&parse_version("1.2.3+15"))
                .unwrap()
                .behaviour,
            Some(Behaviour::Idle)
        );
    }
}
