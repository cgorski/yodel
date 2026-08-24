//! Domain newtypes: sample rate, baud rate, tone pair, and bits.
//!
//! All constructors validate their input and return
//! [`ConfigError`](crate::error::ConfigError) on bad values, making illegal
//! modem configurations unrepresentable.

use crate::error::ConfigError;

/// Number of index bits of the shared sine lookup table.
pub(crate) const TABLE_BITS: u32 = 12;
/// Number of entries in the shared sine lookup table.
pub(crate) const TABLE_LEN: usize = 1 << TABLE_BITS;
/// Bit mask for a sine-table index.
pub(crate) const TABLE_MASK: usize = TABLE_LEN - 1;

/// Full-cycle sine table in signed 16-bit PCM, computed at compile time.
///
/// Entry `i` is `round(sin(2π · i / 4096) · 32767)`. Values are produced by
/// [`sine_entry`], a const fn using an odd Taylor polynomial after quadrant
/// range reduction — accurate to well below one LSB of the i16 output.
/// Shared by the modulator (waveform synthesis) and the demodulator
/// (quadrature correlation references).
pub(crate) static SINE_I16: [i16; TABLE_LEN] = build_sine_table();

/// Builds [`SINE_I16`] at compile time.
const fn build_sine_table() -> [i16; TABLE_LEN] {
    let mut table = [0i16; TABLE_LEN];
    let mut i = 0;
    while i < TABLE_LEN {
        table[i] = sine_entry(i);
        i += 1;
    }
    table
}

/// Computes `round(sin(2π · i / TABLE_LEN) · 32767)` in const context.
///
/// The angle is reduced to `[-π/2, π/2]` using sine's quadrant symmetry,
/// then evaluated with the Taylor polynomial
/// `x - x³/3! + x⁵/5! - x⁷/7! + x⁹/9! - x¹¹/11!`, whose truncation error on
/// that interval is below 4e-9 — far under the 1/32767 quantization step.
const fn sine_entry(i: usize) -> i16 {
    const PI: f64 = core::f64::consts::PI;
    let x = 2.0 * PI * (i as f64) / (TABLE_LEN as f64); // [0, 2π)
    // Quadrant reduction to [-π/2, π/2]: sin(x) = sin(π − x) on the middle
    // half-turn, and sin(x) = sin(x − 2π) near the top of the cycle.
    let r = if x <= 0.5 * PI {
        x
    } else if x <= 1.5 * PI {
        PI - x
    } else {
        x - 2.0 * PI
    };
    let r2 = r * r;
    let s = r
        * (1.0
            + r2 * (-1.0 / 6.0
                + r2 * (1.0 / 120.0
                    + r2 * (-1.0 / 5_040.0
                        + r2 * (1.0 / 362_880.0 + r2 * (-1.0 / 39_916_800.0))))));
    let scaled = s * 32_767.0;
    // Round half away from zero (f64::round is unavailable in const fn).
    if scaled >= 0.0 {
        (scaled + 0.5) as i16
    } else {
        (scaled - 0.5) as i16
    }
}

/// Returns one raw sine-table entry, for callers doing their own
/// interpolation.
///
/// [`sine_at`] takes the nearest entry, which quantizes the angle to
/// 0.088 degrees. That is far below the noise for waveform synthesis but
/// is the dominant error term for `geo`'s `cos(latitude)`, so that
/// module interpolates between neighbours instead.
pub(crate) fn sine_table_at(index: usize) -> i16 {
    SINE_I16.get(index & TABLE_MASK).copied().unwrap_or(0)
}

/// Looks up the i16 sine of a 32-bit phase (nearest table entry).
///
/// The full u32 range `0..2^32` represents one cycle `0..2π`, so wrapping
/// addition on phases is addition modulo 2π.
pub(crate) fn sine_at(phase: u32) -> i16 {
    let idx = (phase >> (32 - TABLE_BITS)) as usize & TABLE_MASK;
    SINE_I16.get(idx).copied().unwrap_or(0)
}

/// Looks up the i16-scale sine of a 32-bit phase, linearly interpolated
/// between neighbouring entries and rounded.
///
/// [`sine_at`] truncates the phase to a table index, quantising the
/// angle to 0.088 degrees. That is far below the noise for waveform
/// synthesis, where the phase names a *moment*. It is the dominant
/// error term wherever the phase names a *direction* instead, which is
/// why both of `geo`'s users — `cos(latitude)` and the bearing search —
/// come here rather than to [`sine_at`].
///
/// The interpolation **rounds**. Arithmetic-shifting the product floors
/// instead, which biases the result one way whenever the delta keeps a
/// consistent sign, as it does across any quarter turn. Rounding leaves
/// the table's own half-LSB and the curvature residual, neither of
/// which shares a sign.
///
/// The result carries the same q15 scale as [`sine_at`] but is returned
/// as `i32`: interpolation between two `i16` endpoints stays inside
/// their range, so the value always fits, and an `i32` saves every
/// caller a widening.
pub(crate) fn sine_at_interpolated(phase: u32) -> i32 {
    let index = (phase >> (32 - TABLE_BITS)) as usize & TABLE_MASK;
    let next = (index + 1) & TABLE_MASK;
    let a = i64::from(sine_table_at(index));
    let b = i64::from(sine_table_at(next));
    let fraction_bits = 32 - TABLE_BITS;
    let fraction = i64::from(phase & ((1 << fraction_bits) - 1));
    // Half an LSB before the shift is what turns a floor into a round.
    let value = a + (((b - a) * fraction + (1 << (fraction_bits - 1))) >> fraction_bits);
    // Between two i16 endpoints, so inside i16's range and far inside
    // i32's.
    #[allow(clippy::cast_possible_truncation)]
    {
        value as i32
    }
}

/// Looks up the f32 sine of a 32-bit phase with linear interpolation.
#[cfg(feature = "mod")]
pub(crate) fn sine_at_f32(phase: u32) -> f32 {
    let idx = (phase >> (32 - TABLE_BITS)) as usize & TABLE_MASK;
    let frac_bits = phase & ((1 << (32 - TABLE_BITS)) - 1);
    let frac = frac_bits as f32 / (1u32 << (32 - TABLE_BITS)) as f32;
    let a = SINE_I16.get(idx).copied().unwrap_or(0) as f32;
    let b = SINE_I16.get((idx + 1) & TABLE_MASK).copied().unwrap_or(0) as f32;
    (a + (b - a) * frac) / 32_767.0
}

/// Computes `round(hz · 2^32 / sample_rate)` — the per-sample phase step.
pub(crate) const fn phase_increment(hz: u32, sample_rate: u32) -> u32 {
    ((((hz as u64) << 32) + (sample_rate as u64) / 2) / (sample_rate as u64)) as u32
}

/// Lowest supported sample rate, in Hz.
pub const SAMPLE_RATE_MIN: u32 = 8_000;
/// Highest supported sample rate, in Hz.
pub const SAMPLE_RATE_MAX: u32 = 48_000;
/// Lowest supported baud rate, in bits per second.
pub const BAUD_MIN: u32 = 1;
/// Highest supported baud rate, in bits per second.
pub const BAUD_MAX: u32 = 9_600;

/// An audio sample rate in Hz, validated to `8_000..=48_000`.
///
/// The tested set is {8000, 11025, 22050, 44100, 48000}; any rate in range
/// is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRate(u32);

impl SampleRate {
    /// Creates a validated sample rate.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::SampleRateOutOfRange`] when `hz` is outside
    /// `8_000..=48_000`.
    pub const fn new(hz: u32) -> Result<Self, ConfigError> {
        if hz >= SAMPLE_RATE_MIN && hz <= SAMPLE_RATE_MAX {
            Ok(Self(hz))
        } else {
            Err(ConfigError::SampleRateOutOfRange {
                got: hz,
                min: SAMPLE_RATE_MIN,
                max: SAMPLE_RATE_MAX,
            })
        }
    }

    /// The rate in Hz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        self.0
    }
}

/// A signalling rate in bits per second, validated to `1..=9_600`.
///
/// Bell 202 uses 1200 baud ([`BaudRate::BELL_202`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaudRate(u32);

impl BaudRate {
    /// The Bell 202 standard rate: 1200 baud.
    pub const BELL_202: Self = Self(1_200);

    /// Creates a validated baud rate.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BaudRateInvalid`] when `bps` is outside
    /// `1..=9_600`.
    pub const fn new(bps: u32) -> Result<Self, ConfigError> {
        if bps >= BAUD_MIN && bps <= BAUD_MAX {
            Ok(Self(bps))
        } else {
            Err(ConfigError::BaudRateInvalid {
                got: bps,
                min: BAUD_MIN,
                max: BAUD_MAX,
            })
        }
    }

    /// The rate in bits per second.
    #[must_use]
    pub const fn bps(self) -> u32 {
        self.0
    }
}

/// The mark/space tone frequencies of an FSK modem, in Hz.
///
/// *Mark* carries a logical one, *space* a logical zero. Bell 202 uses
/// 1200 Hz mark and 2200 Hz space ([`TonePair::BELL_202`]).
///
/// # Provenance of the presets
///
/// The Bell 202 and Bell 103 tone pairs originate in AT&T Bell System
/// Technical References — PUB 41212, "Data Sets 202S and 202T Interface
/// Specification" (August 1974), and PUB 41101, "Data Set 103A Interface
/// Specification" (February 1967). PUB 41212 does not appear to be
/// obtainable online; the values here are the universally observed
/// amateur-radio convention rather than a transcription from the primary
/// document. A useful and freely readable treatment of what "Bell 202"
/// means in amateur practice — including how loosely the name is
/// applied — is Finnegan, K. W. (W6KWF) and Benson, B., "Clarifying the
/// Amateur Bell 202 Modem", TAPR/ARRL Digital Communications Conference,
/// 2014.
///
/// Bell 202 is **not** ITU-T V.23, though the two are often conflated:
/// V.23 uses 1300 Hz mark / 2100 Hz space at 1200 baud.
///
/// [`TonePair::HF_APRS`] (1600/1800 Hz) is an operating convention with
/// no standards document at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TonePair {
    mark: u32,
    space: u32,
}

impl TonePair {
    /// The Bell 202 preset: 1200 Hz mark, 2200 Hz space.
    pub const BELL_202: Self = Self {
        mark: 1_200,
        space: 2_200,
    };

    /// The 300-baud HF APRS convention: 1600 Hz mark, 1800 Hz space
    /// (a 200 Hz shift centered near 1700 Hz, as used on 10.147 MHz).
    ///
    /// ```
    /// use warble::TonePair;
    /// assert_eq!(TonePair::HF_APRS.mark_hz(), 1_600);
    /// assert_eq!(TonePair::HF_APRS.space_hz(), 1_800);
    /// ```
    pub const HF_APRS: Self = Self {
        mark: 1_600,
        space: 1_800,
    };

    /// The Bell 103 *originate*-side tones: 1270 Hz mark, 1070 Hz space.
    ///
    /// ```
    /// use warble::TonePair;
    /// assert_eq!(TonePair::BELL_103_ORIGINATE.mark_hz(), 1_270);
    /// assert_eq!(TonePair::BELL_103_ORIGINATE.space_hz(), 1_070);
    /// ```
    pub const BELL_103_ORIGINATE: Self = Self {
        mark: 1_270,
        space: 1_070,
    };

    /// The Bell 103 *answer*-side tones: 2225 Hz mark, 2025 Hz space.
    ///
    /// ```
    /// use warble::TonePair;
    /// assert_eq!(TonePair::BELL_103_ANSWER.mark_hz(), 2_225);
    /// assert_eq!(TonePair::BELL_103_ANSWER.space_hz(), 2_025);
    /// ```
    pub const BELL_103_ANSWER: Self = Self {
        mark: 2_225,
        space: 2_025,
    };

    /// Creates a validated tone pair for use at the given sample rate.
    ///
    /// Both tones must be nonzero and strictly below the Nyquist frequency
    /// (half the sample rate) so they are representable in sampled audio.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ToneOutOfRange`] naming the offending tone.
    pub const fn new(mark: u32, space: u32, sample_rate: SampleRate) -> Result<Self, ConfigError> {
        let nyquist = sample_rate.hz() / 2;
        if mark == 0 || mark >= nyquist {
            return Err(ConfigError::ToneOutOfRange { got: mark, nyquist });
        }
        if space == 0 || space >= nyquist {
            return Err(ConfigError::ToneOutOfRange {
                got: space,
                nyquist,
            });
        }
        Ok(Self { mark, space })
    }

    /// The mark (logical one) tone in Hz.
    #[must_use]
    pub const fn mark_hz(self) -> u32 {
        self.mark
    }

    /// The space (logical zero) tone in Hz.
    #[must_use]
    pub const fn space_hz(self) -> u32 {
        self.space
    }
}

/// How a modem profile keys bits onto the transmitted waveform.
///
/// Most profiles in this crate are audio FSK: bits select one of two
/// tones ([`ModulationScheme::ToneAfsk`]). G3RUH 9600-baud packet instead
/// transmits a scrambled, band-limited baseband pulse waveform directly
/// ([`ModulationScheme::ScrambledBaseband`]); it has no audio tones at
/// all, so profiles using it ignore their [`TonePair`].
///
/// # Examples
///
/// New to modems? Every named profile tells you its scheme:
///
/// ```
/// use warble::{ModemProfile, ModulationScheme};
/// assert_eq!(ModemProfile::BELL_202.scheme(), ModulationScheme::ToneAfsk);
/// ```
///
/// In a TNC pipeline the scheme decides which physical front end the
/// configuration selects — tone correlators or the baseband filter chain;
/// see [`ModemProfile::G3RUH_9600`] (with the `g3ruh` feature) for the
/// baseband case.
///
/// Note for the protocol-minded: the scheme changes *only* the layer
/// below the bit stream — HDLC framing, NRZI coding, AX.25 and APRS are
/// shared by both variants (G3RUH additionally scrambles the NRZI-coded
/// bits with the `x^17 + x^12 + 1` LFSR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModulationScheme {
    /// Audio FSK: each bit keys one of the two [`TonePair`] tones.
    ToneAfsk,
    /// G3RUH-style scrambled direct-baseband pulses (no audio tones).
    ScrambledBaseband,
}

/// A named modem profile: a [`BaudRate`] bundled with its [`TonePair`].
///
/// Profiles are conveniences, not modes: any validated baud/tone
/// combination keeps working through the checked `new()` constructors of
/// the modulator, demodulator and TNC configs. The named constants cover
/// the common audio-FSK dialects; all of their tones fit under the
/// Nyquist frequency of every supported sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModemProfile {
    baud: BaudRate,
    tones: TonePair,
    scheme: ModulationScheme,
}

impl ModemProfile {
    /// Bell 202: 1200 baud, 1200 Hz mark / 2200 Hz space — the VHF APRS
    /// standard and this crate's default profile.
    ///
    /// ```
    /// use warble::ModemProfile;
    /// assert_eq!(ModemProfile::BELL_202.baud().bps(), 1_200);
    /// assert_eq!(ModemProfile::BELL_202.tones().mark_hz(), 1_200);
    /// assert_eq!(ModemProfile::BELL_202.tones().space_hz(), 2_200);
    /// ```
    pub const BELL_202: Self = Self {
        baud: BaudRate(1_200),
        tones: TonePair::BELL_202,
        scheme: ModulationScheme::ToneAfsk,
    };

    /// HF APRS: 300 baud, 1600 Hz mark / 1800 Hz space — the de-facto
    /// convention for APRS on HF (e.g. 10.147 MHz).
    ///
    /// ```
    /// use warble::ModemProfile;
    /// assert_eq!(ModemProfile::HF_APRS_300.baud().bps(), 300);
    /// assert_eq!(ModemProfile::HF_APRS_300.tones().mark_hz(), 1_600);
    /// assert_eq!(ModemProfile::HF_APRS_300.tones().space_hz(), 1_800);
    /// ```
    pub const HF_APRS_300: Self = Self {
        baud: BaudRate(300),
        tones: TonePair::HF_APRS,
        scheme: ModulationScheme::ToneAfsk,
    };

    /// Bell 103, originate side: 300 baud, 1270 Hz mark / 1070 Hz space.
    ///
    /// [`ModemProfile::BELL_103`] is an alias for this side; the answer
    /// side is [`ModemProfile::BELL_103_ANSWER`].
    ///
    /// ```
    /// use warble::ModemProfile;
    /// assert_eq!(ModemProfile::BELL_103_ORIGINATE.baud().bps(), 300);
    /// assert_eq!(ModemProfile::BELL_103_ORIGINATE.tones().mark_hz(), 1_270);
    /// assert_eq!(ModemProfile::BELL_103_ORIGINATE.tones().space_hz(), 1_070);
    /// ```
    pub const BELL_103_ORIGINATE: Self = Self {
        baud: BaudRate(300),
        tones: TonePair::BELL_103_ORIGINATE,
        scheme: ModulationScheme::ToneAfsk,
    };

    /// Bell 103, answer side: 300 baud, 2225 Hz mark / 2025 Hz space.
    ///
    /// ```
    /// use warble::ModemProfile;
    /// assert_eq!(ModemProfile::BELL_103_ANSWER.baud().bps(), 300);
    /// assert_eq!(ModemProfile::BELL_103_ANSWER.tones().mark_hz(), 2_225);
    /// assert_eq!(ModemProfile::BELL_103_ANSWER.tones().space_hz(), 2_025);
    /// ```
    pub const BELL_103_ANSWER: Self = Self {
        baud: BaudRate(300),
        tones: TonePair::BELL_103_ANSWER,
        scheme: ModulationScheme::ToneAfsk,
    };

    /// Bell 103 (originate side): 300 baud, 1270 Hz mark / 1070 Hz space.
    ///
    /// ```
    /// use warble::ModemProfile;
    /// assert_eq!(ModemProfile::BELL_103, ModemProfile::BELL_103_ORIGINATE);
    /// ```
    pub const BELL_103: Self = Self::BELL_103_ORIGINATE;

    /// G3RUH 9600-baud packet: scrambled direct-baseband FSK
    /// ([`ModulationScheme::ScrambledBaseband`]).
    ///
    /// # Examples
    ///
    /// New to 9600-baud packet? Select this profile and the TNC swaps in
    /// the baseband front end and the LFSR scrambler for you:
    ///
    /// ```
    /// use warble::{ModemProfile, ModulationScheme};
    /// assert_eq!(ModemProfile::G3RUH_9600.baud().bps(), 9_600);
    /// assert_eq!(
    ///     ModemProfile::G3RUH_9600.scheme(),
    ///     ModulationScheme::ScrambledBaseband,
    /// );
    /// ```
    ///
    /// In a TNC pipeline, build a config from it (44.1 kHz and 48 kHz are
    /// the tested rates; ≥ 2 samples per bit is required):
    ///
    /// ```
    /// # #[cfg(feature = "tnc")] {
    /// use warble::tnc::TncConfig;
    /// use warble::{ModemProfile, SampleRate};
    /// let cfg = TncConfig::from_profile(SampleRate::new(44_100)?, ModemProfile::G3RUH_9600)?;
    /// assert_eq!(cfg.baud().bps(), 9_600);
    /// # }
    /// # Ok::<(), warble::ConfigError>(())
    /// ```
    ///
    /// Note for the protocol-minded: the profile's [`TonePair`] is a
    /// placeholder (the Bell 202 pair, valid at every supported sample
    /// rate) — baseband transmission has no audio tones, and the tones
    /// are never used when this scheme is selected.
    #[cfg(feature = "g3ruh")]
    pub const G3RUH_9600: Self = Self {
        baud: BaudRate(9_600),
        tones: TonePair::BELL_202,
        scheme: ModulationScheme::ScrambledBaseband,
    };

    /// Bundles a validated baud rate and tone pair into a tone-AFSK
    /// profile.
    #[must_use]
    pub const fn new(baud: BaudRate, tones: TonePair) -> Self {
        Self {
            baud,
            tones,
            scheme: ModulationScheme::ToneAfsk,
        }
    }

    /// The profile's baud rate.
    #[must_use]
    pub const fn baud(self) -> BaudRate {
        self.baud
    }

    /// The profile's tone pair.
    #[must_use]
    pub const fn tones(self) -> TonePair {
        self.tones
    }

    /// The profile's modulation scheme.
    #[must_use]
    pub const fn scheme(self) -> ModulationScheme {
        self.scheme
    }
}

/// A per-chip device preset for the riscv32 ESP32 family: one variant
/// resolves to a complete, validated modem configuration sized to that
/// chip's compute budget.
///
/// The variants and their budgets come straight from the README's
/// "Will it run on my chip?" feasibility analysis (session 8): every
/// cycle figure quoted below is **ESTIMATED** (extrapolated from a
/// **MEASURED** desktop-class host benchmark to rv32 with stated,
/// conservative assumptions — see the README section for the
/// arithmetic). No on-device number is claimed as verified; each
/// variant's [`DevicePreset::expected_cpu`] note repeats its label.
///
/// Every preset selects the fixed-point `i16` sample path (the crate's
/// `no_std` default): on the no-FPU ESP32-C3/C6/H2 that path is native
/// integer arithmetic with **no soft-float penalty**. Only the
/// FPU-equipped ESP32-P4 has headroom for the explicitly separate
/// `_f32` API twins, and even there the `i16` path remains the
/// recommended default.
///
/// The shortest path from a preset to a running decoder (with the
/// `tnc` feature) is [`DevicePreset::tnc_config`]:
///
/// ```
/// # #[cfg(feature = "tnc")] {
/// use warble::DevicePreset;
/// use warble::ax25::Address;
/// use warble::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, TncTransmitter};
///
/// // ESP32-C3: 1200-baud AFSK on the single balanced decision chain
/// // (~390 ESTIMATED rv32 cycles/sample, ~12% of the 48 kHz budget at
/// // 160 MHz — unconfirmed without on-device measurement).
/// let config = DevicePreset::Esp32C3.tnc_config()?;
/// let mut rx = DefaultTncReceiver::new(config).unwrap();
///
/// // Feed i16 PCM samples (here: a frame synthesized by the matching
/// // transmitter; on hardware, your ADC/radio samples) and collect
/// // the decoded frames.
/// let tx = TncTransmitter::new(config);
/// // Same capacity `DefaultTncReceiver` uses, so anything the
/// // transmitter can build, the receiver can hold.
/// let mut frame_buf = [0u8; MAX_FRAME_BYTES];
/// let len = tx
///     .build_frame_raw(
///         Address::new(b"APRS", 0).unwrap(),
///         Address::new(b"N0CALL", 7).unwrap(),
///         &[],
///         b"hello from a preset",
///         &mut frame_buf,
///     )
///     .unwrap();
/// let mut decoded = 0;
/// for sample in tx.frame_samples_i16(&frame_buf[..len]) {
///     if let Some(frame) = rx.push_i16(sample) {
///         assert_eq!(frame.info(), b"hello from a preset");
///         decoded += 1;
///     }
/// }
/// assert_eq!(decoded, 1);
/// # }
/// # Ok::<(), warble::ConfigError>(())
/// ```
///
/// Distinct from the *mode* presets: [`ModemProfile`] names a
/// dialect on the air (Bell 202, G3RUH 9600, …) independent of the
/// hardware; a `DevicePreset` names a chip and picks the profile *and*
/// the CPU-budget knobs for it. See also the ESP32 hardware guide
/// (`examples/esp32-riscv/README.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePreset {
    /// ESP32-C3 (single-core RV32IMC, 160 MHz, no FPU), conservative:
    /// 1200-baud Bell 202 AFSK on the single balanced decision chain.
    ///
    /// Expected CPU: ~390 ESTIMATED rv32 cycles/sample — ~12% of the
    /// 3333 cycles available per 48 kHz sample at 160 MHz.
    /// Unconfirmed without on-device measurement.
    Esp32C3,
    /// ESP32-C3, full capability: 1200-baud Bell 202 AFSK with the
    /// full 11-chain emphasis-compensating diversity bank (better
    /// recovery on tilted real-world channels, at about 3.4× the CPU —
    /// the sample-rate front end, not the chains, dominates).
    ///
    /// Expected CPU: ~1330 ESTIMATED rv32 cycles/sample — ~40% of the
    /// core at 160 MHz / 48 kHz. Unconfirmed without on-device
    /// measurement.
    Esp32C3FullBank,
    /// ESP32-C6 (160 MHz, no FPU), conservative: same budget as the
    /// C3 (the feasibility table's C6 verdict is "Same as C3").
    ///
    /// Expected CPU: ~390 ESTIMATED rv32 cycles/sample — ~12% at
    /// 160 MHz / 48 kHz. Unconfirmed without on-device measurement.
    Esp32C6,
    /// ESP32-C6, full capability: the full 11-chain bank, same budget
    /// as [`DevicePreset::Esp32C3FullBank`].
    ///
    /// Expected CPU: ~1330 ESTIMATED rv32 cycles/sample — ~40% of the
    /// core at 160 MHz / 48 kHz. Unconfirmed without on-device
    /// measurement.
    Esp32C6FullBank,
    /// ESP32-H2 (96 MHz, no FPU): conservative-only. Single-chain
    /// 1200-baud AFSK; the full 11-chain bank is ESTIMATED at ~66% of
    /// the 48 kHz budget at 96 MHz, which leaves little room for the
    /// rest of a firmware, so no full-bank variant exists for this
    /// chip.
    ///
    /// Expected CPU: ~390 ESTIMATED rv32 cycles/sample — ~20% of the
    /// 2000 cycles available per 48 kHz sample at 96 MHz. Unconfirmed
    /// without on-device measurement.
    Esp32H2,
    /// ESP32-P4 (dual-core, 400 MHz, with FPU): 1200-baud Bell 202
    /// AFSK with the full 11-chain bank — "everything comfortable"
    /// per the feasibility table. The FPU also makes the `_f32` API
    /// twins viable on this chip, but the preset keeps the integer
    /// `i16` path (it is never slower).
    ///
    /// Expected CPU: ~1330 ESTIMATED rv32 cycles/sample — ~16% of one
    /// core (8333 cycles available per 48 kHz sample at 400 MHz).
    Esp32P4,
    /// ESP32-P4 running G3RUH 9600-baud scrambled-baseband packet:
    /// the 400 MHz budget affords the higher symbol rate comfortably.
    ///
    /// Expected CPU: ~125 ESTIMATED rv32 cycles/sample against the
    /// 8333 cycles available per 48 kHz sample at 400 MHz.
    #[cfg(feature = "g3ruh")]
    Esp32P4G3ruh,
}

impl DevicePreset {
    /// Every preset variant, for exhaustive iteration in tests and
    /// tooling.
    pub const ALL: &'static [DevicePreset] = &[
        DevicePreset::Esp32C3,
        DevicePreset::Esp32C3FullBank,
        DevicePreset::Esp32C6,
        DevicePreset::Esp32C6FullBank,
        DevicePreset::Esp32H2,
        DevicePreset::Esp32P4,
        #[cfg(feature = "g3ruh")]
        DevicePreset::Esp32P4G3ruh,
    ];

    /// The modem profile the preset selects: Bell 202 1200-baud AFSK
    /// everywhere except [`DevicePreset::Esp32P4G3ruh`], which selects
    /// G3RUH 9600 scrambled baseband.
    #[must_use]
    pub const fn profile(self) -> ModemProfile {
        match self {
            #[cfg(feature = "g3ruh")]
            DevicePreset::Esp32P4G3ruh => ModemProfile::G3RUH_9600,
            _ => ModemProfile::BELL_202,
        }
    }

    /// The recommended sample rate: 48 kHz for every preset — the rate
    /// the feasibility budgets are computed at, and a tested rate for
    /// both the AFSK and G3RUH paths.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        SampleRate(48_000)
    }

    /// Whether the preset runs the full 11-chain emphasis-compensating
    /// diversity bank (`true`) or the single balanced decision chain
    /// (`false`). Only the 160 MHz chips' full-bank variants and the
    /// 400 MHz P4 afford the full bank; G3RUH always runs one chain.
    ///
    /// This knob trades CPU, **not RAM**: the chain bank is a
    /// fixed-size array, so a receiver is 40 848 B (MEASURED via
    /// `size_of` on a 32-bit-comparable layout) either way. Nor is the
    /// CPU trade as steep as the chain counts suggest — the
    /// sample-rate front end runs at every sweep length, so the full
    /// bank costs about 3.4× the single chain, since the banks a sweep
    /// does not read are now skipped per sample.
    #[must_use]
    pub const fn full_chain_bank(self) -> bool {
        matches!(
            self,
            DevicePreset::Esp32C3FullBank | DevicePreset::Esp32C6FullBank | DevicePreset::Esp32P4
        )
    }

    /// A one-line human description of the chip and the mode the
    /// preset configures.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            DevicePreset::Esp32C3 => {
                "ESP32-C3 (160 MHz, no FPU): 1200-baud Bell 202 AFSK, single \
                 balanced decision chain, i16 fixed-point path"
            }
            DevicePreset::Esp32C3FullBank => {
                "ESP32-C3 (160 MHz, no FPU): 1200-baud Bell 202 AFSK, full \
                 11-chain diversity bank, i16 fixed-point path"
            }
            DevicePreset::Esp32C6 => {
                "ESP32-C6 (160 MHz, no FPU): 1200-baud Bell 202 AFSK, single \
                 balanced decision chain, i16 fixed-point path"
            }
            DevicePreset::Esp32C6FullBank => {
                "ESP32-C6 (160 MHz, no FPU): 1200-baud Bell 202 AFSK, full \
                 11-chain diversity bank, i16 fixed-point path"
            }
            DevicePreset::Esp32H2 => {
                "ESP32-H2 (96 MHz, no FPU): 1200-baud Bell 202 AFSK, single \
                 balanced decision chain, i16 fixed-point path"
            }
            DevicePreset::Esp32P4 => {
                "ESP32-P4 (400 MHz, FPU): 1200-baud Bell 202 AFSK, full \
                 11-chain diversity bank, i16 fixed-point path"
            }
            #[cfg(feature = "g3ruh")]
            DevicePreset::Esp32P4G3ruh => {
                "ESP32-P4 (400 MHz, FPU): G3RUH 9600-baud scrambled baseband, \
                 i16 fixed-point path"
            }
        }
    }

    /// The expected CPU cost of the preset's receive path, quoting the
    /// README feasibility analysis with its honesty labels intact
    /// (**ESTIMATED** = extrapolated from a MEASURED host benchmark to
    /// rv32 with stated conservative assumptions; not verified on
    /// device).
    #[must_use]
    pub const fn expected_cpu(self) -> &'static str {
        match self {
            DevicePreset::Esp32C3 | DevicePreset::Esp32C6 => {
                "~390 ESTIMATED rv32 cycles/sample: ~12% of the 3333 cycles \
                 available per 48 kHz sample at 160 MHz. Unconfirmed without \
                 on-device measurement."
            }
            DevicePreset::Esp32C3FullBank | DevicePreset::Esp32C6FullBank => {
                "~1330 ESTIMATED rv32 cycles/sample: ~40% of the core at \
                 160 MHz / 48 kHz (about 3.4x the single-chain variant). \
                 Unconfirmed without on-device measurement."
            }
            DevicePreset::Esp32H2 => {
                "~390 ESTIMATED rv32 cycles/sample: ~20% of the 2000 cycles \
                 available per 48 kHz sample at 96 MHz. Unconfirmed without \
                 on-device measurement."
            }
            DevicePreset::Esp32P4 => {
                "~1330 ESTIMATED rv32 cycles/sample: ~16% of one core (8333 \
                 cycles available per 48 kHz sample at 400 MHz)."
            }
            #[cfg(feature = "g3ruh")]
            DevicePreset::Esp32P4G3ruh => {
                "~125 ESTIMATED rv32 cycles/sample against the 8333 cycles \
                 available per 48 kHz sample at 400 MHz."
            }
        }
    }
}

/// A single binary symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bit {
    /// Logical zero — keyed as the space tone.
    Zero,
    /// Logical one — keyed as the mark tone.
    One,
}

impl From<bool> for Bit {
    fn from(b: bool) -> Self {
        if b { Bit::One } else { Bit::Zero }
    }
}

impl From<Bit> for bool {
    fn from(bit: Bit) -> bool {
        match bit {
            Bit::Zero => false,
            Bit::One => true,
        }
    }
}

impl From<Bit> for u8 {
    fn from(bit: Bit) -> u8 {
        match bit {
            Bit::Zero => 0,
            Bit::One => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_presets_resolve_to_consistent_parts() {
        for &preset in DevicePreset::ALL {
            let profile = preset.profile();
            let rate = preset.sample_rate();
            assert_eq!(rate.hz(), 48_000, "{preset:?}");
            // Rates consistent: at least 2 samples per bit at the
            // recommended rate.
            assert!(rate.hz() / profile.baud().bps() >= 2, "{preset:?}");
            // The tone pair re-validates at the recommended rate.
            assert!(
                TonePair::new(profile.tones().mark_hz(), profile.tones().space_hz(), rate).is_ok(),
                "{preset:?}"
            );
            assert!(!preset.description().is_empty(), "{preset:?}");
            // Honesty label preserved verbatim in every CPU note.
            assert!(preset.expected_cpu().contains("ESTIMATED"), "{preset:?}");
        }
    }

    #[test]
    fn device_preset_taxonomy_matches_feasibility_table() {
        // Full bank only where the table says the chip affords it.
        assert!(!DevicePreset::Esp32C3.full_chain_bank());
        assert!(DevicePreset::Esp32C3FullBank.full_chain_bank());
        assert!(!DevicePreset::Esp32C6.full_chain_bank());
        assert!(DevicePreset::Esp32C6FullBank.full_chain_bank());
        // H2 (96 MHz) is conservative-only.
        assert!(!DevicePreset::Esp32H2.full_chain_bank());
        assert!(DevicePreset::Esp32P4.full_chain_bank());
        #[cfg(feature = "g3ruh")]
        {
            assert!(!DevicePreset::Esp32P4G3ruh.full_chain_bank());
            assert_eq!(
                DevicePreset::Esp32P4G3ruh.profile().scheme(),
                ModulationScheme::ScrambledBaseband
            );
        }
    }

    #[test]
    fn sample_rate_accepts_boundaries() {
        assert_eq!(SampleRate::new(8_000).map(SampleRate::hz), Ok(8_000));
        assert_eq!(SampleRate::new(48_000).map(SampleRate::hz), Ok(48_000));
    }

    #[test]
    fn sample_rate_accepts_tested_set() {
        for hz in [8_000, 11_025, 22_050, 44_100, 48_000] {
            assert!(SampleRate::new(hz).is_ok(), "{hz} should be accepted");
        }
    }

    #[test]
    fn sample_rate_rejects_below_min() {
        assert_eq!(
            SampleRate::new(7_999),
            Err(ConfigError::SampleRateOutOfRange {
                got: 7_999,
                min: 8_000,
                max: 48_000
            })
        );
    }

    #[test]
    fn sample_rate_rejects_above_max() {
        assert_eq!(
            SampleRate::new(48_001),
            Err(ConfigError::SampleRateOutOfRange {
                got: 48_001,
                min: 8_000,
                max: 48_000
            })
        );
    }

    #[test]
    fn sample_rate_rejects_zero() {
        assert!(SampleRate::new(0).is_err());
    }

    #[test]
    fn baud_rate_accepts_boundaries() {
        assert_eq!(BaudRate::new(1).map(BaudRate::bps), Ok(1));
        assert_eq!(BaudRate::new(9_600).map(BaudRate::bps), Ok(9_600));
        assert_eq!(BaudRate::new(1_200).map(BaudRate::bps), Ok(1_200));
    }

    #[test]
    fn baud_rate_rejects_zero() {
        assert_eq!(
            BaudRate::new(0),
            Err(ConfigError::BaudRateInvalid {
                got: 0,
                min: 1,
                max: 9_600
            })
        );
    }

    #[test]
    fn baud_rate_rejects_above_max() {
        assert_eq!(
            BaudRate::new(9_601),
            Err(ConfigError::BaudRateInvalid {
                got: 9_601,
                min: 1,
                max: 9_600
            })
        );
    }

    #[test]
    fn baud_rate_bell_202_preset() {
        assert_eq!(BaudRate::BELL_202.bps(), 1_200);
    }

    #[test]
    fn tone_pair_bell_202_preset() {
        assert_eq!(TonePair::BELL_202.mark_hz(), 1_200);
        assert_eq!(TonePair::BELL_202.space_hz(), 2_200);
    }

    #[test]
    fn tone_pair_accepts_below_nyquist() {
        let sr = match SampleRate::new(8_000) {
            Ok(sr) => sr,
            Err(e) => panic!("unexpected: {e}"),
        };
        let pair = TonePair::new(1_200, 2_200, sr);
        assert_eq!(pair.map(TonePair::mark_hz), Ok(1_200));
    }

    #[test]
    fn tone_pair_rejects_zero_mark() {
        let sr = match SampleRate::new(48_000) {
            Ok(sr) => sr,
            Err(e) => panic!("unexpected: {e}"),
        };
        assert_eq!(
            TonePair::new(0, 2_200, sr),
            Err(ConfigError::ToneOutOfRange {
                got: 0,
                nyquist: 24_000
            })
        );
    }

    #[test]
    fn tone_pair_rejects_space_at_nyquist() {
        let sr = match SampleRate::new(8_000) {
            Ok(sr) => sr,
            Err(e) => panic!("unexpected: {e}"),
        };
        assert_eq!(
            TonePair::new(1_200, 4_000, sr),
            Err(ConfigError::ToneOutOfRange {
                got: 4_000,
                nyquist: 4_000
            })
        );
    }

    #[test]
    fn tone_pair_accepts_just_below_nyquist() {
        let sr = match SampleRate::new(8_000) {
            Ok(sr) => sr,
            Err(e) => panic!("unexpected: {e}"),
        };
        assert!(TonePair::new(1_200, 3_999, sr).is_ok());
    }

    #[test]
    fn bit_from_bool_roundtrip() {
        assert_eq!(Bit::from(true), Bit::One);
        assert_eq!(Bit::from(false), Bit::Zero);
        assert!(bool::from(Bit::One));
        assert!(!bool::from(Bit::Zero));
    }

    #[test]
    fn bit_to_u8() {
        assert_eq!(u8::from(Bit::Zero), 0);
        assert_eq!(u8::from(Bit::One), 1);
    }
}
