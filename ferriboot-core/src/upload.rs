//! Bounded, packet-at-a-time OTA sessions. Time is supplied by the caller so
//! timeout/restart/error behavior is testable without a hardware executor.
use embedded_storage::nor_flash::NorFlash;
use sha2::{Digest, Sha256};

use crate::{
    Error, FirmwareUpdater, ImageHeader, ImageSlots,
    image::{IMAGE_HEADER_SIZE, TLV_INFO_SIZE},
    updater::UpdateProgress,
};

pub const UPLOAD_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone, Copy)]
pub struct UploadRequest<'a> {
    pub off: u32,
    pub len: Option<u32>,
    pub sha: Option<&'a [u8]>,
    pub upgrade: bool,
    pub data: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadError<E> {
    Length,
    Hash,
    Overrun,
    Pending,
    Version,
    Header,
    Storage(Error<E>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadReply {
    Offset(u32),
    Complete { off: u32, image: ImageHeader },
}

// All slot ownership is represented here. CleanupPending also guards flash
// operations while their session/writer state is temporarily owned by a call.
#[allow(clippy::large_enum_variant)]
enum State {
    Idle,
    CollectingHeader {
        session: Session,
        bytes: [u8; IMAGE_HEADER_SIZE],
        len: usize,
    },
    Writing {
        session: Session,
        progress: UpdateProgress,
        header: [u8; IMAGE_HEADER_SIZE],
    },
    CleanupPending,
}

impl State {
    fn session(&self) -> Option<&Session> {
        match self {
            Self::CollectingHeader { session, .. } | Self::Writing { session, .. } => Some(session),
            Self::Idle | Self::CleanupPending => None,
        }
    }

    fn session_mut(&mut self) -> Option<&mut Session> {
        match self {
            Self::CollectingHeader { session, .. } | Self::Writing { session, .. } => Some(session),
            Self::Idle | Self::CleanupPending => None,
        }
    }

    fn has_written(&self) -> bool {
        matches!(self, Self::Writing { .. } | Self::CleanupPending)
    }

    fn matches_start(&self, req: &UploadRequest<'_>) -> bool {
        match self {
            Self::CollectingHeader {
                session,
                bytes,
                len,
            } => session.matches_start(req, &bytes[..*len]),
            Self::Writing {
                session, header, ..
            } => session.matches_start(req, header),
            Self::Idle | Self::CleanupPending => false,
        }
    }
}

struct Session {
    total: u32,
    sha: [u8; 32],
    sha_len: usize,
    upgrade: bool,
    received: u32,
    hasher: Sha256,
    last_activity: u64,
}

impl Session {
    fn matches_start(&self, req: &UploadRequest<'_>, prefix: &[u8]) -> bool {
        // A header prefix is not a transfer identity: different same-sized
        // images commonly share it. Only a supplied hash makes offset-zero
        // retransmission distinguishable from a genuinely new upload.
        let Some(sha) = req.sha else {
            return false;
        };
        if self.sha_len == 0 || sha != &self.sha[..self.sha_len] {
            return false;
        }
        let len = req.data.len().min(prefix.len());
        req.len.is_none_or(|n| n == self.total)
            && req.upgrade == self.upgrade
            && req.data[..len] == prefix[..len]
    }

    fn check_request<E>(
        &mut self,
        req: &UploadRequest<'_>,
        now_ms: u64,
    ) -> Result<Option<UploadReply>, UploadError<E>> {
        if req.len.is_some_and(|n| n != self.total) {
            return Err(UploadError::Length);
        }
        if req.sha.is_some_and(|h| h != &self.sha[..self.sha_len]) {
            return Err(UploadError::Hash);
        }
        if req.off != self.received {
            self.last_activity = now_ms;
            return Ok(Some(UploadReply::Offset(self.received)));
        }
        if u64::from(self.received) + req.data.len() as u64 > u64::from(self.total) {
            return Err(UploadError::Overrun);
        }
        Ok(None)
    }

    fn finish<F: NorFlash>(
        self,
        progress: UpdateProgress,
        updater: &mut FirmwareUpdater<F>,
        active: &ImageHeader,
        check_file_hash: bool,
    ) -> Result<UploadReply, UploadError<F::Error>> {
        updater
            .finish_progress(progress)
            .map_err(UploadError::Storage)?;
        if check_file_hash && self.sha_len != 0 {
            let digest = self.hasher.finalize();
            if digest[..self.sha_len] != self.sha[..self.sha_len] {
                return Err(UploadError::Hash);
            }
        }
        let image = updater
            .inspect_update_received(self.total)
            .map_err(UploadError::Storage)?;
        if self.upgrade && !image.version.semver_cmp(&active.version).is_gt() {
            return Err(UploadError::Version);
        }
        Ok(UploadReply::Complete {
            off: self.total,
            image,
        })
    }
}

/// Owns RAM session state, never a flash borrow. New offset-zero sessions
/// replace interrupted transfers unless a supplied transfer hash unambiguously
/// matches the active session; only then is the packet treated as a retry.
/// Cancelled/rejected transfers retain a cleanup obligation until erasing
/// succeeds. Callers must keep this object alive across cleanup failures.
pub struct FirmwareUpload {
    state: State,
    check_file_hash: bool,
}

impl FirmwareUpload {
    pub const fn new(check_file_hash: bool) -> Self {
        Self {
            state: State::Idle,
            check_file_hash,
        }
    }

    /// A transfer or a failed cleanup still owns the update slot.
    pub fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Flash may have been modified, including by a cancelled/rejected upload
    /// whose cleanup failed. Any cached inactive image must be invalidated.
    pub fn has_written(&self) -> bool {
        self.state.has_written()
    }

    /// A cancelled or rejected candidate still needs to be erased. Until this
    /// clears, the candidate must not be cached or scheduled for boot.
    pub fn cleanup_required(&self) -> bool {
        matches!(self.state, State::CleanupPending)
    }

    /// Explicit cancellation, used by erase/reset/session replacement.
    /// Erase written candidates so an aborted valid file cannot be rediscovered.
    /// On failure, subsequent calls retry the erase without resuming the transfer.
    pub fn abort<F: NorFlash>(
        &mut self,
        updater: &mut FirmwareUpdater<F>,
    ) -> Result<(), Error<F::Error>> {
        if self.state.has_written() {
            self.state = State::CleanupPending;
            // Only reached once this upload has written: the slot is ours.
            updater.erase_update_unchecked()?;
        }
        self.state = State::Idle;
        Ok(())
    }

    /// Expire an idle transfer or retry pending cleanup immediately. Returns
    /// `true` when either operation succeeds; errors preserve the obligation.
    pub fn expire<F: NorFlash>(
        &mut self,
        updater: &mut FirmwareUpdater<F>,
        now_ms: u64,
    ) -> Result<bool, Error<F::Error>> {
        if self.cleanup_required()
            || self
                .state
                .session()
                .is_some_and(|s| now_ms.saturating_sub(s.last_activity) >= UPLOAD_TIMEOUT_MS)
        {
            self.abort(updater)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn receive<F: NorFlash>(
        &mut self,
        updater: &mut FirmwareUpdater<F>,
        images: &ImageSlots,
        req: UploadRequest<'_>,
        now_ms: u64,
    ) -> Result<UploadReply, UploadError<F::Error>> {
        let expired = self.expire(updater, now_ms).map_err(UploadError::Storage)?;
        // Never select a pre-expiration cache after cleanup has erased its slot.
        let images = ImageSlots {
            inactive: if expired { None } else { images.inactive },
            ..*images
        };
        if req.sha.is_some_and(|h| h.is_empty() || h.len() > 32) {
            return Err(UploadError::Hash);
        }
        if req.off == 0 && !self.state.matches_start(&req) {
            self.start(updater, &images, &req, now_ms)?;
        }
        let Some(s) = self.state.session_mut() else {
            return Ok(UploadReply::Offset(0));
        };
        if let Some(reply) = s.check_request(&req, now_ms)? {
            return Ok(reply);
        }
        // Keep cleanup ownership before moving any written session out of self.
        let guard = if self.state.has_written() {
            State::CleanupPending
        } else {
            State::Idle
        };
        let state = core::mem::replace(&mut self.state, guard);
        let mut state = match self.append(updater, &images.active, state, req.data) {
            Ok(state) => state,
            Err(error) => {
                self.abort(updater).map_err(UploadError::Storage)?;
                return Err(error);
            }
        };
        let session = state.session_mut().expect("active upload session");
        session.last_activity = now_ms;
        session.hasher.update(req.data);
        session.received += req.data.len() as u32;
        if session.received < session.total {
            let off = session.received;
            self.state = state;
            return Ok(UploadReply::Offset(off));
        }
        // Finish consumes the writer/hash; the guard remains until success or
        // a successful erase, including when basic format inspection fails.
        let result = match state {
            State::Writing {
                session, progress, ..
            } => session.finish(progress, updater, &images.active, self.check_file_hash),
            _ => Err(UploadError::Header),
        };
        if result.is_err() {
            self.abort(updater).map_err(UploadError::Storage)?;
        } else {
            self.state = State::Idle;
        }
        result
    }

    fn start<F: NorFlash>(
        &mut self,
        updater: &mut FirmwareUpdater<F>,
        images: &ImageSlots,
        req: &UploadRequest<'_>,
        now_ms: u64,
    ) -> Result<(), UploadError<F::Error>> {
        let total = req.len.ok_or(UploadError::Length)?;
        if total < IMAGE_HEADER_SIZE as u32 || total > updater.max_image_size() {
            return Err(UploadError::Length);
        }
        if req.data.len() as u64 > u64::from(total) {
            return Err(UploadError::Overrun);
        }
        if updater
            .update_is_in_use(images)
            .map_err(UploadError::Storage)?
        {
            return Err(UploadError::Pending);
        }
        // Reject a complete invalid header before cancelling the old transfer.
        if req.data.len() >= IMAGE_HEADER_SIZE {
            Self::admit::<F::Error>(
                &req.data[..IMAGE_HEADER_SIZE],
                total,
                req.upgrade,
                &images.active,
            )?;
        }
        self.abort(updater).map_err(UploadError::Storage)?;
        let mut sha = [0; 32];
        let sha_len = req.sha.map_or(0, |h| h.len());
        sha[..sha_len].copy_from_slice(req.sha.unwrap_or(&[]));
        self.state = State::CollectingHeader {
            session: Session {
                total,
                sha,
                sha_len,
                upgrade: req.upgrade,
                received: 0,
                hasher: Sha256::new(),
                last_activity: now_ms,
            },
            bytes: [0; IMAGE_HEADER_SIZE],
            len: 0,
        };
        Ok(())
    }

    fn admit<E>(
        raw: &[u8],
        total: u32,
        upgrade: bool,
        active: &ImageHeader,
    ) -> Result<(), UploadError<E>> {
        let bytes: &[u8; IMAGE_HEADER_SIZE] = raw.try_into().map_err(|_| UploadError::Header)?;
        let h = ImageHeader::parse(bytes).ok_or(UploadError::Header)?;
        h.check().map_err(|_| UploadError::Header)?;
        if h.hashed_len()
            .and_then(|n| n.checked_add(TLV_INFO_SIZE))
            .is_none_or(|n| n > total)
        {
            return Err(UploadError::Header);
        }
        if upgrade && !h.version.semver_cmp(&active.version).is_gt() {
            return Err(UploadError::Version);
        }
        Ok(())
    }

    fn append<F: NorFlash>(
        &mut self,
        updater: &mut FirmwareUpdater<F>,
        active: &ImageHeader,
        state: State,
        data: &[u8],
    ) -> Result<State, UploadError<F::Error>> {
        match state {
            State::CollectingHeader {
                session,
                mut bytes,
                mut len,
            } => {
                let take = (IMAGE_HEADER_SIZE - len).min(data.len());
                bytes[len..len + take].copy_from_slice(&data[..take]);
                len += take;
                if len < IMAGE_HEADER_SIZE {
                    return Ok(State::CollectingHeader {
                        session,
                        bytes,
                        len,
                    });
                }
                Self::admit(&bytes, session.total, session.upgrade, active)?;
                // Refuse before taking cleanup ownership: a refused slot holds
                // a trial's fallback image, which cleanup must never erase.
                // `start` already checked, but with caller-cached headers.
                updater.ensure_update_slot_free().map_err(|e| match e {
                    Error::UpdateSlotInUse => UploadError::Pending,
                    e => UploadError::Storage(e),
                })?;
                // begin_progress may fail after partially erasing the trailer.
                self.state = State::CleanupPending;
                let mut progress = updater.begin_progress().map_err(UploadError::Storage)?;
                updater
                    .write_progress(&mut progress, &bytes)
                    .map_err(UploadError::Storage)?;
                updater
                    .write_progress(&mut progress, &data[take..])
                    .map_err(UploadError::Storage)?;
                Ok(State::Writing {
                    session,
                    progress,
                    header: bytes,
                })
            }
            State::Writing {
                session,
                mut progress,
                header,
            } => {
                updater
                    .write_progress(&mut progress, data)
                    .map_err(UploadError::Storage)?;
                Ok(State::Writing {
                    session,
                    progress,
                    header,
                })
            }
            State::Idle | State::CleanupPending => unreachable!("active upload session"),
        }
    }
}
