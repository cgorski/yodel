//! Tone discrimination: soft mark/space decisions from raw PCM.
//!
//! # Quadrature correlation
//!
//! An FSK receiver must decide, sample by sample, which of two tones
//! dominates the input. A phase-independent way to measure the
//! energy of a single tone of frequency `f` in a window of `N` samples
//! `x[k]` is **quadrature correlation**: correlate the window against both
//! a sine and a cosine reference at `f`,
//!
//! ```text
//! I = Σ x[k]·cos(2π·f·k/Fs)      Q = Σ x[k]·sin(2π·f·k/Fs)
//! ```
//!
//! and form the envelope `E = I² + Q²`. Because sine and cosine span every
//! possible phase of the tone, `E` is independent of the (unknown) phase of
//! the incoming tone — it is a single-bin sliding discrete Fourier
//! magnitude. The window length defaults to one bit period, the shortest
//! time a tone is guaranteed to persist: shorter windows lose selectivity,
//! longer windows smear bit transitions.
//!
//! # Observation window and tone orthogonality
//!
//! One bit period is not always the right window, because two tones are
//! only *orthogonal* under non-coherent detection when the frequency
//! shift `Δf` and the observation time `T` satisfy `Δf·T ∈ ℤ`. Off that
//! grid the two correlators leak into each other by
//!
//! ```text
//! ρ(h) = |sin(πh) / (πh)|,   h = Δf·T   (the modulation index at T)
//! ```
//!
//! and that crosstalk is a noise term no amount of averaging removes.
//! Over a one-bit window `h` is just `Δf / baud`:
//!
//! | profile | Δf | baud | `h` | crosstalk `ρ` |
//! |---|---|---|---|---|
//! | Bell 202 | 1000 Hz | 1200 | 0.833 | 0.191 |
//! | HF APRS 300 | 200 Hz | 300 | 0.667 | **0.413** |
//! | Bell 103 | 200 Hz | 300 | 0.667 | **0.413** |
//!
//! Bell 202 is nearly orthogonal already; the 300-baud profiles are not,
//! and pay for it heavily. [`QuadratureCorrelator::new`] therefore
//! stretches the window to the shortest whole multiple of `1/Δf` that
//! still covers a bit — driving `ρ` to zero — but only when the one-bit
//! crosstalk is bad enough to be worth the extra transition smearing.
//!
//! That trade-off is **measured, not assumed**. Against a
//! reference-generated 100-frame increasing-noise ramp:
//!
//! | window | 300 Bd (ρ = 0.413) | 1200 Bd (ρ = 0.191) |
//! |---|---|---|
//! | 1.00 bit | 58 | **74** |
//! | 1.20 bit | 71 | 73 |
//! | 1.33 bit | 73 | 69 |
//! | **1.50 bit** | **75** | 65 |
//! | 2.00 bit | 63 | 28 |
//!
//! At 300 baud the optimum is exactly the orthogonal point (1.5 bits =
//! 5 ms = 1/200 Hz) and is worth +17 frames. At 1200 baud the orthogonal
//! point (1.2 bits) is *not* an improvement — there was only ρ = 0.191 to
//! recover, and it costs more in smearing than it returns — so Bell 202
//! keeps its one-bit window and is bit-identical to before.
//!
//! The default [`QuadratureCorrelator`] runs one such correlator pair for
//! the mark tone and one for the space tone and reports the signed metric
//! `mark_env − space_env` (scaled): positive means "mark", negative means
//! "space", and the magnitude is a confidence that a bit-clock recovery
//! loop can use to detect transitions.
//!
//! The sliding window is updated incrementally: each new sample's
//! contribution is added to the running `I`/`Q` sums and the contribution
//! of the sample that just left the window is subtracted. Per-sample
//! reference values come from the shared compile-time sine table (cosine is
//! the same table read a quarter turn ahead), so the i16 path needs no
//! floating point at all — products are accumulated in `i64`, which cannot
//! overflow (40 samples · 2¹⁵ · 2¹⁵ ≪ 2⁶³).
//!
//! Alternative front ends (e.g. a delay-line multiplier FM detector) can be
//! plugged in by implementing [`Discriminator`].

use crate::error::ConfigError;
use crate::types::{BaudRate, SampleRate, TonePair, phase_increment, sine_at};

/// Maximum window length in samples: the orthogonal 1.5-bit window at
/// the highest supported rate (48 000 Hz) and the lowest preset baud
/// (300 Bd → 48 000 / 200 Hz shift = 240). The window in use is always
/// derived from the rate, baud and tone shift and only capped here;
/// Bell 202 at 48 kHz uses 40 samples exactly as before.
///
/// Raising this from one bit period (160) costs every correlator
/// `(240 − 160)·16 = 1280` bytes of window history. `TncReceiver` holds
/// three two-tone banks, so its footprint grows by ~7.7 KiB; see the
/// RAM table in `README.md`.
pub const MAX_WINDOW: usize = 240;

/// Longest window for which [`ToneCorrelator::scale`]'s multiply-shift
/// is provably exact, from its own bound `n²·2²⁴ < 2³⁹` ⇒ `n ≤ 181`.
/// Longer windows (only the stretched 300-baud ones reach here) take a
/// real division instead, which is the operation the fast path is
/// documented to be bit-identical to.
const RECIP_MAX_LEN: usize = 181;

/// The correlator observation window, in samples, for a given rate,
/// baud and tone shift.
///
/// Returns the shortest whole multiple of `1/shift` that covers at least
/// one bit period — the shortest window over which the two tones are
/// mutually orthogonal — or `None` to keep the plain one-bit window,
/// which happens when either:
///
/// * the one-bit crosstalk `ρ(Δf/baud)` is already small. `ρ` decreases
///   monotonically on `h ∈ (0, 1]`, so thresholding `ρ > 0.3` is exactly
///   thresholding `h < 0.75`, i.e. `4·shift < 3·baud`. Bell 202's 0.833
///   sits above the line and is left alone; the 300-baud profiles' 0.667
///   sits below it. See the module docs for the measurements behind the
///   0.3 threshold.
/// * the orthogonal window would exceed [`MAX_WINDOW`]. Every supported
///   rate keeps the 300-baud presets inside it (`8000/200 = 40` through
///   `48000/200 = 240`), but a caller-built [`TonePair`] with a very
///   narrow shift can ask for more than fits, and a truncated window
///   would not be orthogonal anyway.
fn orthogonal_window(sr: u32, bd: u32, shift: u32, samples_per_bit: usize) -> Option<usize> {
    if shift == 0 || 4 * u64::from(shift) >= 3 * u64::from(bd) {
        return None;
    }
    // Samples in 1/shift seconds, rounded to nearest.
    let period = ((u64::from(sr) + u64::from(shift) / 2) / u64::from(shift)) as usize;
    if period == 0 {
        return None;
    }
    let window = period * samples_per_bit.div_ceil(period);
    (window <= MAX_WINDOW).then_some(window)
}

/// A pluggable tone discriminator: PCM samples in, soft decisions out.
///
/// Implementations consume one sample at a time and return a signed metric:
/// **positive** means the mark tone (logical one) dominates, **negative**
/// means the space tone (logical zero) dominates, and the magnitude grows
/// with confidence. The metric scale is implementation-defined but must be
/// consistent over time so zero crossings mark tone transitions.
///
/// Implementations should return a metric whose **zero-crossing rate**
/// is dominated by real tone transitions rather than by noise, because
/// the bit-clock loop downstream ([`crate::Slicer`]) retimes on every
/// sign change. A metric that is only low-noise is not sufficient:
/// crossing rate depends on the second spectral moment, not the noise
/// power, so an unfiltered statistic can be accurate in amplitude and
/// still be unusable as a timing reference. [`QuadratureCorrelator`]
/// low-pass filters its tone envelopes for exactly this reason — see
/// the measurements on its `push`.
///
/// [`QuadratureCorrelator`] returns an amplitude-domain difference,
/// which does not saturate anywhere in the supported input range.
/// (Before the envelope smoothing was applied to this path it returned
/// a power-domain difference that pinned at `i32::MAX` above ≈ 35.7% of
/// full scale; that limitation is gone.)
pub trait Discriminator {
    /// Pushes one i16 PCM sample and returns the updated soft metric.
    fn push_i16(&mut self, sample: i16) -> i32;

    /// Pushes one f32 PCM sample (nominal range `[-1.0, 1.0]`) and returns
    /// the updated soft metric.
    fn push_f32(&mut self, sample: f32) -> i32;
}

/// One sliding-window quadrature correlator tuned to a single tone.
///
/// Keeps the last `len` samples in a fixed ring buffer together with
/// running in-phase (`i_sum`) and quadrature (`q_sum`) correlation sums;
/// see the module docs for the math. Sample values are stored as `i32`
/// (i16 PCM directly; f32 PCM scaled to the same range) so both paths share
/// one integer engine.
#[derive(Debug, Clone)]
struct ToneCorrelator {
    /// Ring buffer of per-sample (in-phase, quadrature) contributions.
    window: [(i64, i64); MAX_WINDOW],
    /// Number of valid entries (window length = samples per bit).
    len: usize,
    /// Next write position in `window`.
    pos: usize,
    /// Reference oscillator phase (u32 turns, as in the modulator).
    phase: u32,
    /// Per-sample reference phase increment for this tone.
    phase_inc: u32,
    /// Running Σ x[k]·sin(ref) over the window.
    i_sum: i64,
    /// Running Σ x[k]·cos(ref) over the window.
    q_sum: i64,
    /// Round-up fixed-point reciprocal of `len`:
    /// `ceil(2^RECIP_SHIFT / len)`, used to strength-reduce the
    /// per-sample normalization division to a multiply-shift (see
    /// [`ToneCorrelator::scale`] for the exactness proof).
    recip: u64,
}

impl ToneCorrelator {
    /// Quarter turn in u32 phase units: sin(φ + τ/4) = cos(φ).
    const QUARTER_TURN: u32 = 1 << 30;

    /// Fixed-point shift of the window-length reciprocal. Chosen so the
    /// multiply-shift in [`ToneCorrelator::scale`] is exact over the
    /// full operand range (proof there) while the intermediate product
    /// stays inside `u64`.
    const RECIP_SHIFT: u32 = 39;

    fn new(tone_hz: u32, sample_rate: u32, len: usize) -> Self {
        let len = len.clamp(1, MAX_WINDOW);
        Self {
            window: [(0, 0); MAX_WINDOW],
            len,
            pos: 0,
            phase: 0,
            phase_inc: phase_increment(tone_hz, sample_rate),
            i_sum: 0,
            q_sum: 0,
            // Zero marks "too long for the fast path" (see RECIP_MAX_LEN).
            recip: if len <= RECIP_MAX_LEN {
                (1u64 << Self::RECIP_SHIFT).div_ceil(len as u64)
            } else {
                0
            },
        }
    }

    /// Truncating division of a correlation sum by `len·256`, computed
    /// as a shift plus one multiply-shift — no hardware division on the
    /// per-sample path.
    ///
    /// # Exactness proof (bit-identical to `sum / (len·256)`)
    ///
    /// Truncating signed division is sign·floor on the magnitude, and
    /// for unsigned values `floor(floor(a/256)/n) = floor(a/(256·n))`,
    /// so `mag >> 8` handles the 256 factor exactly. For the remaining
    /// division by `n = len`: with `m = ceil(2^39/n) = (2^39 + e)/n`
    /// (`0 ≤ e < n`), `(x·m) >> 39` equals `floor(x/n)` whenever
    /// `x·e < 2^39`. Input samples are bounded by 2^17 (raw/i16 paths
    /// clamp at 2^15; the TNC's band-passed and pre-emphasized taps
    /// stay under 4× that) and references by 2^15, so
    /// `|sum| < len·2^32`, `x = mag>>8 < n·2^24`, and
    /// `x·e < n²·2^24 ≤ 181²·2^24 < 2^39`. The product
    /// `x·m < n·2^24·(2^39/n + 1) < 2^63 + 2^32` cannot overflow `u64`.
    ///
    /// That bound is why the fast path stops at [`RECIP_MAX_LEN`] = 181:
    /// beyond it `n²·2^24` crosses `2^39`, and raising the shift to
    /// compensate would push the product past `u64`. The stretched
    /// windows [`orthogonal_window`] produces for the 300-baud profiles
    /// are the only ones that get there, and they divide instead — the
    /// per-sample cost lands on the profiles that gained 17 frames from
    /// the longer window, never on Bell 202.
    #[inline]
    fn scale(&self, sum: i64) -> i64 {
        let x = sum.unsigned_abs() >> 8;
        let q = if self.recip != 0 {
            (x.wrapping_mul(self.recip) >> Self::RECIP_SHIFT) as i64
        } else {
            (x / self.len as u64) as i64
        };
        if sum < 0 { -q } else { q }
    }

    /// Slides the window by one sample and returns the tone envelope
    /// `I² + Q²`, right-shifted to a compact range.
    fn push(&mut self, sample: i32) -> i64 {
        let sin_ref = sine_at(self.phase) as i64;
        let cos_ref = sine_at(self.phase.wrapping_add(Self::QUARTER_TURN)) as i64;
        self.phase = self.phase.wrapping_add(self.phase_inc);

        let contrib = ((sample as i64) * sin_ref, (sample as i64) * cos_ref);
        if let Some(slot) = self.window.get_mut(self.pos) {
            let (old_i, old_q) = *slot;
            self.i_sum = self.i_sum.wrapping_sub(old_i).wrapping_add(contrib.0);
            self.q_sum = self.q_sum.wrapping_sub(old_q).wrapping_add(contrib.1);
            *slot = contrib;
        }
        // Conditional wrap instead of `%`: `pos` is always `< len`.
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }

        // Normalize by the window length before squaring so the envelope
        // scale is rate-independent, and drop the reference amplitude
        // (2¹⁵) so the result fits comfortably in i64/i32 ranges. The
        // division is strength-reduced to a multiply-shift, proven
        // bit-exact in [`ToneCorrelator::scale`].
        let i_n = self.scale(self.i_sum);
        let q_n = self.scale(self.q_sum);
        i_n.saturating_mul(i_n)
            .saturating_add(q_n.saturating_mul(q_n))
    }
}

/// Default [`Discriminator`]: a dual-tone quadrature correlator bank.
///
/// See the module docs for the underlying math. Construct one with
/// [`QuadratureCorrelator::new`]; the window length is one bit period
/// (`sample_rate / baud` samples, capped at [`MAX_WINDOW`]).
///
/// # Metric sign convention
///
/// After roughly one bit period of settling, a pure mark tone drives the
/// metric positive and a pure space tone drives it negative — the
/// invariant every bit-clock recovery loop above relies on:
///
/// ```
/// use yodel::{BaudRate, Bit, Discriminator, Modulator, ModulatorConfig,
///              QuadratureCorrelator, SampleRate};
///
/// let sr = SampleRate::new(48_000)?;
/// let baud = BaudRate::new(1_200)?;
/// let config = ModulatorConfig::bell_202(sr)?;
/// let mut disc = QuadratureCorrelator::new(sr, baud, config.tones())?;
///
/// // Three bit periods (120 samples) of the 1200 Hz mark tone: the
/// // sliding one-bit window fills and the metric settles positive.
/// let mut metric = 0;
/// for s in Modulator::new(config).i16_samples([Bit::One; 3].into_iter()) {
///     metric = disc.push_i16(s);
/// }
/// assert!(metric > 0, "mark tone must give a positive metric: {metric}");
///
/// // A fresh bank fed the 2200 Hz space tone settles negative.
/// let mut disc = QuadratureCorrelator::new(sr, baud, config.tones())?;
/// for s in Modulator::new(config).i16_samples([Bit::Zero; 3].into_iter()) {
///     metric = disc.push_i16(s);
/// }
/// assert!(metric < 0, "space tone must give a negative metric: {metric}");
/// # Ok::<(), yodel::ConfigError>(())
/// ```
#[derive(Debug, Clone)]
pub struct QuadratureCorrelator {
    mark: ToneCorrelator,
    space: ToneCorrelator,
    /// One-pole smoothed amplitude-scale (√power) tone envelopes, used
    /// only by the envelope tap below.
    mark_env: i64,
    space_env: i64,
    /// Second smoothing stage (cascaded one-pole) for the envelope tap.
    mark_env2: i64,
    space_env2: i64,
    /// Envelope smoothing shift, scaled with the bit period so the filter
    /// time constant (2^shift samples) stays a fraction of a bit at every
    /// sample rate.
    env_shift: u32,
}

impl QuadratureCorrelator {
    /// Builds a correlator bank for the given rates and tone pair.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when the sample rate
    /// yields fewer than 2 samples per bit, making tone decisions
    /// meaningless.
    pub fn new(
        sample_rate: SampleRate,
        baud: BaudRate,
        tones: TonePair,
    ) -> Result<Self, ConfigError> {
        let sr = sample_rate.hz();
        let bd = baud.bps();
        let samples_per_bit = (sr / bd) as usize;
        if samples_per_bit < 2 {
            return Err(ConfigError::BaudExceedsSampleRate {
                baud: bd,
                sample_rate: sr,
            });
        }
        let shift = tones.space_hz().abs_diff(tones.mark_hz());
        let len = orthogonal_window(sr, bd, shift, samples_per_bit)
            .unwrap_or(samples_per_bit)
            .min(MAX_WINDOW);
        // Each of the two cascaded smoothing poles has a time constant of
        // 2^env_shift samples, so the constant tops out at 8 samples
        // (>>3 from 32 samples per bit upwards: a fifth of a bit at 40
        // samples per bit, a smaller fraction above that) and fades to no
        // smoothing below 16 samples per bit, where any lag would smear
        // the bit transitions the clock recovery needs.
        //
        // Scaled by the BIT period, not by `len`: the lag that matters is
        // the one measured against bit transitions, so a window stretched
        // for tone orthogonality must not also slow the smoother.
        let env_shift = match samples_per_bit {
            0..16 => 0,
            16..24 => 1,
            24..32 => 2,
            _ => 3,
        };
        Ok(Self {
            mark: ToneCorrelator::new(tones.mark_hz(), sr, len),
            space: ToneCorrelator::new(tones.space_hz(), sr, len),
            mark_env: 0,
            space_env: 0,
            mark_env2: 0,
            space_env2: 0,
            env_shift,
        })
    }

    /// Slides both tone correlators by one sample and returns the
    /// smoothed `(mark, space)` amplitude envelopes.
    ///
    /// This is the tap consumed by the parallel slicer bank in
    /// [`crate::tnc::TncReceiver`]: each of its decision chains compares
    /// `mark` against a differently scaled `space`, so the raw per-tone
    /// amplitudes must be exposed before any comparison. Amplitude scale
    /// (√ of the correlator power) keeps a channel tilt at its true dB
    /// amount rather than twice it, and a cascaded two-pole smoother
    /// (both stages `>> env_shift`, the shift 0..3 depending on the
    /// samples per bit) takes out window scalloping without smearing bit
    /// transitions.
    ///
    /// Callers must use either this tap or the [`Discriminator`] metric,
    /// never both on one instance (each advances the correlators).
    pub(crate) fn push_envelopes(&mut self, sample: i32) -> (i64, i64) {
        let mark_power = self.mark.push(sample);
        let space_power = self.space.push(sample);
        // Powers are non-negative by construction, so the casts are
        // lossless.
        let mark_amp = (mark_power as u64).isqrt() as i64;
        let space_amp = (space_power as u64).isqrt() as i64;
        self.mark_env += (mark_amp - self.mark_env) >> self.env_shift;
        self.space_env += (space_amp - self.space_env) >> self.env_shift;
        self.mark_env2 += (self.mark_env - self.mark_env2) >> self.env_shift;
        self.space_env2 += (self.space_env - self.space_env2) >> self.env_shift;
        (self.mark_env2, self.space_env2)
    }

    /// Shared integer core for both sample paths.
    ///
    /// Takes the **smoothed amplitude** difference, not the raw power
    /// difference. Since only the sign reaches the bit slicer and
    /// `sign(√a − √b) ≡ sign(a − b)`, the square roots alone would change
    /// nothing; the smoothing is the point, and it is worth a great deal.
    ///
    /// # Why the clock needs a smooth statistic
    ///
    /// The bit-clock loop ([`crate::Slicer`]) nudges its phase on every
    /// *sign change* of this metric, so what matters to timing recovery
    /// is not the metric's noise power but its **zero-crossing rate**.
    /// Those are different quantities: by Rice's formula the crossing
    /// rate is set by the second spectral moment, and a one-bit boxcar
    /// correlator has a triangular autocorrelation whose corner at the
    /// origin makes that moment diverge in continuous time — bounded
    /// only by the sample rate. So an unsmoothed metric hands the loop
    /// a flood of noise crossings precisely when the signal is weak.
    ///
    /// MEASURED at 48 kHz, Bell 202, crossings per bit period:
    ///
    /// | input | unsmoothed | smoothed |
    /// |---|---|---|
    /// | noise only | **4.40** | 0.899 |
    /// | signal, −1 dB SNR | 0.729 | 0.498 |
    /// | signal, −3 dB SNR | 0.926 | 0.499 |
    ///
    /// 0.498 is exactly the transition density of random data. The
    /// smoothed statistic therefore gives the loop essentially *no*
    /// noise-induced crossings down to −3 dB, while the unsmoothed one
    /// gives it nearly as many noise crossings as real ones — and since
    /// a first-order loop's bandwidth scales with its update rate, that
    /// inflates the effective loop bandwidth by ~9× exactly when the
    /// loop can least afford it.
    ///
    /// MEASURED effect on decode, changing nothing else: FX.25 recovery
    /// on a reference-generated 100-frame noise ramp went **60 → 92**
    /// frames (the reference implementation scores 82, and 91 on its
    /// best tuned profile). Every pinned corpus row was byte-identical.
    /// The amplitude domain beats the power domain by a further ~3
    /// frames because correlator power is `χ²₂`-distributed — heavily
    /// tailed — so averaging powers is dominated by single excursions,
    /// while `√` compresses the tail.
    ///
    /// [`crate::tnc::TncReceiver`] already took this tap via
    /// [`Self::push_envelopes`]; only the bare [`Discriminator`] path
    /// (used by [`crate::AfskDemodulator`], and hence by the FX.25 and
    /// IL2P receive chains) was missing it.
    ///
    /// Cost: two integer square roots per sample. MEASURED, the
    /// single-chain FX.25 decode of a 100-frame file went 0.091 s →
    /// 0.165 s. `TncReceiver` already paid this and is unaffected.
    fn push(&mut self, sample: i32) -> i32 {
        // Amplitude scale (√power ≈ 2²¹ for a full-scale matched tone),
        // so unlike the previous power-domain metric this difference
        // comfortably fits `i32` and does **not** saturate anywhere in
        // the supported input range. The clamp is retained as a
        // total-function guard, not because it is expected to bite.
        let (mark_env, space_env) = self.push_envelopes(sample);
        let diff = mark_env.saturating_sub(space_env);
        diff.clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }
}

impl Discriminator for QuadratureCorrelator {
    fn push_i16(&mut self, sample: i16) -> i32 {
        self.push(sample as i32)
    }

    fn push_f32(&mut self, sample: f32) -> i32 {
        // Map the nominal [-1, 1] float range onto the i16 scale so both
        // paths produce comparable metrics; clamp handles hot signals.
        let scaled = (sample * 32_767.0).clamp(-32_768.0, 32_767.0) as i32;
        self.push(scaled)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    const RATES: [u32; 5] = [8_000, 11_025, 22_050, 44_100, 48_000];

    fn bank(sr_hz: u32) -> QuadratureCorrelator {
        let sr = SampleRate::new(sr_hz).unwrap();
        let baud = BaudRate::new(1_200).unwrap();
        let tones = TonePair::new(1_200, 2_200, sr).unwrap();
        QuadratureCorrelator::new(sr, baud, tones).unwrap()
    }

    /// Feeds `n` samples of a pure tone and returns the final metric.
    fn settle_tone_i16(sr_hz: u32, tone_hz: u32) -> i32 {
        let mut d = bank(sr_hz);
        let inc = phase_increment(tone_hz, sr_hz);
        let mut phase = 0u32;
        let mut metric = 0;
        for _ in 0..(3 * sr_hz / 1_200) {
            metric = d.push_i16(sine_at(phase));
            phase = phase.wrapping_add(inc);
        }
        metric
    }

    fn settle_tone_f32(sr_hz: u32, tone_hz: u32) -> i32 {
        let mut d = bank(sr_hz);
        let inc = phase_increment(tone_hz, sr_hz);
        let mut phase = 0u32;
        let mut metric = 0;
        for _ in 0..(3 * sr_hz / 1_200) {
            metric = d.push_f32(sine_at(phase) as f32 / 32_767.0);
            phase = phase.wrapping_add(inc);
        }
        metric
    }

    /// Pinned confidence floor for a clean full-scale tone: comfortably
    /// above noise-driven metrics, established empirically across rates.
    const CONFIDENCE_FLOOR: i32 = 100_000;

    macro_rules! tone_tests {
        ($($name:ident, $fname:ident: $sr:expr;)*) => {$(
            #[test]
            fn $name() {
                let mark = settle_tone_i16($sr, 1_200);
                let space = settle_tone_i16($sr, 2_200);
                assert!(mark > CONFIDENCE_FLOOR, "mark metric {mark} at {}", $sr);
                assert!(space < -CONFIDENCE_FLOOR, "space metric {space} at {}", $sr);
            }

            #[test]
            fn $fname() {
                let mark = settle_tone_f32($sr, 1_200);
                let space = settle_tone_f32($sr, 2_200);
                assert!(mark > CONFIDENCE_FLOOR, "mark metric {mark} at {}", $sr);
                assert!(space < -CONFIDENCE_FLOOR, "space metric {space} at {}", $sr);
            }
        )*};
    }

    tone_tests! {
        separates_tones_i16_8000, separates_tones_f32_8000: 8_000;
        separates_tones_i16_11025, separates_tones_f32_11025: 11_025;
        separates_tones_i16_22050, separates_tones_f32_22050: 22_050;
        separates_tones_i16_44100, separates_tones_f32_44100: 44_100;
        separates_tones_i16_48000, separates_tones_f32_48000: 48_000;
    }

    #[test]
    fn silence_gives_zero_metric() {
        for sr in RATES {
            let mut d = bank(sr);
            let mut metric = 1;
            for _ in 0..200 {
                metric = d.push_i16(0);
            }
            assert_eq!(metric, 0, "silence metric at {sr}");
        }
    }

    #[test]
    fn metric_is_phase_independent() {
        // Same tone, four starting phases: metric sign must not flip.
        for start in [0u32, 1 << 30, 1 << 31, 3 << 30] {
            let mut d = bank(48_000);
            let inc = phase_increment(1_200, 48_000);
            let mut phase = start;
            let mut metric = 0;
            for _ in 0..120 {
                metric = d.push_i16(sine_at(phase));
                phase = phase.wrapping_add(inc);
            }
            assert!(metric > CONFIDENCE_FLOOR, "phase {start}: {metric}");
        }
    }

    #[test]
    fn metric_scales_with_amplitude() {
        let mut loud = bank(48_000);
        let mut quiet = bank(48_000);
        let inc = phase_increment(1_200, 48_000);
        let mut phase = 0u32;
        let (mut lm, mut qm) = (0, 0);
        for _ in 0..120 {
            let s = sine_at(phase);
            lm = loud.push_i16(s);
            qm = quiet.push_i16(s / 4);
            phase = phase.wrapping_add(inc);
        }
        assert!(lm > qm, "loud {lm} vs quiet {qm}");
        assert!(qm > 0, "quiet tone still detected: {qm}");
    }

    #[test]
    fn rejects_one_sample_per_bit() {
        let sr = SampleRate::new(8_000).unwrap();
        let baud = BaudRate::new(4_800).unwrap();
        let tones = TonePair::new(1_200, 2_200, sr).unwrap();
        let err = QuadratureCorrelator::new(sr, baud, tones).unwrap_err();
        assert_eq!(
            err,
            ConfigError::BaudExceedsSampleRate {
                baud: 4_800,
                sample_rate: 8_000
            }
        );
    }

    #[test]
    fn window_matches_bit_period() {
        // 48000/1200 = 40; the constructor must derive the window from
        // the rates and produce a working bank (covered by tone tests).
        let d = bank(48_000);
        assert_eq!(d.mark.len, 40);
    }

    #[test]
    fn window_covers_full_bit_at_300_baud() {
        // The slowest preset at the highest rate must get at least a full
        // one-bit correlation window, not a truncated one.
        let sr = SampleRate::new(48_000).unwrap();
        let baud = BaudRate::new(300).unwrap();
        let tones = TonePair::new(1_600, 1_800, sr).unwrap();
        let d = QuadratureCorrelator::new(sr, baud, tones).unwrap();
        assert!(d.mark.len >= 160, "got {}", d.mark.len);
    }

    /// The 300-baud profiles must get the orthogonal window: the shortest
    /// whole multiple of 1/shift covering a bit, which for a 200 Hz shift
    /// at 300 baud is 1.5 bits. Measured worth +17 frames (58 -> 75); see
    /// the module docs.
    #[test]
    fn narrow_shift_profiles_get_the_orthogonal_window() {
        let baud = BaudRate::new(300).unwrap();
        for (rate, want) in [(48_000u32, 240usize), (44_100, 221), (22_050, 110)] {
            let sr = SampleRate::new(rate).unwrap();
            for (mark, space) in [(1_600u32, 1_800u32), (1_270, 1_070), (2_225, 2_025)] {
                let tones = TonePair::new(mark, space, sr).unwrap();
                let d = QuadratureCorrelator::new(sr, baud, tones).unwrap();
                assert_eq!(
                    d.mark.len, want,
                    "{rate} Hz, {mark}/{space}: window should be 1/shift-aligned"
                );
                // Orthogonality is the point: shift * T_obs must be a
                // whole number of cycles.
                let cycles = 200.0 * d.mark.len as f64 / f64::from(rate);
                assert!(
                    (cycles - cycles.round()).abs() < 0.01,
                    "{rate} Hz: shift*T_obs = {cycles}, not orthogonal"
                );
            }
        }
    }

    /// Bell 202's one-bit crosstalk (0.191) is below the threshold, so it
    /// must keep the plain one-bit window at every supported rate --
    /// widening it measured *worse* (74 -> 73), and the corpus rows are
    /// pinned against exactly this path.
    #[test]
    fn bell_202_keeps_the_one_bit_window() {
        for rate in RATES {
            let sr = SampleRate::new(rate).unwrap();
            let baud = BaudRate::new(1_200).unwrap();
            let tones = TonePair::new(1_200, 2_200, sr).unwrap();
            let d = QuadratureCorrelator::new(sr, baud, tones).unwrap();
            assert_eq!(d.mark.len, (rate / 1_200) as usize, "at {rate} Hz");
        }
    }

    /// A shift so narrow that its orthogonal window would not fit must
    /// fall back to one bit rather than silently truncating to a
    /// non-orthogonal length.
    #[test]
    fn unfittable_orthogonal_window_falls_back_to_one_bit() {
        let sr = SampleRate::new(48_000).unwrap();
        let baud = BaudRate::new(300).unwrap();
        // 50 Hz shift wants 960 samples; MAX_WINDOW is 240.
        let tones = TonePair::new(1_600, 1_650, sr).unwrap();
        let d = QuadratureCorrelator::new(sr, baud, tones).unwrap();
        assert_eq!(d.mark.len, 160);
    }

    /// `ToneCorrelator::scale`'s multiply-shift is only bit-exact while
    /// `x_max·e < 2^RECIP_SHIFT` and `x_max·m` fits `u64`. Random streams
    /// cannot disprove that — it is a worst-case bound, and typical data
    /// never approaches it — so this checks the arithmetic precondition
    /// directly for every window length the fast path accepts. This is
    /// what pins [`RECIP_MAX_LEN`].
    #[test]
    fn multiply_shift_stays_inside_its_exactness_bound() {
        const S: u32 = ToneCorrelator::RECIP_SHIFT;
        let mut fast = 0usize;
        for len in 1..=MAX_WINDOW {
            let c = ToneCorrelator::new(1_200, 48_000, len);
            if c.recip == 0 {
                continue; // division fallback: exact by construction
            }
            fast += 1;
            // From the proof: |sum| < len·2^32, so x = |sum|>>8 < len·2^24.
            let x_max = (len as u64) << 24;
            let e = c.recip.wrapping_mul(len as u64).wrapping_sub(1u64 << S);
            assert!(
                e < len as u64,
                "len {len}: reciprocal residue {e} must be < len"
            );
            assert!(
                x_max.checked_mul(e).is_some_and(|p| p < (1u64 << S)),
                "len {len}: x_max*e = {x_max}*{e} reaches 2^{S}; the multiply-shift is no \
                 longer provably exact. RECIP_MAX_LEN is too large."
            );
            assert!(
                x_max.checked_mul(c.recip).is_some(),
                "len {len}: x_max*recip overflows u64"
            );
        }
        assert_eq!(fast, RECIP_MAX_LEN, "every len up to the cap must be fast");
    }

    #[test]
    fn f32_path_clamps_hot_input() {
        let mut d = bank(48_000);
        // Absurdly hot input must not wrap/panic; sign must stay sane.
        let inc = phase_increment(1_200, 48_000);
        let mut phase = 0u32;
        let mut metric = 0;
        for _ in 0..120 {
            metric = d.push_f32(sine_at(phase) as f32); // ~32767x too hot
            phase = phase.wrapping_add(inc);
        }
        assert!(metric > 0);
    }

    /// Reference (pre-optimization) tone correlator: the original
    /// per-sample `%` ring wrap and i64 truncating divisions, kept as
    /// the ground truth for the strength-reduced production path.
    #[derive(Debug, Clone)]
    struct RefToneCorrelator {
        window: [(i64, i64); MAX_WINDOW],
        len: usize,
        pos: usize,
        phase: u32,
        phase_inc: u32,
        i_sum: i64,
        q_sum: i64,
    }

    impl RefToneCorrelator {
        fn new(tone_hz: u32, sample_rate: u32, len: usize) -> Self {
            Self {
                window: [(0, 0); MAX_WINDOW],
                len: len.clamp(1, MAX_WINDOW),
                pos: 0,
                phase: 0,
                phase_inc: phase_increment(tone_hz, sample_rate),
                i_sum: 0,
                q_sum: 0,
            }
        }

        fn push(&mut self, sample: i32) -> i64 {
            let sin_ref = sine_at(self.phase) as i64;
            let cos_ref = sine_at(self.phase.wrapping_add(ToneCorrelator::QUARTER_TURN)) as i64;
            self.phase = self.phase.wrapping_add(self.phase_inc);
            let contrib = ((sample as i64) * sin_ref, (sample as i64) * cos_ref);
            if let Some(slot) = self.window.get_mut(self.pos) {
                let (old_i, old_q) = *slot;
                self.i_sum = self.i_sum.wrapping_sub(old_i).wrapping_add(contrib.0);
                self.q_sum = self.q_sum.wrapping_sub(old_q).wrapping_add(contrib.1);
                *slot = contrib;
            }
            self.pos = (self.pos + 1) % self.len.max(1);
            let n = self.len as i64;
            let i_n = self.i_sum / (n * 256);
            let q_n = self.q_sum / (n * 256);
            i_n.saturating_mul(i_n)
                .saturating_add(q_n.saturating_mul(q_n))
        }
    }

    /// Minimal LCG (Numerical Recipes constants) for reproducible
    /// pseudo-random sample streams without any dependency.
    struct Lcg(u64);

    impl Lcg {
        fn next_u32(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 32) as u32
        }
    }

    /// The strength-reduced `ToneCorrelator::push` must be bit-identical
    /// to the original division-based implementation, sample by sample,
    /// over long LCG-random streams at every supported rate/baud/tone
    /// combination and input scale (including the ~2×-i16 range the
    /// TNC's pre-emphasized tap can produce).
    ///
    /// The final entries carry windows past [`RECIP_MAX_LEN`], which take
    /// the division fallback rather than the multiply-shift — the two
    /// must still agree, and the fallback must not be reached early.
    #[test]
    fn push_matches_division_reference_on_random_streams() {
        for (sr, baud, win) in [
            (8_000u32, 1_200u32, None),
            (11_025, 1_200, None),
            (22_050, 1_200, None),
            (44_100, 1_200, None),
            (48_000, 1_200, None),
            (48_000, 300, None),
            (9_600, 1_200, None),
            // Stretched 300-baud windows: both sides of RECIP_MAX_LEN.
            (44_100, 300, Some(181)),
            (44_100, 300, Some(182)),
            (44_100, 300, Some(221)),
            (48_000, 300, Some(240)),
        ] {
            for tone in [1_200u32, 1_600, 1_800, 2_200] {
                let len = win.unwrap_or((sr / baud) as usize);
                let mut new = ToneCorrelator::new(tone, sr, len);
                let mut old = RefToneCorrelator::new(tone, sr, len);
                // 181 is the proven bound, written as a literal so this
                // stays sensitive to RECIP_MAX_LEN moving.
                assert_eq!(
                    new.recip == 0,
                    len > 181,
                    "fast path must apply exactly up to len 181 (len {len})"
                );
                let mut rng = Lcg(u64::from(sr) * 31 + u64::from(tone));
                for i in 0..5_000 {
                    // i16-scale plus a stretch of ~2× hot samples
                    // (pre-emphasis headroom).
                    let r = rng.next_u32();
                    let s = if i % 977 < 100 {
                        ((r & 0x3_FFFF) as i32) - 0x2_0000
                    } else {
                        ((r & 0xFFFF) as i32) - 0x8000
                    };
                    assert_eq!(
                        new.push(s),
                        old.push(s),
                        "divergence at sample {i} (sr {sr}, baud {baud}, tone {tone})"
                    );
                }
            }
        }
    }
}
