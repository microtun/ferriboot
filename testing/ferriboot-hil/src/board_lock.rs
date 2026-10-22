//! Hold an OS lock for the entire physical-board test, across test binaries.
//! This file never stores flash content or decides which sectors to restore.
use sha2::{Digest, Sha256};
use std::fs::File;

use crate::Board;

pub(crate) struct BoardLock {
    _file: File,
}

impl BoardLock {
    pub(crate) fn acquire(board: &Board, port: &str) -> crate::Result<Self> {
        let mut identity = Sha256::new();
        identity.update(port.as_bytes());
        identity.update([0]);
        if board.probe.is_some() {
            identity.update(
                std::env::var("PROBE_RS_PROBE")
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
        let name = format!("{:x}", identity.finalize());
        let dir = crate::artifacts::target_root(board).join("board-locks");
        std::fs::create_dir_all(&dir)?;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(format!("{name}.lock")))?;
        file.try_lock()
            .map_err(|error| format!("DUT is already in use or cannot be locked: {error}"))?;
        Ok(Self { _file: file })
    }
}
