//! The std-gated WSPR receive engine: buffered capture → decoded
//! messages.
//!
//! This is the buffer-owning half of the RX split documented on
//! [`crate::wspr`]: everything here allocates (`Vec`) and uses f32
//! transcendentals, so it is compiled only with `wspr` + `std`. The
//! buffer-free math (deinterleave, Fano search, message unpack) lives
//! in the parent module and stays no_std.
//!
//! # Pipeline
//!
//! 1. **Mix + decimate** (12 kHz i16 → 375 Hz complex f32): a complex
//!    mixer at the window center, then two cascaded real-coefficient
//!    FIR decimators (↓8 to 1500 Hz, ↓4 to 375 Hz; Blackman-windowed
//!    sinc, hand-designed at runtime). RAM: the *surviving* baseband
//!    of a ~114 s capture is ≈ 43 k complex f32 ≈ 342 KB, but that is
//!    only what is left at the end. Measured peak heap for a full
//!    114 s / 12 kHz capture is **15 574 696 B ≈ 14.85 MiB**, all live
//!    at once inside the decimator: the padded `Vec<i16>` copy
//!    (2 768 768 B), the mixed complex-f32 signal at the *input* rate
//!    (11 075 072 B), the stage-1 1500 Hz output (1 384 384 B) and the
//!    stage-2 375 Hz output (346 096 B). f32 (not i16 pairs at half
//!    the size) is chosen because this path is std-only, and float
//!    keeps the FFT/DFT numerics simple and headroom-free — but a
//!    ~15 MiB peak for a two-minute capture is exactly why
//!    [`WsprDecoder`] is std-gated and never offered to the embedded
//!    build.
//! 2. **Candidate search**: a hand-rolled radix-2 FFT (N = 1024, bin
//!    width 375/1024 ≈ 0.366 Hz — exactly a quarter tone spacing)
//!    averaged over the capture; tone-quad comb power on the 1.4648 Hz
//!    grid inside the configured window; local maxima become
//!    candidates (best N by comb power).
//! 3. **Sync alignment**: per-candidate short-window 4-tone powers on
//!    a 32-sample (85 ms) hop grid, correlated with the published sync
//!    vector for coarse time; then a joint fine search over ±0.55 Hz
//!    and ±1 hop.
//! 4. **Demod + decode**: per-symbol 4-bin DFT powers → per-coded-bit
//!    soft metrics (scaled `log2(2p) − ½`, the classic Fano bias) →
//!    [`deinterleave`] → [`fano_decode`] (hard node cap) →
//!    [`WsprMessage::unpack`].
//!
//! Sensitivity is measured, not assumed: the reference decoder's
//! −31 dB figure belongs to that implementation; this engine's pinned
//! test floor is −22 dB SNR in 2500 Hz (see `tests/wspr_rx.rs`).

use core::fmt;

use super::{
    FANO_DELTA, FANO_NODE_CAP, SYMBOL_COUNT, SYNC_VECTOR, WsprMessage, deinterleave, fano_decode,
};

/// Decimated (baseband) sample rate in Hz.
const FS_BASE: f32 = 375.0;

/// Samples per channel symbol at the decimated rate.
const SYM_LEN: usize = 256;

/// Hop between sync-search windows, in decimated samples (⅛ symbol).
const HOP: usize = 32;

/// FFT length of the candidate search (bin = 375/1024 ≈ 0.366 Hz,
/// exactly a quarter of the 1.4648 Hz tone spacing).
const FFT_LEN: usize = 1024;

/// Tone spacing in Hz (12000/8192 = 375/256).
const SPACING: f32 = 375.0 / 256.0;

/// The only capture sample rate the engine accepts.
const INPUT_RATE: u32 = 12_000;

/// Total decimation factor (12 kHz → 375 Hz).
const DECIM: usize = 32;

/// Decimated samples a full transmission spans.
const TX_LEN: usize = SYMBOL_COUNT * SYM_LEN;

/// Errors from decoder configuration or capture validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WsprRxError {
    /// The search window is invalid: the half-width must be within
    /// `10..=100` Hz and every tone-0 candidate (center ± window plus
    /// three tone spacings) must stay above 0 Hz and below Nyquist.
    WindowInvalid {
        /// The requested window center in Hz.
        center_hz: u32,
        /// The requested half-width in Hz.
        window_hz: u32,
    },
    /// `max_candidates` is outside `1..=16`.
    CandidatesInvalid {
        /// The rejected count.
        got: usize,
    },
    /// The Fano node-visit cap is zero.
    CapInvalid,
    /// The capture is shorter than one full transmission.
    CaptureTooShort {
        /// Samples supplied.
        got: usize,
        /// Samples required (one transmission at 12 kHz).
        need: usize,
    },
}

impl fmt::Display for WsprRxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WindowInvalid {
                center_hz,
                window_hz,
            } => write!(
                f,
                "search window {center_hz}±{window_hz} Hz is invalid: half-width must be \
                 10..=100 Hz and all four tones must stay within (0, 6000) Hz"
            ),
            Self::CandidatesInvalid { got } => {
                write!(f, "max candidates {got} is out of range: must be 1..=16")
            }
            Self::CapInvalid => write!(f, "the Fano node-visit cap must be nonzero"),
            Self::CaptureTooShort { got, need } => write!(
                f,
                "capture of {got} samples is too short: a full WSPR transmission needs \
                 {need} samples at 12000 Hz (~110.6 s)"
            ),
        }
    }
}

impl std::error::Error for WsprRxError {}

/// A validated receive configuration: search window, candidate budget
/// and Fano overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsprDecoderConfig {
    center_hz: u32,
    window_hz: u32,
    max_candidates: usize,
    fano_cap: u32,
}

impl WsprDecoderConfig {
    /// Creates a configuration searching tone-0 frequencies within
    /// `center_hz ± window_hz` (audio Hz; the conventional WSPR
    /// sub-band is 1500 ± 100).
    ///
    /// # Errors
    ///
    /// [`WsprRxError::WindowInvalid`] when the half-width is outside
    /// `10..=100` Hz or any tone in the window would leave
    /// `(0, 6000)` Hz (the decimation filters are designed for a
    /// ≤ 100 Hz half-width; wider bands alias).
    pub fn new(center_hz: u32, window_hz: u32) -> Result<Self, WsprRxError> {
        let err = WsprRxError::WindowInvalid {
            center_hz,
            window_hz,
        };
        if !(10..=100).contains(&window_hz) {
            return Err(err);
        }
        // Highest tone: center + window + 3 spacings (< Nyquist);
        // lowest tone-0: center - window (> 0).
        let top = f64::from(center_hz) + f64::from(window_hz) + 3.0 * f64::from(SPACING);
        if center_hz <= window_hz || top >= f64::from(INPUT_RATE) / 2.0 {
            return Err(err);
        }
        Ok(Self {
            center_hz,
            window_hz,
            max_candidates: 3,
            fano_cap: FANO_NODE_CAP,
        })
    }

    /// Sets how many decodes a capture may return (best candidates by
    /// comb power first; default 3).
    ///
    /// This caps *successful decodes*, not demodulation attempts: the
    /// candidate loop stops early only once that many results are in
    /// hand, so a capture with nothing decodable in it always walks
    /// every candidate and pays the full search cost regardless of the
    /// setting.
    ///
    /// # Errors
    ///
    /// [`WsprRxError::CandidatesInvalid`] outside `1..=16`.
    pub fn max_candidates(mut self, n: usize) -> Result<Self, WsprRxError> {
        if !(1..=16).contains(&n) {
            return Err(WsprRxError::CandidatesInvalid { got: n });
        }
        self.max_candidates = n;
        Ok(self)
    }

    /// Overrides the Fano node-visit cap (default [`FANO_NODE_CAP`]).
    /// Lower caps bound decode time tighter but give up on noisier
    /// signals sooner.
    ///
    /// # Errors
    ///
    /// [`WsprRxError::CapInvalid`] when zero.
    pub fn fano_cap(mut self, cap: u32) -> Result<Self, WsprRxError> {
        if cap == 0 {
            return Err(WsprRxError::CapInvalid);
        }
        self.fano_cap = cap;
        Ok(self)
    }
}

/// One decoded transmission from a capture.
#[derive(Debug, Clone, PartialEq)]
pub struct WsprDecode {
    /// The recovered message.
    pub message: WsprMessage,
    /// Measured tone-0 audio frequency in Hz.
    pub freq_hz: f32,
    /// Measured start of the transmission within the capture, in
    /// seconds (includes ≈ 22 ms of decimation-filter group delay).
    pub dt_seconds: f32,
    /// Estimated SNR in the 2500 Hz reference bandwidth, dB. An
    /// estimate from the per-symbol tone/off-tone power ratio — a
    /// quality metric, not a calibrated measurement.
    pub snr_db: f32,
    /// Sync-vector correlation score (higher is a cleaner alignment;
    /// 1.0 would be a perfect noiseless correlation).
    pub sync_score: f32,
}

/// The buffered WSPR receive engine (`wspr` + `std` only).
///
/// Feed [`WsprDecoder::decode`] a whole capture — at least one full
/// transmission (1 327 104 samples ≈ 110.6 s) of 16-bit mono PCM at
/// **12 kHz** (other rates are rejected at the CLI layer; the engine
/// itself is fixed-rate by design, mirroring the exact-timing rule of
/// the modulator). Returns every message it could decode, best
/// candidates first, deduplicated.
#[derive(Debug, Clone)]
pub struct WsprDecoder {
    config: WsprDecoderConfig,
}

impl WsprDecoder {
    /// Creates a decoder from a validated configuration.
    #[must_use]
    pub fn new(config: WsprDecoderConfig) -> Self {
        Self { config }
    }

    /// Decodes every WSPR transmission found in `samples` (16-bit mono
    /// PCM at 12 kHz), returning up to `max_candidates` results sorted
    /// by candidate strength.
    ///
    /// # Errors
    ///
    /// [`WsprRxError::CaptureTooShort`] when fewer than 1 327 104
    /// samples (one full transmission) are supplied. Per-candidate
    /// failures (Fano cap exhausted, invalid unpack) are not errors:
    /// those candidates are simply absent from the result.
    pub fn decode(&self, samples: &[i16]) -> Result<Vec<WsprDecode>, WsprRxError> {
        let need = TX_LEN * DECIM;
        if samples.len() < need {
            return Err(WsprRxError::CaptureTooShort {
                got: samples.len(),
                need,
            });
        }
        let center = self.config.center_hz as f32;
        // Pad with two symbols of silence so a transmission that fills
        // the capture exactly still leaves the sync search room to
        // slide (and absorbs the decimation filters' group delay).
        let mut padded = Vec::with_capacity(samples.len() + 2 * SYM_LEN * DECIM);
        padded.extend_from_slice(samples);
        padded.resize(samples.len() + 2 * SYM_LEN * DECIM, 0);
        let baseband = decimate(&padded, center);
        let mut results: Vec<WsprDecode> = Vec::new();
        for cand in search_candidates(&baseband, self.config.window_hz as f32) {
            if results.len() >= self.config.max_candidates {
                break;
            }
            // Skip candidates within half a tone spacing of an already
            // decoded signal.
            if results
                .iter()
                .any(|r| (r.freq_hz - (center + cand)).abs() < SPACING / 2.0)
            {
                continue;
            }
            if let Some(decode) = self.try_candidate(&baseband, cand, center)
                && !results.iter().any(|r| r.message == decode.message)
            {
                results.push(decode);
            }
        }
        Ok(results)
    }

    /// Aligns, demodulates and decodes one candidate frequency
    /// (baseband Hz). `None` on any failure: weak sync, Fano cap,
    /// invalid unpack.
    fn try_candidate(&self, z: &[(f32, f32)], f0: f32, center: f32) -> Option<WsprDecode> {
        // Coarse time sync at the candidate frequency: 4-tone powers
        // on the hop grid, correlated with the sync vector.
        let hops = hop_powers(z, f0);
        let max_start = (hops.len()).checked_sub(SYMBOL_COUNT * (SYM_LEN / HOP))?;
        let (mut best_t, mut best_score) = (0usize, f32::MIN);
        for t in 0..=max_start {
            let s = sync_score(&hops, t);
            if s > best_score {
                best_score = s;
                best_t = t;
            }
        }
        // Joint fine search: ±1 hop, ±0.55 Hz in 0.183 Hz steps
        // (half the FFT bin), maximizing the same sync correlation on
        // full-precision symbol powers.
        let mut best: Option<([[f32; 4]; SYMBOL_COUNT], f32, usize, f32)> = None;
        for dt in -1isize..=1 {
            let Some(t) = best_t.checked_add_signed(dt) else {
                continue;
            };
            if t > max_start {
                continue;
            }
            for step in -3i32..=3 {
                let f = f0 + step as f32 * (FS_BASE / FFT_LEN as f32 / 2.0);
                let powers = symbol_powers(z, f, t * HOP);
                let score = sync_score_powers(&powers);
                if best.as_ref().is_none_or(|b| score > b.1) {
                    best = Some((powers, score, t, f));
                }
            }
        }
        let (powers, score, t, f) = best?;
        // Normalized sync quality: correlation over total pair-power.
        let total: f32 = powers
            .iter()
            .map(|p| p[0] + p[1] + p[2] + p[3])
            .sum::<f32>()
            .max(f32::MIN_POSITIVE);
        let quality = score / total;

        // Soft metrics per channel position, then deinterleave.
        let mut channel = [[0i32; 2]; SYMBOL_COUNT];
        for (i, p) in powers.iter().enumerate() {
            let sync = usize::from(SYNC_VECTOR[i]);
            let lo = p[sync].max(f32::MIN_POSITIVE);
            let hi = p[sync + 2].max(f32::MIN_POSITIVE);
            let p1 = hi / (lo + hi);
            channel[i] = [bit_metric(1.0 - p1), bit_metric(p1)];
        }
        let mut metrics = [[0i32; 2]; SYMBOL_COUNT];
        deinterleave(&channel, &mut metrics);
        let packed = fano_decode(&metrics, FANO_DELTA, self.config.fano_cap).ok()?;
        let message = WsprMessage::unpack(&packed).ok()?;

        // SNR estimate: selected-tone power vs off-pair tone power
        // (noise per ≈1.46 Hz DFT bin), referred to 2500 Hz.
        let symbols = message.channel_symbols();
        let mut sig = 0.0f32;
        let mut noise = 0.0f32;
        for (i, p) in powers.iter().enumerate() {
            let sync = usize::from(SYNC_VECTOR[i]);
            sig += p[usize::from(symbols[i])];
            // The two tones of the unused sync parity carry no signal.
            noise += (p[1 - sync] + p[3 - sync]) / 2.0;
        }
        let n = SYMBOL_COUNT as f32;
        let bin_bw = FS_BASE / SYM_LEN as f32; // ≈ 1.4648 Hz
        let n0 = (noise / n / bin_bw).max(f32::MIN_POSITIVE);
        let snr_db = 10.0 * (((sig - noise) / n).max(f32::MIN_POSITIVE) / (n0 * 2500.0)).log10();

        Some(WsprDecode {
            message,
            freq_hz: center + f,
            dt_seconds: (t * HOP) as f32 / FS_BASE,
            snr_db,
            sync_score: quality,
        })
    }
}

/// Fano bit metric for probability `p`: `16 × (log2(2p) − ½)`,
/// clamped to `[-64, 8]`.
fn bit_metric(p: f32) -> i32 {
    let m = 16.0 * ((2.0 * p.max(1e-6)).log2() - 0.5);
    m.clamp(-64.0, 8.0).round() as i32
}

/// Mixes the 12 kHz capture down by `center` Hz and decimates ×32 to
/// 375 Hz complex baseband via two cascaded FIR stages (↓8 then ↓4).
fn decimate(samples: &[i16], center: f32) -> Vec<(f32, f32)> {
    // Stage filters: Blackman-windowed sinc lowpass, sized for a
    // ≤ 100 Hz half-window plus the 4.4 Hz tone span. Stage 1 passes
    // ±150 Hz of 12 kHz (stopband well before the 1385 Hz alias edge).
    // Stage 2's `fc` argument is 180 Hz at 1500 Hz, giving a measured
    // −3 dB edge of 167.5 Hz; the ↓4 folds at 187.5 Hz, where the
    // response is −8.59 dB. Unlike the FT8 cascade, this one is flat
    // across the whole search window: worst case −0.29 dB at the top
    // tone of a ±100 Hz window.
    let taps1 = design_lowpass(31, 150.0 / 12_000.0);
    let taps2 = design_lowpass(63, 180.0 / 1_500.0);

    // Mix to complex baseband: z[n] = x[n] · e^{-j2π·center·n/12000}.
    let step = -2.0 * core::f32::consts::PI * center / INPUT_RATE as f32;
    let mut mixed = Vec::with_capacity(samples.len());
    // Phase accumulated in f64 (from an f32 step) and wrapped by TAU,
    // so the recurrence does not drift over a 114 s capture.
    let mut phase = 0.0f64;
    let step64 = f64::from(step);
    for &s in samples {
        let (sin, cos) = (phase as f32).sin_cos();
        let x = f32::from(s);
        mixed.push((x * cos, x * sin));
        phase += step64;
        if phase < -core::f64::consts::TAU {
            phase += core::f64::consts::TAU;
        }
    }
    let stage1 = fir_decimate(&mixed, &taps1, 8);
    fir_decimate(&stage1, &taps2, 4)
}

/// Blackman-windowed sinc lowpass; `fc` is the cutoff as a fraction of
/// the sample rate. Odd `n`, unity DC gain.
fn design_lowpass(n: usize, fc: f32) -> Vec<f32> {
    let mid = (n - 1) as f32 / 2.0;
    let mut taps: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 - mid;
            let sinc = if t == 0.0 {
                2.0 * fc
            } else {
                (2.0 * core::f32::consts::PI * fc * t).sin() / (core::f32::consts::PI * t)
            };
            let w = 0.42 - 0.5 * (2.0 * core::f32::consts::PI * i as f32 / (n - 1) as f32).cos()
                + 0.08 * (4.0 * core::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
            sinc * w
        })
        .collect();
    let sum: f32 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

/// Filters a complex signal with real taps and keeps every `m`-th
/// output (computed only at kept positions).
fn fir_decimate(z: &[(f32, f32)], taps: &[f32], m: usize) -> Vec<(f32, f32)> {
    let out_len = z.len() / m;
    let mut out = Vec::with_capacity(out_len);
    for k in 0..out_len {
        let end = k * m; // newest input sample of this output
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (j, &t) in taps.iter().enumerate() {
            if let Some(&(zr, zi)) = end.checked_sub(j).and_then(|i| z.get(i)) {
                re += t * zr;
                im += t * zi;
            }
        }
        out.push((re, im));
    }
    out
}

/// Averaged FFT power spectrum, tone-quad comb, local maxima: returns
/// candidate tone-0 frequencies (baseband Hz), strongest first.
fn search_candidates(z: &[(f32, f32)], window: f32) -> Vec<f32> {
    // Average |FFT|² over non-overlapping blocks.
    let blocks = z.len() / FFT_LEN;
    let mut spectrum = vec![0.0f32; FFT_LEN];
    let mut buf = vec![(0.0f32, 0.0f32); FFT_LEN];
    for b in 0..blocks {
        buf.copy_from_slice(&z[b * FFT_LEN..(b + 1) * FFT_LEN]);
        fft_inplace(&mut buf);
        for (s, &(re, im)) in spectrum.iter_mut().zip(buf.iter()) {
            *s += re * re + im * im;
        }
    }
    let bin_hz = FS_BASE / FFT_LEN as f32; // ≈ 0.366 Hz
    let spacing_bins = 4i32; // SPACING / bin_hz, exact
    // Signed bin b (negative = negative baseband frequency) maps to
    // FFT index b mod N.
    let at = |b: i32| spectrum[(b.rem_euclid(FFT_LEN as i32)) as usize];
    let comb = |b: i32| (0..4).map(|k| at(b + k * spacing_bins)).sum::<f32>();
    let lo = (-window / bin_hz).ceil() as i32;
    let hi = (window / bin_hz).floor() as i32;
    let mut peaks: Vec<(f32, i32)> = Vec::new();
    for b in lo..=hi {
        let c = comb(b);
        if c >= comb(b - 1) && c > comb(b + 1) {
            peaks.push((c, b));
        }
    }
    peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
    // Dedupe peaks within one tone spacing of a stronger one.
    let mut picked: Vec<i32> = Vec::new();
    for &(_, b) in &peaks {
        if picked.iter().all(|&p| (p - b).abs() > spacing_bins) {
            picked.push(b);
        }
    }
    picked.into_iter().map(|b| b as f32 * bin_hz).collect()
}

/// In-place iterative radix-2 DIT FFT (hand-rolled, dependency-free).
fn fft_inplace(buf: &mut [(f32, f32)]) {
    let n = buf.len();
    debug_assert!(n.is_power_of_two());
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = (i as u32).reverse_bits() >> (32 - bits);
        let j = j as usize;
        if j > i {
            buf.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * core::f32::consts::PI / len as f32;
        let (wsin, wcos) = ang.sin_cos();
        for start in (0..n).step_by(len) {
            let (mut wr, mut wi) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (ar, ai) = buf[start + k];
                let (br, bi) = buf[start + k + len / 2];
                let tr = br * wr - bi * wi;
                let ti = br * wi + bi * wr;
                buf[start + k] = (ar + tr, ai + ti);
                buf[start + k + len / 2] = (ar - tr, ai - ti);
                let nwr = wr * wcos - wi * wsin;
                wi = wr * wsin + wi * wcos;
                wr = nwr;
            }
        }
        len <<= 1;
    }
}

/// 4-tone DFT powers over a 256-sample window at frequency `f0`
/// (baseband Hz), starting at decimated sample `start`.
fn window_powers(z: &[(f32, f32)], f0: f32, start: usize) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for (k, slot) in out.iter_mut().enumerate() {
        let step = -2.0 * core::f32::consts::PI * (f0 + k as f32 * SPACING) / FS_BASE;
        let (dsin, dcos) = step.sin_cos();
        let (mut wr, mut wi) = (1.0f32, 0.0f32);
        let (mut ar, mut ai) = (0.0f32, 0.0f32);
        for &(zr, zi) in &z[start..(start + SYM_LEN).min(z.len())] {
            ar += zr * wr - zi * wi;
            ai += zr * wi + zi * wr;
            let nwr = wr * dcos - wi * dsin;
            wi = wr * dsin + wi * dcos;
            wr = nwr;
        }
        *slot = ar * ar + ai * ai;
    }
    out
}

/// 4-tone powers on the hop grid (window start every [`HOP`] samples).
fn hop_powers(z: &[(f32, f32)], f0: f32) -> Vec<[f32; 4]> {
    let count = (z.len().saturating_sub(SYM_LEN)) / HOP + 1;
    (0..count).map(|h| window_powers(z, f0, h * HOP)).collect()
}

/// Sync correlation of hop-grid powers starting at hop `t`: the
/// odd/even tone-pair power difference signed by the sync vector.
fn sync_score(hops: &[[f32; 4]], t: usize) -> f32 {
    let per_sym = SYM_LEN / HOP;
    SYNC_VECTOR
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let p = &hops[t + i * per_sym];
            let d = (p[1] + p[3]) - (p[0] + p[2]);
            if s == 1 { d } else { -d }
        })
        .sum()
}

/// Symbol-aligned 4-tone powers for all 162 symbols at offset `start`
/// (decimated samples) and frequency `f0`.
fn symbol_powers(z: &[(f32, f32)], f0: f32, start: usize) -> [[f32; 4]; SYMBOL_COUNT] {
    let mut out = [[0.0f32; 4]; SYMBOL_COUNT];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = window_powers(z, f0, start + i * SYM_LEN);
    }
    out
}

/// Sync correlation over full symbol-aligned powers.
fn sync_score_powers(powers: &[[f32; 4]; SYMBOL_COUNT]) -> f32 {
    powers
        .iter()
        .zip(SYNC_VECTOR.iter())
        .map(|(p, &s)| {
            let d = (p[1] + p[3]) - (p[0] + p[2]);
            if s == 1 { d } else { -d }
        })
        .sum()
}
