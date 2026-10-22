//! Host-testable board boot path: checked partitions -> authenticated image ->
//! checked Cortex-M vectors -> fresh boot-info -> fault-hardened confirmation.
//! Hardware publication/jump is last.
use embedded_storage::nor_flash::NorFlash;
use ferriboot_core::{
    BootConfig, BootDecision, Error, Fih, ImageVersion, Policy, PublicKey,
    SecurityConfigFingerprint, SecurityCounterStore, counter_permits, fih, reauthenticate,
};

use crate::{
    FlashBounds, FlashError, Layout, LayoutError, Partition, Region, SharedFlash,
    boot_info::{BootData, SharedDataError, boot_save_shared_data},
    vector, xip,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootError<E> {
    Layout(LayoutError),
    Boot(Error<E>),
    Vector(vector::VectorError),
    Handoff(SharedDataError),
}

/// A prepared image and boot context. Only `prepare` can construct this value.
/// Flash must remain unchanged from preparation until the hardware jump.
///
/// A prepared boot cannot be executed directly: [`PreparedBoot::confirm`]
/// first authenticates it a second time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedBoot {
    decision: BootDecision,
    bounds: FlashBounds,
    region: Region,
    xip_translation: u32,
    vector: u32,
    stack: u32,
    reset: u32,
    data: BootData,
    max_image_size: u32,
    policy: Policy,
    security_config: SecurityConfigFingerprint,
}

/// A [`PreparedBoot`] that passed the fault-hardened confirmation. Only
/// [`PreparedBoot::confirm`] constructs this value, and
/// [`hardware::execute`](crate::hardware::execute) accepts nothing else.
#[derive(Debug)]
pub struct ConfirmedBoot(PreparedBoot);

impl ConfirmedBoot {
    pub fn plan(&self) -> &PreparedBoot {
        &self.0
    }
}

impl PreparedBoot {
    pub fn decision(&self) -> &BootDecision {
        &self.decision
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub fn vector_address(&self) -> u32 {
        self.vector
    }

    /// Checked ATRANS1 value for the selected physical slot.
    pub fn xip_translation(&self) -> u32 {
        self.xip_translation
    }

    pub fn stack_pointer(&self) -> u32 {
        self.stack
    }

    pub fn reset_handler(&self) -> u32 {
        self.reset
    }

    pub fn shared_data(&self) -> &BootData {
        &self.data
    }

    /// The point of no return, hardened against fault injection.
    ///
    /// Authenticates the image a second time, independently of `prepare`,
    /// re-reads its vectors, re-derives the XIP translation that will be
    /// programmed, re-checks the hardware rollback floor and the key
    /// ring/policy, then waits a random delay and consumes the combined
    /// verdict with [`Fih::require`]. Any disagreement diverges through
    /// [`fih::fault`] (a panic); it never returns.
    ///
    /// Call it as the last step before
    /// [`hardware::execute`](crate::hardware::execute), after the last
    /// externally observable event (UART output). `random` should come from
    /// the TRNG.
    #[inline(never)]
    pub fn confirm<F: NorFlash, S: SecurityCounterStore, R: rand::CryptoRng>(
        self,
        flash: &SharedFlash<F>,
        keys: &[PublicKey],
        counter: &mut S,
        random: &mut R,
    ) -> ConfirmedBoot {
        let verdict = self.recheck(flash, keys, counter);
        fih::random_delay(random.next_u32());
        verdict.require();
        ConfirmedBoot(self)
    }

    fn recheck<F: NorFlash, S: SecurityCounterStore>(
        &self,
        flash: &SharedFlash<F>,
        keys: &[PublicKey],
        counter: &mut S,
    ) -> Fih {
        let image = &self.decision.image;
        // Confirm under the exact configuration the plan was prepared with.
        let config = Fih::eq(
            ferriboot_core::security_config_fingerprint(keys, &self.policy).as_bytes(),
            self.security_config.as_bytes(),
        );
        // Verify the region behind the translation `execute` will program,
        // not whatever `decision.slot` says: a flipped slot bit must not pick
        // what gets verified.
        let translation = xip::atrans1(self.bounds, self.region);
        let mapping =
            Fih::ok(&translation) & Fih::eq_u32(translation.unwrap_or(0), self.xip_translation);
        let Ok(mut partition) = Partition::new(flash, self.region) else {
            return fih::FAIL;
        };
        let authentic = reauthenticate(
            &mut partition,
            keys,
            &self.policy,
            self.max_image_size,
            image,
        );
        let mut words = [0u8; 8];
        let read = ferriboot_core::flash::read(
            &mut partition,
            u32::from(image.header().hdr_size),
            &mut words,
        );
        let stack = u32::from_le_bytes([words[0], words[1], words[2], words[3]]);
        let reset = u32::from_le_bytes([words[4], words[5], words[6], words[7]]);
        let vector = vector::validate(image.header(), self.bounds, self.region, stack, reset);
        let vectors = Fih::ok(&read)
            & Fih::eq_u32(stack, self.stack)
            & Fih::eq_u32(reset, self.reset)
            & Fih::ok(&vector)
            & Fih::eq_u32(vector.unwrap_or(0), self.vector);
        config & mapping & authentic & vectors & counter_permits(counter, image)
    }
}

/// Run the RP bootloader's exact boot path, including resumable upgrades,
/// policies, RP vector validation and the persistent hardware application
/// rollback floor. All board bootloader modes use this entry point, including
/// swap resume. This does not touch reserved SRAM.
///
/// `random` (the TRNG) delays the fault-hardened checks that guard
/// security-counter burns.
pub fn prepare_with_security_counter_store<'k, F, S, R>(
    layout: &Layout,
    flash: &SharedFlash<F>,
    keys: &'k [PublicKey],
    config: BootConfig,
    version: Option<ImageVersion>,
    store: &'k mut S,
    random: &'k mut R,
) -> Result<PreparedBoot, BootError<FlashError<F::Error>>>
where
    F: NorFlash,
    S: SecurityCounterStore,
    R: rand::CryptoRng,
{
    let bounds = layout.bounds();
    let mut loader = crate::bootloader(layout, flash, keys, config)
        .map_err(BootError::Layout)?
        .with_security_counter_store(store)
        .with_entropy(random);
    let decision = loader
        .boot_with_validator(
            &mut |partition: &mut crate::Partition<'_, F>,
                  _: ferriboot_core::Slot,
                  target: ferriboot_core::Slot,
                  image: &ferriboot_core::ValidatedImage| {
                let region = layout.region(target);
                vector::address(image.header(), bounds, region)
                    .map_err(|_| ferriboot_core::ImageError::PlatformRejected)?;
                let mut words = [0; 8];
                ferriboot_core::flash::read(
                    partition,
                    u32::from(image.header().hdr_size),
                    &mut words,
                )?;
                let stack = u32::from_le_bytes(words[..4].try_into().unwrap());
                let reset = u32::from_le_bytes(words[4..].try_into().unwrap());
                vector::validate(image.header(), bounds, region, stack, reset)
                    .map_err(|_| ferriboot_core::ImageError::PlatformRejected)?;
                Ok(())
            },
        )
        .map_err(BootError::Boot)?;
    let max_image_size = loader.max_image_size();
    let region = layout.region(decision.slot);
    let xip_translation = xip::atrans1(bounds, region).map_err(BootError::Layout)?;
    vector::address(decision.image.header(), bounds, region).map_err(BootError::Vector)?;
    let mut partition = crate::Partition::new(flash, region).map_err(BootError::Layout)?;
    let mut words = [0; 8];
    ferriboot_core::flash::read(
        &mut partition,
        u32::from(decision.image.header().hdr_size),
        &mut words,
    )
    .map_err(BootError::Boot)?;
    let stack = u32::from_le_bytes(words[..4].try_into().unwrap());
    let reset = u32::from_le_bytes(words[4..].try_into().unwrap());
    let vector = vector::validate(decision.image.header(), bounds, region, stack, reset)
        .map_err(BootError::Vector)?;
    let security_config = loader.security_config_fingerprint();
    // The loader owns the exclusive mutable store handle until dropped.
    // Read the post-selection hardware floor through that handle.
    let rollback_floor = loader
        .hardware_security_counter()
        .map_err(|e| BootError::Boot(Error::SecurityCounter(e)))?
        .ok_or(BootError::Boot(Error::SecurityCounter(
            ferriboot_core::SecurityCounterError::Read,
        )))?;
    let data = boot_save_shared_data(
        config.mode,
        decision.slot,
        max_image_size,
        version,
        Some(rollback_floor),
    )
    .map_err(BootError::Handoff)?;
    Ok(PreparedBoot {
        decision,
        bounds,
        region,
        xip_translation,
        vector,
        stack,
        reset,
        data,
        max_image_size,
        policy: config.policy,
        security_config,
    })
}
