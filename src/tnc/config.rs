//! Configuration shared by both directions of the TNC pipeline.
//!
//! [`TncConfig`] is the single validated description of a link: the
//! modem profile, the HDLC flag counts, the chain-bank sweep, and the
//! recovery, band-pass and voting policies the receiver reads. It lives
//! apart from either direction because both use it and it changes for
//! reasons of its own. Re-exported from [`crate::tnc`].

use core::fmt;

use crate::aprs::{AprsError, AprsUiError};
use crate::ax25::{Ax25Error, RecoveryPolicy, hdlc};
#[cfg(feature = "g3ruh")]
use crate::baseband::BasebandModulator;
use crate::demodulator::DemodulatorConfig;
use crate::error::ConfigError;
use crate::modulator::ModulatorConfig;
use crate::types::{BaudRate, DevicePreset, ModemProfile, ModulationScheme, SampleRate, TonePair};

/// Largest supported number of parallel slicer chains in a space-gain
/// sweep (see [`SpaceGainSweep`]).
pub const MAX_SWEEP: usize = 9;

/// Total chain-bank size: the sweep-driven chains plus two extra
/// emphasized chains at intermediate gains, widening the de-emphasized
/// channel coverage without retiring any sweep chain.
pub(super) const MAX_CHAINS: usize = MAX_SWEEP + 2;

/// A validated set of space-tone gains for the parallel slicer bank.
///
/// A pre- or de-emphasized channel tilts the two AFSK tone amplitudes by
/// several dB, biasing a raw mark/space envelope comparison toward the
/// louder tone. Rather than estimating the tilt, [`super::TncReceiver`] runs
/// several *decision chains* in parallel: the shared front end runs three
/// [`crate::discriminator::QuadratureCorrelator`] banks per sample (six tone correlators — over
/// the raw, band-passed, and pre-emphasized+band-passed sample streams),
/// and each chain compares `mark` against `gain · space` on one of those
/// envelope pairs with its own bit-clock recovery, NRZI decoder and HDLC
/// deframer, the gains sweeping geometrically across the plausible tilt
/// range. Whichever chain's gain best matches the channel tilt decodes
/// the frame; frames recovered by several chains at once are
/// de-duplicated by content, so each transmission is emitted exactly once.
///
/// Gains are Q8 fixed-point multipliers (`256` = unity). The default sweep
/// spans 0.609×..4.867× (−4.30 dB..+13.75 dB) in nine geometric steps. It
/// therefore covers the +5 dB side of a typical emphasis tilt with margin
/// but stops short of it on the negative side, where the range ends at
/// −4.30 dB.
///
/// # Prior art
///
/// Running several demodulators in parallel over differently emphasized
/// copies of the same audio, and de-duplicating the frames they agree on,
/// is **not this crate's idea**. It is the "strike-twice-hit-once" design
/// of:
///
/// > Sivan Toledo, 4X6IZ, "A High-Performance Sound-Card AX.25 Modem",
/// > *QEX*, July/August 2012.
/// > <https://www.cs.tau.ac.il/~stoledo/Bib/Pubs/QEX-JulAug-2012.pdf>
///
/// That paper establishes the two premises this bank rests on: that
/// mark/space amplitude imbalance is unavoidable at the receiver (radios
/// disagree about the 6 dB/octave pre-emphasis curve, and some bypass it
/// entirely), and that it is cheaper to run parallel demodulators tuned
/// for different tilts than to estimate the tilt and pick one. It also
/// originates the benchmark method this crate uses — replaying the TNC
/// Test CD and counting recovered frames while varying one parameter —
/// which is what `docs/BENCHMARKS.md` and `tests/benchmark.rs` do.
///
/// The **shape** of the sweep, nine chains, geometric, spanning a factor
/// of eight, follows the reference implementation's application of the
/// paper's idea. The gains shipped here were then moved by measurement
/// and span 0.609x..4.867x.
///
/// What is this crate's own: the gain values (measured — see the table on
/// [`SpaceGainSweep::DEFAULT`]), the three input variants rather than
/// two, a per-chain PLL/NRZI/HDLC stack instead of a shared one, and the
/// cross-chain bit-history voting in [`ChainVoting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceGainSweep {
    gains: [u16; MAX_SWEEP],
    len: usize,
}

impl SpaceGainSweep {
    /// The default sweep: nine Q8 gains from 0.609× to 4.867× in
    /// geometric steps of 8^(1/8) ≈ 1.2966 (−4.30 dB to +13.75 dB, an
    /// 18.05 dB span, in 2.25 dB steps).
    pub const DEFAULT: Self = Self {
        gains: [156, 202, 262, 340, 441, 572, 741, 961, 1246],
        len: 9,
    };

    /// A single-chain "sweep" at unity gain: the plain balanced
    /// comparison, for callers that want the smallest possible receiver.
    pub const UNITY: Self = Self {
        gains: [256, 0, 0, 0, 0, 0, 0, 0, 0],
        len: 1,
    };

    /// Builds a sweep from explicit Q8 gains (order is preserved).
    ///
    /// # Errors
    ///
    /// [`ConfigError::SweepLenInvalid`] when `gains` is empty or longer
    /// than [`MAX_SWEEP`]; [`ConfigError::SweepGainZero`] when any gain is
    /// zero (a zero gain would make that chain compare mark against
    /// silence, emitting pure noise decisions).
    pub const fn new(gains: &[u16]) -> Result<Self, ConfigError> {
        if gains.is_empty() || gains.len() > MAX_SWEEP {
            return Err(ConfigError::SweepLenInvalid {
                got: gains.len(),
                max: MAX_SWEEP,
            });
        }
        let mut packed = [0u16; MAX_SWEEP];
        let mut i = 0;
        while i < gains.len() {
            if gains[i] == 0 {
                return Err(ConfigError::SweepGainZero { index: i });
            }
            packed[i] = gains[i];
            i += 1;
        }
        Ok(Self {
            gains: packed,
            len: gains.len(),
        })
    }

    /// The active gains, in sweep order.
    #[must_use]
    pub fn gains(&self) -> &[u16] {
        self.gains.get(..self.len).unwrap_or(&[])
    }

    /// The number of parallel chains this sweep drives.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the sweep has no gains (never true for a validated sweep;
    /// provided for the `len`/`is_empty` convention).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The index of the gain closest to unity (256): the *primary* chain,
    /// whose FCS/oversize rejections feed [`TncStats`] so error counters
    /// keep single-receiver semantics.
    pub(super) fn primary_index(&self) -> usize {
        let mut best = 0;
        let mut best_dist = u16::MAX;
        for (i, &g) in self.gains().iter().enumerate() {
            let dist = g.abs_diff(256);
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
        best
    }
}

impl Default for SpaceGainSweep {
    /// Same as [`SpaceGainSweep::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A TNC-layer failure: any of the composed layers can reject its input.
///
/// Every variant wraps the typed error of the layer that failed, so the
/// rendered message names both the layer and the violated rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TncError {
    /// The APRS information field could not be serialized or parsed.
    Aprs(AprsError),
    /// The AX.25 layer rejected an address, path or frame buffer.
    Ax25(Ax25Error),
    /// The DSP configuration was invalid.
    Config(ConfigError),
}

impl fmt::Display for TncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TncError::Aprs(ref e) => write!(f, "APRS layer: {e}"),
            TncError::Ax25(ref e) => write!(f, "AX.25 layer: {e}"),
            TncError::Config(ref e) => write!(f, "configuration: {e}"),
        }
    }
}

impl core::error::Error for TncError {}

impl From<AprsError> for TncError {
    fn from(e: AprsError) -> Self {
        TncError::Aprs(e)
    }
}

impl From<Ax25Error> for TncError {
    fn from(e: Ax25Error) -> Self {
        TncError::Ax25(e)
    }
}

impl From<ConfigError> for TncError {
    fn from(e: ConfigError) -> Self {
        TncError::Config(e)
    }
}

impl From<AprsUiError> for TncError {
    fn from(e: AprsUiError) -> Self {
        match e {
            AprsUiError::Aprs(inner) => TncError::Aprs(inner),
            AprsUiError::Ax25(inner) => TncError::Ax25(inner),
        }
    }
}

/// Whether the receive input band-pass filter (~900..3500 Hz) is
/// applied ahead of the tone correlators.
///
/// See [`TncConfig::with_band_pass`] for what the filter does and the
/// measured trade-off. Defaults to [`InputBandPass::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputBandPass {
    /// No input band-pass: the mixed raw/filtered/emphasized default
    /// chain bank (the measured default).
    #[default]
    Off,
    /// Band-pass every chain's input (opt-in; see `docs/BENCHMARKS.md`).
    On,
}

impl InputBandPass {
    /// Whether the band-pass is selected.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// Whether cross-chain candidate voting runs on FCS failures in the
/// diversity receiver.
///
/// See [`TncConfig::with_voting`] for the mechanism. Defaults to
/// [`ChainVoting::On`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChainVoting {
    /// Reject FCS failures without a cross-chain vote.
    Off,
    /// Majority-vote aligned chain histories on FCS failures (the
    /// measured default).
    #[default]
    On,
}

impl ChainVoting {
    /// Whether voting is selected.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// A validated TNC configuration: DSP parameters plus HDLC flag counts.
///
/// Wraps a [`ModulatorConfig`] and a [`DemodulatorConfig`] built from the
/// same sample rate, baud rate and tone pair, together with the preamble
/// and tail flag counts used on transmit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TncConfig {
    pub(super) modulator: ModulatorConfig,
    pub(super) demodulator: DemodulatorConfig,
    pub(super) preamble_flags: usize,
    pub(super) tail_flags: usize,
    pub(super) sweep: SpaceGainSweep,
    pub(super) recovery: RecoveryPolicy,
    pub(super) band_pass: InputBandPass,
    pub(super) voting: ChainVoting,
    /// A pristine baseband modulator template when the configuration
    /// selects the G3RUH scrambled-baseband scheme; `None` selects the
    /// tone-AFSK paths (which stay byte-identical to before this field
    /// existed).
    #[cfg(feature = "g3ruh")]
    pub(super) baseband: Option<BasebandModulator>,
}

impl TncConfig {
    /// Builds a configuration from validated parts with the default flag
    /// counts ([`hdlc::DEFAULT_PREAMBLE_FLAGS`] /
    /// [`hdlc::DEFAULT_TAIL_FLAGS`]).
    ///
    /// # Errors
    ///
    /// [`ConfigError::BaudExceedsSampleRate`] when the sample rate yields
    /// fewer than 2 samples per bit.
    pub const fn new(
        sample_rate: SampleRate,
        baud: BaudRate,
        tones: TonePair,
    ) -> Result<Self, ConfigError> {
        let modulator = match ModulatorConfig::new(sample_rate, baud, tones) {
            Ok(m) => m,
            Err(e) => return Err(e),
        };
        let demodulator = match DemodulatorConfig::new(sample_rate, baud, tones) {
            Ok(d) => d,
            Err(e) => return Err(e),
        };
        // The full-width default sweep (and the emphasized chains it
        // brings along) is tuned against Bell-202 pre-/de-emphasis
        // measurements on the real-world corpus; for any other profile
        // it would apply Bell-202-specific tilt compensation to a
        // channel it was never measured on, so non-Bell-202 profiles
        // start from the single balanced chain
        // ([`SpaceGainSweep::UNITY`]) and callers opt into wider banks
        // explicitly via [`TncConfig::with_space_gain_sweep`].
        let is_bell_202 = baud.bps() == 1_200
            && tones.mark_hz() == TonePair::BELL_202.mark_hz()
            && tones.space_hz() == TonePair::BELL_202.space_hz();
        let sweep = if is_bell_202 {
            SpaceGainSweep::DEFAULT
        } else {
            SpaceGainSweep::UNITY
        };
        Ok(Self {
            modulator,
            demodulator,
            preamble_flags: hdlc::DEFAULT_PREAMBLE_FLAGS,
            tail_flags: hdlc::DEFAULT_TAIL_FLAGS,
            sweep,
            // Enabled by default at the TNC layer: repair is
            // sanity-gated (the repaired frame must re-validate the FCS
            // and parse as a UI frame) and measurably recovers
            // real-world frames while keeping the clean-corpus canary at
            // exactly its reference count. `PreDestuffFlip` extends the
            // syndrome-based post-destuff repair with a bounded retry
            // from the raw pre-destuff bit window, fixing errors that
            // hit stuffing bits. Opt out with
            // `with_recovery(RecoveryPolicy::None)`; the raw
            // `HdlcDeframer` default remains `None`.
            recovery: RecoveryPolicy::PreDestuffFlip,
            // Off by default: measured on the corpus it lifts the noisy
            // tracks but costs one flutter-fade frame (see
            // docs/BENCHMARKS.md), so it stays opt-in.
            band_pass: InputBandPass::Off,
            // Cross-chain candidate voting on FCS failures: bounded
            // work, fires only when a frame already failed every
            // recovery pass, and the voted result must still pass the
            // full FCS + UI sanity gate. Default-on (measured: no row
            // regresses; see docs/BENCHMARKS.md).
            voting: ChainVoting::On,
            #[cfg(feature = "g3ruh")]
            baseband: None,
        })
    }

    /// The Bell 202 preset (1200 baud, 1200/2200 Hz) at `sample_rate`.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] when the tones do not fit under the Nyquist
    /// frequency of `sample_rate`.
    pub const fn bell_202(sample_rate: SampleRate) -> Result<Self, ConfigError> {
        Self::from_profile(sample_rate, ModemProfile::BELL_202)
    }

    /// Builds a configuration from a named [`ModemProfile`] at
    /// `sample_rate`.
    ///
    /// [`ModemProfile::BELL_202`] gets the full receiver chain bank
    /// (identical to [`TncConfig::bell_202`]); every other profile
    /// starts from a single balanced decision chain
    /// ([`SpaceGainSweep::UNITY`]), since the wide bank's gains encode
    /// Bell-202-specific emphasis compensation.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] when the profile's tones do not fit under the
    /// Nyquist frequency of `sample_rate`, or the rate yields fewer than
    /// 2 samples per bit.
    pub const fn from_profile(
        sample_rate: SampleRate,
        profile: ModemProfile,
    ) -> Result<Self, ConfigError> {
        // Re-validate the tones against this sample rate (the profile
        // constants are rate-independent).
        let tones = match TonePair::new(
            profile.tones().mark_hz(),
            profile.tones().space_hz(),
            sample_rate,
        ) {
            Ok(t) => t,
            Err(e) => return Err(e),
        };
        let base = match Self::new(sample_rate, profile.baud(), tones) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        match profile.scheme() {
            ModulationScheme::ToneAfsk => Ok(base),
            ModulationScheme::ScrambledBaseband => Self::into_baseband(base, sample_rate, profile),
        }
    }

    /// Attaches the baseband front end selected by a
    /// [`ModulationScheme::ScrambledBaseband`] profile.
    #[cfg(feature = "g3ruh")]
    const fn into_baseband(
        mut base: Self,
        sample_rate: SampleRate,
        profile: ModemProfile,
    ) -> Result<Self, ConfigError> {
        base.baseband = match BasebandModulator::new(sample_rate, profile.baud()) {
            Ok(m) => Some(m),
            Err(e) => return Err(e),
        };
        Ok(base)
    }

    /// Without the `g3ruh` feature no constructor can produce a
    /// scrambled-baseband profile, so this arm is unreachable; it fails
    /// closed with the validation error a baseband profile would hit
    /// first.
    #[cfg(not(feature = "g3ruh"))]
    const fn into_baseband(
        _base: Self,
        sample_rate: SampleRate,
        profile: ModemProfile,
    ) -> Result<Self, ConfigError> {
        Err(ConfigError::BaudExceedsSampleRate {
            baud: profile.baud().bps(),
            sample_rate: sample_rate.hz(),
        })
    }

    /// The configured modulation scheme: [`ModulationScheme::ToneAfsk`]
    /// unless a scrambled-baseband profile (e.g.
    /// [`ModemProfile::G3RUH_9600`], `g3ruh` feature) was selected via
    /// [`TncConfig::from_profile`].
    #[must_use]
    pub const fn scheme(self) -> ModulationScheme {
        #[cfg(feature = "g3ruh")]
        if self.baseband.is_some() {
            return ModulationScheme::ScrambledBaseband;
        }
        ModulationScheme::ToneAfsk
    }

    /// Replaces the transmit preamble and tail flag counts.
    #[must_use]
    pub const fn with_flags(mut self, preamble_flags: usize, tail_flags: usize) -> Self {
        self.preamble_flags = preamble_flags;
        self.tail_flags = tail_flags;
        self
    }

    /// Replaces the receive space-gain sweep (see [`SpaceGainSweep`]).
    #[must_use]
    pub const fn with_space_gain_sweep(mut self, sweep: SpaceGainSweep) -> Self {
        self.sweep = sweep;
        self
    }

    /// The configured receive space-gain sweep.
    #[must_use]
    pub const fn space_gain_sweep(self) -> SpaceGainSweep {
        self.sweep
    }

    /// Replaces the receive FCS recovery policy (see [`RecoveryPolicy`]).
    ///
    /// [`TncConfig`] defaults to [`RecoveryPolicy::PreDestuffFlip`]:
    /// sanity-gated single-bit repair of frames failing the FCS check.
    /// Pass [`RecoveryPolicy::None`] to reject all FCS failures instead.
    ///
    /// [`RecoveryPolicy::None`] is honored everywhere: it also keeps the
    /// cross-chain voting path (see [`TncConfig::with_voting`]) from
    /// running — voting validation ends in the same repair sweep the
    /// policy disables — so no receive push ever enters a repair pass.
    /// [`TncConfig::bounded_latency`] packages this for hard per-call
    /// latency budgets.
    #[must_use]
    pub const fn with_recovery(mut self, recovery: RecoveryPolicy) -> Self {
        self.recovery = recovery;
        self
    }

    /// The configured receive FCS recovery policy.
    #[must_use]
    pub const fn recovery(self) -> RecoveryPolicy {
        self.recovery
    }

    /// Bounds the worst-case work of a single receive push: sets
    /// [`RecoveryPolicy::None`] and [`ChainVoting::Off`], removing every
    /// FCS-failure repair path from the receiver.
    ///
    /// The default configuration trades latency for sensitivity: when a
    /// corrupted frame closes, the pre-destuff repair sweep re-destuffs
    /// the raw bit window once per flipped bit — O(content_bits²) over a
    /// window of up to 4096 bits, potentially per chain — so the one
    /// `push_i16`/`push_f32` call delivering the closing flag can absorb
    /// seconds of work on a slow MCU. With this preset that sweep never
    /// runs anywhere (including the cross-chain voting path), making the
    /// receiver suitable for hard per-call latency budgets
    /// (interrupt-adjacent main loops, shared-MCU duty cycles).
    ///
    /// # Tradeoff
    ///
    /// You lose the repair sweep's recovery of corrupted frames: frames
    /// failing the FCS check are simply rejected (and counted in
    /// [`super::TncStats::fcs_errors`]). Clean-signal decoding is unaffected —
    /// undamaged frames never enter any repair path. What remains per
    /// call is the steady-state per-sample DSP (constant, small — the
    /// dominant, unavoidable cost) plus ordinary frame-close validation
    /// (destuff + FCS + parse, linear in the frame). If you run the
    /// FX.25 FEC layer on top, its Reed-Solomon decode still bursts at
    /// frame close (≈ 0.25–0.6 M RV32 cycles, ≈ 1.5–3.75 ms at
    /// 160 MHz); that is an FX.25 cost, not a [`super::TncReceiver`] one.
    ///
    /// # Combining with a device preset
    ///
    /// [`DevicePreset`] resolution composes: resolve the preset, then
    /// apply the bound.
    ///
    /// ```
    /// use yodel::DevicePreset;
    /// use yodel::ax25::RecoveryPolicy;
    /// use yodel::tnc::{ChainVoting, TncConfig};
    ///
    /// let cfg: TncConfig = DevicePreset::Esp32C3.tnc_config()?.bounded_latency();
    /// assert_eq!(cfg.recovery(), RecoveryPolicy::None);
    /// assert_eq!(cfg.voting(), ChainVoting::Off);
    /// # Ok::<(), yodel::ConfigError>(())
    /// ```
    #[must_use]
    pub const fn bounded_latency(mut self) -> Self {
        self.recovery = RecoveryPolicy::None;
        self.voting = ChainVoting::Off;
        self
    }

    /// Selects whether the receive input band-pass (targeting
    /// ~900..3500 Hz) is applied: cascaded one-pole high-pass and
    /// low-pass stages ahead of the tone correlators, stripping
    /// out-of-band rumble (e.g. de-emphasis boosted low frequencies) and
    /// hiss before they leak into the correlator sidelobes.
    /// [`InputBandPass::Off`] by default.
    ///
    /// This is not a plain on/off switch over one fixed bank: it picks a
    /// different bank. With the default sweep, [`InputBandPass::Off`]
    /// builds the mixed 11-chain bank — 2 raw chains, 4 band-passed and 5
    /// pre-emphasized — with effective Q8 gains `[156, 202, 262, 340,
    /// 194, 572, 256, 961, 441, 215, 345]`. [`InputBandPass::On`] builds
    /// a 9-chain bank in which *every* chain is band-passed, at the
    /// nominal sweep gains `[156, 202, 262, 340, 441, 572, 741, 961,
    /// 1246]`: turning it on therefore also drops the two extra chains
    /// and removes the pre-emphasis diversity entirely.
    #[must_use]
    pub const fn with_band_pass(mut self, band_pass: InputBandPass) -> Self {
        self.band_pass = band_pass;
        self
    }

    /// The configured receive input band-pass selection.
    #[must_use]
    pub const fn band_pass(self) -> InputBandPass {
        self.band_pass
    }

    /// Selects whether cross-chain candidate voting runs: when one
    /// chain's deframer closes a frame that fails the FCS check (even
    /// after bit-flip recovery), the receiver aligns the other chains'
    /// recent bit histories to that chain's raw frame window by
    /// correlation (±-bit slide, ≥80% agreement to qualify),
    /// majority-votes each bit, and destuffs + FCS-checks the voted
    /// window once. Bounded, fixed-buffer work that fires only on FCS
    /// failures; the voted frame must pass the full FCS and UI-frame
    /// sanity gates before being emitted. [`ChainVoting::On`] by
    /// default. With [`RecoveryPolicy::None`] the receiver keeps voting
    /// off regardless of this setting: voting is a repair path, and the
    /// policy promises none run (see [`TncConfig::bounded_latency`]).
    #[must_use]
    pub const fn with_voting(mut self, voting: ChainVoting) -> Self {
        self.voting = voting;
        self
    }

    /// The configured cross-chain candidate voting selection.
    #[must_use]
    pub const fn voting(self) -> ChainVoting {
        self.voting
    }

    /// The configured sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        self.modulator.sample_rate()
    }

    /// The configured baud rate.
    #[must_use]
    pub const fn baud(self) -> BaudRate {
        self.modulator.baud()
    }

    /// The configured tone pair.
    #[must_use]
    pub const fn tones(self) -> TonePair {
        self.modulator.tones()
    }

    /// The transmit preamble flag count.
    #[must_use]
    pub const fn preamble_flags(self) -> usize {
        self.preamble_flags
    }

    /// The transmit tail flag count.
    #[must_use]
    pub const fn tail_flags(self) -> usize {
        self.tail_flags
    }
}

impl DevicePreset {
    /// Resolves the preset to a complete, validated [`TncConfig`]:
    /// the preset's [`ModemProfile`] at its recommended 48 kHz sample
    /// rate, with the receive chain bank sized to the chip's budget
    /// ([`SpaceGainSweep::UNITY`] for the conservative presets, the
    /// full default bank where [`DevicePreset::full_chain_bank`] says
    /// the chip affords it).
    ///
    /// Wrap the result in a [`super::TncReceiver`] / [`super::TncTransmitter`] and
    /// feed `i16` samples — see the example on [`DevicePreset`].
    ///
    /// # Errors
    ///
    /// Never fails for the shipped presets (every profile validates at
    /// 48 kHz); the `Result` is the signature of the underlying checked
    /// constructor, so `?` composes.
    pub const fn tnc_config(self) -> Result<TncConfig, ConfigError> {
        let base = match TncConfig::from_profile(self.sample_rate(), self.profile()) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        let sweep = if self.full_chain_bank() {
            SpaceGainSweep::DEFAULT
        } else {
            SpaceGainSweep::UNITY
        };
        Ok(base.with_space_gain_sweep(sweep))
    }
}
