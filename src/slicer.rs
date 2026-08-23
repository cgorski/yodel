//! Bit-clock recovery: a digital phase-locked loop (PLL) bit slicer.
//!
//! # Loop design
//!
//! The transmitter clocks bits at a nominal baud rate, but the receiver
//! must find *where* in its own sample stream each bit cell sits. The
//! slicer keeps a fixed-point **phase accumulator** — a `u32` whose full
//! range `0..2^32` represents one bit period. Every sample it advances by
//! `2^32 · baud / sample_rate`; when the accumulator wraps, one bit cell
//! has elapsed and the current sign of the discriminator metric is emitted
//! as the bit decision (positive metric ⇒ mark ⇒ [`Bit::One`]).
//!
//! The per-sample step is computed once at construction with rounding
//! (`round(baud · 2^32 / sample_rate)`), so the residual frequency error is
//! below half a phase LSB per sample — at most `spb/2` LSBs of 2³² per bit,
//! about 5·10⁻⁹ of a bit period per bit at 40 samples per bit, negligible
//! over any realistic frame.
//!
//! # Phase nudge (lock-adaptive loop gain)
//!
//! Decisions are taken when the accumulator wraps (phase ≡ 0); for those
//! decisions to land in the **middle** of each bit cell — the point
//! farthest from both edges — the cell *edges* must sit half a period
//! away, at phase ≡ 2³¹. To find the edges, the slicer watches the
//! discriminator metric for **zero crossings**: a sign change marks a tone
//! transition, i.e. a bit-cell boundary at the transmitter. At that
//! instant the accumulator should read `2^31`; the signed offset from
//! that target (`(phase ⊕ 2³¹) as i32`) is the timing error, and the
//! slicer subtracts a fraction of it:
//!
//! ```text
//! phase -= ((phase ^ 0x8000_0000) as i32) >> gain_shift   // on crossings
//! ```
//!
//! The loop gain adapts to a two-state **lock detector**. While
//! *searching* (start-up, or after the clock has evidently slipped) the
//! shift is small (gain 1/2): each crossing halves the residual error, so
//! an alternating preamble acquires lock within a handful of transitions.
//! Once transitions have landed consistently **near the expected phase**
//! (a saturating hysteresis counter of consecutive in-window crossings
//! reaches its threshold) the loop switches to a much smaller gain
//! (1/8): mid-frame, noise- or fade-jittered crossings then barely
//! disturb the sampling instant, so the clock coasts through flutter
//! fades and noisy tails on its own frequency accuracy. Several
//! consecutive far-from-expected crossings drain the counter and drop the
//! loop back to searching gain for fast re-acquisition.
//!
//! The slicer emits **raw tone decisions** — one bit per bit cell, no NRZI
//! or other line decoding.

use crate::error::ConfigError;
use crate::types::{BaudRate, Bit, SampleRate};

/// Digital-PLL bit slicer: soft metrics in, clocked [`Bit`]s out.
///
/// See the module docs for the loop design. Feed it the discriminator
/// metric once per sample via [`Slicer::push`]; it returns `Some(Bit)`
/// whenever a bit cell completes.
///
/// # One decision per bit cell
///
/// At 48 kHz / 1200 Bd a bit cell is exactly 40 samples, so pushing a
/// synthetic metric stream of `n` bit cells yields `n` decisions (±1
/// for the startup phase), each the metric's sign at the sampling
/// instant — positive ⇒ mark ⇒ [`Bit::One`]:
///
/// ```
/// use warble::{BaudRate, Bit, SampleRate, Slicer};
///
/// let mut slicer = Slicer::new(SampleRate::new(48_000)?, BaudRate::new(1_200)?)?;
/// let mut decisions = [Bit::Zero; 20];
/// let mut n = 0;
/// // 16 alternating bit cells of 40 samples each: +1000 = mark, -1000 = space.
/// for cell in 0..16 {
///     let metric = if cell % 2 == 0 { 1_000 } else { -1_000 };
///     for _ in 0..40 {
///         if let Some(bit) = slicer.push(metric) {
///             decisions[n] = bit;
///             n += 1;
///         }
///     }
/// }
/// assert!((15..=17).contains(&n), "one decision per cell, got {n}");
/// // Once the zero crossings have centered the clock, decisions alternate.
/// for pair in decisions[4..n].windows(2) {
///     assert_ne!(pair[0], pair[1]);
/// }
/// # Ok::<(), warble::ConfigError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Slicer {
    /// Bit-cell phase; full u32 range = one bit period.
    phase: u32,
    /// Per-sample phase step: `round(baud · 2^32 / sample_rate)`.
    step: u32,
    /// Sign of the previous metric (`true` = non-negative), for crossing
    /// detection.
    last_positive: bool,
    /// Whether any metric has been seen yet (suppresses a phantom crossing
    /// on the first sample).
    primed: bool,
    /// Lock hysteresis: saturating count of consecutive in-window
    /// transitions (out-of-window ones drain it). `>= LOCK_THRESHOLD`
    /// means locked (slow gain).
    lock: u8,
    /// Loop-gain shift used once locked (higher = more inertia).
    lock_shift: u32,
}

/// Loop-gain shift while searching: correct half the phase error.
const SEARCH_SHIFT: u32 = 1;
/// Default loop-gain shift while locked: correct 1/8 of the phase error.
const LOCK_SHIFT: u32 = 3;
/// Consecutive in-window transitions required to declare lock.
const LOCK_THRESHOLD: u8 = 7;
/// Saturation ceiling for the lock counter (out-of-window transitions
/// must drain this many steps below the threshold to unlock).
const LOCK_MAX: u8 = 12;
/// A transition is "in window" when its absolute phase error is below a
/// quarter of a bit period (|offset| < 2^30).
const WINDOW: i32 = 1 << 30;

impl Slicer {
    /// Builds a slicer for the given sample and baud rates.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when there are fewer
    /// than 2 samples per bit — the loop cannot place a sampling point
    /// between edges it never sees.
    pub fn new(sample_rate: SampleRate, baud: BaudRate) -> Result<Self, ConfigError> {
        let sr = sample_rate.hz();
        let bd = baud.bps();
        if sr / bd < 2 {
            return Err(ConfigError::BaudExceedsSampleRate {
                baud: bd,
                sample_rate: sr,
            });
        }
        // round(bd · 2^32 / sr): the +sr/2 makes truncation a rounding,
        // bounding drift to < 0.5 LSB per sample.
        let step = ((((bd as u64) << 32) + (sr as u64) / 2) / (sr as u64)) as u32;
        Ok(Self {
            phase: 0,
            step,
            last_positive: true,
            primed: false,
            lock: 0,
            lock_shift: LOCK_SHIFT,
        })
    }

    /// Overrides the locked-state loop-gain shift (higher = more inertia:
    /// the clock coasts harder through mid-frame fades but re-centers more
    /// slowly). Currently unused: the receiver bank diversifies chain
    /// timing with [`Slicer::set_initial_phase`] alone and leaves every
    /// chain at the default `LOCK_SHIFT`.
    #[cfg(feature = "tnc")]
    #[allow(dead_code)]
    pub(crate) fn set_lock_shift(&mut self, shift: u32) {
        self.lock_shift = shift.min(15);
    }

    /// Offsets the initial bit-clock phase (full `u32` range = one bit
    /// period), staggering where in the bit cell chains start sampling
    /// before any transitions have been seen.
    #[cfg(feature = "tnc")]
    pub(crate) fn set_initial_phase(&mut self, phase: u32) {
        self.phase = phase;
    }

    /// Advances the loop by one sample's metric.
    ///
    /// Returns `Some(Bit)` when a bit cell completes: [`Bit::One`] when the
    /// metric is positive (mark tone), [`Bit::Zero`] otherwise.
    pub fn push(&mut self, metric: i32) -> Option<Bit> {
        let positive = metric >= 0;
        if self.primed && positive != self.last_positive {
            // Tone transition: the bit-cell edge is "now", i.e. phase
            // should read 2^31 (half a period before the sampling wrap).
            // Remove a lock-dependent fraction of the signed offset from
            // that target (module docs).
            let offset = (self.phase ^ (1 << 31)) as i32;
            let shift = if self.lock >= LOCK_THRESHOLD {
                self.lock_shift
            } else {
                SEARCH_SHIFT
            };
            self.phase = self.phase.wrapping_sub((offset >> shift) as u32);
            // Hysteresis: near-expected transitions build confidence,
            // wild ones drain it.
            if offset.unsigned_abs() < WINDOW as u32 {
                self.lock = (self.lock + 1).min(LOCK_MAX);
            } else {
                self.lock = self.lock.saturating_sub(4);
            }
        }
        self.last_positive = positive;
        self.primed = true;

        let (next, wrapped) = self.phase.overflowing_add(self.step);
        self.phase = next;
        if wrapped {
            Some(if positive { Bit::One } else { Bit::Zero })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn slicer(sr: u32, bd: u32) -> Slicer {
        Slicer::new(SampleRate::new(sr).unwrap(), BaudRate::new(bd).unwrap()).unwrap()
    }

    /// Feeds a synthetic metric stream of `bits` at `spb` samples per bit
    /// and collects the sliced bits.
    fn run(mut s: Slicer, bits: &[Bit], spb: usize) -> Vec<Bit> {
        run_ref(&mut s, bits, spb)
    }

    /// Like [`run`] but leaves the slicer inspectable afterwards.
    fn run_ref(s: &mut Slicer, bits: &[Bit], spb: usize) -> Vec<Bit> {
        let mut out = Vec::new();
        for &b in bits {
            let metric = match b {
                Bit::One => 1_000,
                Bit::Zero => -1_000,
            };
            for _ in 0..spb {
                if let Some(bit) = s.push(metric) {
                    out.push(bit);
                }
            }
        }
        out
    }

    #[test]
    fn construction_rejects_low_sample_rate() {
        let err = Slicer::new(
            SampleRate::new(8_000).unwrap(),
            BaudRate::new(4_800).unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            ConfigError::BaudExceedsSampleRate {
                baud: 4_800,
                sample_rate: 8_000
            }
        );
    }

    #[test]
    fn emits_one_bit_per_cell_when_locked() {
        let bits: Vec<Bit> = (0..64)
            .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
            .collect();
        let out = run(slicer(48_000, 1_200), &bits, 40);
        // One decision per cell (±1 for startup phase).
        assert!(out.len() >= 63 && out.len() <= 65, "got {}", out.len());
        // After a few transitions the pattern alternates cleanly.
        let tail = &out[8..];
        for pair in tail.windows(2) {
            assert_ne!(pair[0], pair[1], "lost alternation: {out:?}");
        }
    }

    #[test]
    fn locks_onto_clean_alternating_pattern_all_rates() {
        for sr in [8_000u32, 11_025, 22_050, 44_100, 48_000] {
            let spb = (sr / 1_200) as usize;
            let bits: Vec<Bit> = (0..64)
                .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
                .collect();
            let out = run(slicer(sr, 1_200), &bits, spb);
            let tail = &out[out.len().saturating_sub(48)..];
            for pair in tail.windows(2) {
                assert_ne!(pair[0], pair[1], "rate {sr}: {out:?}");
            }
        }
    }

    #[test]
    fn recovers_from_deliberate_phase_error() {
        // Push the phase far off, then verify transitions re-center it:
        // long constant runs after a re-lock preamble come out exact.
        let mut s = slicer(48_000, 1_200);
        s.phase = 0x4000_0000; // quarter period off
        let mut bits: Vec<Bit> = (0..16)
            .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
            .collect();
        bits.extend(core::iter::repeat_n(Bit::One, 20));
        bits.extend(core::iter::repeat_n(Bit::Zero, 20));
        let out = run(s, &bits, 40);
        let n = out.len();
        // Final 36 decisions: 18+ ones then 18+ zeros around one boundary.
        let tail = &out[n - 36..];
        assert!(tail[..16].iter().all(|&b| b == Bit::One), "{out:?}");
        assert!(tail[20..].iter().all(|&b| b == Bit::Zero), "{out:?}");
    }

    #[test]
    fn no_phantom_crossing_on_first_sample() {
        // A stream that starts negative must not trigger a nudge on sample
        // one; verify the first decision still lands ~mid-cell (decision
        // count over N cells stays N±1).
        let bits: Vec<Bit> = core::iter::repeat_n(Bit::Zero, 32).collect();
        let out = run(slicer(48_000, 1_200), &bits, 40);
        assert!(out.len() >= 31 && out.len() <= 33);
        assert!(out.iter().all(|&b| b == Bit::Zero));
    }

    #[test]
    fn step_rounding_exact_for_even_ratio() {
        let s = slicer(48_000, 1_200);
        // 1200/48000 = 1/40 of 2^32 = 107374182.4 -> rounds to ...182
        assert_eq!(s.step, 107_374_182);
    }

    #[test]
    fn acquires_lock_on_alternating_preamble() {
        // A clean alternating pattern (flag-like) must reach locked state
        // within a handful of transitions.
        let mut s = slicer(48_000, 1_200);
        let bits: Vec<Bit> = (0..(LOCK_THRESHOLD as usize + 4))
            .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
            .collect();
        run_ref(&mut s, &bits, 40);
        assert!(s.lock >= LOCK_THRESHOLD, "lock counter = {}", s.lock);
    }

    #[test]
    fn holds_phase_through_transition_free_gap() {
        // Once locked, a long constant run (no transitions) must neither
        // unlock the loop nor move the sampling phase off its own clock:
        // the constant-run decisions stay correct and lock persists.
        let mut s = slicer(48_000, 1_200);
        let mut bits: Vec<Bit> = (0..16)
            .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
            .collect();
        bits.extend(core::iter::repeat_n(Bit::One, 64));
        let out = run_ref(&mut s, &bits, 40);
        assert!(s.lock >= LOCK_THRESHOLD, "gap unlocked the loop");
        assert!(out[out.len() - 60..].iter().all(|&b| b == Bit::One));
    }

    #[test]
    fn unlocks_after_wild_transitions_and_relocks() {
        // Force lock, then feed transitions that land far from the
        // expected phase: the hysteresis must drain and drop to searching
        // gain, and a clean preamble must re-acquire afterwards.
        let mut s = slicer(48_000, 1_200);
        let preamble: Vec<Bit> = (0..16)
            .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
            .collect();
        run_ref(&mut s, &preamble, 40);
        assert!(s.lock >= LOCK_THRESHOLD);
        // Wild transitions: alternate the metric sign every ~1/4 bit cell
        // so crossings land far outside the expected-phase window.
        let mut sign = 1;
        for _ in 0..12 {
            for _ in 0..10 {
                s.push(sign * 1_000);
            }
            sign = -sign;
        }
        assert!(s.lock < LOCK_THRESHOLD, "wild edges failed to unlock");
        // Clean preamble re-locks.
        run_ref(&mut s, &preamble, 40);
        assert!(s.lock >= LOCK_THRESHOLD, "failed to re-lock");
    }
}
