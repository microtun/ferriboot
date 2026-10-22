//! Shared `FBTHOOK1` test-hook record used by the three boards' example firmware.
//!
//! This record is **unauthenticated**; the example firmware always reads it.
//! Never deploy these hooks in production. Hardware-specific actions
//! belong to each firmware `main.rs`; this crate only interprets the format.
#![cfg_attr(not(feature = "std"), no_std)]

use ferriboot_core::ImageVersion;

pub const MAGIC: &[u8; 8] = b"FBTHOOK1";
pub const MAX_ENTRIES: usize = 16;
pub const RECORD_LEN: usize = 12 + MAX_ENTRIES * 12;

/// Behaviour codes in the on-flash record, shared across all three boards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Behaviour {
    Normal = 0,
    Confirm = 1,
    Idle = 2,
    ResetNoConfirm = 3,
    Panic = 4,
    Fault = 5,
    HangNoFeed = 6,
    EraseActive = 7,
}

impl Behaviour {
    /// Unknown codes remain distinguishable from `Normal`.
    pub fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            0 => Self::Normal,
            1 => Self::Confirm,
            2 => Self::Idle,
            3 => Self::ResetNoConfirm,
            4 => Self::Panic,
            5 => Self::Fault,
            6 => Self::HangNoFeed,
            7 => Self::EraseActive,
            _ => return None,
        })
    }
}

/// The action selected for an image version. `None` for `behaviour` is an
/// unknown on-flash code, not a request to run normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hook {
    pub raw: u8,
    pub behaviour: Option<Behaviour>,
    pub watchdog_secs: u8,
}

/// A complete fixed-size record read from flash (unused entries are ignored).
pub struct Config {
    record: [u8; RECORD_LEN],
}

impl Config {
    /// Ignore blank, unrelated, or malformed records.
    pub fn from_bytes(record: [u8; RECORD_LEN]) -> Option<Self> {
        (record[..8] == *MAGIC && usize::from(record[8]) <= MAX_ENTRIES).then_some(Self { record })
    }

    /// Return `None` when the selected behaviour is `Normal`. When no entry
    /// matches, use the default behaviour in the record header.
    pub fn select(&self, version: &ImageVersion) -> Option<Hook> {
        let r = &self.record;
        let (raw, watchdog_secs) = r[12..]
            .as_chunks::<12>()
            .0
            .iter()
            .take(usize::from(r[8]))
            .find(|e| {
                ImageVersion::new(
                    e[0],
                    e[1],
                    u16::from_le_bytes([e[2], e[3]]),
                    u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
                ) == *version
            })
            .map_or((r[9], r[10]), |e| (e[8], e[9]));
        let behaviour = Behaviour::from_raw(raw);
        (behaviour != Some(Behaviour::Normal)).then_some(Hook {
            raw,
            behaviour,
            watchdog_secs,
        })
    }
}

#[cfg(feature = "std")]
mod host;
#[cfg(feature = "std")]
pub use host::{Entry, HookConfig, parse_version};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erased_or_invalid_storage_is_ignored() {
        assert!(Config::from_bytes([0xff; RECORD_LEN]).is_none());
        let mut record = [0; RECORD_LEN];
        record[..8].copy_from_slice(MAGIC);
        record[8] = MAX_ENTRIES as u8 + 1;
        assert!(Config::from_bytes(record).is_none());
    }

    #[test]
    fn unknown_behaviour_is_not_normal() {
        let mut record = [0; RECORD_LEN];
        record[..8].copy_from_slice(MAGIC);
        record[9] = 255;
        let config = Config::from_bytes(record).unwrap();
        let hook = config.select(&ImageVersion::new(1, 0, 0, 0)).unwrap();
        assert_eq!(hook.raw, 255);
        assert_eq!(hook.behaviour, None);
    }
}
