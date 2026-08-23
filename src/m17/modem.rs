//! M17 physical layer: 4-level symbol mapping and the RRC-shaped
//! baseband modulator.
//!
//! This is the boundary between bits and audio. It produces the
//! baseband an FM exciter's modulator input takes; the RF 4FSK happens
//! in the radio. Re-exported from [`crate::m17`].

use super::M17Error;
use crate::types::SampleRate;

// ---------------------------------------------------------------------------
// Symbol mapping (M17 spec, Physical Layer, "Dibit symbol mapping")
// ---------------------------------------------------------------------------

/// Maps one dibit (bits MSB-first: `b1 b0`) to a 4-level symbol
/// (M17 spec, Physical Layer): `01` → +3, `00` → +1, `10` → −1,
/// `11` → −3.
#[must_use]
pub const fn dibit_to_symbol(dibit: u8) -> i8 {
    match dibit & 0b11 {
        0b01 => 3,
        0b00 => 1,
        0b10 => -1,
        _ => -3,
    }
}

/// Inverse of [`dibit_to_symbol`] for sliced symbols.
#[must_use]
pub const fn symbol_to_dibit(symbol: i8) -> u8 {
    match symbol {
        3 => 0b01,
        1 => 0b00,
        -1 => 0b10,
        _ => 0b11,
    }
}

/// Symbols per 40 ms physical frame (16 sync bits + 368 payload bits,
/// two bits per symbol).
pub const FRAME_SYMBOLS: usize = 192;

/// Expands a 16-bit sync word into its 8 transmitted symbols.
#[must_use]
pub fn sync_symbols(sync: u16) -> [i8; 8] {
    let mut out = [0i8; 8];
    for (i, s) in out.iter_mut().enumerate() {
        *s = dibit_to_symbol(((sync >> (14 - 2 * i)) & 0b11) as u8);
    }
    out
}

// ---------------------------------------------------------------------------
// Baseband modulator (RRC-shaped 4-level PAM)
// ---------------------------------------------------------------------------

/// M17 symbol rate: 4800 symbols/s (M17 spec, Physical Layer).
pub const SYMBOL_RATE: u32 = 4_800;

/// RRC roll-off α = 0.5 (M17 spec, Physical Layer: root-raised-cosine
/// with a roll-off factor of 0.5).
pub const RRC_ALPHA_NUM: u32 = 1;
/// See [`RRC_ALPHA_NUM`] (α = 1/2 as a ratio, keeping the const int-only).
pub const RRC_ALPHA_DEN: u32 = 2;

/// RRC filter span in symbols on each side of the center tap (total
/// span 8 symbols; the spec fixes α and leaves span to implementations
/// — 8 symbols matches common M17 modems and bounds truncation ripple
/// well below the FEC's noise floor).
pub const RRC_SPAN_SYMBOLS: usize = 8;

/// Largest samples-per-symbol supported (48 kHz / 4800 = 10, the
/// canonical audio rate).
pub const MAX_SPS: usize = 10;

/// Longest RRC filter the fixed-size tap arrays can hold
/// (8 × 10 + 1 = 81).
///
/// [`MAX_SPS`] and this bound are load-bearing but unasserted:
/// `checked_sps` only enforces divisibility by [`SYMBOL_RATE`], and a
/// larger `sps` would make `design_rrc` return `n > MAX_TAPS` and
/// panic on out-of-bounds indexing. What keeps that unreachable is
/// [`SampleRate`], which caps at 48 000 Hz: the only valid M17 rates
/// are the nine multiples of 4800 from 9600 to 48 000 Hz, giving
/// `sps` 2..=10 and `ntaps` 17..=81. Raising that cap would require
/// raising these two constants with it.
pub(super) const MAX_TAPS: usize = RRC_SPAN_SYMBOLS * MAX_SPS + 1;

/// Runtime `sin(x)`: quadrant reduction + odd Taylor polynomial, the
/// same design-time-only approach as the G3RUH baseband filter (plain
/// `core` floats, no `libm`). Only ever called during construction.
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

fn cos_taylor(x: f64) -> f64 {
    sin_taylor(x + 0.5 * core::f64::consts::PI)
}

/// Newton–Raphson square root for tap normalization (construction only).
fn sqrt_newton(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut g = x;
    for _ in 0..40 {
        g = 0.5 * (g + x / g);
    }
    g
}

/// Continuous-time root-raised-cosine impulse response at `t` (in
/// symbol periods) for α = 0.5, with the removable singularities
/// handled explicitly.
fn rrc(t: f64) -> f64 {
    const PI: f64 = core::f64::consts::PI;
    const ALPHA: f64 = 0.5;
    let at = 4.0 * ALPHA * t;
    if t.abs() < 1e-9 {
        return 1.0 - ALPHA + 4.0 * ALPHA / PI;
    }
    if (at.abs() - 1.0).abs() < 1e-9 {
        // t = ±1/(4α): limit form.
        let s = sin_taylor(PI / (4.0 * ALPHA));
        let c = cos_taylor(PI / (4.0 * ALPHA));
        return ALPHA / sqrt_newton(2.0) * ((1.0 + 2.0 / PI) * s + (1.0 - 2.0 / PI) * c);
    }
    (sin_taylor(PI * t * (1.0 - ALPHA)) + at * cos_taylor(PI * t * (1.0 + ALPHA)))
        / (PI * t * (1.0 - at * at))
}

/// Designs the shared float RRC taps (span 8 symbols, `sps`
/// samples/symbol) with unit energy.
pub(super) fn design_rrc(sps: usize, taps: &mut [f64; MAX_TAPS]) -> usize {
    let n = RRC_SPAN_SYMBOLS * sps + 1;
    let mid = (n - 1) / 2;
    let mut energy = 0.0;
    for (i, t) in taps.iter_mut().enumerate().take(n) {
        let x = (i as f64 - mid as f64) / sps as f64;
        *t = rrc(x);
        energy += *t * *t;
    }
    let norm = sqrt_newton(energy);
    for t in taps.iter_mut().take(n) {
        *t /= norm;
    }
    n
}

pub(super) fn checked_sps(sample_rate: SampleRate) -> Result<usize, M17Error> {
    let hz = sample_rate.hz();
    if !hz.is_multiple_of(SYMBOL_RATE) {
        return Err(M17Error::SampleRateInexact { got: hz });
    }
    Ok((hz / SYMBOL_RATE) as usize)
}

/// Streaming M17 baseband modulator: 4-level symbols in, RRC-shaped
/// (α = 0.5, 8-symbol span) i16 PCM out — the transmit half of the
/// crate's second baseband-family resident (structured like the G3RUH
/// `BasebandModulator` of the `g3ruh`-gated `baseband` module: feed
/// one symbol, pull its samples; fixed integer taps, no allocation).
///
/// The sample rate must be a multiple of 4800 Hz; 48 kHz (10
/// samples/symbol) is the canonical M17 audio rate.
#[derive(Debug, Clone)]
pub struct M17Modulator {
    /// Integer TX taps, pre-scaled so a worst-case ±3 symbol stream
    /// peaks near (but under) full scale.
    taps: [i32; MAX_TAPS],
    ntaps: usize,
    sps: usize,
    /// Newest symbol first; length covers the full filter span.
    history: [i8; RRC_SPAN_SYMBOLS + 1],
    /// Samples still to emit for the most recent symbol.
    remaining: usize,
}

impl M17Modulator {
    /// Creates a modulator for the given sample rate.
    ///
    /// # Errors
    ///
    /// [`M17Error::SampleRateInexact`] unless the rate is a multiple of
    /// 4800 Hz.
    pub fn new(sample_rate: SampleRate) -> Result<Self, M17Error> {
        let sps = checked_sps(sample_rate)?;
        let mut f = [0.0f64; MAX_TAPS];
        let ntaps = design_rrc(sps, &mut f);
        // Worst-case output magnitude for ±3 symbols: max over sampling
        // phase of the sum of |tap| at symbol stride. Scale to ~90% FS.
        let mut worst = 0.0f64;
        for p in 0..sps {
            let mut acc = 0.0;
            let mut idx = p;
            while idx < ntaps {
                acc += f[idx].abs();
                idx += sps;
            }
            if acc > worst {
                worst = acc;
            }
        }
        let scale = 30_000.0 / (3.0 * worst);
        let mut taps = [0i32; MAX_TAPS];
        for (q, &h) in taps.iter_mut().zip(f.iter()).take(ntaps) {
            *q = (h * scale + if h >= 0.0 { 0.5 } else { -0.5 }) as i32;
        }
        Ok(Self {
            taps,
            ntaps,
            sps,
            history: [0; RRC_SPAN_SYMBOLS + 1],
            remaining: 0,
        })
    }

    /// Samples emitted per symbol.
    #[must_use]
    pub const fn samples_per_symbol(&self) -> usize {
        self.sps
    }

    /// Feeds the next symbol (must be one of −3, −1, +1, +3; 0 is
    /// accepted as a flush filler). Any samples of the previous symbol
    /// not yet pulled are dropped.
    pub fn feed(&mut self, symbol: i8) {
        for i in (1..self.history.len()).rev() {
            self.history[i] = self.history[i - 1];
        }
        self.history[0] = symbol;
        self.remaining = self.sps;
    }

    /// Pulls the next sample of the current symbol, or `None` when the
    /// symbol is exhausted (feed the next one).
    pub fn next_i16(&mut self) -> Option<i16> {
        if self.remaining == 0 {
            return None;
        }
        let phase = self.sps - self.remaining;
        self.remaining -= 1;
        let mut acc: i64 = 0;
        for (j, &sym) in self.history.iter().enumerate() {
            let idx = phase + j * self.sps;
            if idx < self.ntaps && sym != 0 {
                acc += i64::from(sym) * i64::from(self.taps[idx]);
            }
        }
        Some(acc.clamp(-32_768, 32_767) as i16)
    }
}
