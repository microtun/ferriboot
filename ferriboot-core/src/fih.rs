//! Fault-injection hardening (FIH).
//!
//! [`ValidatedImage`](crate::ValidatedImage) and the board handoff types prove
//! that the *code* took the authenticated path. A voltage, clock or EM glitch
//! makes the *CPU* lie instead: one skipped branch or corrupted register can
//! turn a rejected image into a boot. Ferriboot therefore re-checks, with the
//! primitives below, every step that cannot be undone:
//!
//! * the jump into the application (each board's `confirm` step), and
//! * burning the hardware security counter
//!   ([`BootLoader`](crate::BootLoader) re-authenticates before every burn).
//!
//! The rules:
//!
//! * A verdict is a [`Fih`], never a `bool`. PASS is a 32-bit pattern that a
//!   zeroed register, erased flash (`0xffff_ffff`) or a few bit flips cannot
//!   produce; every other value is a failure.
//! * Comparisons are computed without data-dependent branches, so there is no
//!   single "equal?" branch to flip.
//! * Only [`Fih::require`] consumes a verdict. It checks it three times,
//!   reloading the verdict and its reference constant through [`black_box`]
//!   each time to discourage LLVM from merging the checks. `black_box` is
//!   best effort and does not guarantee the resulting machine-code structure.
//! * Failure diverges through [`fault`], so callers have no `Err` branch to
//!   skip.
//!
//! Wrong *rejections* are not hardened: a glitch that makes a valid image
//! look invalid is a denial of service, and fails closed.
//!
//! This raises the cost of an attack; it does not prove resistance. An
//! instruction-skip campaign against the real binary is what measures it.

use core::hint::black_box;

const PASS_BITS: u32 = 0x3CA5_5AC3;
const FAIL_BITS: u32 = !PASS_BITS;

/// A fault-hardened verdict.
///
/// Deliberately not `PartialEq`: `==` would reintroduce a single glitchable
/// comparison. Combine verdicts with `&` and consume them with
/// [`Fih::require`].
#[must_use = "consume a verdict with `require`"]
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Fih(u32);

pub const PASS: Fih = Fih(PASS_BITS);
pub const FAIL: Fih = Fih(FAIL_BITS);

impl Fih {
    /// PASS iff `diff == 0`, computed without a branch.
    #[inline(always)]
    pub fn from_diff(diff: u32) -> Self {
        // `d | -d` has its top bit set iff d != 0.
        let nonzero = (diff | diff.wrapping_neg()) >> 31;
        let mask = 0u32.wrapping_sub(nonzero); // 0 or all ones
        Self(PASS_BITS ^ mask) // PASS or FAIL (= !PASS)
    }

    /// Constant-time equality of two fixed-size buffers. A loop cut short by
    /// a fault leaves `seen != N`, which is folded into the result instead of
    /// silently comparing a prefix.
    #[inline(never)]
    pub fn eq<const N: usize>(a: &[u8; N], b: &[u8; N]) -> Self {
        let mut diff = 0u32;
        let mut seen = 0u32;
        for (x, y) in a.iter().zip(b) {
            diff |= u32::from(x ^ y);
            seen = black_box(seen.wrapping_add(1));
        }
        Self::from_diff(black_box(diff | (seen ^ N as u32)))
    }

    #[inline(always)]
    pub fn eq_u32(a: u32, b: u32) -> Self {
        Self::from_diff(a ^ b)
    }

    /// PASS iff `a >= b`.
    #[inline(always)]
    pub fn ge(a: u32, b: u32) -> Self {
        Self::from_diff(u32::from(a < b))
    }

    /// PASS iff `result` is `Ok`.
    #[inline(always)]
    pub fn ok<T, E>(result: &Result<T, E>) -> Self {
        Self::from_diff(u32::from(result.is_err()))
    }

    /// Continue only if this verdict is exactly PASS; otherwise [`fault`].
    ///
    /// Three source-level checks, each reloading the verdict *and* its
    /// reference constant through [`black_box`]. This is intended to leave
    /// redundant checks in the binary, but their exact form is not guaranteed
    /// after optimization.
    #[inline(always)]
    pub fn require(self) {
        let v = self.0;
        if black_box(v) != black_box(PASS_BITS) {
            fault()
        }
        if black_box(v) ^ black_box(PASS_BITS) != 0 {
            fault()
        }
        if !black_box(v) != black_box(FAIL_BITS) {
            fault()
        }
    }
}

/// Both verdicts must be exactly PASS. Plain bitwise AND would be wrong: a
/// register glitched to all ones, ANDed with PASS, is PASS.
impl core::ops::BitAnd for Fih {
    type Output = Self;

    #[inline(always)]
    fn bitand(self, other: Self) -> Self {
        Self::from_diff((self.0 ^ PASS_BITS) | (other.0 ^ PASS_BITS))
    }
}

/// A fault was detected or a verdict was corrupted. Never returns: every
/// board panic handler invalidates the boot handoff and resets or halts.
///
/// The loop is intended to repeat an out-of-line `trap` call if one call is
/// skipped. (`trap` does not return `!` so the loop back edge can be emitted.)
/// `track_caller` gives each site its own location, discouraging the compiler
/// from merging separate checks in [`Fih::require`]. The exact instructions
/// remain compiler-dependent.
#[inline(always)]
#[track_caller]
pub fn fault() -> ! {
    loop {
        trap();
    }
}

/// The out-of-line half of [`fault`]: panics. Its signature hides that it
/// never returns, so callers keep their branch back to the call.
#[cold]
#[inline(never)]
#[track_caller]
fn trap() {
    if black_box(true) {
        panic!("fault injection detected")
    }
}

/// Number of distinct delay lengths produced by [`random_delay`].
pub const DELAY_STEPS: u32 = 4096;

/// Spin for `random % DELAY_STEPS` iterations.
///
/// Call it between the last externally observable event (UART output, flash
/// reads) and [`Fih::require`], so a glitch cannot be timed from that event.
/// Feed it from a hardware TRNG: a predictable input makes this a fixed delay.
/// It multiplies the attempts an attacker needs; it does not stop one who can
/// resynchronise on the power or EM trace.
#[inline(never)]
pub fn random_delay(random: u32) {
    let mut remaining = black_box(random % DELAY_STEPS);
    while black_box(remaining) != 0 {
        remaining -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_pass(f: Fih) -> bool {
        f.0 == PASS_BITS
    }

    #[test]
    fn encodings_are_far_apart() {
        assert_eq!((PASS_BITS ^ FAIL_BITS).count_ones(), 32);
        for v in [0, u32::MAX, FAIL_BITS] {
            assert!(!is_pass(Fih(v)));
        }
    }

    #[test]
    fn any_nonzero_diff_fails() {
        assert!(is_pass(Fih::from_diff(0)));
        for bit in 0..32 {
            assert_eq!(Fih::from_diff(1 << bit).0, FAIL_BITS);
        }
    }

    #[test]
    fn and_needs_exact_pass_on_both_sides() {
        assert!(is_pass(PASS & PASS));
        for other in [
            FAIL,
            Fih(0),
            Fih(u32::MAX),
            Fih(PASS_BITS ^ 1),
            Fih(PASS_BITS | 0x8000),
        ] {
            assert!(!is_pass(PASS & other));
            assert!(!is_pass(other & PASS));
        }
    }

    #[test]
    fn comparisons() {
        assert!(is_pass(Fih::eq(&[1, 2, 3], &[1, 2, 3])));
        assert!(!is_pass(Fih::eq(&[1, 2, 3], &[1, 2, 4])));
        assert!(is_pass(Fih::ge(5, 5)));
        assert!(!is_pass(Fih::ge(4, 5)));
        assert!(is_pass(Fih::ok::<(), ()>(&Ok(()))));
        assert!(!is_pass(Fih::ok::<(), ()>(&Err(()))));
    }

    #[test]
    fn delay_terminates() {
        random_delay(u32::MAX);
        random_delay(0);
    }

    #[test]
    #[should_panic(expected = "fault injection detected")]
    fn require_faults_on_fail() {
        FAIL.require();
    }

    #[test]
    #[should_panic(expected = "fault injection detected")]
    fn require_faults_on_corruption() {
        Fih(PASS_BITS ^ 0x0100).require();
    }
}
