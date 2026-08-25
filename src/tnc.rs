//! High-level TNC pipeline: PCM samples ↔ APRS packets.
//!
//! This module ties every layer of the crate together into a
//! terminal-node-controller style API. It composes — never duplicates —
//! the existing building blocks:
//!
//! * transmit: [`crate::aprs::build_ui_frame`] →
//!   [`crate::ax25::hdlc::frame_bits`] → [`crate::nrzi::encode_iter`] →
//!   [`crate::modulator::Modulator`] (the same chain as [`crate::ax25::tx_i16`], with the
//!   flag counts taken from the [`TncConfig`]);
//! * receive: [`TncReceiver`], which builds its own parallel chain bank
//!   (shared tone correlators feeding per-chain bit-clock recovery, NRZI
//!   decoding and HDLC deframing) rather than reusing
//!   [`crate::ax25::FrameReceiver`], plus [`UiFrame::parse`] and, on
//!   demand, [`AprsPacket::parse`].
//!
//! Everything is `no_std` and allocation-free: the transmitter serializes
//! into caller-provided buffers and returns lazy sample iterators; the
//! receiver accumulates into a fixed const-generic buffer. Heap
//! conveniences ([`TncTransmitter::transmit_to_vec_i16`] and friends)
//! appear only with the `alloc` feature.
//!
//! # PHY seam note
//!
//! Two front ends enter this module through *different* mechanisms: the
//! tone-AFSK path goes through the [`crate::discriminator::Discriminator`]
//! trait (via the demodulator), while the G3RUH scrambled-baseband path
//! (`g3ruh` feature) enters as cfg'd `Option<Baseband…>` branches here,
//! because baseband G3RUH replaces the whole discriminator+slicer pair —
//! the trait's granularity is wrong for it. The full rationale, the shape
//! of a future unification, and why it is deferred live in
//! `docs/ARCHITECTURE.md` ("The PHY seam"), which also explains this
//! module's size and internal regions.
//!
//! # Example flow
//!
//! Build a [`TncConfig`] (usually [`TncConfig::bell_202`]), wrap it in a
//! [`TncTransmitter`] and a [`TncReceiver`], then push the transmitter's
//! samples into the receiver: each completed, FCS-valid UI frame comes
//! back as an [`RxFrame`], ready for [`RxFrame::aprs`].

use crate::aprs::{AprsError, AprsPacket, Decoded};
#[cfg(feature = "micE")]
use crate::aprs::{MicE, MicEError, mic_e};
use crate::ax25::frame::MAX_DIGIPEATERS;
use crate::ax25::{Address, Ax25Error, HdlcDeframer, PathHop, RecoveryPolicy, UiFrame, hdlc};
#[cfg(feature = "g3ruh")]
use crate::baseband::BasebandFilter;
use crate::discriminator::QuadratureCorrelator;
use crate::error::ConfigError;
use crate::nrzi::NrziDecoder;
#[cfg(feature = "g3ruh")]
use crate::scrambler::Descrambler;
use crate::slicer::Slicer;
use crate::types::Bit;
#[cfg(any(feature = "g3ruh", test))]
use crate::types::ModulationScheme;
use crate::types::TonePair;

mod config;
mod tx;

// config and tx are an internal split of this module. Everything they
// define is re-exported here, so every public path is unchanged and
// this file keeps only the receiver.
pub use config::*;
pub use tx::*;

// Shared internals the receiver reads from the config module.
use config::MAX_CHAINS;

/// Default receive-buffer capacity in bytes: the address field at its
/// longest (10 addresses × 7 bytes), control, PID, and a 256-byte
/// information field, with slack for the FCS.
pub const MAX_FRAME_BYTES: usize = 330;

/// [`TncReceiver`] with the default [`MAX_FRAME_BYTES`] capacity.
pub type DefaultTncReceiver = TncReceiver<MAX_FRAME_BYTES>;

/// One received, FCS-valid AX.25 UI frame.
///
/// Borrowed from the receiver's internal buffer until the next push; the
/// APRS payload is decoded lazily via [`RxFrame::aprs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxFrame<'a> {
    frame: UiFrame<'a>,
}

impl<'a> RxFrame<'a> {
    /// The destination address (the APRS tocall).
    #[must_use]
    pub const fn dest(&self) -> Address {
        self.frame.dest
    }

    /// The source address (the sending station).
    #[must_use]
    pub const fn src(&self) -> Address {
        self.frame.src
    }

    /// The digipeater path.
    #[must_use]
    pub fn path(&self) -> &[Address] {
        self.frame.path()
    }

    /// The raw information field.
    #[must_use]
    pub const fn info(&self) -> &'a [u8] {
        self.frame.info
    }

    /// The full parsed UI frame.
    #[must_use]
    pub const fn ui_frame(&self) -> &UiFrame<'a> {
        &self.frame
    }

    /// Parses the information field as an APRS packet.
    ///
    /// A Mic-E information field (data type `` ` `` or `'`) cannot be
    /// decoded here — the destination callsign carries half the position
    /// — so with the `micE` feature use [`RxFrame::mic_e`] for those, or
    /// [`RxFrame::decoded`] for a single call that handles both.
    ///
    /// # Errors
    ///
    /// The parse errors documented on [`AprsPacket::parse`].
    pub fn aprs(&self) -> Result<AprsPacket<'a>, AprsError> {
        AprsPacket::parse(self.frame.info)
    }

    /// Decodes the frame as a Mic-E report, combining the destination
    /// callsign with the information field.
    ///
    /// The **strict** Mic-E path, kept alongside [`RxFrame::decoded`]
    /// for the same reason [`RxFrame::aprs`] is kept alongside it: this
    /// one hands back the typed [`MicEError`] saying which byte or
    /// column was wrong, which `decoded` folds into
    /// [`AprsError::MicE`].
    ///
    /// # Errors
    ///
    /// The decode errors documented on [`mic_e::decode_address`].
    #[cfg(feature = "micE")]
    pub fn mic_e(&self) -> Result<MicE<'a>, MicEError> {
        mic_e::decode_address(self.frame.dest, self.frame.info)
    }

    /// Totally decodes the frame: destination address plus information
    /// field, in one call that **cannot fail**.
    ///
    /// [`RxFrame::aprs`] and [`RxFrame::mic_e`] are strict and each
    /// covers half of what arrives on 144.39 MHz; this covers all of
    /// it, labelling what it cannot type instead of rejecting it. Mic-E
    /// lands on [`DecodedKind::MicE`](crate::aprs::DecodedKind::MicE),
    /// reachable via [`Decoded::mic_e`].
    ///
    /// ```
    /// # #[cfg(all(feature = "micE", feature = "mod", feature = "alloc"))] {
    /// use yodel::SampleRate;
    /// use yodel::aprs::{AprsPacket, Status};
    /// use yodel::ax25::Address;
    /// use yodel::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};
    ///
    /// let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
    /// let tx = TncTransmitter::new(cfg);
    /// let mut rx = DefaultTncReceiver::new(cfg)?;
    /// let samples = tx.transmit_to_vec_i16(
    ///     &AprsPacket::Status(Status { text: b"QRV" }),
    ///     Address::new(b"APRS", 0)?,
    ///     Address::new(b"N0CALL", 0)?,
    ///     &[],
    /// )?;
    /// for sample in samples {
    ///     if let Some(frame) = rx.push_i16(sample) {
    ///         assert!(frame.decoded().is_typed());
    ///     }
    /// }
    /// # }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn decoded(&self) -> Decoded<'a> {
        // `dest()` hands back an owned `Address` copy -- a temporary --
        // yet the result is `Decoded<'a>`, not `Decoded<'_>` tied to
        // `&self`. That works because `decode_frame` takes the address
        // *by value* and keeps only the `info` borrow, so nothing in
        // the returned value points at the temporary.
        Decoded::decode_frame(self.dest(), self.info())
    }
}

/// One received UI frame that owns its storage.
///
/// [`RxFrame`] is *lending*: it borrows the receiver's internal buffer
/// and dies at the next push. That is the right shape for the
/// allocation-free core, but it forces every consumer that moves
/// frames across a channel, a thread boundary, or any other
/// outlives-the-loop seam to copy the fields out by hand. `OwnedFrame`
/// is that copy, done once and correctly: fixed-capacity inline
/// storage (no allocation, `no_std`-friendly, `alloc` not required),
/// the digipeater path kept as [`PathHop`]s so per-hop H bits survive
/// the copy.
///
/// Use [`RxFrame`] when you consume the frame inside the receive loop;
/// convert to `OwnedFrame` (via [`OwnedFrame::new`] or `TryFrom`) when
/// the frame must outlive it — e.g. sending decoded frames through a
/// bounded channel to a slower sink (`examples/decode_many_threads.rs`).
///
/// The info field is capped at [`MAX_FRAME_BYTES`]; frames from a
/// [`DefaultTncReceiver`] always fit (its whole frame buffer is that
/// size), so conversion only fails for oversized custom-`N` receivers.
///
/// ```
/// use yodel::SampleRate;
/// use yodel::aprs::{AprsPacket, Status};
/// use yodel::ax25::Address;
/// use yodel::tnc::{DefaultTncReceiver, OwnedFrame, TncConfig, TncTransmitter};
///
/// let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
/// let tx = TncTransmitter::new(cfg);
/// let mut rx = DefaultTncReceiver::new(cfg)?;
/// let mut owned: Option<OwnedFrame> = None;
/// let samples = tx.transmit_to_vec_i16(
///     &AprsPacket::Status(Status { text: b"QRV" }),
///     Address::new(b"APRS", 0)?,
///     Address::new(b"N0CALL", 0)?,
///     &[],
/// )?;
/// for sample in samples {
///     if let Some(frame) = rx.push_i16(sample) {
///         owned = Some(OwnedFrame::new(&frame)?); // copies out of the borrow
///     }
/// }
/// let frame = owned.expect("one frame decodes");
/// // The owned copy is self-contained: the receiver can keep running
/// // (or be dropped) while the frame crosses a channel or thread.
/// drop(rx);
/// assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
/// assert_eq!(frame.info(), b">QRV");
/// assert_eq!(frame.aprs()?, AprsPacket::Status(Status { text: b"QRV" }));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct OwnedFrame {
    dest: Address,
    src: Address,
    hops: [PathHop; MAX_DIGIPEATERS],
    hop_count: usize,
    info: [u8; MAX_FRAME_BYTES],
    info_len: usize,
}

impl OwnedFrame {
    /// Copies a received frame out of the receiver's lending borrow.
    ///
    /// # Errors
    ///
    /// [`Ax25Error::FrameTooLarge`] when the frame's information field
    /// exceeds [`MAX_FRAME_BYTES`] (impossible for frames from a
    /// [`DefaultTncReceiver`], whose whole frame is capped there).
    pub fn new(frame: &RxFrame<'_>) -> Result<Self, Ax25Error> {
        let ui = frame.ui_frame();
        if ui.info.len() > MAX_FRAME_BYTES {
            return Err(Ax25Error::FrameTooLarge {
                len: ui.info.len(),
                max: MAX_FRAME_BYTES,
            });
        }
        let mut hops = [PathHop::unused(ui.src); MAX_DIGIPEATERS];
        let mut hop_count = 0;
        for (slot, hop) in hops.iter_mut().zip(ui.hops()) {
            *slot = hop;
            hop_count += 1;
        }
        let mut info = [0u8; MAX_FRAME_BYTES];
        for (dst, &b) in info.iter_mut().zip(ui.info.iter()) {
            *dst = b;
        }
        Ok(Self {
            dest: ui.dest,
            src: ui.src,
            hops,
            hop_count,
            info,
            info_len: ui.info.len(),
        })
    }

    /// The destination address (the APRS tocall).
    #[must_use]
    pub const fn dest(&self) -> Address {
        self.dest
    }

    /// The source address (the sending station).
    #[must_use]
    pub const fn src(&self) -> Address {
        self.src
    }

    /// The digipeater path with per-hop has-been-repeated (H) bits.
    #[must_use]
    pub fn hops(&self) -> &[PathHop] {
        self.hops.get(..self.hop_count).unwrap_or(&[])
    }

    /// The information field.
    #[must_use]
    pub fn info(&self) -> &[u8] {
        self.info.get(..self.info_len).unwrap_or(&[])
    }

    /// The frame as a borrowed [`UiFrame`] view (e.g. to re-serialize
    /// it with [`UiFrame::build`]).
    ///
    /// # Errors
    ///
    /// None in practice: the hop count came from a parsed frame, so it
    /// is within [`MAX_DIGIPEATERS`]; the `Result` mirrors
    /// [`UiFrame::with_hops`].
    pub fn ui_frame(&self) -> Result<UiFrame<'_>, Ax25Error> {
        UiFrame::with_hops(self.dest, self.src, self.hops(), self.info())
    }

    /// Parses the information field as an APRS packet
    /// (see [`RxFrame::aprs`] for the Mic-E caveat).
    ///
    /// # Errors
    ///
    /// The parse errors documented on [`AprsPacket::parse`].
    pub fn aprs(&self) -> Result<AprsPacket<'_>, AprsError> {
        AprsPacket::parse(self.info())
    }

    /// Decodes the frame as a Mic-E report, combining the destination
    /// callsign with the information field
    /// (see [`RxFrame::mic_e`] for why the strict path is kept).
    ///
    /// # Errors
    ///
    /// The decode errors documented on [`mic_e::decode_address`].
    #[cfg(feature = "micE")]
    pub fn mic_e(&self) -> Result<MicE<'_>, MicEError> {
        mic_e::decode_address(self.dest, self.info())
    }

    /// Totally decodes the frame; see [`RxFrame::decoded`].
    #[must_use]
    pub fn decoded(&self) -> Decoded<'_> {
        Decoded::decode_frame(self.dest(), self.info())
    }
}

impl TryFrom<&RxFrame<'_>> for OwnedFrame {
    type Error = Ax25Error;

    /// Same as [`OwnedFrame::new`].
    fn try_from(frame: &RxFrame<'_>) -> Result<Self, Ax25Error> {
        Self::new(frame)
    }
}

impl PartialEq for OwnedFrame {
    fn eq(&self, other: &Self) -> bool {
        self.dest == other.dest
            && self.src == other.src
            && self.hops() == other.hops()
            && self.info() == other.info()
    }
}

impl Eq for OwnedFrame {}

/// Receive statistics: what the deframer accepted and rejected.
///
/// Counters saturate at [`u32::MAX`] rather than wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TncStats {
    /// Frames that passed the FCS check and parsed as UI frames.
    pub frames_ok: u32,
    /// Frames rejected for a frame-check-sequence mismatch.
    pub fcs_errors: u32,
    /// Frames dropped because they outgrew the receive buffer.
    pub oversize: u32,
    /// FCS-valid frames whose contents did not parse as a UI frame
    /// (bad address field, control or PID byte, or a runt).
    pub malformed: u32,
}

/// Which input variant a decision chain consumes (input diversity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainInput {
    /// The unfiltered sample stream.
    Raw,
    /// The band-passed copy (HP ~900 Hz + LP ~3.5 kHz one-poles).
    BandPassed,
    /// The pre-emphasized copy (first-difference high boost), undoing a
    /// transmitter/channel 6 dB-per-octave de-emphasis at the waveform
    /// level so those chains see a near-flat signal.
    Emphasized,
}

/// One decision chain of the parallel slicer bank: a fixed space-tone
/// gain, its own bit-clock DPLL, NRZI decoder and HDLC deframer.
#[derive(Debug, Clone)]
struct Chain<const N: usize> {
    /// Q8 space-tone gain for this chain's mark/space comparison.
    gain: i64,
    /// Which input variant this chain consumes (input diversity).
    input: ChainInput,
    slicer: Slicer,
    nrzi: NrziDecoder,
    deframer: HdlcDeframer<N>,
    /// Ring of recent post-NRZI bits (cross-chain candidate voting).
    hist: BitHistory,
}

/// Capacity of a chain's bit-history ring: the deframer raw window plus
/// slack for the alignment slide.
const HIST_BITS: usize = hdlc::RAW_BITS + 16;
/// Byte size of the bit-packed history ring.
const HIST_BYTES: usize = HIST_BITS / 8;
/// Largest alignment slide tried when matching another chain's history
/// to a failed frame window (bits deeper into the history).
const VOTE_SLIDE: usize = 8;
/// Minimum whole-window agreement for an aligned copy to join the vote,
/// as a numerator over 10 (i.e. 80%).
const VOTE_AGREE_NUM: usize = 8;

/// A fixed-size ring of recent bits, bit-packed, with a monotone count.
#[derive(Debug, Clone)]
struct BitHistory {
    bits: [u8; HIST_BYTES],
    /// Total bits ever pushed.
    count: u64,
}

impl BitHistory {
    const fn new() -> Self {
        Self {
            bits: [0; HIST_BYTES],
            count: 0,
        }
    }

    /// Records one bit (overwrites the oldest once full).
    const fn push(&mut self, bit: Bit) {
        let idx = (self.count % HIST_BITS as u64) as usize;
        let mask = 1u8 << (idx % 8);
        match bit {
            Bit::One => self.bits[idx / 8] |= mask,
            Bit::Zero => self.bits[idx / 8] &= !mask,
        }
        self.count = self.count.wrapping_add(1);
    }

    /// The bit `back` positions before the most recent one (`back` = 0 is
    /// the newest). `None` when out of range (future or evicted).
    fn get_back(&self, back: usize) -> Option<bool> {
        if back as u64 >= self.count || back >= HIST_BITS {
            return None;
        }
        let abs = self.count - 1 - back as u64;
        let idx = (abs % HIST_BITS as u64) as usize;
        Some((self.bits[idx / 8] >> (idx % 8)) & 1 != 0)
    }
}

/// A cheap fixed-point band-pass (targeting ~900..3500 Hz for Bell 202;
/// the realized corners are rate-dependent, see [`BandPass::new`]): a
/// one-pole high-pass (subtracting a slow low-frequency tracker) cascaded
/// with a one-pole low-pass, both with shift coefficients. States are
/// kept in Q8 so small inputs are not quantized away by the shifts.
#[derive(Debug, Clone, Copy)]
struct BandPass {
    /// Low-frequency tracker state (Q8); output = input − this.
    hp_state: i32,
    /// Low-pass state (Q8).
    lp_state: i32,
    /// High-pass corner shift: cutoff ≈ sr / (2π·2^shift).
    hp_shift: u32,
    /// Low-pass corner shift.
    lp_shift: u32,
}

impl BandPass {
    /// Picks the power-of-two coefficient whose one-pole cutoff
    /// `sr / (2π·2^shift)` lies nearest `cutoff_hz`.
    fn shift_for(sample_rate: u32, cutoff_hz: u32) -> u32 {
        // 2π·cutoff scaled by 128/201 ≈ 1/(2π)⁻¹ avoided: compare
        // sr/2^s against 2π·cutoff using the integer approximation
        // 2π ≈ 710/113; here 2π·f ≈ f·710/113.
        let target = ((cutoff_hz as u64) * 710 / 113) as u32;
        let mut best = 0;
        let mut best_err = u32::MAX;
        for s in 0..12u32 {
            let fc = sample_rate >> s;
            let err = fc.abs_diff(target);
            if err < best_err {
                best_err = err;
                best = s;
            }
        }
        best
    }

    fn new(sample_rate: u32, baud: u32, tones: TonePair) -> Self {
        // Corners derived from the tone pair rather than hardcoded for
        // Bell 202: high-pass at 3/4 of the lowest tone, low-pass one
        // baud (plus slack) above the highest tone to keep the main FSK
        // sidebands. For Bell 202 (1200/2200 Hz at 1200 Bd) that targets
        // 900/3500 Hz, but `shift_for` can only place a corner at
        // `sample_rate >> s`, so what is realized depends on the rate:
        //
        //   48 kHz    -> 955 / 3820 Hz
        //   44.1 kHz  -> 877 / 3509 Hz
        //   22.05 kHz -> 877 / 3509 Hz
        //   11.025 kHz-> 877 / 1755 Hz
        //   8 kHz     -> 637 / 1273 Hz
        //
        // At 11.025 kHz and below the low-pass corner lands at or below
        // the 2200 Hz space tone, so the band-passed stream carries a
        // measured mark/space amplitude tilt (+1.22 dB at 11.025 kHz,
        // +2.52 dB at 8 kHz). The space-gain sweep absorbs it.
        let low = if tones.mark_hz() < tones.space_hz() {
            tones.mark_hz()
        } else {
            tones.space_hz()
        };
        let high = if tones.mark_hz() > tones.space_hz() {
            tones.mark_hz()
        } else {
            tones.space_hz()
        };
        Self {
            hp_state: 0,
            lp_state: 0,
            hp_shift: Self::shift_for(sample_rate, low * 3 / 4),
            lp_shift: Self::shift_for(sample_rate, high + baud + 100),
        }
    }

    /// Filters one sample (i16-scale input, i16-scale output).
    fn push(&mut self, sample: i32) -> i32 {
        let x = sample << 8;
        self.hp_state += (x - self.hp_state) >> self.hp_shift;
        let high_passed = x - self.hp_state;
        self.lp_state += (high_passed - self.lp_state) >> self.lp_shift;
        self.lp_state >> 8
    }
}

/// A first-difference pre-emphasis equalizer: `y[n] = x[n] − a·x[n−1]`
/// with `a` in Q8. This is a cheap +6 dB-per-octave high boost that
/// undoes the −6 dB-per-octave tilt of a de-emphasized channel, restoring
/// a near-flat waveform for the chains that consume it. Output can reach
/// ~2× the input scale, well within the correlators' headroom.
#[derive(Debug, Clone, Copy)]
struct PreEmphasis {
    /// Previous input sample.
    prev: i32,
    /// Q8 feedback coefficient `a`. The only construction is
    /// `PreEmphasis::new(256)`, i.e. `a` = 1.0: a plain first difference.
    a_q8: i32,
}

impl PreEmphasis {
    fn new(a_q8: i32) -> Self {
        Self { prev: 0, a_q8 }
    }

    /// Filters one sample (i16-scale input, ~2× i16-scale output).
    fn push(&mut self, sample: i32) -> i32 {
        let y = sample - ((self.prev * self.a_q8) >> 8);
        self.prev = sample;
        y
    }
}

/// One remembered accepted frame for de-duplication: a cheap content key
/// plus the sample count at which it completed.
#[derive(Debug, Clone, Copy, Default)]
struct SeenFrame {
    /// CRC-16/X.25 of the frame contents (FCS already stripped).
    crc: u16,
    /// Frame content length in bytes.
    len: u16,
    /// Sample counter value when the frame completed.
    seen_at: u64,
    /// Whether this slot holds a real entry.
    valid: bool,
}

/// The G3RUH receive chain: baseband filter front end, PLL slicer,
/// descrambler, NRZI decoder and HDLC deframer — the scrambled-baseband
/// twin of the tone chain bank (single chain; the sweep's gain diversity
/// compensates tone tilt, which baseband transmission does not have).
#[cfg(feature = "g3ruh")]
#[derive(Debug, Clone)]
struct BasebandRx<const N: usize> {
    filter: BasebandFilter,
    slicer: Slicer,
    descrambler: Descrambler,
    nrzi: NrziDecoder,
    deframer: HdlcDeframer<N>,
}

/// APRS-over-AX.25 receiver: PCM samples in, decoded frames out.
///
/// Runs three [`QuadratureCorrelator`] banks per sample — over the raw,
/// band-passed, and pre-emphasized+band-passed sample streams, six tone
/// correlators in all — then feeds their mark/space envelope pairs to a
/// bank of parallel decision chains, one per gain in the configured
/// [`SpaceGainSweep`] (plus two extra emphasized chains in the full-width
/// default bank). Chain *i* compares `mark` against `gain_i · space` (Q8)
/// with its own bit-clock DPLL, NRZI decoder and HDLC deframer, so a
/// channel-tilted transmission is decoded by whichever chain's gain
/// matches the tilt. A frame that passes the FCS in
/// several chains is emitted exactly once: accepted frames are remembered
/// by a `(crc, len)` content key for a short window and repeats within it
/// are dropped as duplicates. Each FCS-valid frame is parsed into an
/// [`RxFrame`]; rejected frames never panic and never surface as errors —
/// they are tallied in [`TncReceiver::stats`].
///
/// `N` is the receive buffer capacity in bytes — the largest frame,
/// excluding FCS, that can be received. [`DefaultTncReceiver`] pins the
/// documented default of [`MAX_FRAME_BYTES`] (330) bytes.
///
/// The sweep length trades CPU, not RAM. The chain bank is allocated at
/// its full width ([`MAX_SWEEP`] + 2 slots) whatever the sweep, so
/// `size_of::<TncReceiver<330>>()` is 40 848 bytes for both
/// [`SpaceGainSweep::UNITY`] and the 11-chain default; the 11 `N`-byte
/// deframer buffers are only about a twelfth of that, and over half the
/// struct (23 448 bytes) is the three correlator banks. What a shorter
/// sweep saves is per-sample work: both the chain loop **and** the
/// correlator banks those chains no longer read. A `UNITY` bank is a
/// single raw chain, so two of the three banks are skipped entirely and
/// decode time drops by MEASURED ~60% (60.7 → 24.2 ns/sample on the
/// host). Before the unused banks were gated it saved only ~28%,
/// because they ran whatever the sweep length.
///
/// # Common path: PCM samples in, one decoded frame out
///
/// Push one sample at a time; each push that completes an FCS-valid UI
/// frame returns it (borrowed until the next push). Here the sample
/// source is the paired transmitter, so exactly one frame comes back:
///
/// ```
/// use yodel::SampleRate;
/// use yodel::aprs::{AprsPacket, Status};
/// use yodel::ax25::Address;
/// use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};
///
/// let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
/// let tx = TncTransmitter::new(cfg);
/// let mut info_buf = [0u8; 32];
/// let mut frame_buf = [0u8; 64];
/// let samples = tx.transmit_i16(
///     &AprsPacket::Status(Status { text: b"QRV" }),
///     Address::new(b"APRS", 0)?,
///     Address::new(b"N0CALL", 0)?,
///     &[],
///     &mut info_buf,
///     &mut frame_buf,
/// )?;
///
/// let mut rx: DefaultTncReceiver = TncReceiver::new(cfg)?;
/// let mut frames = 0;
/// for sample in samples {
///     if let Some(frame) = rx.push_i16(sample) {
///         assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
///         assert_eq!(frame.aprs()?, AprsPacket::Status(Status { text: b"QRV" }));
///         frames += 1;
///     }
/// }
/// assert_eq!(frames, 1);
/// assert_eq!(rx.stats().frames_ok, 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct TncReceiver<const N: usize> {
    discriminator: QuadratureCorrelator,
    /// Second correlator fed by the band-passed input, driving the
    /// band-passed chains (input-filter diversity); the raw correlator
    /// keeps serving the rest.
    disc_filtered: QuadratureCorrelator,
    /// Third correlator fed by the pre-emphasized input, driving the
    /// emphasized chains (waveform-level EQ diversity).
    disc_emphasized: QuadratureCorrelator,
    /// Input band-pass ahead of the filtered correlator (see
    /// [`TncConfig::with_band_pass`]); with `InputBandPass::On` every
    /// chain consumes the filtered stream.
    band_pass: BandPass,
    /// Pre-emphasis EQ ahead of the emphasized correlator.
    pre_emphasis: PreEmphasis,
    /// Band-pass after the pre-emphasis (emphasis doubles hiss; the
    /// low-pass strips it back out before the correlator).
    band_pass_emph: BandPass,
    chains: [Chain<N>; MAX_CHAINS],
    /// Whether any active chain consumes the raw correlator. Computed
    /// once from the built chains; an unconsumed bank is skipped per
    /// sample rather than computed and thrown away.
    needs_raw: bool,
    /// Whether any active chain consumes the band-passed correlator.
    needs_filtered: bool,
    /// Whether any active chain consumes the pre-emphasized correlator.
    needs_emphasized: bool,
    /// Active chains (= sweep length, plus the extra emphasized chains
    /// on a full-width sweep); the rest of the array is idle.
    active: usize,
    /// Index of the chain nearest unity gain: its FCS/oversize rejections
    /// feed [`TncStats`] so error counters keep single-receiver semantics.
    primary: usize,
    /// Recently accepted frame keys for duplicate suppression.
    seen: [SeenFrame; MAX_CHAINS],
    /// Next `seen` slot to overwrite (ring order).
    seen_next: usize,
    /// Samples pushed so far (timestamps the dedup window).
    samples: u64,
    /// Dedup window in samples: chains complete the same frame within a
    /// couple of bit periods of each other, while real on-air repeats
    /// are at least a frame duration apart.
    window: u64,
    /// Copy of the frame being emitted this push (owned so the borrow
    /// outlives the chain iteration).
    out_buf: [u8; N],
    /// Whether cross-chain candidate voting is enabled.
    voting: ChainVoting,
    /// The scrambled-baseband receive chain, replacing the tone chain
    /// bank when the configuration selects a G3RUH profile.
    #[cfg(feature = "g3ruh")]
    baseband: Option<BasebandRx<N>>,
    stats: TncStats,
}

impl<const N: usize> TncReceiver<N> {
    /// Builds a receiver from a validated configuration.
    ///
    /// # Errors
    ///
    /// [`ConfigError::BaudExceedsSampleRate`] when the configuration
    /// yields fewer than 2 samples per bit (already ruled out by
    /// [`TncConfig::new`], kept for defensive construction paths).
    pub fn new(config: TncConfig) -> Result<Self, ConfigError> {
        let demod = config.demodulator;
        let discriminator =
            QuadratureCorrelator::new(demod.sample_rate(), demod.baud(), demod.tones())?;
        let slicer = Slicer::new(demod.sample_rate(), demod.baud())?;
        let sweep = config.sweep;
        let gains = sweep.gains();
        // Two extra emphasized chains (indices >= sweep.len()) at
        // intermediate Q8 gains between the existing emphasized trio's
        // 194/256/441, widening de-emphasized coverage without retiring
        // any sweep chain. Only added to the full-width default bank.
        let extras: usize = if !config.band_pass.is_on() && sweep.len() == MAX_SWEEP {
            2
        } else {
            0
        };
        let chains = core::array::from_fn(|i| {
            let mut slicer = slicer.clone();
            // Sampling-phase diversity: stagger the initial bit-clock
            // phase across chains so at least one chain starts sampling
            // near mid-cell of a short preamble before its DPLL locks.
            // Wraps for the extra chains (i >= MAX_SWEEP), offset half a
            // step so they do not duplicate chain 0/1 phases.
            let stagger = (i as u32).wrapping_mul(u32::MAX / MAX_SWEEP as u32);
            slicer.set_initial_phase(if i >= MAX_SWEEP {
                stagger.wrapping_add(u32::MAX / (2 * MAX_SWEEP as u32))
            } else {
                stagger
            });
            if i >= sweep.len() {
                // Extra emphasized chain (active only when `extras` > 0).
                return Chain {
                    gain: if i == sweep.len() { 215 } else { 345 },
                    input: ChainInput::Emphasized,
                    slicer,
                    nrzi: NrziDecoder::default(),
                    deframer: HdlcDeframer::with_recovery(config.recovery),
                    hist: BitHistory::new(),
                };
            }
            // Input diversity: odd chains take the band-passed stream,
            // the extreme-gain even chains (whose gains rarely match a
            // real tilt) are re-purposed as pre-emphasis EQ chains with
            // near-unity gains (the emphasized signal is ~flat), the
            // rest stay raw; `InputBandPass::On` filters every chain.
            let emphasized = !config.band_pass.is_on()
                && sweep.len() >= 3
                && i % 2 == 0
                && i + 5 >= sweep.len()
                && i > 0;
            let input = if emphasized {
                ChainInput::Emphasized
            } else if config.band_pass.is_on() || i % 2 == 1 {
                ChainInput::BandPassed
            } else {
                ChainInput::Raw
            };
            let gain = if emphasized {
                match sweep.len() - i {
                    1 => 441,
                    3 => 256,
                    _ => 194,
                }
            } else {
                gains.get(i).copied().unwrap_or(256) as i64
            };
            Chain {
                gain,
                input,
                slicer,
                nrzi: NrziDecoder::default(),
                deframer: HdlcDeframer::with_recovery(config.recovery),
                hist: BitHistory::new(),
            }
        });
        let spb = (demod.sample_rate().hz() / demod.baud().bps()) as u64;
        let active = (sweep.len() + extras).clamp(1, MAX_CHAINS);
        // Which correlator banks any active chain reads. The rest are
        // skipped per sample; see `push_sample`.
        let live = &chains[..active];
        let needs_raw = live.iter().any(|c| matches!(c.input, ChainInput::Raw));
        let needs_filtered = live
            .iter()
            .any(|c| matches!(c.input, ChainInput::BandPassed));
        let needs_emphasized = live
            .iter()
            .any(|c| matches!(c.input, ChainInput::Emphasized));
        Ok(Self {
            disc_filtered: discriminator.clone(),
            disc_emphasized: discriminator.clone(),
            discriminator,
            band_pass: BandPass::new(demod.sample_rate().hz(), demod.baud().bps(), demod.tones()),
            pre_emphasis: PreEmphasis::new(256),
            band_pass_emph: BandPass::new(
                demod.sample_rate().hz(),
                demod.baud().bps(),
                demod.tones(),
            ),
            chains,
            needs_raw,
            needs_filtered,
            needs_emphasized,
            active,
            primary: sweep.primary_index(),
            seen: [SeenFrame::default(); MAX_CHAINS],
            seen_next: 0,
            samples: 0,
            window: spb.saturating_mul(32),
            out_buf: [0; N],
            // `RecoveryPolicy::None` promises "no FCS-failure repair,
            // ever", and cross-chain voting is itself a repair path
            // whose window validation ends in the same O(content_bits²)
            // pre-destuff bit-flip sweep the policy exists to disable
            // (`HdlcDeframer::try_voted_window`). Honor the policy here:
            // with recovery off, voting is off too, so no push can ever
            // enter a repair sweep. Bounded-latency configurations
            // ([`TncConfig::bounded_latency`]) rely on this invariant.
            voting: match config.recovery {
                RecoveryPolicy::None => ChainVoting::Off,
                RecoveryPolicy::SingleBitFlip | RecoveryPolicy::PreDestuffFlip => config.voting,
            },
            #[cfg(feature = "g3ruh")]
            baseband: match config.scheme() {
                ModulationScheme::ToneAfsk => None,
                ModulationScheme::ScrambledBaseband => Some(BasebandRx {
                    filter: BasebandFilter::new(demod.sample_rate(), demod.baud()),
                    slicer: Slicer::new(demod.sample_rate(), demod.baud())?,
                    descrambler: Descrambler::default(),
                    nrzi: NrziDecoder::default(),
                    deframer: HdlcDeframer::with_recovery(config.recovery),
                }),
            },
            stats: TncStats::default(),
        })
    }

    /// The receive statistics so far.
    #[must_use]
    pub const fn stats(&self) -> TncStats {
        self.stats
    }

    /// Pushes one `i16` PCM sample; returns a decoded frame when one
    /// completes.
    ///
    /// The returned frame borrows the internal buffer until the next
    /// push. Frames failing the FCS check, outgrowing the buffer, or not
    /// parsing as UI frames are counted in [`TncReceiver::stats`] and
    /// yield `None`; a frame decoded by several chains at once is emitted
    /// only once.
    pub fn push_i16(&mut self, sample: i16) -> Option<RxFrame<'_>> {
        self.push_sample(sample as i32)
    }

    /// Pushes one `f32` PCM sample; the twin of [`TncReceiver::push_i16`].
    pub fn push_f32(&mut self, sample: f32) -> Option<RxFrame<'_>> {
        let scaled = (sample * 32_767.0).clamp(-32_768.0, 32_767.0) as i32;
        self.push_sample(scaled)
    }

    /// Shared entry: runs the correlator banks the active chains
    /// consume, then advances the chain bank.
    ///
    /// Which of the three input variants are live is fixed when the
    /// chains are built, so the unused banks are skipped entirely rather
    /// than computed and discarded. That matters most exactly where it
    /// is most wanted: [`SpaceGainSweep::UNITY`] builds a single raw
    /// chain, so two of the three banks — two correlator pairs, the
    /// band-pass, the pre-emphasis and a second band-pass — were pure
    /// waste on the conservative [`crate::DevicePreset`] variants that
    /// embedded users are steered towards.
    fn push_sample(&mut self, sample: i32) -> Option<RxFrame<'_>> {
        #[cfg(feature = "g3ruh")]
        if self.baseband.is_some() {
            return self.push_baseband(sample);
        }
        let raw = if self.needs_raw {
            self.discriminator.push_envelopes(sample)
        } else {
            (0, 0)
        };
        let filtered = if self.needs_filtered {
            let filtered_sample = self.band_pass.push(sample);
            self.disc_filtered.push_envelopes(filtered_sample)
        } else {
            (0, 0)
        };
        let emphasized = if self.needs_emphasized {
            let emphasized_sample = self.band_pass_emph.push(self.pre_emphasis.push(sample));
            self.disc_emphasized.push_envelopes(emphasized_sample)
        } else {
            (0, 0)
        };
        self.push_envelopes(raw, filtered, emphasized)
    }

    /// The scrambled-baseband receive path: FIR low-pass + baseline/amplitude
    /// centering → PLL slicer → descrambler → NRZI decode → HDLC
    /// deframe. Mirrors the tone path's accounting on [`TncStats`].
    #[cfg(feature = "g3ruh")]
    fn push_baseband(&mut self, sample: i32) -> Option<RxFrame<'_>> {
        self.samples = self.samples.wrapping_add(1);
        let rx = self.baseband.as_mut()?;
        let metric = rx.filter.push(sample);
        let line = rx.slicer.push(metric)?;
        // RX order: raw sliced bits are descrambled BEFORE NRZI decode
        // (the exact inverse of the TX NRZI → scramble → synthesize).
        let data = rx.nrzi.decode(rx.descrambler.descramble(line));
        let event = rx.deframer.push(data)?;
        let len = match event {
            Ok(frame) => {
                let len = frame.len().min(N);
                for (dst, src) in self.out_buf.iter_mut().zip(frame.iter()) {
                    *dst = *src;
                }
                len
            }
            Err(Ax25Error::FcsMismatch { .. }) => {
                self.stats.fcs_errors = self.stats.fcs_errors.saturating_add(1);
                return None;
            }
            Err(Ax25Error::FrameTooLarge { .. }) => {
                self.stats.oversize = self.stats.oversize.saturating_add(1);
                return None;
            }
            Err(_) => {
                self.stats.malformed = self.stats.malformed.saturating_add(1);
                return None;
            }
        };
        match UiFrame::parse(self.out_buf.get(..len).unwrap_or(&[])) {
            Ok(frame) => {
                self.stats.frames_ok = self.stats.frames_ok.saturating_add(1);
                Some(RxFrame { frame })
            }
            Err(_) => {
                self.stats.malformed = self.stats.malformed.saturating_add(1);
                None
            }
        }
    }

    /// Advances every active chain by one `(mark, space)` envelope pair
    /// and merges their frame events.
    fn push_envelopes(
        &mut self,
        raw: (i64, i64),
        filtered: (i64, i64),
        emphasized: (i64, i64),
    ) -> Option<RxFrame<'_>> {
        self.samples = self.samples.wrapping_add(1);
        let mut out_len: Option<usize> = None;
        let mut fcs_failed: Option<usize> = None;
        for i in 0..self.active {
            let Some(chain) = self.chains.get_mut(i) else {
                break;
            };
            let (mark, space) = match chain.input {
                ChainInput::Raw => raw,
                ChainInput::BandPassed => filtered,
                ChainInput::Emphasized => emphasized,
            };
            // Per-chain comparison: mark vs gain-scaled space, clamped
            // into the i32 metric range the slicer expects. Magnitudes
            // reach ~2²¹ and gains ≤4×, so the product fits in i64.
            let scaled = (space.saturating_mul(chain.gain)) >> 8;
            let metric = (mark - scaled).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            let Some(line) = chain.slicer.push(metric) else {
                continue;
            };
            let data = chain.nrzi.decode(line);
            chain.hist.push(data);
            let Some(event) = chain.deframer.push(data) else {
                continue;
            };
            match event {
                Ok(frame) => {
                    let key = (crate::ax25::crc16_x25(frame), frame.len() as u16);
                    let fresh = Self::register(
                        &mut self.seen,
                        &mut self.seen_next,
                        self.samples,
                        self.window,
                        key,
                    );
                    if fresh && out_len.is_none() {
                        // First fresh frame this sample: copy it out so
                        // the borrow survives the remaining chains.
                        let len = frame.len().min(N);
                        for (dst, src) in self.out_buf.iter_mut().zip(frame.iter()) {
                            *dst = *src;
                        }
                        out_len = Some(len);
                    }
                }
                Err(Ax25Error::FcsMismatch { .. }) => {
                    if fcs_failed.is_none() {
                        fcs_failed = Some(i);
                    }
                    if i == self.primary {
                        self.stats.fcs_errors = self.stats.fcs_errors.saturating_add(1);
                    }
                }
                Err(Ax25Error::FrameTooLarge { .. }) => {
                    if i == self.primary {
                        self.stats.oversize = self.stats.oversize.saturating_add(1);
                    }
                }
                Err(_) => {
                    if i == self.primary {
                        self.stats.malformed = self.stats.malformed.saturating_add(1);
                    }
                }
            }
        }
        if out_len.is_none()
            && self.voting.is_on()
            && let Some(failed) = fcs_failed
        {
            out_len = self.try_vote(failed);
        }
        let len = out_len?;
        match UiFrame::parse(self.out_buf.get(..len).unwrap_or(&[])) {
            Ok(frame) => {
                self.stats.frames_ok = self.stats.frames_ok.saturating_add(1);
                Some(RxFrame { frame })
            }
            Err(_) => {
                self.stats.malformed = self.stats.malformed.saturating_add(1);
                None
            }
        }
    }

    /// Cross-chain candidate voting on an FCS failure: aligns every
    /// other chain's recent bit history to the failed chain's raw frame
    /// window by correlation (slide 0..[`VOTE_SLIDE`] bits, whole-window
    /// agreement ≥ 80% to qualify), majority-votes each bit over the
    /// aligned copies (each qualified copy weighs 2 against the failed
    /// chain's own 1), and destuffs + FCS-checks the voted window once
    /// (plus one single-bit-flip pass). Bounded, fixed-buffer work.
    /// Returns the emitted-frame length when the voted frame validates,
    /// is fresh, and was copied into `out_buf`.
    fn try_vote(&mut self, failed: usize) -> Option<usize> {
        let (window, total) = {
            let chain = self.chains.get(failed)?;
            let (w, t) = chain.deframer.failed_window()?;
            (*w, t)
        };
        // The failed chain's own history ends `lag` bits after the
        // window's last bit only if extra bits were pushed since; here
        // the deframer event fires on the same push, so the newest
        // history bit IS the window's last bit.
        let mut votes = [0i16; hdlc::RAW_BITS];
        // Seed with the failed chain's own copy (weight 1; qualified
        // other-chain copies weigh 2 so one healthy chain outvotes it).
        for (i, vote) in votes.iter_mut().enumerate().take(total) {
            let bit = (window[i / 8] >> (i % 8)) & 1 != 0;
            *vote += if bit { 1 } else { -1 };
        }
        let mut voters = 1usize;
        for c in 0..self.active {
            if c == failed {
                continue;
            }
            let Some(chain) = self.chains.get(c) else {
                continue;
            };
            // Try aligning this chain's history to the window: the
            // window's bit `i` (0-based from its start) maps to history
            // position `back = total - 1 - i + slide` for some small
            // slide (this chain may run ahead of the failed chain).
            let mut best_slide = None;
            let mut best_agree = 0usize;
            for slide in 0..=VOTE_SLIDE {
                let mut agree = 0usize;
                let mut have = 0usize;
                for i in 0..total {
                    let Some(h) = chain.hist.get_back(total - 1 - i + slide) else {
                        continue;
                    };
                    have += 1;
                    let w = (window[i / 8] >> (i % 8)) & 1 != 0;
                    if h == w {
                        agree += 1;
                    }
                }
                if have == total && agree > best_agree {
                    best_agree = agree;
                    best_slide = Some(slide);
                }
            }
            let Some(slide) = best_slide else {
                continue;
            };
            if best_agree * 10 < total * VOTE_AGREE_NUM {
                continue;
            }
            for (i, vote) in votes.iter_mut().enumerate().take(total) {
                if let Some(h) = chain.hist.get_back(total - 1 - i + slide) {
                    *vote += if h { 2 } else { -2 };
                }
            }
            voters += 1;
        }
        if voters < 2 {
            // No other chain qualified; nothing to vote with.
            return None;
        }
        let mut voted = [0u8; hdlc::RAW_BYTES];
        for (i, &v) in votes.iter().enumerate().take(total) {
            // Ties keep the failed chain's own bit (v == 0 cannot occur
            // with the odd total weight 1 + 2·voters, but stay exhaustive).
            let bit = if v > 0 {
                true
            } else if v < 0 {
                false
            } else {
                (window[i / 8] >> (i % 8)) & 1 != 0
            };
            if bit {
                voted[i / 8] |= 1 << (i % 8);
            }
        }
        let content_len = {
            let chain = self.chains.get_mut(failed)?;
            chain.deframer.try_voted_window(&voted, total)?
        };
        let (key, len) = {
            let chain = self.chains.get(failed)?;
            let frame = chain.deframer.frame_bytes(content_len);
            ((crate::ax25::crc16_x25(frame), frame.len() as u16), {
                let len = frame.len().min(N);
                for (dst, src) in self.out_buf.iter_mut().zip(frame.iter()) {
                    *dst = *src;
                }
                len
            })
        };
        let fresh = Self::register(
            &mut self.seen,
            &mut self.seen_next,
            self.samples,
            self.window,
            key,
        );
        if fresh { Some(len) } else { None }
    }

    /// Records an accepted frame key; returns `true` when the frame is
    /// fresh (not seen within the dedup window) and should be emitted.
    fn register(
        seen: &mut [SeenFrame; MAX_CHAINS],
        next: &mut usize,
        now: u64,
        window: u64,
        (crc, len): (u16, u16),
    ) -> bool {
        for entry in seen.iter_mut() {
            if entry.valid
                && entry.crc == crc
                && entry.len == len
                && now.wrapping_sub(entry.seen_at) <= window
            {
                // Duplicate from a parallel chain: refresh the timestamp
                // so a third chain a bit later is still suppressed.
                entry.seen_at = now;
                return false;
            }
        }
        let slot = *next % MAX_CHAINS;
        if let Some(entry) = seen.get_mut(slot) {
            *entry = SeenFrame {
                crc,
                len,
                seen_at: now,
                valid: true,
            };
        }
        *next = (*next + 1) % MAX_CHAINS;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax25::Address;
    use crate::types::DevicePreset;

    fn addr(callsign: &[u8], ssid: u8) -> Address {
        Address::new(callsign, ssid).unwrap()
    }

    /// Every device preset resolves without error to a config holding
    /// the invariants the docs promise: the recommended 48 kHz rate,
    /// the preset profile's parameters, the chain bank sized per
    /// [`DevicePreset::full_chain_bank`] — and the receiver
    /// constructor (which re-validates the whole config) accepts it.
    #[test]
    fn every_device_preset_resolves_to_a_valid_config() {
        for &preset in DevicePreset::ALL {
            let config = preset
                .tnc_config()
                .unwrap_or_else(|e| panic!("{preset:?} failed to resolve: {e}"));
            assert_eq!(config.sample_rate().hz(), 48_000, "{preset:?}");
            assert_eq!(
                config.baud().bps(),
                preset.profile().baud().bps(),
                "{preset:?}"
            );
            assert_eq!(config.scheme(), preset.profile().scheme(), "{preset:?}");
            let expected_sweep = if preset.full_chain_bank() {
                SpaceGainSweep::DEFAULT
            } else {
                SpaceGainSweep::UNITY
            };
            assert_eq!(config.space_gain_sweep(), expected_sweep, "{preset:?}");
            assert!(DefaultTncReceiver::new(config).is_ok(), "{preset:?}");
        }
    }

    /// The presets resolve in const context — the enum is
    /// const-friendly all the way to the config.
    #[test]
    fn device_preset_resolution_is_const() {
        const CONFIG: TncConfig = match DevicePreset::Esp32C3.tnc_config() {
            Ok(c) => c,
            Err(_) => panic!("Esp32C3 must resolve"),
        };
        assert_eq!(CONFIG.baud().bps(), 1_200);
    }

    /// Host decode per preset: synthesize a frame with the preset's
    /// own transmitter (tone AFSK or scrambled baseband as the preset
    /// selects), feed the i16 samples through a preset-built receiver,
    /// and require the frame back intact.
    #[test]
    fn every_device_preset_decodes_a_synthesized_frame() {
        for &preset in DevicePreset::ALL {
            let config = preset.tnc_config().unwrap();
            let tx = TncTransmitter::new(config);
            let mut rx = DefaultTncReceiver::new(config).unwrap();
            let text = b"preset round trip";
            let mut frame_buf = [0u8; MAX_FRAME_BYTES];
            let len = tx
                .build_frame_raw(
                    addr(b"APRS", 0),
                    addr(b"N0CALL", 7),
                    &[],
                    text,
                    &mut frame_buf,
                )
                .unwrap();
            let mut decoded = 0;
            for s in tx.frame_samples_i16(&frame_buf[..len]) {
                if let Some(frame) = rx.push_i16(s) {
                    assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL", "{preset:?}");
                    assert_eq!(frame.info(), text, "{preset:?}");
                    decoded += 1;
                }
            }
            assert_eq!(decoded, 1, "{preset:?} decoded {decoded} frames");
        }
    }

    /// The G3RUH preset (when built) really selects the baseband
    /// scheme at its fixed 9600 Bd rate.
    #[cfg(feature = "g3ruh")]
    #[test]
    fn p4_g3ruh_preset_selects_baseband() {
        let config = DevicePreset::Esp32P4G3ruh.tnc_config().unwrap();
        assert_eq!(config.scheme(), ModulationScheme::ScrambledBaseband);
        assert_eq!(config.baud().bps(), 9_600);
    }
}
