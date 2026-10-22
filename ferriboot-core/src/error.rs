/// Reasons an image is rejected. These never indicate a flash failure.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// The header magic is erased: the slot holds no image.
    NoImage,
    /// The header is not erased but is not a valid MCUboot header (wrong
    /// magic, inconsistent sizes, ...).
    InvalidHeader,
    /// Header flags request a feature this port does not support
    /// (encryption or compression).
    UnsupportedFlags(u32),
    /// The image is marked `IMAGE_F_NON_BOOTABLE`.
    NonBootable,
    /// Authenticated image rejected by the board's executable-format checks.
    PlatformRejected,
    /// The image (including its TLVs) does not fit in front of the trailer.
    ImageTooLarge,
    InvalidTlvInfo,
    TruncatedTlv,
    InvalidTlvLength(u16),
    DuplicateTlv(u16),
    /// A TLV that must be covered by the signature appears in the
    /// unprotected area.
    UnprotectedTlvNotAllowed(u16),
    UnsupportedPureSignature,
    MissingSha256,
    HashMismatch,
    MissingKeyHash,
    /// The key hash / public key in the image does not match any trusted key.
    UnknownKey,
    MissingSignature,
    SignatureInvalid,
    InvalidPublicKey,
    MissingVid,
    WrongVid,
    MissingCid,
    WrongCid,
    /// Image version is below [`crate::Policy::min_version`].
    VersionTooLow,
    /// Downgrade prevention rejected an older image in the secondary slot.
    Downgrade,
    MissingSecurityCounter,
    SecurityCounterTooLow,
    SecurityCounterTooHigh,
}

/// Static configuration problems detected when constructing a bootloader or updater.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Primary and secondary slots must have the same size.
    SlotSizeMismatch,
    /// Area size is not a multiple of the flash erase size.
    MisalignedArea,
    /// Flash read size must be non-zero and fit the bounded read bounce buffer.
    UnsupportedReadSize,
    /// Flash write size must be a power of two no larger than `BOOT_MAX_ALIGN` (8).
    UnsupportedWriteSize,
    /// Swap-using-scratch needs a scratch area.
    MissingScratch,
    /// Scratch must hold a data sector and a separate metadata sector.
    ScratchTooSmall,
    /// The trailer does not fit in the slot.
    TrailerDoesNotFit,
}

/// Errors returned by the bootloader and updater.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// Hardware rollback storage failed. Never fall back to another image.
    SecurityCounter(crate::SecurityCounterError),
    /// The underlying flash reported an error.
    Flash(E),
    /// An image failed validation.
    Image(ImageError),
    /// Invalid slot configuration.
    Config(ConfigError),
    /// No slot contains an image that may be booted.
    NoBootableImage,
    /// A trailer contains a corrupt magic.
    BadTrailer,
    /// The direct-XIP trial marker write returned success but read-back
    /// could not confirm that the marker was persisted.
    TrialMarkerNotPersisted,
    /// The running image is an unconfirmed trial and the update slot holds
    /// the image the bootloader falls back to. Confirm the running image
    /// before replacing or erasing the update slot.
    UpdateSlotInUse,
}

impl<E> From<ImageError> for Error<E> {
    fn from(value: ImageError) -> Self {
        Self::Image(value)
    }
}

impl<E> From<ConfigError> for Error<E> {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}
