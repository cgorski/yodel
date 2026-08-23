//! G3RUH 9600-baud direct-baseband FSK front end.
//!
//! Implements the modem design of:
//!
//! > Miller, J. (G3RUH), "9600 Baud Packet Radio Modem Design",
//! > Proceedings of the ARRL 7th Computer Networking Conference,
//! > October 1988, pp. 135-140.
//! > <https://www.amsat.org/amsat/articles/g3ruh/109.html>
//!
//! The scrambler that pairs with it is in [`crate::scrambler`]. Filter
//! shapes and tap counts here are this crate's own choices, sized by
//! measurement rather than taken from the paper.
//!
//! # Why a separate front end?
//!
//! G3RUH packet is *not* audio FSK: the (scrambled, NRZI-coded) bit stream
//! is transmitted as a band-limited **baseband pulse waveform** driving the
//! radio's modulator directly. There are no mark/space tones, so the
//! quadrature-correlator discriminator does not apply; instead the receiver
//! low-pass filters the samples, removes DC, and slices the sign of the
//! filtered waveform under a recovered bit clock.
//!
//! # Transmit synthesis
//!
//! [`BasebandModulator`] emits one full-scale level per bit (`+` for
//! [`Bit::One`], `-` for [`Bit::Zero`]) and shapes every level *change* as a
//! half-cosine ramp centered on the bit boundary — half a bit before the
//! boundary to half a bit after. Within a bit cell whose neighbour differs,
//! the waveform is `level · sin(π·u)` (`u` = position in the cell), which
//! reaches full amplitude exactly at mid-cell (where the receiver samples)
//! and crosses zero exactly on the boundary (where the receiver's PLL looks
//! for edges). The fastest possible waveform (alternating bits) is a pure
//! half-baud sine, so the synthesized spectrum is confined to roughly the
//! baud rate. The shaping uses the crate's shared compile-time sine table;
//! fractional samples per bit (e.g. 44100 Hz / 9600 Bd = 4.59375) are
//! handled with the same integer remainder accumulator as the tone
//! modulator — zero drift over any run of bits.
//!
//! Because a transition is centered on the boundary, the modulator emits
//! each bit **one bit late** (it must know whether the *next* bit differs).
//! The iterator adapters flush that final bit automatically; manual users
//! call [`BasebandModulator::finish`].
//!
//! # Receive chain
//!
//! [`BasebandDemodulator`] composes, per sample:
//!
//! 1. a **windowed-sinc FIR low-pass** (cutoff 0.8 × the baud rate, length
//!    three bit times, Hamming window, Q15 integer taps) suppressing
//!    out-of-band noise;
//! 2. running **baseline and amplitude trackers** driven by quantized
//!    decision feedback, whose baseline removes DC offset and channel
//!    imbalance;
//! 3. a zero-threshold **slicer** under the crate's fractional-N PLL
//!    ([`crate::slicer::Slicer`]), which nudges the bit clock toward the
//!    waveform's zero crossings and samples one decision per bit cell.
//!
//! The output is the **raw channel bit stream** — still scrambled and
//! NRZI-coded. Feed it through [`Descrambler`](crate::scrambler::Descrambler)
//! and then [`NrziDecoder`](crate::nrzi::NrziDecoder) (in that order) before
//! HDLC deframing; the TNC layer does exactly that when configured with
//! [`ModemProfile::G3RUH_9600`](crate::types::ModemProfile::G3RUH_9600).

#[cfg(any(feature = "mod", feature = "demod"))]
use crate::error::ConfigError;
#[cfg(feature = "mod")]
use crate::types::sine_at;
#[cfg(any(feature = "mod", feature = "demod"))]
use crate::types::{BaudRate, Bit, SampleRate};

/// Full-scale output amplitude of the i16 path.
#[cfg(feature = "mod")]
const AMPLITUDE: i32 = 32_767;

/// Streaming baseband pulse modulator: scrambled-NRZI bits in, band-limited
/// PCM out.
///
/// See the [module docs](self) for the waveform design. Feed one bit at a
/// time with [`BasebandModulator::feed`], drain samples with
/// [`BasebandModulator::next_i16`] / [`BasebandModulator::next_f32`], and
/// call [`BasebandModulator::finish`] after the last bit (the shaping
/// lookahead delays emission by one bit); or use the iterator adapters
/// [`BasebandModulator::i16_samples`] / [`BasebandModulator::f32_samples`],
/// which flush automatically. Owns no buffers, never allocates.
///
/// # Examples
///
/// New to baseband packet? Bits become full-scale levels — a one is
/// positive, a zero negative — with smooth cosine edges between them:
///
/// ```
/// use warble::{BasebandModulator, BaudRate, Bit, SampleRate};
///
/// let sr = SampleRate::new(48_000)?;
/// let baud = BaudRate::new(9_600)?;
/// let m = BasebandModulator::new(sr, baud)?;
/// let samples: Vec<i16> = m.i16_samples([Bit::One; 4].into_iter()).collect();
/// assert_eq!(samples.len(), 4 * 5); // 48000 / 9600 = 5 samples per bit
/// assert!(samples.iter().all(|&s| s == 32_767)); // steady ones: flat level
/// # Ok::<(), warble::ConfigError>(())
/// ```
///
/// In a real G3RUH transmit chain the modulator is the last stage, fed by
/// the scrambler, which is fed by the NRZI encoder:
///
/// ```
/// use warble::{BasebandModulator, BaudRate, Bit, SampleRate, Scrambler, nrzi};
///
/// let bits = [Bit::Zero, Bit::One, Bit::One, Bit::Zero];
/// let m = BasebandModulator::new(SampleRate::new(48_000)?, BaudRate::new(9_600)?)?;
/// let pcm: Vec<i16> = m
///     .i16_samples(Scrambler::default().scramble_iter(nrzi::encode_iter(bits.into_iter())))
///     .collect();
/// assert_eq!(pcm.len(), 4 * 5);
/// # Ok::<(), warble::ConfigError>(())
/// ```
///
/// Note for the DSP-minded: at 44 100 Hz a 9600-baud bit spans 4.59375
/// samples; the remainder accumulator emits 4- and 5-sample cells so that
/// any run of bits totals exactly `floor(bits · rate / baud)` samples:
///
/// ```
/// use warble::{BasebandModulator, BaudRate, Bit, SampleRate};
///
/// let m = BasebandModulator::new(SampleRate::new(44_100)?, BaudRate::new(9_600)?)?;
/// let n = m
///     .i16_samples(core::iter::repeat_n(Bit::One, 3_200))
///     .count();
/// assert_eq!(n, 3_200 * 44_100 / 9_600); // = 14_700, zero drift
/// # Ok::<(), warble::ConfigError>(())
/// ```
#[cfg(feature = "mod")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasebandModulator {
    /// Whole samples per bit: `sample_rate / baud`.
    whole_per_bit: u32,
    /// Fractional remainder per bit: `sample_rate % baud`.
    rem_per_bit: u32,
    /// Baud rate (denominator of the remainder accumulator).
    baud: u32,
    /// Remainder accumulator; an extra sample is emitted when it reaches
    /// `baud`.
    rem_acc: u32,
    /// Level (±1) of the bit awaiting emission (the shaping lookahead).
    pending_level: i32,
    /// Whether a bit is awaiting emission.
    have_pending: bool,
    /// Whether the pending bit differs from its predecessor (its first
    /// half rides the incoming transition ramp).
    pending_in_trans: bool,
    /// Level (±1) of the cell currently being emitted.
    level: i32,
    /// Whether the emitting cell's first half is cosine-shaped.
    first_shaped: bool,
    /// Whether the emitting cell's second half is cosine-shaped.
    second_shaped: bool,
    /// Cell phase; full u32 range = one bit cell.
    ph: u32,
    /// Per-sample cell-phase step for the emitting cell (`2^32 / n`).
    ph_step: u32,
    /// Samples still owed for the emitting cell.
    remaining: u32,
}

#[cfg(feature = "mod")]
impl BasebandModulator {
    /// Builds a modulator for the given sample and baud rates.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when the rates yield
    /// fewer than 2 samples per bit — the half-cosine edge shaping needs at
    /// least one sample on each side of a bit boundary.
    pub const fn new(sample_rate: SampleRate, baud: BaudRate) -> Result<Self, ConfigError> {
        let sr = sample_rate.hz();
        let bd = baud.bps();
        if sr / bd < 2 {
            return Err(ConfigError::BaudExceedsSampleRate {
                baud: bd,
                sample_rate: sr,
            });
        }
        Ok(Self {
            whole_per_bit: sr / bd,
            rem_per_bit: sr % bd,
            baud: bd,
            rem_acc: 0,
            pending_level: 0,
            have_pending: false,
            pending_in_trans: false,
            level: 0,
            first_shaped: false,
            second_shaped: false,
            ph: 0,
            ph_step: 0,
            remaining: 0,
        })
    }

    /// Starts emitting one bit cell with the given shaping flags.
    fn start_cell(&mut self, level: i32, first_shaped: bool, second_shaped: bool) {
        self.rem_acc += self.rem_per_bit;
        let extra = if self.rem_acc >= self.baud {
            self.rem_acc -= self.baud;
            1
        } else {
            0
        };
        let n = self.whole_per_bit + extra;
        self.level = level;
        self.first_shaped = first_shaped;
        self.second_shaped = second_shaped;
        self.ph = 0;
        // n >= 2 by construction, so the division is safe and the step
        // places sample k at cell position k/n exactly.
        self.ph_step = ((1u64 << 32) / n as u64) as u32;
        self.remaining = n;
    }

    /// Queues one bit for modulation.
    ///
    /// The *previous* bit's samples become available (the transition
    /// shaping needs one bit of lookahead); drain them with
    /// [`BasebandModulator::next_i16`] / [`BasebandModulator::next_f32`]
    /// before feeding the next bit — undrained samples are discarded.
    /// After the final bit, call [`BasebandModulator::finish`].
    pub fn feed(&mut self, bit: Bit) {
        let new_level = match bit {
            Bit::One => 1,
            Bit::Zero => -1,
        };
        if self.have_pending {
            let out_trans = self.pending_level != new_level;
            let (level, in_trans) = (self.pending_level, self.pending_in_trans);
            self.start_cell(level, in_trans, out_trans);
            self.pending_in_trans = out_trans;
        } else {
            self.have_pending = true;
            self.pending_in_trans = false;
        }
        self.pending_level = new_level;
    }

    /// Flushes the final fed bit (emitted flat into its second half, since
    /// no successor exists). A no-op when nothing is pending.
    pub fn finish(&mut self) {
        if self.have_pending {
            let (level, in_trans) = (self.pending_level, self.pending_in_trans);
            self.start_cell(level, in_trans, false);
            self.have_pending = false;
            self.pending_in_trans = false;
        }
    }

    /// The current sample's magnitude (0..=32767) and level sign, or
    /// `None` when the cell is exhausted.
    fn next_raw(&mut self) -> Option<i32> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let in_first_half = self.ph < (1 << 31);
        let shaped = if in_first_half {
            self.first_shaped
        } else {
            self.second_shaped
        };
        // sin(π·u) over the cell: cell phase / 2 maps [0, 2^32) onto the
        // sine table's [0, π) half-cycle, which is non-negative.
        let magnitude = if shaped {
            sine_at(self.ph >> 1) as i32
        } else {
            AMPLITUDE
        };
        self.ph = self.ph.wrapping_add(self.ph_step);
        Some(self.level * magnitude)
    }

    /// Pulls the next i16 PCM sample of the current cell, or `None` when it
    /// is exhausted (feed the next bit, or [`BasebandModulator::finish`]).
    pub fn next_i16(&mut self) -> Option<i16> {
        self.next_raw()
            .map(|v| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
    }

    /// Pulls the next f32 PCM sample (nominal range `-1.0..=1.0`), or
    /// `None` when the current cell is exhausted.
    pub fn next_f32(&mut self) -> Option<f32> {
        self.next_raw().map(|v| v as f32 / AMPLITUDE as f32)
    }

    /// Adapts a bit iterator into an iterator of i16 PCM samples,
    /// flushing the final bit automatically.
    pub fn i16_samples<I>(self, bits: I) -> BasebandI16Samples<I>
    where
        I: Iterator<Item = Bit>,
    {
        BasebandI16Samples {
            modulator: self,
            bits,
            flushed: false,
        }
    }

    /// Adapts a bit iterator into an iterator of f32 PCM samples,
    /// flushing the final bit automatically.
    pub fn f32_samples<I>(self, bits: I) -> BasebandF32Samples<I>
    where
        I: Iterator<Item = Bit>,
    {
        BasebandF32Samples {
            modulator: self,
            bits,
            flushed: false,
        }
    }
}

/// Iterator of i16 PCM samples over a bit iterator.
///
/// Created by [`BasebandModulator::i16_samples`].
#[cfg(feature = "mod")]
#[derive(Debug, Clone)]
pub struct BasebandI16Samples<I> {
    modulator: BasebandModulator,
    bits: I,
    flushed: bool,
}

#[cfg(feature = "mod")]
impl<I> Iterator for BasebandI16Samples<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        loop {
            if let Some(sample) = self.modulator.next_i16() {
                return Some(sample);
            }
            match self.bits.next() {
                Some(bit) => self.modulator.feed(bit),
                None => {
                    if self.flushed {
                        return None;
                    }
                    self.flushed = true;
                    self.modulator.finish();
                }
            }
        }
    }
}

/// Iterator of f32 PCM samples over a bit iterator.
///
/// Created by [`BasebandModulator::f32_samples`].
#[cfg(feature = "mod")]
#[derive(Debug, Clone)]
pub struct BasebandF32Samples<I> {
    modulator: BasebandModulator,
    bits: I,
    flushed: bool,
}

#[cfg(feature = "mod")]
impl<I> Iterator for BasebandF32Samples<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.modulator.next_f32() {
                return Some(sample);
            }
            match self.bits.next() {
                Some(bit) => self.modulator.feed(bit),
                None => {
                    if self.flushed {
                        return None;
                    }
                    self.flushed = true;
                    self.modulator.finish();
                }
            }
        }
    }
}

/// Largest supported FIR length in taps (`FIR_SPAN_BITS` bit times at
/// 9600 baud, and the cap every lower baud rate clamps to).
#[cfg(feature = "demod")]
pub const MAX_FIR_TAPS: usize = 15;

/// Q15 unity: FIR taps are normalized so their sum is this value.
#[cfg(feature = "demod")]
const TAP_UNITY: i32 = 1 << 15;

/// Time constant of the baseline and amplitude trackers, as a right
/// shift: each estimate has roughly `2^BASELINE_SHIFT` samples of
/// memory.
///
/// **Why not a peak/valley midpoint.** G3RUH scrambles the data
/// precisely so the channel stream is DC-balanced: over any window of
/// more than a few bits, ones and zeros are equiprobable, so an average
/// is a valid estimator of the threshold and drives noise down rather
/// than up. A peak/valley midpoint is an **order statistic** — set by
/// the single largest excursion in its window, which at low
/// signal-to-noise is a noise spike. It was borrowed from the AFSK
/// envelope path, where marks and spaces do differ in amplitude and no
/// balance can be assumed, and it cost nearly half the achievable
/// frames here.
///
/// **Why decision feedback rather than a plain mean.** "Equiprobable"
/// only holds *asymptotically*. Over a finite window the ones/zeros
/// imbalance is a random walk, so a plain mean carries a data-dependent
/// error that scales as `1/sqrt(window)` — at this time constant, an
/// RMS wander of roughly a tenth of the eye, occasionally far worse.
/// That error sits directly on the decision and is *common to every bit
/// in the window*, so the slicer cannot average it away.
///
/// Subtracting the decided symbol before averaging (classically
/// "quantized feedback") removes the data term at source: what is
/// averaged is the residual, which contains only channel offset and
/// noise. Measured on the reference 9600-baud noise ramp, this recovered
/// a further frame at 48 kHz and three at 44.1 kHz over a plain mean.
///
/// A peak/valley midpoint is an **order statistic**, so it is set by the
/// single largest excursion in its window — which at low signal-to-noise
/// is a noise spike, not a symbol. That biases the threshold and costs
/// bits. This front end originally used one (borrowed from the AFSK
/// envelope path, where marks and spaces do differ in amplitude and no
/// balance can be assumed); on the reference 9600-baud noise ramp it
/// recovered 35 frames of 100 against a mean's 64.
///
/// 512 samples is about 102 bit periods at 5 samples per bit, so the
/// estimate converges well inside the default 32-flag (256-bit)
/// preamble. Longer is quieter but acquires too slowly: measured
/// recovery peaks here and falls off sharply by 2^12.
#[cfg(feature = "demod")]
const BASELINE_SHIFT: u32 = 9;

/// Span of the receive FIR in bit periods.
///
/// One bit period leaves only ~5 taps at 9600 baud, which is far too
/// short for a usable low-pass — the transition band swamps the
/// stopband. Three bit periods is 15 taps there, and costs nothing at
/// lower baud rates because they already clamp at [`MAX_FIR_TAPS`].
#[cfg(feature = "demod")]
const FIR_SPAN_BITS: u32 = 3;

/// Receive low-pass cutoff, as a fraction of the baud rate.
///
/// G3RUH's design shapes the *transmitted* waveform so that the pulse
/// arriving at the detector is already a Nyquist pulse — flat to
/// 0.34·fb, −6 dB at 0.5·fb, and band-limited to about 0.66·fb (a raised
/// cosine with roll-off ≈ 0.31). The transmitter, not the receiver,
/// carries the matched filter.
///
/// So this filter's job is only to reject noise above the signal band,
/// not to shape the pulse: filtering harder distorts an already-correct
/// pulse and *costs* frames to inter-symbol interference, which is
/// measurable — dropping to 0.5 roughly halves recovery.
///
/// The nominal figure sits above the 0.66 band edge on purpose. At the
/// 15 taps available at 9600 baud the windowed-sinc transition band is
/// very wide, so a nominal 0.8 puts the realised −3 dB point near the
/// theoretical band edge. Measured best across both tested rates.
#[cfg(feature = "demod")]
const FIR_CUTOFF_RATIO: f64 = 0.8;

/// Runtime `sin(x)` for filter design: quadrant range reduction onto
/// `[-π/2, π/2]` followed by an odd Taylor polynomial (truncation error
/// below 4e-9 on that interval — far under the Q15 tap quantization).
/// Plain `core` float arithmetic; no `std`, no `libm`.
#[cfg(feature = "demod")]
fn sin_taylor(x: f64) -> f64 {
    const PI: f64 = core::f64::consts::PI;
    // Reduce to [0, 2π): tap design only ever needs |x| < a few hundred.
    let mut r = x % (2.0 * PI);
    if r < 0.0 {
        r += 2.0 * PI;
    }
    // Quadrant symmetry onto [-π/2, π/2].
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

/// Streaming baseband demodulator front end: PCM samples in, raw channel
/// bits out.
///
/// Per sample it runs a windowed-sinc FIR low-pass (cutoff 0.8 × the baud
/// rate, length three bit times), subtracts a running baseline maintained
/// by quantized decision feedback (DC removal), and feeds the centered
/// metric to the crate's PLL bit slicer, which emits one decision per
/// recovered bit cell. See the [module docs](self) for the design.
///
/// The emitted bits are still **scrambled and NRZI-coded**: descramble
/// first, then NRZI-decode, then deframe.
///
/// # Examples
///
/// New to bit recovery? The demodulator turns a waveform back into the
/// bits that produced it — here a full modulate → demodulate loop over an
/// alternating warm-up plus payload (the first bits are PLL settling
/// time, so we check the tail):
///
/// ```
/// use warble::{BasebandDemodulator, BasebandModulator, BaudRate, Bit, SampleRate};
///
/// let sr = SampleRate::new(48_000)?;
/// let baud = BaudRate::new(9_600)?;
/// let bits: Vec<Bit> = (0..64).map(|i| Bit::from(i % 2 == 0)).collect();
/// let pcm: Vec<i16> = BasebandModulator::new(sr, baud)?
///     .i16_samples(bits.iter().copied())
///     .collect();
/// let mut rx = BasebandDemodulator::new(sr, baud)?;
/// let out: Vec<Bit> = pcm.iter().filter_map(|&s| rx.push_i16(s)).collect();
/// let tail = &out[out.len() - 16..];
/// for pair in tail.windows(2) {
///     assert_ne!(pair[0], pair[1]); // alternation recovered
/// }
/// # Ok::<(), warble::ConfigError>(())
/// ```
///
/// In a receive chain the recovered bits go to the descrambler, then the
/// NRZI decoder:
///
/// ```
/// use warble::{BasebandDemodulator, BaudRate, Descrambler, NrziDecoder, SampleRate};
///
/// let mut front = BasebandDemodulator::new(SampleRate::new(44_100)?, BaudRate::new(9_600)?)?;
/// let mut descrambler = Descrambler::default();
/// let mut nrzi = NrziDecoder::default();
/// for sample in [0i16; 32] {
///     if let Some(raw) = front.push_i16(sample) {
///         let _data_bit = nrzi.decode(descrambler.descramble(raw));
///     }
/// }
/// # Ok::<(), warble::ConfigError>(())
/// ```
///
/// Note for the DSP-minded: the FIR spans three bit periods, so its length
/// is `3 · sample_rate / baud` rounded to the nearest odd count (capped at
/// [`MAX_FIR_TAPS`]), and the taps are a Hamming-windowed sinc at cutoff
/// 0.8 × baud, quantized to Q15 with unity DC gain, so a constant input
/// passes unscaled:
///
/// ```
/// use warble::{BasebandDemodulator, BaudRate, SampleRate};
///
/// // 44_100 / 9_600 = 4.59 samples per bit -> a 15-tap filter (three bit
/// // periods, which is also the cap); construction is checked.
/// assert!(BasebandDemodulator::new(SampleRate::new(44_100)?, BaudRate::new(9_600)?).is_ok());
/// // Fewer than 2 samples per bit is rejected, not mis-decoded.
/// assert!(BasebandDemodulator::new(SampleRate::new(8_000)?, BaudRate::new(9_600)?).is_err());
/// # Ok::<(), warble::ConfigError>(())
/// ```
#[cfg(feature = "demod")]
#[derive(Debug, Clone)]
pub struct BasebandDemodulator {
    /// FIR low-pass + baseline/amplitude AGC (the metric front half).
    filter: BasebandFilter,
    /// Fractional-N PLL bit slicer (zero-threshold decisions).
    slicer: crate::slicer::Slicer,
}

/// The metric-producing half of the baseband receiver: windowed-sinc FIR
/// low-pass plus running decision-feedback DC removal. Shared between
/// [`BasebandDemodulator`] and the TNC receiver (which supplies its own
/// slicer chain).
#[cfg(feature = "demod")]
#[derive(Debug, Clone)]
pub(crate) struct BasebandFilter {
    /// Q15 FIR taps (windowed sinc), first `taps_len` entries valid.
    taps: [i32; MAX_FIR_TAPS],
    /// Ring of the last `taps_len` input samples.
    history: [i32; MAX_FIR_TAPS],
    /// Number of active taps (odd, `3..=MAX_FIR_TAPS`).
    taps_len: usize,
    /// Next ring slot to overwrite.
    pos: usize,
    /// Running DC baseline: the decision threshold. See
    /// [`BASELINE_SHIFT`].
    baseline: i32,
    /// Running estimate of the symbol amplitude about the baseline.
    amplitude: i32,
}

#[cfg(feature = "demod")]
impl BasebandFilter {
    /// Designs the FIR (cutoff 0.8 × baud, length three bit times, Hamming
    /// window, exact unity DC gain in Q15) and zeroes the trackers.
    pub(crate) fn new(sample_rate: SampleRate, baud: BaudRate) -> Self {
        let sr = sample_rate.hz();
        let bd = baud.bps();
        // Span several bit times (see `FIR_SPAN_BITS`), rounded to the
        // nearest odd tap count and kept within the const-bounded
        // buffers.
        let spb = FIR_SPAN_BITS * ((sr + bd / 2) / bd);
        let len = if spb.is_multiple_of(2) { spb + 1 } else { spb } as usize;
        let len = len.clamp(3, MAX_FIR_TAPS);
        // Windowed sinc: h[k] = 2fc·sinc(2fc·(k−c))·w[k] with
        // fc = FIR_CUTOFF_RATIO·baud / sample_rate and a Hamming window,
        // then normalized to exact unity DC gain in Q15.
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
            // Hamming: 0.54 − 0.46·cos(2πk/(len−1)); cos(y) = sin(y+π/2).
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
        // Push any rounding residue into the center tap so the DC gain is
        // exactly unity.
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

    /// Filters one i16-scale sample and returns the centered slicer
    /// metric: FIR low-pass, then subtract the tracked baseline.
    pub(crate) fn push(&mut self, sample: i32) -> i32 {
        // FIR: write into the ring, convolve the active taps.
        if let Some(slot) = self.history.get_mut(self.pos) {
            *slot = sample;
        }
        // Conditional-wrap ring advance (bit-exact `% taps_len` for a
        // pos always in `0..taps_len`): no divide in the sample path.
        self.pos += 1;
        if self.pos == self.taps_len {
            self.pos = 0;
        }
        // Two-slice linear convolution, bit-exact vs. the former per-tap
        // `(pos + k) % taps_len` ring indexing: tap k pairs with
        // history[(pos + k) mod len], i.e. history[pos..len] pairs with
        // taps[0..len-pos] and history[0..pos] with taps[len-pos..len].
        // i64 accumulation makes the split-sum order immaterial: each
        // product is |sample| * |tap| <= 2^15 * ~2^15 = ~2^30, and at
        // most MAX_FIR_TAPS (15) of them sum to well under 2^35 — no
        // overflow, so the sum equals the modulo-indexed sum exactly.
        // Symmetric-coefficient folding was considered and skipped: the
        // taps are symmetric except for the rounding residue pushed into
        // the center tap, and folding a split ring costs more index
        // bookkeeping than the <=15 multiplies it saves.
        let len = self.taps_len;
        let pos = self.pos;
        let mut acc = 0i64;
        let (hist_old, taps_head) = (
            self.history.get(pos..len).unwrap_or(&[]),
            self.taps.get(..len - pos).unwrap_or(&[]),
        );
        for (&h, &t) in hist_old.iter().zip(taps_head) {
            acc += (h as i64) * (t as i64);
        }
        let (hist_new, taps_tail) = (
            self.history.get(..pos).unwrap_or(&[]),
            self.taps.get(len - pos..len).unwrap_or(&[]),
        );
        for (&h, &t) in hist_new.iter().zip(taps_tail) {
            acc += (h as i64) * (t as i64);
        }
        let filtered = (acc >> 15) as i32;
        // DC baseline tracking: a slow mean, which for a scrambled
        // (hence DC-balanced) stream *is* the decision threshold, and
        // which averages noise down instead of chasing it. See
        // `BASELINE_SHIFT`.
        // Decision-feedback (quantized-feedback) baseline restoration.
        let metric = filtered - self.baseline;
        let sign = if metric >= 0 { 1 } else { -1 };
        self.amplitude += (metric.abs() - self.amplitude) >> BASELINE_SHIFT;
        let residual = filtered - sign * self.amplitude;
        self.baseline += (residual - self.baseline) >> BASELINE_SHIFT;
        metric
    }
}

#[cfg(feature = "demod")]
impl BasebandDemodulator {
    /// Builds a demodulator front end for the given sample and baud rates.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when the rates yield
    /// fewer than 2 samples per bit — the PLL cannot place a sampling
    /// point between edges it never sees.
    pub fn new(sample_rate: SampleRate, baud: BaudRate) -> Result<Self, ConfigError> {
        let slicer = crate::slicer::Slicer::new(sample_rate, baud)?;
        Ok(Self {
            filter: BasebandFilter::new(sample_rate, baud),
            slicer,
        })
    }

    /// Pushes one i16 PCM sample; returns `Some(Bit)` when a bit cell
    /// completes. The emitted bit is the raw (scrambled, NRZI-coded)
    /// channel bit.
    pub fn push_i16(&mut self, sample: i16) -> Option<Bit> {
        let metric = self.filter.push(sample as i32);
        self.slicer.push(metric)
    }

    /// Pushes one f32 PCM sample (nominal `[-1.0, 1.0]`); the twin of
    /// [`BasebandDemodulator::push_i16`].
    pub fn push_f32(&mut self, sample: f32) -> Option<Bit> {
        let scaled = (sample * 32_767.0).clamp(-32_768.0, 32_767.0) as i16;
        self.push_i16(scaled)
    }
}

#[cfg(all(test, feature = "mod"))]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn rates(sr: u32, bd: u32) -> (SampleRate, BaudRate) {
        (
            SampleRate::new(sr).unwrap_or_else(|e| panic!("rate: {e}")),
            BaudRate::new(bd).unwrap_or_else(|e| panic!("baud: {e}")),
        )
    }

    fn modulator(sr: u32, bd: u32) -> BasebandModulator {
        let (sr, bd) = rates(sr, bd);
        BasebandModulator::new(sr, bd).unwrap_or_else(|e| panic!("config: {e}"))
    }

    // ---- TX ----

    #[test]
    fn rejects_fewer_than_two_samples_per_bit() {
        let (sr, bd) = rates(8_000, 9_600);
        assert_eq!(
            BasebandModulator::new(sr, bd).map(|_| ()),
            Err(ConfigError::BaudExceedsSampleRate {
                baud: 9_600,
                sample_rate: 8_000
            })
        );
    }

    #[test]
    fn steady_ones_are_flat_positive_full_scale() {
        let v: Vec<i16> = modulator(48_000, 9_600)
            .i16_samples(core::iter::repeat_n(Bit::One, 10))
            .collect();
        assert_eq!(v.len(), 50);
        assert!(v.iter().all(|&s| s == 32_767), "{v:?}");
    }

    #[test]
    fn steady_zeros_are_flat_negative_full_scale() {
        let v: Vec<i16> = modulator(48_000, 9_600)
            .i16_samples(core::iter::repeat_n(Bit::Zero, 10))
            .collect();
        assert!(v.iter().all(|&s| s == -32_767), "{v:?}");
    }

    #[test]
    fn sample_count_exact_at_44100() {
        // 44100 / 9600 = 4.59375 samples per bit.
        let n = modulator(44_100, 9_600)
            .i16_samples(core::iter::repeat_n(Bit::One, 9_600))
            .count();
        assert_eq!(n, 44_100);
    }

    #[test]
    fn transition_crosses_zero_at_boundary_and_peaks_mid_cell() {
        // One transition: ...111 000... The waveform must cross zero at
        // the 1->0 boundary and be at full scale at every cell midpoint.
        let bits = [
            Bit::One,
            Bit::One,
            Bit::One,
            Bit::Zero,
            Bit::Zero,
            Bit::Zero,
        ];
        let v: Vec<i16> = modulator(48_000, 9_600)
            .i16_samples(bits.into_iter())
            .collect();
        assert_eq!(v.len(), 30);
        // Boundary between bit 2 and bit 3 is at sample 15; the first
        // sample of a shaped cell sits exactly on the boundary (sin 0 = 0).
        assert_eq!(v[15], 0);
        // Mid-cell samples (k = 5*cell + 2, u = 0.4) are near full scale
        // even in shaped cells (sin(0.4π) ≈ 0.951).
        for cell in 0..6 {
            let mid = v[5 * cell + 2] as i32;
            assert!(mid.abs() >= 31_000, "cell {cell}: {mid}");
        }
        // Sign matches the bit.
        for cell in 0..3 {
            assert!(v[5 * cell + 2] > 0);
        }
        for cell in 3..6 {
            assert!(v[5 * cell + 2] < 0);
        }
    }

    #[test]
    fn alternating_bits_are_a_smooth_half_baud_tone() {
        // 1010... at 5 samples/bit is a pure 4800 Hz sine; adjacent-sample
        // steps must stay under the sine's max slope with table margin.
        let bits: Vec<Bit> = (0..40).map(|i| Bit::from(i % 2 == 0)).collect();
        let v: Vec<i16> = modulator(48_000, 9_600)
            .i16_samples(bits.into_iter())
            .collect();
        // Max slope of a 4800 Hz full-scale sine at 48 kHz:
        // 32767·sin(2π·4800/48000) ≈ 19260, plus table margin.
        for w in v.windows(2) {
            let step = (w[1] as i32 - w[0] as i32).abs();
            assert!(step <= 19_800, "step {step}");
        }
    }

    #[test]
    fn i16_and_f32_paths_agree() {
        let bits: Vec<Bit> = (0..32).map(|i| Bit::from(i % 3 == 0)).collect();
        let vi: Vec<i16> = modulator(44_100, 9_600)
            .i16_samples(bits.iter().copied())
            .collect();
        let vf: Vec<f32> = modulator(44_100, 9_600)
            .f32_samples(bits.iter().copied())
            .collect();
        assert_eq!(vi.len(), vf.len());
        for (a, b) in vi.iter().zip(vf.iter()) {
            assert!((*a as f32 / 32_767.0 - b).abs() < 1e-4);
        }
    }

    #[test]
    fn manual_feed_finish_matches_iterator() {
        let bits = [Bit::One, Bit::Zero, Bit::Zero, Bit::One];
        let via_iter: Vec<i16> = modulator(44_100, 9_600)
            .i16_samples(bits.iter().copied())
            .collect();
        let mut m = modulator(44_100, 9_600);
        let mut manual = Vec::new();
        for b in bits {
            m.feed(b);
            while let Some(s) = m.next_i16() {
                manual.push(s);
            }
        }
        m.finish();
        while let Some(s) = m.next_i16() {
            manual.push(s);
        }
        assert_eq!(via_iter, manual);
    }

    #[test]
    fn no_samples_before_second_feed() {
        // One bit of lookahead: the first fed bit emits nothing until the
        // next feed (or finish) reveals its trailing edge.
        let mut m = modulator(48_000, 9_600);
        assert_eq!(m.next_i16(), None);
        m.feed(Bit::One);
        assert_eq!(m.next_i16(), None);
        m.finish();
        assert_eq!(m.next_i16(), Some(32_767));
    }

    // ---- RX ----

    #[cfg(feature = "demod")]
    fn demodulator(sr: u32, bd: u32) -> BasebandDemodulator {
        let (sr, bd) = rates(sr, bd);
        BasebandDemodulator::new(sr, bd).unwrap_or_else(|e| panic!("config: {e}"))
    }

    #[cfg(feature = "demod")]
    #[test]
    fn fir_taps_sum_to_unity() {
        for (sr, bd) in [(44_100, 9_600), (48_000, 9_600), (22_050, 9_600)] {
            let d = demodulator(sr, bd);
            let sum: i32 = d.filter.taps.iter().take(d.filter.taps_len).sum();
            assert_eq!(sum, TAP_UNITY, "{sr}/{bd}");
            assert!(d.filter.taps_len % 2 == 1);
            // The center tap dominates a low-pass.
            let center = d.filter.taps[d.filter.taps_len / 2];
            assert!(
                d.filter
                    .taps
                    .iter()
                    .take(d.filter.taps_len)
                    .all(|&t| t <= center)
            );
        }
    }

    #[cfg(feature = "demod")]
    #[test]
    fn baseline_converges_onto_a_channel_dc_offset() {
        // The property that matters: given a *modulated* signal riding
        // on a channel DC offset, the tracked baseline converges onto
        // the offset, so the centered metric swings symmetrically about
        // zero and the slicer can threshold at zero.
        //
        // Note this is not tested with a constant input.
        // Decision-feedback restoration subtracts the *decided* symbol
        // before averaging, so an unbroken constant reads as an unbroken
        // run of ones — which is exactly what it is — and correctly
        // yields a sustained non-zero metric rather than decaying to
        // ambiguity. Scrambling guarantees transitions on any real
        // signal, so the degenerate case never arises on the air.
        const OFFSET: i32 = 4_000;
        const SWING: i32 = 10_000;
        let mut f = BasebandFilter::new(
            SampleRate::new(48_000).unwrap_or_else(|e| panic!("{e}")),
            BaudRate::new(9_600).unwrap_or_else(|e| panic!("{e}")),
        );
        // Alternating symbols at 5 samples per bit, offset by OFFSET.
        let mut positives = 0i64;
        let mut negatives = 0i64;
        for i in 0..(16 << BASELINE_SHIFT) {
            let symbol = if (i / 5) % 2 == 0 { SWING } else { -SWING };
            let metric = f.push(OFFSET + symbol);
            // Sample the settled tail only.
            if i > (8 << BASELINE_SHIFT) {
                if metric > 0 {
                    positives += 1;
                } else {
                    negatives += 1;
                }
            }
        }
        // The baseline sits on the channel offset, not on the symbols.
        assert!(
            (f.baseline - OFFSET).abs() < SWING / 4,
            "baseline {} did not converge onto the {OFFSET} offset",
            f.baseline
        );
        // ...so the metric spends about half its time either side of
        // zero rather than being biased into one symbol.
        let ratio = positives as f64 / (positives + negatives) as f64;
        assert!(
            (0.4..=0.6).contains(&ratio),
            "metric biased: {ratio:.3} of samples positive"
        );
    }

    #[cfg(feature = "demod")]
    #[test]
    fn loopback_recovers_bits_both_rates() {
        for sr in [44_100u32, 48_000] {
            // Warm-up alternation, then a patterned payload.
            let mut bits: Vec<Bit> = (0..32).map(|i| Bit::from(i % 2 == 0)).collect();
            let payload: Vec<Bit> = (0..200).map(|i| Bit::from((i * 7) % 3 == 0)).collect();
            bits.extend(payload.iter().copied());
            let pcm: Vec<i16> = modulator(sr, 9_600)
                .i16_samples(bits.iter().copied())
                .collect();
            let mut rx = demodulator(sr, 9_600);
            let out: Vec<Bit> = pcm.iter().filter_map(|&s| rx.push_i16(s)).collect();
            // The recovered stream must contain the payload contiguously.
            // The first few payload bits ride the warm-up→payload
            // tracker/PLL transient (real frames put an HDLC flag
            // preamble there), so match from bit 8 on.
            let target = &payload[8..];
            let found = out.windows(target.len()).any(|w| w == target);
            assert!(found, "rate {sr}: payload not recovered");
        }
    }

    #[cfg(feature = "demod")]
    #[test]
    fn loopback_survives_dc_offset_and_attenuation() {
        let mut bits: Vec<Bit> = (0..32).map(|i| Bit::from(i % 2 == 0)).collect();
        let payload: Vec<Bit> = (0..150).map(|i| Bit::from((i * 5) % 4 < 2)).collect();
        bits.extend(payload.iter().copied());
        let pcm: Vec<i16> = modulator(44_100, 9_600)
            .i16_samples(bits.iter().copied())
            .collect();
        let mut rx = demodulator(44_100, 9_600);
        let out: Vec<Bit> = pcm
            .iter()
            .map(|&s| (s as i32 / 4 + 3_000).clamp(-32_768, 32_767) as i16)
            .filter_map(|s| rx.push_i16(s))
            .collect();
        let target = &payload[8..];
        let found = out.windows(target.len()).any(|w| w == target);
        assert!(found, "payload not recovered under DC + attenuation");
    }

    #[cfg(feature = "demod")]
    #[test]
    fn f32_path_matches_i16_path() {
        let bits: Vec<Bit> = (0..100).map(|i| Bit::from(i % 2 == 0)).collect();
        let pcm_i: Vec<i16> = modulator(48_000, 9_600)
            .i16_samples(bits.iter().copied())
            .collect();
        let mut rx_i = demodulator(48_000, 9_600);
        let out_i: Vec<Bit> = pcm_i.iter().filter_map(|&s| rx_i.push_i16(s)).collect();
        let pcm_f: Vec<f32> = modulator(48_000, 9_600)
            .f32_samples(bits.iter().copied())
            .collect();
        let mut rx_f = demodulator(48_000, 9_600);
        let out_f: Vec<Bit> = pcm_f.iter().filter_map(|&s| rx_f.push_f32(s)).collect();
        assert_eq!(out_i, out_f);
    }
}
