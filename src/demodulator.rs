//! Bell 202 AFSK demodulator: PCM samples in, bits out.
//!
//! The demodulator composes two stages, mirroring the modulator's
//! push/pull streaming style:
//!
//! 1. a [`Discriminator`] (default: the dual-tone quadrature correlator
//!    bank, [`QuadratureCorrelator`]) turns each PCM sample into a signed
//!    soft metric — positive for the mark tone, negative for space;
//! 2. a [`Slicer`] (a digital PLL) recovers the bit clock from metric zero
//!    crossings and samples one raw tone decision per bit cell.
//!
//! Feed samples with [`Demodulator::push_sample_i16`] /
//! [`Demodulator::push_sample_f32`] — each returns `Some(Bit)` whenever a
//! bit cell completes — or wrap a sample iterator with
//! [`Demodulator::i16_bits`] / [`Demodulator::f32_bits`] and pull bits.
//!
//! Because tone discrimination needs roughly one bit period of signal
//! history and the PLL needs a few transitions to lock, prefix
//! transmissions with a short alternating preamble (16–32 bits of
//! `1 0 1 0 …`) and treat the demodulated preamble region as settling
//! time; every payload bit after it is recovered exactly on a clean
//! channel. The output is the raw tone decision stream — no NRZI or other
//! line decoding.
//!
//! Like the modulator, the demodulator owns only fixed-capacity state: no
//! allocation, no `std`.

use crate::discriminator::{Discriminator, QuadratureCorrelator};
use crate::error::ConfigError;
use crate::slicer::Slicer;
use crate::types::{BaudRate, Bit, SampleRate, TonePair};

/// Validated demodulator configuration (mirror of `ModulatorConfig`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemodulatorConfig {
    sample_rate: SampleRate,
    baud: BaudRate,
    tones: TonePair,
}

impl DemodulatorConfig {
    /// Builds a configuration from validated parts.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when the sample rate
    /// yields fewer than 2 samples per bit.
    pub const fn new(
        sample_rate: SampleRate,
        baud: BaudRate,
        tones: TonePair,
    ) -> Result<Self, ConfigError> {
        if sample_rate.hz() / baud.bps() < 2 {
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

    /// Builds the standard Bell 202 configuration (1200 Bd, 1200/2200 Hz)
    /// at the given sample rate.
    ///
    /// # Errors
    ///
    /// Propagates validation errors from the underlying constructors.
    pub const fn bell_202(sample_rate: SampleRate) -> Result<Self, ConfigError> {
        let baud = match BaudRate::new(1_200) {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let tones = match TonePair::new(1_200, 2_200, sample_rate) {
            Ok(t) => t,
            Err(e) => return Err(e),
        };
        Self::new(sample_rate, baud, tones)
    }

    /// The configured sample rate.
    pub const fn sample_rate(self) -> SampleRate {
        self.sample_rate
    }

    /// The configured baud rate.
    pub const fn baud(self) -> BaudRate {
        self.baud
    }

    /// The configured tone pair.
    pub const fn tones(self) -> TonePair {
        self.tones
    }
}

/// Streaming AFSK demodulator, generic over the tone [`Discriminator`].
///
/// Use the [`AfskDemodulator`] alias for the default quadrature-correlator
/// front end, constructed with [`Demodulator::new`]. Alternative front ends
/// plug in via [`Demodulator::with_discriminator`].
///
/// # Common path: samples in, bits out
///
/// Modulate an alternating preamble plus a payload, push every sample
/// through the demodulator, and recover the payload exactly (the
/// preamble region is PLL settling time):
///
/// ```
/// use warble::{AfskDemodulator, Bit, DemodulatorConfig, Modulator,
///              ModulatorConfig, SampleRate};
///
/// let sr = SampleRate::new(48_000)?;
/// let payload = [Bit::One, Bit::One, Bit::Zero, Bit::One];
/// // 32 alternating preamble bits lock the PLL; two trailing bits let
/// // the last payload bit cell complete inside the sample stream.
/// let bits = (0..32)
///     .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
///     .chain(payload)
///     .chain([Bit::Zero, Bit::Zero]);
///
/// let samples = Modulator::new(ModulatorConfig::bell_202(sr)?).i16_samples(bits);
/// let mut demod = AfskDemodulator::new(DemodulatorConfig::bell_202(sr)?)?;
/// let mut recovered = [Bit::Zero; 64];
/// let mut n = 0;
/// for s in samples {
///     // One bit per completed bit cell: 40 samples at 48 kHz / 1200 Bd.
///     if let Some(bit) = demod.push_sample_i16(s) {
///         recovered[n] = bit;
///         n += 1;
///     }
/// }
/// // 38 bit cells complete inside the stream (±1 for startup phase).
/// assert!(n >= 36, "only {n} bits");
/// // The payload follows the settling region exactly.
/// assert!(recovered[..n].windows(payload.len()).any(|w| w == payload));
/// # Ok::<(), warble::ConfigError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Demodulator<D> {
    discriminator: D,
    slicer: Slicer,
}

/// The default demodulator: quadrature-correlator front end + PLL slicer.
pub type AfskDemodulator = Demodulator<QuadratureCorrelator>;

impl AfskDemodulator {
    /// Builds a demodulator with the default [`QuadratureCorrelator`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when the
    /// configuration yields fewer than 2 samples per bit (already ruled out
    /// by [`DemodulatorConfig::new`], kept for direct construction paths).
    pub fn new(config: DemodulatorConfig) -> Result<Self, ConfigError> {
        let discriminator =
            QuadratureCorrelator::new(config.sample_rate(), config.baud(), config.tones())?;
        Self::with_discriminator(config, discriminator)
    }
}

impl<D: Discriminator> Demodulator<D> {
    /// Builds a demodulator around a caller-provided discriminator.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudExceedsSampleRate`] when the
    /// configuration yields fewer than 2 samples per bit.
    pub fn with_discriminator(
        config: DemodulatorConfig,
        discriminator: D,
    ) -> Result<Self, ConfigError> {
        let slicer = Slicer::new(config.sample_rate(), config.baud())?;
        Ok(Self {
            discriminator,
            slicer,
        })
    }

    /// Pushes one i16 PCM sample; returns `Some(Bit)` when a bit cell
    /// completes.
    pub fn push_sample_i16(&mut self, sample: i16) -> Option<Bit> {
        let metric = self.discriminator.push_i16(sample);
        self.slicer.push(metric)
    }

    /// Pushes one f32 PCM sample (nominal `[-1.0, 1.0]`); returns
    /// `Some(Bit)` when a bit cell completes.
    pub fn push_sample_f32(&mut self, sample: f32) -> Option<Bit> {
        let metric = self.discriminator.push_f32(sample);
        self.slicer.push(metric)
    }

    /// Wraps an i16 sample iterator, yielding recovered bits.
    pub fn i16_bits<I>(self, samples: I) -> I16Bits<I::IntoIter, D>
    where
        I: IntoIterator<Item = i16>,
    {
        I16Bits {
            demodulator: self,
            samples: samples.into_iter(),
        }
    }

    /// Wraps an f32 sample iterator, yielding recovered bits.
    pub fn f32_bits<I>(self, samples: I) -> F32Bits<I::IntoIter, D>
    where
        I: IntoIterator<Item = f32>,
    {
        F32Bits {
            demodulator: self,
            samples: samples.into_iter(),
        }
    }
}

/// Iterator of recovered bits over an i16 PCM sample iterator.
///
/// Created by [`Demodulator::i16_bits`].
#[derive(Debug, Clone)]
pub struct I16Bits<I, D> {
    demodulator: Demodulator<D>,
    samples: I,
}

impl<I, D> Iterator for I16Bits<I, D>
where
    I: Iterator<Item = i16>,
    D: Discriminator,
{
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        loop {
            let sample = self.samples.next()?;
            if let Some(bit) = self.demodulator.push_sample_i16(sample) {
                return Some(bit);
            }
        }
    }
}

/// Iterator of recovered bits over an f32 PCM sample iterator.
///
/// Created by [`Demodulator::f32_bits`].
#[derive(Debug, Clone)]
pub struct F32Bits<I, D> {
    demodulator: Demodulator<D>,
    samples: I,
}

impl<I, D> Iterator for F32Bits<I, D>
where
    I: Iterator<Item = f32>,
    D: Discriminator,
{
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        loop {
            let sample = self.samples.next()?;
            if let Some(bit) = self.demodulator.push_sample_f32(sample) {
                return Some(bit);
            }
        }
    }
}
