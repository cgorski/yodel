//! The std-gated FT8 receive engine: buffered capture → decoded
//! messages.
//!
//! This is the buffer-owning half of the RX split documented on
//! [`crate::ft8`]: everything here allocates (`Vec`) and uses f32
//! transcendentals, so it is compiled only with `ft8` + `std`. The
//! buffer-free math (Gray-demap LLRs, the capped LDPC min-sum decoder,
//! CRC-14 verify, message unpack) lives in the parent module and stays
//! no_std.
//!
//! # Pipeline
//!
//! 1. **Mix + decimate** (12 kHz i16 → 800 Hz complex f32): a complex
//!    mixer at the window center, then two cascaded real-coefficient
//!    FIR decimators (↓5 to 2400 Hz, ↓3 to 800 Hz; Blackman-windowed
//!    sinc, designed at runtime). At 800 Hz a channel symbol is
//!    exactly 128 samples and the 6.25 Hz tone spacing is exactly two
//!    bins of the 256-point search FFT — every grid in the pipeline is
//!    integer.
//! 2. **Candidate search**: a hand-rolled radix-2 FFT (N = 256, bin
//!    width 800/256 = 3.125 Hz — half a tone spacing; a local twin of
//!    the WSPR engine's FFT, kept module-private in both places so the
//!    modes stay decoupled) averaged over the capture; 8-tone comb
//!    power on the 6.25 Hz grid inside the configured window; local
//!    maxima become candidates (best N by comb power).
//! 3. **Costas sync**: per-candidate 8-tone powers on a 32-sample
//!    (40 ms) hop grid, correlated with the three 7-symbol Costas
//!    arrays for coarse time over the whole capture slack (a 15 s
//!    capture leaves ≈ 2.4 s of slide around the 12.64 s
//!    transmission); then a joint fine search over ±2.3 Hz (0.78 Hz
//!    steps, a quarter bin) and ±1 hop.
//! 4. **Demod + decode**: per-symbol coherent 8-bin DFT energies →
//!    per-bit LLRs ([`llrs_from_energies`], max-log Gray demap) →
//!    [`ldpc_decode`] (min-sum, hard iteration cap) → [`verify_crc`] →
//!    [`unpack_message`].
//!
//! # RAM budget (measured)
//!
//! For a 15 s capture at 12 kHz: a padded **owned copy** of the
//! caller's 180 k i16 samples (≈ 368 KB — [`Ft8Decoder::decode`]
//! appends two symbols of silence rather than borrowing the slice), a
//! transient mixed copy at the input rate (≈ 1.5 MB complex f32,
//! freed after decimation), a ≈ 294 KB transient at the 2400 Hz
//! intermediate rate, and the persistent 800 Hz baseband of ≈ 12.3 k
//! complex f32 ≈ **98 KB** plus ≈ 12 KB of hop-grid powers per
//! candidate. Measured peak heap for such a capture is 2 231 032 B ≈
//! **2.13 MiB**. This is a workstation engine; only the decode math in
//! the parent module fits the ~3 KB embedded class.
//!
//! Sensitivity is measured, not assumed: the reference implementation
//! decodes FT8 down to roughly −21 dB SNR in 2500 Hz — that
//! figure belongs to that decoder. This engine's pinned test floor is
//! −14 dB SNR in 2500 Hz (see `tests/ft8_rx.rs`).

use core::fmt;

use super::{
    CODEWORD_LEN, COSTAS, Ft8Text, SYMBOL_COUNT, ldpc_decode, llrs_from_energies,
    message_from_codeword, unpack_message, verify_crc,
};

/// Decimated (baseband) sample rate in Hz.
const FS_BASE: f32 = 800.0;

/// Samples per channel symbol at the decimated rate (0.16 s).
const SYM_LEN: usize = 128;

/// Hop between sync-search windows, in decimated samples (¼ symbol).
const HOP: usize = 32;

/// FFT length of the candidate search (bin = 800/256 = 3.125 Hz,
/// exactly half the 6.25 Hz tone spacing).
const FFT_LEN: usize = 256;

/// Tone spacing in Hz.
const SPACING: f32 = 6.25;

/// The only capture sample rate the engine accepts.
const INPUT_RATE: u32 = 12_000;

/// Total decimation factor (12 kHz → 800 Hz).
const DECIM: usize = 15;

/// Decimated samples a full transmission spans (79 × 128).
const TX_LEN: usize = SYMBOL_COUNT * SYM_LEN;

/// Errors from decoder configuration or capture validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ft8RxError {
    /// The search window is invalid: the half-width must be within
    /// `50..=300` Hz, the center must be strictly greater than the
    /// half-width (so tone 0 at the bottom of the window stays above
    /// 0 Hz), and the top of the window (center + window + seven tone
    /// spacings) must stay below the 6 kHz Nyquist limit.
    WindowInvalid {
        /// The requested window center in Hz.
        center_hz: u32,
        /// The requested half-width in Hz.
        window_hz: u32,
    },
    /// `max_candidates` is outside `1..=32`.
    CandidatesInvalid {
        /// The rejected count.
        got: usize,
    },
    /// The capture is shorter than one full transmission.
    CaptureTooShort {
        /// Samples supplied.
        got: usize,
        /// Samples required (one transmission at 12 kHz).
        need: usize,
    },
}

impl fmt::Display for Ft8RxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WindowInvalid {
                center_hz,
                window_hz,
            } => write!(
                f,
                "search window {center_hz}±{window_hz} Hz is invalid: half-width must be \
                 50..=300 Hz and all eight tones must stay within (0, 6000) Hz"
            ),
            Self::CandidatesInvalid { got } => {
                write!(f, "max candidates {got} is out of range: must be 1..=32")
            }
            Self::CaptureTooShort { got, need } => write!(
                f,
                "capture of {got} samples is too short: a full FT8 transmission needs \
                 {need} samples at 12000 Hz (~12.64 s)"
            ),
        }
    }
}

impl std::error::Error for Ft8RxError {}

/// A validated receive configuration: search window and candidate
/// budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ft8DecoderConfig {
    center_hz: u32,
    window_hz: u32,
    max_candidates: usize,
}

impl Ft8DecoderConfig {
    /// Creates a configuration searching tone-0 frequencies within
    /// `center_hz ± window_hz` (audio Hz; a common workstation default
    /// is 1500 ± 300, covering 1200–1800 Hz of the audio passband).
    ///
    /// # Errors
    ///
    /// [`Ft8RxError::WindowInvalid`] when the half-width is outside
    /// `50..=300` Hz, the center is not above the half-width, or the
    /// top tone of the window would reach the 6 kHz Nyquist limit.
    /// Note that the decimation cascade is not flat across a full
    /// ±300 Hz window: it measures ≈ 3 dB down at ±300 Hz and ≈ 5 dB
    /// down at the highest tone of an edge-of-window signal, so wide
    /// windows trade sensitivity at the edges.
    pub fn new(center_hz: u32, window_hz: u32) -> Result<Self, Ft8RxError> {
        let err = Ft8RxError::WindowInvalid {
            center_hz,
            window_hz,
        };
        if !(50..=300).contains(&window_hz) {
            return Err(err);
        }
        // Highest tone: center + window + 7 spacings; must stay below
        // Nyquist. Lowest tone-0: center - window, above 0.
        let top = f64::from(center_hz) + f64::from(window_hz) + 7.0 * f64::from(SPACING);
        if center_hz <= window_hz || top >= f64::from(INPUT_RATE) / 2.0 {
            return Err(err);
        }
        Ok(Self {
            center_hz,
            window_hz,
            max_candidates: 6,
        })
    }

    /// Sets how many decodes a capture may return (best candidates by
    /// comb power first; default 6).
    ///
    /// This caps *successful decodes*, not demodulation attempts: the
    /// candidate loop stops early only once that many results are in
    /// hand, so a capture with nothing decodable in it always walks
    /// every candidate. Worst case (pure noise) is therefore
    /// independent of the setting — ≈ 157 ms per capture at 1, 6 or 32
    /// candidates, versus 5.5–20 ms when a clean signal decodes
    /// immediately.
    ///
    /// # Errors
    ///
    /// [`Ft8RxError::CandidatesInvalid`] outside `1..=32`.
    pub fn max_candidates(mut self, n: usize) -> Result<Self, Ft8RxError> {
        if !(1..=32).contains(&n) {
            return Err(Ft8RxError::CandidatesInvalid { got: n });
        }
        self.max_candidates = n;
        Ok(self)
    }
}

/// One decoded transmission from a capture.
#[derive(Debug, Clone, PartialEq)]
pub struct Ft8Decode {
    /// The rendered message text (e.g. `"CQ K1ABC FN42"`).
    pub message: Ft8Text,
    /// The recovered 77-bit payload (MSB-first, left-justified).
    pub payload: [u8; super::PAYLOAD_LEN],
    /// Measured tone-0 audio frequency in Hz.
    pub freq_hz: f32,
    /// Measured start of the transmission within the capture, in
    /// seconds. Quantized to the 40 ms sync-hop grid (the resolution
    /// of the time search) and including ≈ 15 ms of decimation-filter
    /// group delay.
    pub dt_seconds: f32,
    /// Estimated SNR in the 2500 Hz reference bandwidth, dB. An
    /// estimate from the per-symbol tone/off-tone energy ratio — a
    /// quality metric, not a calibrated measurement.
    pub snr_db: f32,
    /// Normalized Costas sync correlation (higher is a cleaner
    /// alignment). Each Costas symbol contributes `hit − (rest)/7`,
    /// normalized by the total Costas energy: 0 is uncorrelated noise
    /// and 1.0 is the noiseless ceiling. Amplitude-invariant; a clean
    /// signal measures ≈ 0.988.
    pub sync_score: f32,
}

/// The buffered FT8 receive engine (`ft8` + `std` only).
///
/// Feed [`Ft8Decoder::decode`] a whole capture — at least one full
/// transmission (151 680 samples ≈ 12.64 s; a 15 s cycle capture gives
/// the sync search its full slide room) of 16-bit mono PCM at
/// **12 kHz** (the engine is fixed-rate by design, mirroring the exact
/// symbol-timing rule of the modulator). Returns every message it
/// could decode, best candidates first, deduplicated by payload.
#[derive(Debug, Clone)]
pub struct Ft8Decoder {
    config: Ft8DecoderConfig,
}

impl Ft8Decoder {
    /// Creates a decoder from a validated configuration.
    #[must_use]
    pub fn new(config: Ft8DecoderConfig) -> Self {
        Self { config }
    }

    /// Decodes every FT8 transmission found in `samples` (16-bit mono
    /// PCM at 12 kHz), returning up to `max_candidates` results sorted
    /// by candidate strength.
    ///
    /// # Errors
    ///
    /// [`Ft8RxError::CaptureTooShort`] when fewer than 151 680 samples
    /// (one full transmission) are supplied. Per-candidate failures
    /// (LDPC cap, CRC mismatch, unsupported payload type) are not
    /// errors: those candidates are simply absent from the result.
    ///
    /// # Known blind spot
    ///
    /// The all-zero 77-bit payload is never reported: it is a valid
    /// codeword with CRC 0 and a degenerate fixed point the LDPC
    /// decoder converges to on pure noise, so it is rejected as a
    /// candidate. That makes `Ft8Message::free_text("")` — and an
    /// all-spaces free-text message, which packs to the same payload —
    /// undecodable by this engine, however clean the signal. Any
    /// message with a non-blank payload decodes normally.
    pub fn decode(&self, samples: &[i16]) -> Result<Vec<Ft8Decode>, Ft8RxError> {
        let need = TX_LEN * DECIM;
        if samples.len() < need {
            return Err(Ft8RxError::CaptureTooShort {
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
        let mut results: Vec<Ft8Decode> = Vec::new();
        for cand in search_candidates(&baseband, self.config.window_hz as f32) {
            if results.len() >= self.config.max_candidates {
                break;
            }
            // Skip candidates within one tone spacing of an already
            // decoded signal.
            if results
                .iter()
                .any(|r| (r.freq_hz - (center + cand)).abs() < SPACING)
            {
                continue;
            }
            if let Some(decode) = self.try_candidate(&baseband, cand, center)
                && !results.iter().any(|r| r.payload == decode.payload)
            {
                results.push(decode);
            }
        }
        Ok(results)
    }

    /// Aligns, demodulates and decodes one candidate frequency
    /// (baseband Hz). `None` on any failure: weak sync, LDPC cap,
    /// CRC mismatch, unsupported payload type.
    fn try_candidate(&self, z: &[(f32, f32)], f0: f32, center: f32) -> Option<Ft8Decode> {
        // Coarse time sync: 8-tone energies on the hop grid,
        // correlated with the three Costas arrays.
        let hops = hop_energies(z, f0);
        let max_start = hops.len().checked_sub(SYMBOL_COUNT * (SYM_LEN / HOP))?;
        let (mut best_t, mut best_score) = (0usize, f32::MIN);
        for t in 0..=max_start {
            let s = costas_score_hops(&hops, t);
            if s > best_score {
                best_score = s;
                best_t = t;
            }
        }
        // Joint fine search: ±1 hop, ±2.34 Hz in 0.78 Hz steps (a
        // quarter of the search-FFT bin), maximizing the Costas
        // correlation on full-precision symbol energies.
        let mut best: Option<(Vec<[f32; 8]>, f32, usize, f32)> = None;
        for dt in -1isize..=1 {
            let Some(t) = best_t.checked_add_signed(dt) else {
                continue;
            };
            if t > max_start {
                continue;
            }
            for step in -3i32..=3 {
                let f = f0 + step as f32 * (FS_BASE / FFT_LEN as f32 / 4.0);
                let energies = symbol_energies(z, f, t * HOP);
                let score = costas_score_symbols(&energies);
                if best.as_ref().is_none_or(|b| score > b.1) {
                    best = Some((energies, score, t, f));
                }
            }
        }
        let (energies, score, t, f) = best?;
        // Normalized sync quality: the per-symbol `hit − (rest)/7`
        // correlation over the total Costas energy, so the scale runs
        // from 0 (uncorrelated noise) to 1.0 (noiseless). Measured
        // ≈ 0.988 on a clean signal, amplitude-invariant.
        let costas_total: f32 = costas_positions()
            .map(|(pos, _)| energies[pos].iter().sum::<f32>())
            .sum::<f32>()
            .max(f32::MIN_POSITIVE);
        let quality = score / costas_total;

        // Data-symbol energies → LLRs → LDPC → CRC → unpack.
        let mut data = [[0.0f32; 8]; 58];
        for (j, slot) in data.iter_mut().enumerate() {
            let position = if j < 29 { 7 + j } else { 43 + (j - 29) };
            *slot = energies[position];
        }
        let llr = llrs_from_energies(&data);
        let codeword = ldpc_decode(&llr).ok()?;
        let message = message_from_codeword(&codeword);
        let payload = verify_crc(&message).ok()?;
        // The all-zero payload is a valid codeword with CRC 0 (a
        // degenerate fixed point weak candidates can converge to on
        // pure noise): never report it as a decode.
        if payload.iter().all(|&b| b == 0) {
            return None;
        }
        let text = unpack_message(&payload).ok()?;

        // SNR estimate: decoded-tone energy vs off-tone energy (noise
        // per 6.25 Hz coherent bin), referred to 2500 Hz.
        let symbols = symbols_of(&codeword);
        let mut sig = 0.0f32;
        let mut noise = 0.0f32;
        for (i, e) in energies.iter().enumerate() {
            let tone = usize::from(symbols[i]);
            sig += e[tone];
            let off: f32 = e.iter().sum::<f32>() - e[tone];
            noise += off / 7.0;
        }
        let n = SYMBOL_COUNT as f32;
        let bin_bw = FS_BASE / SYM_LEN as f32; // 6.25 Hz
        let n0 = (noise / n / bin_bw).max(f32::MIN_POSITIVE);
        let snr_db = 10.0 * (((sig - noise) / n).max(f32::MIN_POSITIVE) / (n0 * 2500.0)).log10();

        Some(Ft8Decode {
            message: text,
            payload,
            freq_hz: center + f,
            dt_seconds: (t * HOP) as f32 / FS_BASE,
            snr_db,
            sync_score: quality,
        })
    }
}

/// The 79 channel symbols of a decoded codeword (for the SNR metric).
fn symbols_of(codeword: &[u8; CODEWORD_LEN]) -> [u8; SYMBOL_COUNT] {
    super::symbols_from_codeword(codeword)
}

/// The 21 Costas (position, expected tone) pairs.
fn costas_positions() -> impl Iterator<Item = (usize, u8)> {
    [0usize, 36, 72]
        .into_iter()
        .flat_map(|base| COSTAS.iter().enumerate().map(move |(i, &t)| (base + i, t)))
}

/// Mixes the 12 kHz capture down by `center` Hz and decimates ×15 to
/// 800 Hz complex baseband via two cascaded FIR stages (↓5 then ↓3).
fn decimate(samples: &[i16], center: f32) -> Vec<(f32, f32)> {
    // Stage filters: Blackman-windowed sinc lowpass, sized for a
    // ≤ 300 Hz half-window plus the 43.75 Hz tone span. The `fc`
    // arguments are 380 Hz at 12 kHz and 385 Hz at 2400 Hz; the
    // measured −3 dB edges are 298 Hz (stage 1) and 365 Hz (stage 2).
    // Stage 1's stopband sits well before the 2050 Hz alias edge of
    // the ↓5; the ↓3 folds at 400 Hz, where the cascade is −9.34 dB.
    //
    // The cascade is *not* flat across the ±300 Hz search window.
    // Measured cascade response versus offset from the window center:
    // 0.00 dB at 0 Hz, −0.33 dB at 100 Hz, −1.33 dB at 200 Hz,
    // −3.09 dB at 300 Hz and −5.23 dB at 343.75 Hz (the top tone of a
    // signal parked at the edge of a ±300 Hz window). Edge-of-window
    // signals therefore give up ≈ 5 dB of sensitivity relative to
    // center.
    let taps1 = design_lowpass(47, 380.0 / 12_000.0);
    let taps2 = design_lowpass(63, 385.0 / 2_400.0);

    // Mix to complex baseband: z[n] = x[n] · e^{-j2π·center·n/12000}.
    let step64 = -2.0 * core::f64::consts::PI * f64::from(center) / f64::from(INPUT_RATE);
    let mut mixed = Vec::with_capacity(samples.len());
    // Phase recurrence in f64 with wrap to avoid drift.
    let mut phase = 0.0f64;
    for &s in samples {
        let (sin, cos) = (phase as f32).sin_cos();
        let x = f32::from(s);
        mixed.push((x * cos, x * sin));
        phase += step64;
        if phase < -core::f64::consts::TAU {
            phase += core::f64::consts::TAU;
        }
    }
    let stage1 = fir_decimate(&mixed, &taps1, 5);
    fir_decimate(&stage1, &taps2, 3)
}

/// Blackman-windowed sinc lowpass; `fc` is the cutoff as a fraction of
/// the sample rate. Odd `n`, unity DC gain. (A local twin of the WSPR
/// engine's designer — kept private in both modules on purpose.)
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

/// Averaged FFT power spectrum, 8-tone comb, local maxima: returns
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
    let bin_hz = FS_BASE / FFT_LEN as f32; // 3.125 Hz
    let spacing_bins = 2i32; // SPACING / bin_hz, exact
    // Signed bin b (negative = negative baseband frequency) maps to
    // FFT index b mod N.
    let at = |b: i32| spectrum[(b.rem_euclid(FFT_LEN as i32)) as usize];
    let comb = |b: i32| (0..8).map(|k| at(b + k * spacing_bins)).sum::<f32>();
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

/// In-place iterative radix-2 DIT FFT (hand-rolled, dependency-free; a
/// local twin of the WSPR engine's — both are module-private).
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

/// 8-tone DFT energies over a 128-sample window at frequency `f0`
/// (baseband Hz), starting at decimated sample `start`.
fn window_energies(z: &[(f32, f32)], f0: f32, start: usize) -> [f32; 8] {
    let mut out = [0.0f32; 8];
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

/// 8-tone energies on the hop grid (window start every [`HOP`]
/// samples).
fn hop_energies(z: &[(f32, f32)], f0: f32) -> Vec<[f32; 8]> {
    let count = (z.len().saturating_sub(SYM_LEN)) / HOP + 1;
    (0..count)
        .map(|h| window_energies(z, f0, h * HOP))
        .collect()
}

/// Costas correlation of hop-grid energies starting at hop `t`: the
/// expected-tone energy minus the mean off-tone energy, summed over
/// the 21 sync symbols.
fn costas_score_hops(hops: &[[f32; 8]], t: usize) -> f32 {
    let per_sym = SYM_LEN / HOP;
    costas_positions()
        .map(|(pos, tone)| {
            let e = &hops[t + pos * per_sym];
            let sum: f32 = e.iter().sum();
            let hit = e[usize::from(tone)];
            hit - (sum - hit) / 7.0
        })
        .sum()
}

/// Symbol-aligned 8-tone energies for all 79 symbols at offset `start`
/// (decimated samples) and frequency `f0`.
fn symbol_energies(z: &[(f32, f32)], f0: f32, start: usize) -> Vec<[f32; 8]> {
    (0..SYMBOL_COUNT)
        .map(|i| window_energies(z, f0, start + i * SYM_LEN))
        .collect()
}

/// Costas correlation over full symbol-aligned energies.
fn costas_score_symbols(energies: &[[f32; 8]]) -> f32 {
    costas_positions()
        .map(|(pos, tone)| {
            let e = &energies[pos];
            let sum: f32 = e.iter().sum();
            let hit = e[usize::from(tone)];
            hit - (sum - hit) / 7.0
        })
        .sum()
}
