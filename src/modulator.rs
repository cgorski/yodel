//! Bell 202 AFSK modulator: bits in, PCM samples out.
//!
//! # DSP overview
//!
//! The modulator generates *continuous-phase* frequency-shift keying. A
//! single **phase accumulator** — an unsigned 32-bit integer whose full
//! range `0..2^32` represents one waveform cycle `0..2π` — is advanced once
//! per output sample by a per-tone **phase increment**:
//!
//! ```text
//! increment = round(tone_hz * 2^32 / sample_rate)
//! phase     = phase.wrapping_add(increment)   // wraps == modulo 2π
//! ```
//!
//! Switching between the mark and space tones only swaps the increment; the
//! accumulator itself is never reset, so the waveform stays continuous
//! (no clicks) across bit boundaries. The sample value is a sine of the
//! current phase, taken from a compile-time lookup table indexed by the
//! accumulator's top bits.
//!
//! # Fractional samples per bit
//!
//! At 44 100 Hz and 1200 baud each bit spans 44100 / 1200 = 36.75 samples,
//! which no integer count can represent per bit. The modulator therefore
//! keeps an integer **remainder accumulator**: each bit emits
//! `sample_rate / baud` whole samples, the division remainder is added to
//! the accumulator, and whenever the accumulator reaches `baud` one extra
//! sample is emitted. Over any run of bits the emitted total is exactly
//! `floor(bits * sample_rate / baud)` — zero drift.
//!
//! # Example
//!
//! ```
//! use warble::{Bit, Modulator, ModulatorConfig, SampleRate};
//!
//! let config = ModulatorConfig::bell_202(SampleRate::new(48_000)?)?;
//! let bits = [Bit::One, Bit::Zero, Bit::One];
//! let samples: Vec<i16> = Modulator::new(config)
//!     .i16_samples(bits.into_iter())
//!     .collect();
//! assert_eq!(samples.len(), 3 * 40); // 48000 / 1200 = 40 samples per bit
//! # Ok::<(), warble::ConfigError>(())
//! ```

use crate::error::ConfigError;
use crate::types::{BaudRate, Bit, SampleRate, TonePair, phase_increment, sine_at, sine_at_f32};

/// A validated modulator configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModulatorConfig {
    sample_rate: SampleRate,
    baud: BaudRate,
    tones: TonePair,
}

impl ModulatorConfig {
    /// Creates a configuration from validated parts.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when each bit would
    /// span less than one sample.
    pub const fn new(
        sample_rate: SampleRate,
        baud: BaudRate,
        tones: TonePair,
    ) -> Result<Self, ConfigError> {
        if baud.bps() > sample_rate.hz() {
            return Err(ConfigError::BaudExceedsSampleRate {
                baud: baud.bps(),
                sample_rate: sample_rate.hz(),
            });
        }
        Ok(Self {
            sample_rate,
            baud,
            tones,
        })
    }

    /// Creates the Bell 202 preset (1200 baud, 1200/2200 Hz tones) at the
    /// given sample rate.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] if the tones do not fit under the Nyquist
    /// frequency of `sample_rate`.
    pub const fn bell_202(sample_rate: SampleRate) -> Result<Self, ConfigError> {
        let tones = match TonePair::new(1_200, 2_200, sample_rate) {
            Ok(t) => t,
            Err(e) => return Err(e),
        };
        Self::new(sample_rate, BaudRate::BELL_202, tones)
    }

    /// The configured sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        self.sample_rate
    }

    /// The configured baud rate.
    #[must_use]
    pub const fn baud(self) -> BaudRate {
        self.baud
    }

    /// The configured tone pair.
    #[must_use]
    pub const fn tones(self) -> TonePair {
        self.tones
    }
}

/// Streaming continuous-phase AFSK modulator.
///
/// Feed one bit at a time with [`Modulator::feed`], then drain that bit's
/// samples with [`Modulator::next_i16`] or [`Modulator::next_f32`]; or use
/// the iterator adapters [`Modulator::i16_samples`] /
/// [`Modulator::f32_samples`]. The modulator owns no buffers and never
/// allocates.
#[derive(Debug, Clone)]
pub struct Modulator {
    /// Phase accumulator; full u32 range == one waveform cycle.
    phase: u32,
    /// Per-sample phase increment for the mark tone.
    inc_mark: u32,
    /// Per-sample phase increment for the space tone.
    inc_space: u32,
    /// Increment currently in effect (mark or space).
    inc_current: u32,
    /// Whole samples per bit: `sample_rate / baud`.
    whole_per_bit: u32,
    /// Fractional remainder per bit: `sample_rate % baud`.
    rem_per_bit: u32,
    /// Baud rate (denominator of the remainder accumulator).
    baud: u32,
    /// Remainder accumulator; an extra sample is emitted when it reaches
    /// `baud`.
    rem_acc: u32,
    /// Samples still owed for the bit most recently fed.
    remaining: u32,
}

impl Modulator {
    /// Creates a modulator from a validated configuration.
    #[must_use]
    pub fn new(config: ModulatorConfig) -> Self {
        let sr = config.sample_rate.hz();
        Self {
            phase: 0,
            inc_mark: phase_increment(config.tones.mark_hz(), sr),
            inc_space: phase_increment(config.tones.space_hz(), sr),
            inc_current: phase_increment(config.tones.mark_hz(), sr),
            whole_per_bit: sr / config.baud.bps(),
            rem_per_bit: sr % config.baud.bps(),
            baud: config.baud.bps(),
            rem_acc: 0,
            remaining: 0,
        }
    }

    /// Queues one bit for modulation, selecting its tone.
    ///
    /// Any samples not yet pulled for a previously fed bit are discarded;
    /// pull with [`Modulator::next_i16`] / [`Modulator::next_f32`] until
    /// they return `None` before feeding the next bit.
    pub fn feed(&mut self, bit: Bit) {
        self.inc_current = match bit {
            Bit::Zero => self.inc_space,
            Bit::One => self.inc_mark,
        };
        self.rem_acc += self.rem_per_bit;
        let extra = if self.rem_acc >= self.baud {
            self.rem_acc -= self.baud;
            1
        } else {
            0
        };
        self.remaining = self.whole_per_bit + extra;
    }

    /// Pulls the next i16 PCM sample of the current bit, or `None` when the
    /// bit is exhausted (feed the next bit to continue).
    ///
    /// This path is integer-only: table lookup plus a u32 phase addition.
    pub fn next_i16(&mut self) -> Option<i16> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let sample = sine_at(self.phase);
        self.phase = self.phase.wrapping_add(self.inc_current);
        Some(sample)
    }

    /// Pulls the next f32 PCM sample (nominal range `-1.0..=1.0`) of the
    /// current bit, or `None` when the bit is exhausted.
    pub fn next_f32(&mut self) -> Option<f32> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let sample = sine_at_f32(self.phase);
        self.phase = self.phase.wrapping_add(self.inc_current);
        Some(sample)
    }

    /// Adapts a bit iterator into an iterator of i16 PCM samples.
    pub fn i16_samples<I>(self, bits: I) -> I16Samples<I>
    where
        I: Iterator<Item = Bit>,
    {
        I16Samples {
            modulator: self,
            bits,
        }
    }

    /// Adapts a bit iterator into an iterator of f32 PCM samples.
    pub fn f32_samples<I>(self, bits: I) -> F32Samples<I>
    where
        I: Iterator<Item = Bit>,
    {
        F32Samples {
            modulator: self,
            bits,
        }
    }
}

/// Iterator of i16 PCM samples over a bit iterator.
///
/// Created by [`Modulator::i16_samples`].
#[derive(Debug, Clone)]
pub struct I16Samples<I> {
    modulator: Modulator,
    bits: I,
}

impl<I> Iterator for I16Samples<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        loop {
            if let Some(sample) = self.modulator.next_i16() {
                return Some(sample);
            }
            let bit = self.bits.next()?;
            self.modulator.feed(bit);
        }
    }
}

/// Iterator of f32 PCM samples over a bit iterator.
///
/// Created by [`Modulator::f32_samples`].
#[derive(Debug, Clone)]
pub struct F32Samples<I> {
    modulator: Modulator,
    bits: I,
}

impl<I> Iterator for F32Samples<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.modulator.next_f32() {
                return Some(sample);
            }
            let bit = self.bits.next()?;
            self.modulator.feed(bit);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::types::{SINE_I16, TABLE_LEN};
    use std::vec::Vec;

    fn bell(sr: u32) -> ModulatorConfig {
        let rate = match SampleRate::new(sr) {
            Ok(r) => r,
            Err(e) => panic!("bad rate: {e}"),
        };
        match ModulatorConfig::bell_202(rate) {
            Ok(c) => c,
            Err(e) => panic!("bad config: {e}"),
        }
    }

    fn drain_i16(m: &mut Modulator) -> Vec<i16> {
        let mut v = Vec::new();
        while let Some(s) = m.next_i16() {
            v.push(s);
        }
        v
    }

    // ---- sine table ----

    #[test]
    fn table_cardinal_points() {
        assert_eq!(SINE_I16[0], 0);
        assert_eq!(SINE_I16[TABLE_LEN / 4], 32_767);
        assert_eq!(SINE_I16[TABLE_LEN / 2], 0);
        assert_eq!(SINE_I16[3 * TABLE_LEN / 4], -32_767);
    }

    #[test]
    fn table_odd_symmetry() {
        for i in 1..TABLE_LEN {
            assert_eq!(
                SINE_I16[i],
                -SINE_I16[TABLE_LEN - i],
                "symmetry broken at {i}"
            );
        }
    }

    #[test]
    fn table_within_i16_and_monotonic_first_quarter() {
        // Non-decreasing: adjacent entries may tie near the peak where the
        // quantized sine plateaus.
        for i in 1..TABLE_LEN / 4 {
            assert!(SINE_I16[i] >= SINE_I16[i - 1], "decreasing at {i}");
        }
    }

    #[test]
    fn table_matches_libm_sine() {
        for (i, &got_i16) in SINE_I16.iter().enumerate() {
            let expected = (core::f64::consts::TAU * i as f64 / TABLE_LEN as f64).sin() * 32_767.0;
            let got = got_i16 as f64;
            assert!(
                (got - expected).abs() <= 0.5 + 1e-6,
                "entry {i}: {got} vs {expected}"
            );
        }
    }

    // ---- phase increment ----

    #[test]
    fn phase_increment_rounding() {
        // 1200 * 2^32 / 48000 = 107374182.4 -> 107374182
        assert_eq!(phase_increment(1_200, 48_000), 107_374_182);
        // 2200 * 2^32 / 48000 = 196852667.7 -> 196852668
        assert_eq!(phase_increment(2_200, 48_000), 196_852_668);
        // Exact case: 12000 Hz at 48000 = quarter cycle per sample.
        assert_eq!(phase_increment(12_000, 48_000), 1 << 30);
    }

    // ---- config ----

    #[test]
    fn config_baud_exceeds_sample_rate_rejected() {
        let sr = SampleRate::new(8_000).unwrap_or_else(|_| panic!());
        let baud = BaudRate::new(9_600).unwrap_or_else(|_| panic!());
        let tones = TonePair::new(1_200, 2_200, sr).unwrap_or_else(|_| panic!());
        assert_eq!(
            ModulatorConfig::new(sr, baud, tones),
            Err(ConfigError::BaudExceedsSampleRate {
                baud: 9_600,
                sample_rate: 8_000
            })
        );
    }

    #[test]
    fn config_accessors() {
        let c = bell(48_000);
        assert_eq!(c.sample_rate().hz(), 48_000);
        assert_eq!(c.baud().bps(), 1_200);
        assert_eq!(c.tones().mark_hz(), 1_200);
        assert_eq!(c.tones().space_hz(), 2_200);
    }

    #[test]
    fn bell_202_preset_at_all_tested_rates() {
        for sr in [8_000, 11_025, 22_050, 44_100, 48_000] {
            assert!(
                ModulatorConfig::bell_202(SampleRate::new(sr).unwrap_or_else(|_| panic!())).is_ok()
            );
        }
    }

    // ---- pinned samples ----

    #[test]
    fn mark_tone_48k_first_16_samples_pinned() {
        let mut m = Modulator::new(bell(48_000));
        m.feed(Bit::One);
        let v = drain_i16(&mut m);
        assert_eq!(v.len(), 40);
        assert_eq!(
            &v[..16],
            &[
                0, 5106, 10087, 14867, 19236, 23134, 26497, 29177, 31160, 32359, 32767, 32367,
                31176, 29200, 26527, 23205
            ]
        );
    }

    #[test]
    fn space_tone_48k_first_16_samples_pinned() {
        let mut m = Modulator::new(bell(48_000));
        m.feed(Bit::Zero);
        let v = drain_i16(&mut m);
        assert_eq!(v.len(), 40);
        assert_eq!(
            &v[..16],
            &[
                0, 9271, 17827, 24910, 29915, 32482, 32367, 29578, 24380, 17146, 8497, -854,
                -10087, -18537, -25456, -30273
            ]
        );
    }

    #[test]
    fn mark_tone_period_is_40_samples_at_48k() {
        // 1200 Hz at 48 kHz: exactly 40 samples per cycle, so sample 0 of
        // a pure mark stream repeats every 40 samples.
        // 1200 Hz at 48 kHz: 40 samples per cycle. The rounded phase
        // increment and truncating table lookup allow a small residual, so
        // compare within one table-step of amplitude (2π/4096 · 32767 ≈ 50).
        let m = Modulator::new(bell(48_000));
        let v: Vec<i16> = m.i16_samples(core::iter::repeat_n(Bit::One, 4)).collect();
        for (a, b) in [(0, 40), (1, 41), (39, 119)] {
            let diff = (v[a] as i32 - v[b] as i32).abs();
            assert!(diff <= 51, "period mismatch at {a}/{b}: {diff}");
        }
    }

    // ---- samples-per-bit exactness ----

    #[test]
    fn samples_per_bit_exact_over_10000_bits_at_44100() {
        // 44100 / 1200 = 36.75 samples per bit; over 10_000 bits the total
        // must be exactly 367_500 with zero drift.
        let m = Modulator::new(bell(44_100));
        let n = m
            .i16_samples(core::iter::repeat_n(Bit::One, 10_000))
            .count();
        assert_eq!(n, 367_500);
    }

    #[test]
    fn samples_per_bit_pattern_at_44100() {
        // Per-bit counts must follow 36,37,37,37 repeating (0.75 fraction).
        let mut m = Modulator::new(bell(44_100));
        let mut counts = Vec::new();
        for _ in 0..8 {
            m.feed(Bit::One);
            counts.push(drain_i16(&mut m).len());
        }
        assert_eq!(counts, [36, 37, 37, 37, 36, 37, 37, 37]);
    }

    #[test]
    fn samples_per_bit_exact_at_11025() {
        // 11025 / 1200 = 9.1875; over 10_000 bits: 91_875 samples.
        let m = Modulator::new(bell(11_025));
        let n = m
            .i16_samples(core::iter::repeat_n(Bit::Zero, 10_000))
            .count();
        assert_eq!(n, 91_875);
    }

    #[test]
    fn samples_per_bit_integral_at_48k() {
        let m = Modulator::new(bell(48_000));
        let n = m.i16_samples(core::iter::repeat_n(Bit::One, 1_000)).count();
        assert_eq!(n, 40_000);
    }

    // ---- phase continuity ----

    #[test]
    fn phase_continuous_across_bit_transitions() {
        // Continuous-phase FSK: the step between adjacent samples must
        // never exceed the steepest slope of the faster tone, even at bit
        // boundaries. Max step = 32767 * sin(2π*2200/48000) ≈ 9271, plus
        // margin for table quantization (±one table step ≈ 50 each side).
        let bits = [
            Bit::One,
            Bit::Zero,
            Bit::One,
            Bit::One,
            Bit::Zero,
            Bit::Zero,
            Bit::One,
            Bit::Zero,
        ];
        let v: Vec<i16> = Modulator::new(bell(48_000))
            .i16_samples(bits.into_iter())
            .collect();
        let max_step = 9_500i32;
        for w in v.windows(2) {
            let step = (w[1] as i32 - w[0] as i32).abs();
            assert!(step <= max_step, "discontinuity: step {step}");
        }
    }

    #[test]
    fn phase_continuous_at_44100_fractional_bits() {
        let v: Vec<i16> = Modulator::new(bell(44_100))
            .i16_samples([Bit::One, Bit::Zero].iter().copied().cycle().take(200))
            .collect();
        // Max slope of 2200 Hz at 44100 Hz: 32767*sin(2π*2200/44100) ≈ 10077,
        // plus table-quantization margin.
        for w in v.windows(2) {
            let step = (w[1] as i32 - w[0] as i32).abs();
            assert!(step <= 10_400, "discontinuity: step {step}");
        }
    }

    // ---- i16 / f32 agreement ----

    #[test]
    fn i16_and_f32_paths_agree() {
        let bits = [Bit::One, Bit::Zero, Bit::One, Bit::Zero];
        let vi: Vec<i16> = Modulator::new(bell(44_100))
            .i16_samples(bits.iter().copied())
            .collect();
        let vf: Vec<f32> = Modulator::new(bell(44_100))
            .f32_samples(bits.iter().copied())
            .collect();
        assert_eq!(vi.len(), vf.len());
        for (a, b) in vi.iter().zip(vf.iter()) {
            let ai = *a as f32 / 32_767.0;
            // Nearest-entry vs interpolated lookup differ by at most one
            // table step: 2π/4096 ≈ 0.00153.
            assert!((ai - b).abs() < 2.0e-3, "i16 {ai} vs f32 {b}");
        }
    }

    #[test]
    fn f32_samples_within_unit_range() {
        let vf: Vec<f32> = Modulator::new(bell(8_000))
            .f32_samples(core::iter::repeat_n(Bit::Zero, 50))
            .collect();
        for s in vf {
            assert!((-1.0..=1.0).contains(&s));
        }
    }

    // ---- streaming behaviour ----

    #[test]
    fn next_returns_none_before_feed() {
        let mut m = Modulator::new(bell(48_000));
        assert_eq!(m.next_i16(), None);
        assert_eq!(m.next_f32(), None);
    }

    #[test]
    fn feed_then_drain_then_none() {
        let mut m = Modulator::new(bell(48_000));
        m.feed(Bit::One);
        assert_eq!(drain_i16(&mut m).len(), 40);
        assert_eq!(m.next_i16(), None);
    }

    #[test]
    fn iterator_empty_bits_yields_no_samples() {
        let mut it = Modulator::new(bell(48_000)).i16_samples(core::iter::empty());
        assert_eq!(it.next(), None);
    }

    #[test]
    fn iterator_matches_manual_feed_drain() {
        let bits = [Bit::Zero, Bit::One, Bit::One];
        let via_iter: Vec<i16> = Modulator::new(bell(22_050))
            .i16_samples(bits.iter().copied())
            .collect();
        let mut m = Modulator::new(bell(22_050));
        let mut manual = Vec::new();
        for b in bits {
            m.feed(b);
            manual.extend(drain_i16(&mut m));
        }
        assert_eq!(via_iter, manual);
    }

    #[test]
    fn modulator_starts_at_zero_phase() {
        let mut m = Modulator::new(bell(48_000));
        m.feed(Bit::Zero);
        assert_eq!(m.next_i16(), Some(0));
        let mut m2 = Modulator::new(bell(48_000));
        m2.feed(Bit::One);
        assert_eq!(m2.next_i16(), Some(0));
    }
}
