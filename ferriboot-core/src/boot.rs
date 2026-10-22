//! The boot decision (`boot_go`).

use core::hint::black_box;

use embedded_storage::nor_flash::NorFlash;

use crate::{
    ConfigError, Error, ImageError, SecurityCounterError, SecurityCounterStore,
    fih::{self, Fih},
    flash,
    image::ImageHeader,
    trailer::{SwapState, SwapType, Trailer},
    validate::{
        Policy, PublicKey, SecurityConfigFingerprint, ValidatedImage,
        security_config_fingerprint as fingerprint_security_config, validate_image,
    },
};

/// Upgrade strategy (`MCUBOOT_SWAP_USING_SCRATCH`, `MCUBOOT_OVERWRITE_ONLY`,
/// `MCUBOOT_DIRECT_XIP[_REVERT]`).
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeMode {
    /// Swap primary and secondary through a scratch area. Supports test
    /// upgrades with automatic revert, and resumes after power loss.
    SwapScratch,
    /// Copy the secondary image over the primary one. No revert.
    Overwrite,
    /// Execute directly from whichever slot holds the newest valid image.
    /// With `revert`, an image must be marked pending (magic) to be
    /// considered, and an image that was booted once without being
    /// confirmed is erased on the next boot.
    DirectXip { revert: bool },
}

impl UpgradeMode {
    /// Map a bootloader crate's strategy features to a mode. Board
    /// bootloaders use the same three names through
    /// [`upgrade_mode_from_features!`](crate::upgrade_mode_from_features):
    ///
    /// | Features | Mode |
    /// |---|---|
    /// | none | [`UpgradeMode::SwapScratch`] |
    /// | `overwrite-only` | [`UpgradeMode::Overwrite`] |
    /// | `direct-xip` | [`UpgradeMode::DirectXip`] without revert |
    /// | `direct-xip-revert` | [`UpgradeMode::DirectXip`] with revert |
    ///
    /// # Panics
    ///
    /// If more than one strategy is selected. Evaluated in a `const` item, as
    /// the macro does, this is a compile-time error.
    pub const fn from_features(
        overwrite_only: bool,
        direct_xip: bool,
        direct_xip_revert: bool,
    ) -> Self {
        match (overwrite_only, direct_xip, direct_xip_revert) {
            (false, false, false) => Self::SwapScratch,
            (true, false, false) => Self::Overwrite,
            (false, true, false) => Self::DirectXip { revert: false },
            (false, false, true) => Self::DirectXip { revert: true },
            _ => panic!(
                "select at most one upgrade strategy feature: `overwrite-only`, `direct-xip` or `direct-xip-revert`"
            ),
        }
    }
}

/// Bootloader configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootConfig {
    pub mode: UpgradeMode,
    /// Reject installing an image with a lower `major.minor.revision` than
    /// the one in the primary slot (`MCUBOOT_DOWNGRADE_PREVENTION`).
    ///
    /// Test and permanent upgrades and bootstraps are checked in every mode,
    /// against the primary header whenever one parses, even if that image no
    /// longer validates. Only a swap-mode revert of an unconfirmed test image
    /// is exempt. Rejected upgrades are erased; a rejected bootstrap leaves
    /// the secondary image in place and the device does not boot.
    ///
    /// Unlike upstream, swap-mode bootstraps are checked too. Otherwise a
    /// broken primary image (bit rot, or one glitched validation) would
    /// reinstall the previous version still sitting in the secondary slot.
    ///
    /// The reference version comes from an unauthenticated header, so this
    /// is best effort against anyone who can erase that header. Use a
    /// security counter for a rollback floor that survives that.
    pub downgrade_prevention: bool,
    /// If no upgrade is pending and the primary slot holds no valid image,
    /// copy a valid secondary image over it (`MCUBOOT_BOOTSTRAP`). As
    /// upstream, a pending upgrade over an invalid primary is also copied
    /// rather than swapped.
    pub bootstrap: bool,
    /// Policy every booted or installed image must satisfy. Give the
    /// application's [`FirmwareUpdater`](crate::FirmwareUpdater) the same
    /// policy so it only schedules images this bootloader will install.
    pub policy: Policy,
    /// In direct-XIP revert mode, return the flash error instead of booting a
    /// candidate when the trial-consumed (`copy_done`) marker cannot be persisted.
    /// Defaults to `true` to prevent booting a trial if the marker cannot
    /// be written or verified. Set `false` only for upstream-compatible,
    /// best-effort testing.
    pub direct_xip_fail_closed: bool,
    /// Execution addresses of the primary and secondary slot headers. Ports
    /// with flash remapping can supply the same runtime address for both slots.
    /// In direct-XIP mode an image with `IMAGE_F_ROM_FIXED` (`imgtool --rom-fixed`) is only
    /// booted from the slot whose address equals its `load_addr`
    /// (`boot_rom_address_check`). `None` skips the check.
    pub slot_addresses: Option<[u32; 2]>,
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            mode: UpgradeMode::SwapScratch,
            downgrade_prevention: false,
            bootstrap: false,
            policy: Policy::default(),
            direct_xip_fail_closed: true,
            slot_addresses: None,
        }
    }
}

/// Image slots.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Slot {
    Primary = 0,
    Secondary = 1,
}

impl Slot {
    pub const fn other(self) -> Self {
        match self {
            Self::Primary => Self::Secondary,
            Self::Secondary => Self::Primary,
        }
    }

    /// Physical slot number at protocol and shared-memory boundaries.
    pub const fn number(self) -> u8 {
        self as u8
    }

    pub const fn from_number(number: u8) -> Option<Self> {
        match number {
            0 => Some(Self::Primary),
            1 => Some(Self::Secondary),
            _ => None,
        }
    }
}

/// Board-specific acceptance checks, after authentication but before installing
/// an image. `stored` identifies the readable flash area; `target` identifies
/// where the image will execute (primary for swap/overwrite, `stored` for XIP).
/// Implementations must only read flash. Reject malformed executable layouts
/// with `Error::Image(ImageError::PlatformRejected)` and propagate flash errors.
pub trait ImageValidator<F: NorFlash> {
    fn validate(
        &mut self,
        flash: &mut F,
        stored: Slot,
        target: Slot,
        image: &ValidatedImage,
    ) -> Result<(), Error<F::Error>>;
}

impl<F, V> ImageValidator<F> for V
where
    F: NorFlash,
    V: FnMut(&mut F, Slot, Slot, &ValidatedImage) -> Result<(), Error<F::Error>>,
{
    fn validate(
        &mut self,
        flash: &mut F,
        stored: Slot,
        target: Slot,
        image: &ValidatedImage,
    ) -> Result<(), Error<F::Error>> {
        self(flash, stored, target, image)
    }
}

/// Why a secondary image is being considered, independently of trailer state.
#[derive(Clone, Copy)]
pub(crate) enum CandidateKind {
    Upgrade,
    Rollback,
    Bootstrap,
}

impl CandidateKind {
    pub(crate) fn for_swap(swap_type: SwapType) -> Self {
        if swap_type == SwapType::Revert {
            Self::Rollback
        } else {
            Self::Upgrade
        }
    }

    /// Only a swap-mode revert of an unconfirmed test image may install an
    /// older version. Bootstraps are checked in every mode (see
    /// [`BootConfig::downgrade_prevention`]).
    fn checks_downgrade(self, mode: UpgradeMode) -> bool {
        mode == UpgradeMode::Overwrite || matches!(self, Self::Upgrade | Self::Bootstrap)
    }
}

/// What the bootloader decided to boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootDecision {
    /// Slot holding the image to execute. Always `Primary` except in
    /// direct-XIP mode.
    pub slot: Slot,
    /// The authenticated image.
    pub image: ValidatedImage,
    /// The upgrade operation performed (or resumed) during this boot.
    pub performed: SwapType,
}

/// A counter handle that either owns a typed store borrow or represents no store.
/// `()` is used before a store is attached, without an allocated or erased
/// trait object. This trait is an implementation detail of [`BootLoader`].
#[doc(hidden)]
pub trait CounterHandle {
    fn attached(&self) -> bool;
    fn capacity(&self) -> Option<u32>;
    fn read(&mut self) -> Result<Option<u32>, SecurityCounterError>;
    fn advance(&mut self, value: u32) -> Result<(), SecurityCounterError>;
}

impl CounterHandle for () {
    fn attached(&self) -> bool {
        false
    }

    fn capacity(&self) -> Option<u32> {
        None
    }

    fn read(&mut self) -> Result<Option<u32>, SecurityCounterError> {
        Ok(None)
    }

    fn advance(&mut self, _value: u32) -> Result<(), SecurityCounterError> {
        unreachable!("cannot advance a counter that is not attached")
    }
}

impl<S: SecurityCounterStore> CounterHandle for &mut S {
    fn attached(&self) -> bool {
        true
    }

    fn capacity(&self) -> Option<u32> {
        Some(SecurityCounterStore::capacity(&**self))
    }

    fn read(&mut self) -> Result<Option<u32>, SecurityCounterError> {
        SecurityCounterStore::get(&mut **self).map(Some)
    }

    fn advance(&mut self, value: u32) -> Result<(), SecurityCounterError> {
        SecurityCounterStore::advance(&mut **self, value)
    }
}

/// Optional typed entropy handle. Unlike an absent RNG placeholder, a
/// *present* handle must implement `rand::CryptoRng`.
#[doc(hidden)]
pub trait EntropyHandle {
    fn random_word(&mut self) -> Option<u32>;
}

impl EntropyHandle for () {
    fn random_word(&mut self) -> Option<u32> {
        None
    }
}

impl<R: rand::CryptoRng> EntropyHandle for &mut R {
    fn random_word(&mut self) -> Option<u32> {
        Some(self.next_u32())
    }
}

/// The bootloader state machine over three flash areas.
/// Optional counter and RNG handles are tracked in the type, avoiding dynamic
/// dispatch while keeping `BootLoader::new` usable without either handle.
pub struct BootLoader<'k, F: NorFlash, C = (), R = ()> {
    counter: C,
    counter_floor: Option<u32>,
    entropy: R,
    pub(crate) primary: F,
    pub(crate) secondary: F,
    pub(crate) scratch: Option<F>,
    pub(crate) keys: &'k [PublicKey],
    pub(crate) config: BootConfig,
    pub(crate) slot_trailer: Trailer,
    pub(crate) scratch_trailer: Option<Trailer>,
}

impl<'k, F: NorFlash> BootLoader<'k, F> {
    /// Create a bootloader over the given areas. `scratch` is required for
    /// [`UpgradeMode::SwapScratch`] and ignored otherwise.
    pub fn new(
        primary: F,
        secondary: F,
        scratch: Option<F>,
        keys: &'k [PublicKey],
        config: BootConfig,
    ) -> Result<Self, ConfigError> {
        flash::check_geometry::<F>()?;
        let size = primary.capacity() as u32;
        if secondary.capacity() as u32 != size {
            return Err(ConfigError::SlotSizeMismatch);
        }
        let slot_trailer = Trailer::for_slot::<F>(size)?;
        let scratch_trailer = match (config.mode, scratch.as_ref()) {
            (UpgradeMode::SwapScratch, None) => return Err(ConfigError::MissingScratch),
            (UpgradeMode::SwapScratch, Some(s)) => {
                let ssize = s.capacity() as u32;
                if ssize < (F::ERASE_SIZE as u32).saturating_mul(2) {
                    return Err(ConfigError::ScratchTooSmall);
                }
                Some(Trailer::for_scratch::<F>(ssize)?)
            }
            _ => None,
        };
        Ok(Self {
            counter: (),
            counter_floor: None,
            entropy: (),
            primary,
            secondary,
            scratch,
            keys,
            config,
            slot_trailer,
            scratch_trailer,
        })
    }
}

impl<'k, F: NorFlash, C: CounterHandle, R: EntropyHandle> BootLoader<'k, F, C, R> {
    /// Read the actual nonvolatile security-counter floor for an optional
    /// MCUboot BLINFO TLV, after boot selection and any counter advances.
    /// `None` means no hardware store was attached. This is not the image's
    /// declared `SEC_CNT` and must not replace authorization checks.
    pub fn hardware_security_counter(&mut self) -> Result<Option<u32>, SecurityCounterError> {
        self.counter.read()
    }

    /// Require a protected SEC_CNT TLV on every image and enforce a persistent
    /// hardware floor, including after interrupted swaps. Board bootloaders
    /// must attach their hardware store on every boot. The internal policy
    /// fingerprint describes static verification policy, not changing fuses.
    pub fn with_security_counter_store<S: SecurityCounterStore>(
        self,
        store: &'k mut S,
    ) -> BootLoader<'k, F, &'k mut S, R> {
        BootLoader {
            counter: store,
            counter_floor: self.counter_floor,
            entropy: self.entropy,
            primary: self.primary,
            secondary: self.secondary,
            scratch: self.scratch,
            keys: self.keys,
            config: self.config,
            slot_trailer: self.slot_trailer,
            scratch_trailer: self.scratch_trailer,
        }
    }

    /// Supply a cryptographically secure random source (a hardware TRNG on
    /// production boards). Fault-hardened checks before irreversible steps
    /// are then preceded by a random delay (see [`fih::random_delay`]).
    /// Without one they still run, undelayed.
    pub fn with_entropy<S: rand::CryptoRng>(
        self,
        random: &'k mut S,
    ) -> BootLoader<'k, F, C, &'k mut S> {
        BootLoader {
            counter: self.counter,
            counter_floor: self.counter_floor,
            entropy: random,
            primary: self.primary,
            secondary: self.secondary,
            scratch: self.scratch,
            keys: self.keys,
            config: self.config,
            slot_trailer: self.slot_trailer,
            scratch_trailer: self.scratch_trailer,
        }
    }

    /// Decide what to boot, performing or resuming any pending upgrade first.
    pub fn boot(&mut self) -> Result<BootDecision, Error<F::Error>> {
        self.boot_with_validator(&mut |_: &mut F,
                                       _: Slot,
                                       _: Slot,
                                       _: &ValidatedImage|
         -> Result<(), Error<F::Error>> { Ok(()) })
    }

    /// Authenticate and check the board's executable format before starting an
    /// upgrade. The installed image is checked again before handoff. Resuming a
    /// partially committed swap relies on acceptance before its first step;
    /// split images cannot be revalidated until that swap has completed.
    pub fn boot_with_validator(
        &mut self,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<BootDecision, Error<F::Error>> {
        self.load_counter_floor()?;
        // A swap or overwrite can destroy the selected image's confirmation
        // trailer. Consume that confirmation before modifying either slot.
        // Direct-XIP does not move trailers: burning the counter of *both*
        // validated slots here could reject the newer-version slot before
        // candidate selection. Its selected image is handled below instead.
        // Split images during swap resume simply fail authentication.
        if self.counter.attached() && !matches!(self.config.mode, UpgradeMode::DirectXip { .. }) {
            let slot = Slot::Primary;
            if self.confirmed_for_counter(slot)? {
                match self.validate_for_boot(slot, slot, validator) {
                    Ok(image) => self.advance_counter(slot, &image)?,
                    Err(Error::Image(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        let decision = match self.config.mode {
            UpgradeMode::SwapScratch => self.boot_swap(validator),
            UpgradeMode::Overwrite => self.boot_overwrite(validator),
            UpgradeMode::DirectXip { revert } => self.boot_direct_xip(revert, validator),
        }?;
        if self.counter.attached() {
            // Only the *selected* direct-XIP image may raise the rollback floor.
            // Overwrite and XIP without revert have permanent semantics;
            // swap/test and XIP/revert must wait for image_ok (never burn on trial).
            if self.confirmed_for_counter(decision.slot)? {
                self.advance_counter(decision.slot, &decision.image)?;
            }
            // Last handoff guard, independent of trailer/candidate/resume paths.
            self.load_counter_floor()?;
            self.check_counter(&decision.image)?;
        }
        Ok(decision)
    }

    fn confirmed_for_counter(&mut self, slot: Slot) -> Result<bool, Error<F::Error>> {
        if matches!(
            self.config.mode,
            UpgradeMode::Overwrite | UpgradeMode::DirectXip { revert: false }
        ) {
            return Ok(true);
        }
        let state = self.state(slot)?;
        if state.image_ok == crate::Flag::Set {
            return Ok(true);
        }
        // Swap's factory image has an erased trailer and is implicitly
        // permanent (`FirmwareUpdater::confirm` is a no-op for that state).
        // Do not confuse a trailer being rebuilt during swap with factory.
        if self.config.mode == UpgradeMode::SwapScratch
            && slot == Slot::Primary
            && state.magic == crate::MagicState::Unset
        {
            let scratch_trailer = self.scratch_trailer.expect("scratch mode");
            let scratch = self.scratch.as_mut().expect("scratch mode");
            return Ok(
                scratch_trailer.read_state(scratch)?.magic == crate::MagicState::Unset
                    && self.slot_trailer.read_status(&mut self.primary)?.is_none(),
            );
        }
        Ok(false)
    }

    fn load_counter_floor(&mut self) -> Result<(), Error<F::Error>> {
        if let Some(floor) = self.counter.read().map_err(Error::SecurityCounter)? {
            if floor > self.counter.capacity().expect("attached counter")
                || self.counter_floor.is_some_and(|old| floor < old)
            {
                return Err(Error::SecurityCounter(SecurityCounterError::InvalidState));
            }
            self.counter_floor = Some(floor);
        }
        Ok(())
    }

    fn check_counter(&self, image: &ValidatedImage) -> Result<(), Error<F::Error>> {
        if let Some(floor) = self.counter_floor {
            let value = image
                .security_counter()
                .ok_or(ImageError::MissingSecurityCounter)?;
            if value < floor {
                return Err(ImageError::SecurityCounterTooLow.into());
            }
            if self
                .counter
                .capacity()
                .is_some_and(|capacity| value > capacity)
            {
                return Err(ImageError::SecurityCounterTooHigh.into());
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn advance_counter(
        &mut self,
        slot: Slot,
        image: &ValidatedImage,
    ) -> Result<(), Error<F::Error>> {
        self.check_counter(image)?;
        if self.counter.attached() {
            let value = image
                .security_counter()
                .ok_or(ImageError::MissingSecurityCounter)?;
            if value > self.counter_floor.unwrap_or(0) {
                // A burn cannot be undone. If one glitch had made validation
                // accept an unsigned image with `SEC_CNT = capacity`, burning
                // it would lock out every legitimately signed image forever.
                let verdict = self.reauthenticate(slot, image);
                self.random_delay();
                verdict.require();
                self.counter
                    .advance(value)
                    .map_err(Error::SecurityCounter)?;
                let actual = self
                    .counter
                    .read()
                    .map_err(Error::SecurityCounter)?
                    .expect("attached counter");
                if actual < value {
                    return Err(Error::SecurityCounter(SecurityCounterError::NotPersisted));
                }
                if actual > self.counter.capacity().expect("attached counter") {
                    return Err(Error::SecurityCounter(SecurityCounterError::InvalidState));
                }
                self.counter_floor = Some(actual);
            }
        }
        Ok(())
    }

    /// Authenticate `slot` again and compare with `image`; see
    /// [`reauthenticate`](crate::reauthenticate). The counter floor and
    /// policy are re-applied exactly as for the first validation.
    pub(crate) fn reauthenticate(&mut self, slot: Slot, image: &ValidatedImage) -> Fih {
        let again = self.validate(slot);
        Fih::ok(&again) & again.map_or(fih::FAIL, |again| image.agrees_with(&again))
    }

    fn random_delay(&mut self) {
        if let Some(word) = self.entropy.random_word() {
            fih::random_delay(word);
        }
    }

    /// Largest image that fits in a slot.
    pub fn max_image_size(&self) -> u32 {
        self.slot_trailer.max_image_size()
    }

    /// Fingerprint of the exact key ring and image policy used by this
    /// bootloader. Platforms publish this in the boot handoff so the
    /// application updater can reject configuration drift.
    pub fn security_config_fingerprint(&self) -> SecurityConfigFingerprint {
        fingerprint_security_config(self.keys, &self.config.policy)
    }

    /// Give back the flash areas.
    pub fn release(self) -> (F, F, Option<F>) {
        (self.primary, self.secondary, self.scratch)
    }

    pub(crate) fn slot(&mut self, slot: Slot) -> &mut F {
        match slot {
            Slot::Primary => &mut self.primary,
            Slot::Secondary => &mut self.secondary,
        }
    }

    pub(crate) fn state(&mut self, slot: Slot) -> Result<SwapState, Error<F::Error>> {
        let t = self.slot_trailer;
        t.read_state(self.slot(slot))
    }

    pub(crate) fn header(&mut self, slot: Slot) -> Result<Option<ImageHeader>, Error<F::Error>> {
        ImageHeader::read(self.slot(slot))
    }

    /// Authenticate the image in a slot against the key ring and policy.
    pub(crate) fn validate(&mut self, slot: Slot) -> Result<ValidatedImage, Error<F::Error>> {
        let max = self.slot_trailer.max_image_size();
        let keys = self.keys;
        let mut policy = self.config.policy;
        if let Some(floor) = self.counter_floor {
            policy.min_security_counter = Some(floor.max(policy.min_security_counter.unwrap_or(0)));
        }
        let image = validate_image(self.slot(slot), keys, &policy, max)?;
        self.check_counter(&image)?;
        Ok(image)
    }

    pub(crate) fn validate_for_boot(
        &mut self,
        stored: Slot,
        target: Slot,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<ValidatedImage, Error<F::Error>> {
        let image = self.validate(stored)?;
        validator.validate(self.slot(stored), stored, target, &image)?;
        Ok(image)
    }

    /// Erase a slot: trailer first (back to front) so an interrupted erase
    /// cannot leave a pending upgrade behind, then the image area.
    pub(crate) fn scramble_slot(&mut self, slot: Slot) -> Result<(), Error<F::Error>> {
        warn!("erasing {:?} slot", slot);
        let t = self.slot_trailer;
        let f = self.slot(slot);
        t.scramble(f)?;
        flash::erase(f, 0, t.first_sector_off())
    }

    pub(crate) fn scramble_trailer(&mut self, slot: Slot) -> Result<(), Error<F::Error>> {
        let t = self.slot_trailer;
        t.scramble(self.slot(slot))
    }

    /// Validate the secondary image as an upgrade candidate
    /// (`boot_validate_slot` for the secondary slot). Invalid images are
    /// erased so they are not retried on every boot. Returns `None` if there
    /// is nothing to install.
    pub(crate) fn check_upgrade_candidate(
        &mut self,
        kind: CandidateKind,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<Option<ValidatedImage>, Error<F::Error>> {
        match self.validate_for_boot(Slot::Secondary, Slot::Primary, validator) {
            Ok(img) => {
                // Upgrades and bootstraps are compared with the primary
                // header in every mode; swap-mode reverts are exempt (see
                // `CandidateKind::checks_downgrade`).
                if self.config.downgrade_prevention
                    && kind.checks_downgrade(self.config.mode)
                    && let Some(primary) = self.header(Slot::Primary)?
                    && img.header().version.semver_cmp(&primary.version).is_lt()
                {
                    error!(
                        "rejecting downgrade {} -> {}",
                        primary.version,
                        img.header().version
                    );
                    // A bootstrap candidate may be the last authentic image on
                    // the device. Refusing to boot is recoverable; erasing it
                    // is not.
                    if !matches!(kind, CandidateKind::Bootstrap) {
                        self.scramble_slot(Slot::Secondary)?;
                    }
                    return Ok(None);
                }
                Ok(Some(img))
            }
            Err(Error::Image(ImageError::NoImage)) => {
                // A trailer without an image (e.g. an interrupted erase):
                // drop it so it cannot block future updates.
                self.scramble_trailer(Slot::Secondary)?;
                Ok(None)
            }
            Err(Error::Image(ImageError::NonBootable)) => {
                // Upstream treats this like an erased header.
                self.scramble_trailer(Slot::Secondary)?;
                Ok(None)
            }
            Err(Error::Image(e)) => {
                error!("image in the secondary slot is not valid: {:?}", e);
                self.scramble_slot(Slot::Secondary)?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the primary slot holds no acceptable image, established by
    /// two independent validations.
    ///
    /// Callers act on `true` irreversibly: the secondary image is copied over
    /// the primary one (bootstrap), or copied instead of swapped so that the
    /// old image is lost. A single fault that makes a valid primary image look
    /// invalid must not cause that, so a rejection is repeated, and the second
    /// verdict is consumed through [`Fih::require`] behind a random delay.
    ///
    /// If the two runs disagree, the primary slot is treated as valid: nothing
    /// is overwritten, and the handoff validation decides whether it boots.
    /// Flash errors are returned, never treated as a rejection.
    pub(crate) fn primary_rejection_confirmed(
        &mut self,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<bool, Error<F::Error>> {
        match self.validate_for_boot(Slot::Primary, Slot::Primary, validator) {
            Ok(_) => return Ok(false),
            Err(Error::Image(_)) => {}
            Err(e) => return Err(e),
        }
        let accepted: u32 = match self.validate_for_boot(Slot::Primary, Slot::Primary, validator) {
            Ok(_) => 1,
            Err(Error::Image(_)) => 0,
            Err(e) => return Err(e),
        };
        if black_box(accepted) != 0 {
            warn!("primary slot validation is inconsistent; not replacing it");
            return Ok(false);
        }
        // A skipped branch above must not reach the overwrite. `black_box`
        // stops the compiler from using that branch to fold this check away.
        self.random_delay();
        Fih::from_diff(black_box(accepted)).require();
        Ok(true)
    }

    /// `MCUBOOT_BOOTSTRAP` in `boot_prepare_image_for_update`: when nothing
    /// is pending and the primary slot is not valid, a valid secondary image
    /// is installed as a REVERT.
    pub(crate) fn bootstrap_candidate(
        &mut self,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<Option<ValidatedImage>, Error<F::Error>> {
        if self.header(Slot::Secondary)?.is_none() {
            return Ok(None);
        }
        let image = self.check_upgrade_candidate(CandidateKind::Bootstrap, validator)?;
        if image.is_some() {
            warn!("bootstrapping primary slot from secondary slot");
        }
        Ok(image)
    }

    /// Validate the primary image before handing over to it.
    pub(crate) fn validate_primary_for_boot(
        &mut self,
        performed: SwapType,
        validator: &mut impl ImageValidator<F>,
    ) -> Result<BootDecision, Error<F::Error>> {
        match self.validate_for_boot(Slot::Primary, Slot::Primary, validator) {
            Ok(image) => {
                info!(
                    "booting primary slot: version {}, {} bytes",
                    image.header().version,
                    image.size()
                );
                Ok(BootDecision {
                    slot: Slot::Primary,
                    image,
                    performed,
                })
            }
            Err(Error::Image(e)) => {
                error!("image in the primary slot is not valid: {:?}", e);
                Err(Error::NoBootableImage)
            }
            Err(e) => Err(e),
        }
    }
}
