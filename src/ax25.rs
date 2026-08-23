//! AX.25 unnumbered-information (UI) frame layer.
//!
//! This module implements the data-link layer that rides on top of the
//! NRZI + AFSK physical layers: AX.25 addresses ([`addr`]), the
//! CRC-16/X.25 frame check sequence ([`fcs`]), HDLC bit-level framing with
//! zero-bit stuffing and flag delimiting ([`hdlc`]), and UI frame
//! building/parsing ([`frame`]).
//!
//! Everything here is `no_std`, allocation-free, and streaming: fixed
//! const-generic buffers on the receive side, iterators on the transmit
//! side. Each layer is independently usable; [`tx_i16`] / [`tx_f32`] and
//! [`FrameReceiver`] wire the full stack together when the `mod` / `demod`
//! features are also enabled.
//!
//! # Specification
//!
//! > Beech, W. A. (NJ7P), Nielsen, D. E. (N7LEM) and Taylor, J. (N7OO),
//! > "AX.25 Link Access Protocol for Amateur Packet Radio", Version 2.2,
//! > July 1998, Tucson Amateur Packet Radio / American Radio Relay
//! > League. <https://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>
//!
//! Section references used by the submodules here:
//!
//! | Feature | AX.25 2.2 |
//! |---|---|
//! | Address field encoding, SSID octet | §3.12, §3.12.2 |
//! | H bit (has-been-repeated) | §3.12.4 |
//! | C bit (command/response) | §6.1.2 (introduced in v2.0 §2.4.1.2) |
//! | UI control byte `0x03` | §4.3.3, Fig 4.4 |
//! | PID `0xF0`, "no layer 3" | §3.4, Fig 3.2 |
//! | Flag `0x7E`, bit stuffing, bit order | §3.1, §3.6, §3.8 |
//! | Frame check sequence | §3.7 |
//!
//! Where this crate implements v2.0 behaviour rather than v2.2 the
//! difference is noted on the item ([`addr::H_BIT`] is the one that
//! matters, because the two versions overload the same bit position).
//!
//! # Wire format
//!
//! A UI frame on the air is, in order: preamble flags (`0x7E`), then the
//! bit-stuffed frame octets — destination address, source address, up to
//! [`frame::MAX_DIGIPEATERS`] digipeater addresses, control `0x03`, PID
//! `0xF0`, information field, and the FCS appended little-endian — then
//! tail flags. Octets are serialized LSB-first, NRZI-encoded, and keyed as
//! Bell 202 AFSK tones.

use core::fmt;

pub mod addr;
pub mod fcs;
pub mod frame;
pub mod hdlc;

pub use addr::{Address, Callsign, PathHop, Ssid};
pub use fcs::{Fcs, crc16_x25};
pub use frame::UiFrame;
pub use hdlc::{HdlcDeframer, RecoveryPolicy};

#[cfg(feature = "demod")]
use crate::demodulator::Demodulator;
#[cfg(feature = "demod")]
use crate::discriminator::{Discriminator, QuadratureCorrelator};
#[cfg(feature = "mod")]
use crate::modulator::{F32Samples, I16Samples, Modulator};
#[cfg(feature = "mod")]
use crate::nrzi;
#[cfg(feature = "demod")]
use crate::nrzi::NrziDecoder;
#[cfg(feature = "demod")]
use crate::types::Bit;

/// An AX.25 protocol violation: an invalid field value on build, or a
/// malformed/corrupted frame on parse.
///
/// Every variant carries the offending value together with the rule it
/// violated, so the rendered message is self-explanatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ax25Error {
    /// A callsign contained a character outside `A-Z` / `0-9`.
    InvalidCallsignChar {
        /// The rejected byte.
        got: u8,
    },
    /// A callsign was empty or longer than six characters.
    CallsignLengthInvalid {
        /// The rejected length in bytes.
        got: usize,
    },
    /// An SSID was outside `0..=15`.
    SsidOutOfRange {
        /// The rejected SSID value.
        got: u8,
    },
    /// A frame did not fit in the available buffer.
    FrameTooLarge {
        /// The required or received length in bytes.
        len: usize,
        /// The maximum the buffer can hold, in bytes.
        max: usize,
    },
    /// A received frame was shorter than the AX.25 minimum.
    FrameTooShort {
        /// The received length in bytes.
        len: usize,
        /// The minimum valid length in bytes.
        min: usize,
    },
    /// The received frame check sequence did not match the frame contents.
    FcsMismatch {
        /// The FCS carried by the frame.
        expected: u16,
        /// The FCS computed over the received contents.
        computed: u16,
    },
    /// The control field was not `0x03` (UI frame).
    InvalidControl {
        /// The rejected control byte.
        got: u8,
    },
    /// The PID field was not `0xF0` (no layer 3).
    InvalidPid {
        /// The rejected PID byte.
        got: u8,
    },
    /// More digipeater addresses than the fixed path capacity.
    TooManyDigipeaters {
        /// The offered or received number of digipeaters.
        got: usize,
        /// The maximum supported ([`frame::MAX_DIGIPEATERS`]).
        max: usize,
    },
}

impl fmt::Display for Ax25Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Ax25Error::InvalidCallsignChar { got } => write!(
                f,
                "callsign byte 0x{got:02X} is invalid: must be an uppercase letter A-Z or digit 0-9"
            ),
            Ax25Error::CallsignLengthInvalid { got } => write!(
                f,
                "callsign length {got} is invalid: must be 1..=6 characters"
            ),
            Ax25Error::SsidOutOfRange { got } => {
                write!(f, "SSID {got} is out of range: must be within 0..=15")
            }
            Ax25Error::FrameTooLarge { len, max } => write!(
                f,
                "frame of {len} bytes is too large: the buffer holds at most {max} bytes"
            ),
            Ax25Error::FrameTooShort { len, min } => write!(
                f,
                "frame of {len} bytes is too short: an AX.25 UI frame needs at least {min} bytes"
            ),
            Ax25Error::FcsMismatch { expected, computed } => write!(
                f,
                "frame check sequence mismatch: frame carries 0x{expected:04X} but contents compute to 0x{computed:04X}"
            ),
            Ax25Error::InvalidControl { got } => write!(
                f,
                "control field 0x{got:02X} is invalid: a UI frame requires 0x03"
            ),
            Ax25Error::InvalidPid { got } => write!(
                f,
                "PID field 0x{got:02X} is invalid: no-layer-3 requires 0xF0"
            ),
            Ax25Error::TooManyDigipeaters { got, max } => write!(
                f,
                "{got} digipeater addresses exceed the supported maximum of {max}"
            ),
        }
    }
}

impl core::error::Error for Ax25Error {}

/// Modulates a complete AX.25 frame (without FCS) into `i16` PCM samples.
///
/// This is the full documented transmit composition, allocation-free and
/// lazy end to end:
///
/// 1. [`hdlc::frame_bits`] appends the CRC-16/X.25 FCS, serializes the
///    octets LSB-first, inserts a zero after five consecutive ones, and
///    surrounds the frame with the default flag counts
///    ([`hdlc::DEFAULT_PREAMBLE_FLAGS`] / [`hdlc::DEFAULT_TAIL_FLAGS`]);
/// 2. [`nrzi::encode_iter`] converts data bits to line-level bits
///    (zero toggles, one holds);
/// 3. [`Modulator::i16_samples`] keys the line bits as continuous-phase
///    Bell 202 tones.
///
/// Each stage is public; compose them manually for custom flag counts.
#[cfg(feature = "mod")]
pub fn tx_i16(
    frame: &[u8],
    modulator: Modulator,
) -> I16Samples<nrzi::EncodeIter<hdlc::FrameBits<'_>>> {
    modulator.i16_samples(nrzi::encode_iter(hdlc::frame_bits(
        frame,
        hdlc::DEFAULT_PREAMBLE_FLAGS,
        hdlc::DEFAULT_TAIL_FLAGS,
    )))
}

/// Modulates a complete AX.25 frame (without FCS) into `f32` PCM samples.
///
/// The `f32` twin of [`tx_i16`]; see there for the layer-by-layer
/// composition.
#[cfg(feature = "mod")]
pub fn tx_f32(
    frame: &[u8],
    modulator: Modulator,
) -> F32Samples<nrzi::EncodeIter<hdlc::FrameBits<'_>>> {
    modulator.f32_samples(nrzi::encode_iter(hdlc::frame_bits(
        frame,
        hdlc::DEFAULT_PREAMBLE_FLAGS,
        hdlc::DEFAULT_TAIL_FLAGS,
    )))
}

/// The full receive composition: PCM samples in, validated AX.25 frames out.
///
/// Wires a [`Demodulator`] (samples to raw bits), an [`NrziDecoder`]
/// (line bits to data bits) and an [`HdlcDeframer`] (flag hunting,
/// destuffing, FCS validation) into one allocation-free push machine.
/// `N` is the receive buffer capacity in bytes, which the deframer
/// fills with the frame contents *and* the two FCS bytes: the largest
/// frame contents that can be received is `N - 2`.
#[cfg(feature = "demod")]
#[derive(Debug, Clone)]
pub struct FrameReceiver<const N: usize, D = QuadratureCorrelator> {
    demodulator: Demodulator<D>,
    nrzi: NrziDecoder,
    deframer: HdlcDeframer<N>,
}

#[cfg(feature = "demod")]
impl<const N: usize, D: Discriminator> FrameReceiver<N, D> {
    /// Wraps a configured demodulator into a frame receiver.
    #[must_use]
    pub fn new(demodulator: Demodulator<D>) -> Self {
        Self {
            demodulator,
            nrzi: NrziDecoder::default(),
            deframer: HdlcDeframer::new(),
        }
    }

    /// Pushes one `i16` PCM sample; returns a frame when one completes.
    ///
    /// `Some(Ok(frame))` yields the validated frame contents (FCS already
    /// checked and stripped), borrowed from the internal buffer until the
    /// next push. `Some(Err(_))` reports a typed receive error (bad FCS,
    /// oversize frame); garbage between flags is discarded silently.
    pub fn push_sample_i16(&mut self, sample: i16) -> Option<Result<&[u8], Ax25Error>> {
        match self.demodulator.push_sample_i16(sample) {
            Some(line) => self.push_line_bit(line),
            None => None,
        }
    }

    /// Pushes one `f32` PCM sample; returns a frame when one completes.
    ///
    /// The `f32` twin of [`FrameReceiver::push_sample_i16`].
    pub fn push_sample_f32(&mut self, sample: f32) -> Option<Result<&[u8], Ax25Error>> {
        match self.demodulator.push_sample_f32(sample) {
            Some(line) => self.push_line_bit(line),
            None => None,
        }
    }

    /// NRZI-decodes one recovered line bit and feeds the deframer.
    fn push_line_bit(&mut self, line: Bit) -> Option<Result<&[u8], Ax25Error>> {
        let data = self.nrzi.decode(line);
        self.deframer.push(data)
    }
}
