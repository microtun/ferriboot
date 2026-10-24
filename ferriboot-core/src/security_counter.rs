//! Monotonic, non-flash rollback floor for authenticated application images.

/// Counter failures are fatal to boot; they must never become image rejection
/// (which could otherwise select another slot or erase a recoverable image).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityCounterError {
    Read,
    Write,
    InvalidState,
    Exhausted,
    NotPersisted,
    Busy,
    Locked,
}

/// Hardware-backed monotonic counter. Implementations must survive resets and
/// power loss, never decrease, check hardware errors and verify every burn.
/// Reserve the backing fuses exclusively for this application security epoch.
/// `advance(n)` means at least `n`, and is idempotent for `n <= get()`.
pub trait SecurityCounterStore {
    fn get(&mut self) -> Result<u32, SecurityCounterError>;
    fn advance(&mut self, value: u32) -> Result<(), SecurityCounterError>;
    /// Largest representable epoch. Reject unrepresentable images even on trial.
    fn capacity(&self) -> u32;
}

/// Fault-hardened handoff check of `image` against the hardware floor.
///
/// Reads the floor twice and requires both reads to agree, so one corrupted
/// read cannot lower it, then checks `floor <= image counter <= capacity`
/// without data-dependent branches. Any store error is a failure. An image
/// without a protected `SEC_CNT` TLV fails: a store is only attached when
/// the policy requires one.
pub fn counter_permits<S: SecurityCounterStore>(
    store: &mut S,
    image: &crate::ValidatedImage,
) -> crate::Fih {
    use crate::Fih;
    let first = store.get();
    let second = store.get();
    let floor = first.unwrap_or(u32::MAX);
    let value = image.security_counter();
    let have = value.unwrap_or(0);
    Fih::ok(&first)
        & Fih::ok(&second)
        & Fih::eq_u32(floor, second.unwrap_or(0))
        & Fih::eq_u32(u32::from(value.is_some()), 1)
        & Fih::ge(have, floor)
        & Fih::ge(store.capacity(), have)
}

/// Decode a raw OTP thermometer word (low bits burn first). Reject holes and
/// bits outside the allocation instead of silently weakening the floor.
pub fn thermometer_value(word: u32, bits: u32) -> Result<u32, SecurityCounterError> {
    if bits == 0 || bits > 32 {
        return Err(SecurityCounterError::InvalidState);
    }
    let mask = u32::MAX >> (32 - bits);
    if word & !mask != 0 || word & word.wrapping_add(1) != 0 {
        return Err(SecurityCounterError::InvalidState);
    }
    Ok(word.count_ones())
}

/// Decode a thermometer spanning consecutive raw OTP rows. Once a row is
/// partially used, every later row must still be empty.
pub fn thermometer_rows_value(rows: &[u32], bits: u32) -> Result<u32, SecurityCounterError> {
    let mut value = 0u32;
    let mut ended = false;
    for &row in rows {
        let count = thermometer_value(row, bits)?;
        if ended && count != 0 {
            return Err(SecurityCounterError::InvalidState);
        }
        value = value
            .checked_add(count)
            .ok_or(SecurityCounterError::InvalidState)?;
        ended |= count < bits;
    }
    Ok(value)
}

/// Decode raw OTP bits stored in groups of three redundant 24-bit rows.
/// Each logical bit is the majority of its three copies. Validate the voted
/// thermometer, not individual rows: one stuck/misread bit in one copy is
/// tolerable, but two matching faults or a gap in the voted prefix are not.
/// `rows` must consist of complete triples.
pub fn majority_thermometer_rows_value(
    rows: &[u32],
    bits: u32,
) -> Result<u32, SecurityCounterError> {
    if rows.is_empty() || !rows.len().is_multiple_of(3) || bits == 0 || bits > 24 {
        return Err(SecurityCounterError::InvalidState);
    }
    let mask = (1u32 << bits) - 1;
    let mut value = 0u32;
    let mut ended = false;
    for triple in rows.as_chunks::<3>().0 {
        if triple.iter().any(|&row| row & !mask != 0) {
            return Err(SecurityCounterError::InvalidState);
        }
        let voted = (triple[0] & triple[1]) | (triple[0] & triple[2]) | (triple[1] & triple[2]);
        let count = thermometer_value(voted, bits)?;
        if ended && count != 0 {
            return Err(SecurityCounterError::InvalidState);
        }
        value = value
            .checked_add(count)
            .ok_or(SecurityCounterError::InvalidState)?;
        ended |= count < bits;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_otp_page_boundaries_and_gaps() {
        for value in 0..=1536 {
            let mut rows = [0u32; 64];
            for n in 0..value {
                rows[n / 24] |= 1 << (n % 24);
            }
            assert_eq!(thermometer_rows_value(&rows, 24), Ok(value as u32));
        }
        for rows in [[0, 1], [1, 1], [0xff_ffff, 2], [0x100_0000, 0]] {
            assert_eq!(
                thermometer_rows_value(&rows, 24),
                Err(SecurityCounterError::InvalidState)
            );
        }
    }
    #[test]
    fn thermometer_boundaries_and_corruption() {
        for bits in [16, 24, 32] {
            for n in 0..=bits {
                let word = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
                assert_eq!(thermometer_value(word, bits), Ok(n));
            }
        }
        for (word, bits) in [(2, 24), (5, 24), (1 << 24, 24), (0, 0), (0, 33)] {
            assert_eq!(
                thermometer_value(word, bits),
                Err(SecurityCounterError::InvalidState)
            );
        }
    }
    #[test]
    fn triple_vote_boundaries_and_single_copy_faults() {
        // 21 triples (63 rows): 504 epochs; the 64th row is unused.
        const CAPACITY: u32 = 504;
        for value in 0..=CAPACITY {
            let mut rows = [0u32; 64];
            for n in 0..value {
                let first = ((n / 24) * 3) as usize;
                for row in &mut rows[first..first + 3] {
                    *row |= 1 << (n % 24);
                }
            }
            assert_eq!(majority_thermometer_rows_value(&rows[..63], 24), Ok(value));
            if value != 0 {
                // A missing bit in one replica does not change the vote.
                let first = (((value - 1) / 24) * 3) as usize;
                rows[first] ^= 1 << ((value - 1) % 24);
                assert_eq!(majority_thermometer_rows_value(&rows[..63], 24), Ok(value));
            }
            if value < CAPACITY {
                // A spurious bit in one replica does not change the vote.
                let first = ((value / 24) * 3) as usize;
                rows[first + 1] |= 1 << (value % 24);
                assert_eq!(majority_thermometer_rows_value(&rows[..63], 24), Ok(value));
            }
        }
    }

    #[test]
    fn triple_vote_invalid_states() {
        assert_eq!(majority_thermometer_rows_value(&[0u32; 63], 24), Ok(0));
        let mut rows = [0u32; 63];
        rows[0] = 0b101;
        rows[1] = 0b101;
        assert_eq!(
            majority_thermometer_rows_value(&rows, 24),
            Err(SecurityCounterError::InvalidState)
        );
        rows[0] = 0;
        rows[1] = 0;
        rows[2] = 1 << 24;
        assert_eq!(
            majority_thermometer_rows_value(&rows, 24),
            Err(SecurityCounterError::InvalidState)
        );
        rows[2] = 0;
        rows[3] = 1;
        rows[4] = 1;
        assert_eq!(
            majority_thermometer_rows_value(&rows, 24),
            Err(SecurityCounterError::InvalidState)
        );
        assert_eq!(
            majority_thermometer_rows_value(&[], 24),
            Err(SecurityCounterError::InvalidState)
        );
        assert_eq!(
            majority_thermometer_rows_value(&[0, 0], 24),
            Err(SecurityCounterError::InvalidState)
        );
    }

    #[test]
    fn interrupted_triple_writes_keep_majority_monotonic_and_resume() {
        for old in [0u32, 1, 23, 24, 25, 502, 503] {
            let mut rows = [0u32; 63];
            for n in 0..old {
                for copy in 0..3 {
                    rows[(n / 24 * 3) as usize + copy] |= 1 << (n % 24);
                }
            }
            let first = (old / 24 * 3) as usize;
            let bit = 1 << (old % 24);
            for burned_copies in 0..=3 {
                let mut interrupted = rows;
                for copy in 0..burned_copies {
                    interrupted[first + copy] |= bit;
                }
                let voted = old + u32::from(burned_copies >= 2);
                assert_eq!(majority_thermometer_rows_value(&interrupted, 24), Ok(voted));
                for copy in 0..3 {
                    interrupted[first + copy] |= bit;
                }
                assert_eq!(
                    majority_thermometer_rows_value(&interrupted, 24),
                    Ok(old + 1)
                );
                assert!(
                    interrupted[first..first + 3]
                        .iter()
                        .all(|row| row & bit != 0)
                );
            }
        }
    }
}
