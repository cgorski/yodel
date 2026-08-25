//! Permanent bit-exact equivalence test for the G3RUH baseband FIR
//! optimization (two-slice linear convolution replacing per-tap modulo
//! ring indexing in `BasebandFilter::push`).
//!
//! `reference` below is a verbatim copy of the ORIGINAL modulo
//! implementation (tap design + `%`-indexed convolution + peak/valley
//! tracker) as it stood before the optimization. Driving it into the
//! crate's public [`Slicer`] reproduces the original
//! `BasebandDemodulator` exactly; the test asserts the current
//! demodulator emits an identical `Option<Bit>` for every sample of
//! long LCG-random i16 streams at several sample rates and seeds. Any
//! numeric deviation in the filter metric would flip a slicer decision
//! or its timing somewhere in 200k random samples, so this pins the
//! optimization to bit-exactness permanently.
#![cfg(all(feature = "demod", feature = "mod"))]

use yodel::{BasebandDemodulator, BaudRate, SampleRate, Slicer};

/// Reference copy of the pre-optimization baseband filter (modulo ring
/// indexing). Kept in sync with nothing: this is frozen.
mod reference {
    pub const MAX_FIR_TAPS: usize = 15;
    const TAP_UNITY: i32 = 1 << 15;
    /// Mirrors the crate's `BASELINE_SHIFT`.
    const BASELINE_SHIFT: u32 = 9;
    /// Mirrors the crate's `FIR_SPAN_BITS`.
    const FIR_SPAN_BITS: u32 = 3;
    /// Mirrors the crate's `FIR_CUTOFF_RATIO`.
    const FIR_CUTOFF_RATIO: f64 = 0.8;

    /// Verbatim copy of the crate's design-time `sin(x)` helper.
    fn sin_taylor(x: f64) -> f64 {
        const PI: f64 = core::f64::consts::PI;
        let mut r = x % (2.0 * PI);
        if r < 0.0 {
            r += 2.0 * PI;
        }
        let r = if r <= 0.5 * PI {
            r
        } else if r <= 1.5 * PI {
            PI - r
        } else {
            r - 2.0 * PI
        };
        let r2 = r * r;
        r * (1.0
            + r2 * (-1.0 / 6.0
                + r2 * (1.0 / 120.0
                    + r2 * (-1.0 / 5_040.0 + r2 * (1.0 / 362_880.0 + r2 * (-1.0 / 39_916_800.0))))))
    }

    /// The original modulo-indexed FIR front end.
    pub struct ModuloFilter {
        taps: [i32; MAX_FIR_TAPS],
        history: [i32; MAX_FIR_TAPS],
        taps_len: usize,
        pos: usize,
        baseline: i32,
        amplitude: i32,
    }

    impl ModuloFilter {
        pub fn new(sr: u32, bd: u32) -> Self {
            let spb = FIR_SPAN_BITS * ((sr + bd / 2) / bd);
            let len = if spb.is_multiple_of(2) { spb + 1 } else { spb } as usize;
            let len = len.clamp(3, MAX_FIR_TAPS);
            let fc = FIR_CUTOFF_RATIO * bd as f64 / sr as f64;
            let center = (len - 1) as f64 / 2.0;
            let pi = core::f64::consts::PI;
            let mut raw = [0.0f64; MAX_FIR_TAPS];
            let mut sum = 0.0f64;
            for (k, slot) in raw.iter_mut().enumerate().take(len) {
                let t = k as f64 - center;
                let x = 2.0 * pi * fc * t;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    sin_taylor(x) / x
                };
                let window =
                    0.54 - 0.46 * sin_taylor(2.0 * pi * k as f64 / (len - 1) as f64 + pi / 2.0);
                *slot = sinc * window;
                sum += *slot;
            }
            let mut taps = [0i32; MAX_FIR_TAPS];
            let mut acc = 0i32;
            for (k, slot) in taps.iter_mut().enumerate().take(len) {
                *slot = (raw[k] / sum * TAP_UNITY as f64 + 0.5) as i32;
                acc += *slot;
            }
            if let Some(center_tap) = taps.get_mut(len / 2) {
                *center_tap += TAP_UNITY - acc;
            }
            Self {
                taps,
                history: [0; MAX_FIR_TAPS],
                taps_len: len,
                pos: 0,
                baseline: 0,
                amplitude: 0,
            }
        }

        /// The ORIGINAL push: per-tap `%` ring indexing.
        pub fn push(&mut self, sample: i32) -> i32 {
            self.history[self.pos] = sample;
            self.pos = (self.pos + 1) % self.taps_len;
            let mut acc = 0i64;
            for k in 0..self.taps_len {
                let idx = (self.pos + k) % self.taps_len;
                acc += self.history[idx] as i64 * self.taps[k] as i64;
            }
            let filtered = (acc >> 15) as i32;
            let metric = filtered - self.baseline;
            let sign = if metric >= 0 { 1 } else { -1 };
            self.amplitude += (metric.abs() - self.amplitude) >> BASELINE_SHIFT;
            let residual = filtered - sign * self.amplitude;
            self.baseline += (residual - self.baseline) >> BASELINE_SHIFT;
            metric
        }
    }
}

/// Minimal LCG (Numerical Recipes constants) for reproducible i16 noise.
struct Lcg(u64);

impl Lcg {
    fn next_i16(&mut self) -> i16 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as i16
    }
}

/// The current demodulator must emit exactly the same `Option<Bit>` as
/// the frozen modulo reference for every single sample of long random
/// streams, across several sample rates and seeds.
#[test]
fn fir_two_slice_matches_modulo_reference_sample_by_sample() {
    const SAMPLES: usize = 200_000;
    let baud = 9_600u32;
    for &sr in &[19_200u32, 22_050, 44_100, 48_000] {
        for &seed in &[1u64, 0xDEAD_BEEF, 0x1234_5678_9ABC_DEF0] {
            let sample_rate = SampleRate::new(sr).expect("rate");
            let baud_rate = BaudRate::new(baud).expect("baud");
            let mut new = BasebandDemodulator::new(sample_rate, baud_rate).expect("config");
            let mut old_filter = reference::ModuloFilter::new(sr, baud);
            let mut old_slicer = Slicer::new(sample_rate, baud_rate).expect("config");
            let mut rng = Lcg(seed);
            for n in 0..SAMPLES {
                let s = rng.next_i16();
                let got = new.push_i16(s);
                let want = old_slicer.push(old_filter.push(s as i32));
                assert_eq!(got, want, "rate {sr}, seed {seed:#x}, sample {n}");
            }
        }
    }
}
