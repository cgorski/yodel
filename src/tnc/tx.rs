//! Transmit half of the TNC pipeline: APRS packet to PCM samples.
//!
//! [`TncTransmitter`] composes the existing layers rather than
//! duplicating them, and hands back lazy sample iterators so no audio is
//! buffered. Re-exported from [`crate::tnc`].

#[cfg(feature = "alloc")]
use super::MAX_FRAME_BYTES;
use super::config::{TncConfig, TncError};

use crate::aprs::AprsPacket;
use crate::ax25::{Address, Ax25Error, UiFrame, hdlc};
#[cfg(feature = "g3ruh")]
use crate::baseband::BasebandI16Samples;
use crate::modulator::{F32Samples, I16Samples, Modulator};
use crate::nrzi;
#[cfg(feature = "g3ruh")]
use crate::scrambler::Scrambler;

/// The lazy sample iterator type of the `i16` transmit path.
///
/// Yields the tone-AFSK waveform for tone profiles; with the `g3ruh`
/// feature and a scrambled-baseband profile it yields the scrambled
/// baseband waveform instead.
#[derive(Debug, Clone)]
pub struct TxI16Samples<'a> {
    inner: TxI16Inner<'a>,
}

/// Private front-end selector of [`TxI16Samples`].
#[derive(Debug, Clone)]
enum TxI16Inner<'a> {
    /// Tone AFSK: NRZI bits drive the continuous-phase tone modulator.
    Tone(I16Samples<nrzi::EncodeIter<hdlc::FrameBits<'a>>>),
    /// G3RUH: NRZI bits are scrambled, then baseband-synthesized.
    #[cfg(feature = "g3ruh")]
    Baseband(
        BasebandI16Samples<crate::scrambler::ScrambleIter<nrzi::EncodeIter<hdlc::FrameBits<'a>>>>,
    ),
}

impl Iterator for TxI16Samples<'_> {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        match self.inner {
            TxI16Inner::Tone(ref mut it) => it.next(),
            #[cfg(feature = "g3ruh")]
            TxI16Inner::Baseband(ref mut it) => it.next(),
        }
    }
}

/// The lazy sample iterator type of the `f32` transmit path.
///
/// The `f32` twin of [`TxI16Samples`].
#[derive(Debug, Clone)]
pub struct TxF32Samples<'a> {
    inner: TxF32Inner<'a>,
}

/// Private front-end selector of [`TxF32Samples`].
#[derive(Debug, Clone)]
enum TxF32Inner<'a> {
    /// Tone AFSK: NRZI bits drive the continuous-phase tone modulator.
    Tone(F32Samples<nrzi::EncodeIter<hdlc::FrameBits<'a>>>),
    /// G3RUH: NRZI bits are scrambled, then baseband-synthesized.
    #[cfg(feature = "g3ruh")]
    Baseband(
        crate::baseband::BasebandF32Samples<
            crate::scrambler::ScrambleIter<nrzi::EncodeIter<hdlc::FrameBits<'a>>>,
        >,
    ),
}

impl Iterator for TxF32Samples<'_> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        match self.inner {
            TxF32Inner::Tone(ref mut it) => it.next(),
            #[cfg(feature = "g3ruh")]
            TxF32Inner::Baseband(ref mut it) => it.next(),
        }
    }
}

/// APRS-over-AX.25 transmitter: packets in, PCM samples out.
///
/// Serializes an [`AprsPacket`] (or raw information bytes) into a UI
/// frame in a caller-provided buffer, then returns a lazy iterator over
/// the modulated samples: HDLC framing with the configured flag counts,
/// NRZI encoding, and continuous-phase AFSK — exactly the
/// [`crate::ax25::tx_i16`] composition with configurable flags.
///
/// # Common path: one packet, small fixed buffers, lazy samples
///
/// Everything is allocation-free: the packet serializes into the
/// caller's buffers and the returned iterator synthesizes each `i16`
/// on demand.
///
/// ```
/// use warble::SampleRate;
/// use warble::aprs::{AprsPacket, Status};
/// use warble::ax25::Address;
/// use warble::tnc::{TncConfig, TncTransmitter};
///
/// let tx = TncTransmitter::new(TncConfig::bell_202(SampleRate::new(48_000)?)?);
/// let packet = AprsPacket::Status(Status { text: b"QRV" });
/// let mut info_buf = [0u8; 32]; // holds the 4-byte info field
/// let mut frame_buf = [0u8; 64]; // holds the 20-byte UI frame body
/// let samples = tx.transmit_i16(
///     &packet,
///     Address::new(b"APRS", 0)?,   // destination "tocall"
///     Address::new(b"N0CALL", 0)?, // source station
///     &[],                         // no digipeater path
///     &mut info_buf,
///     &mut frame_buf,
/// )?;
/// // 48 000 Hz / 1200 Bd = 40 samples per bit, exactly: the count is
/// // (preamble + stuffed frame + FCS + tail flag bits) · 40.
/// let n = samples.count();
/// assert_eq!(n % 40, 0);
/// assert!(n > 0);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TncTransmitter {
    config: TncConfig,
}

impl TncTransmitter {
    /// Wraps a validated configuration.
    #[must_use]
    pub const fn new(config: TncConfig) -> Self {
        Self { config }
    }

    /// The wrapped configuration.
    #[must_use]
    pub const fn config(&self) -> TncConfig {
        self.config
    }

    /// Serializes `packet` into `info_buf` and the surrounding UI frame
    /// into `frame_buf`, returning the frame body length in `frame_buf`.
    ///
    /// The addresses follow the APRS convention: `dest` is the protocol
    /// tocall (e.g. `APRS`), `src` the sending station, `path` the
    /// digipeater list.
    ///
    /// # Errors
    ///
    /// [`TncError::Aprs`] when `info_buf` is too small for the packet;
    /// [`TncError::Ax25`] when the path is too long or `frame_buf` is too
    /// small for the frame.
    pub fn build_frame(
        &self,
        packet: &AprsPacket<'_>,
        dest: Address,
        src: Address,
        path: &[Address],
        info_buf: &mut [u8],
        frame_buf: &mut [u8],
    ) -> Result<usize, TncError> {
        Ok(crate::aprs::build_ui_frame(
            packet, dest, src, path, info_buf, frame_buf,
        )?)
    }

    /// Modulates a pre-built AX.25 frame body (without FCS) into a lazy
    /// `i16` sample iterator using the configured flag counts.
    ///
    /// The TX pipeline is: stuffed HDLC bits → NRZI → (scrambler, G3RUH
    /// profiles only) → waveform synthesis.
    #[must_use]
    pub fn frame_samples_i16<'a>(&self, frame: &'a [u8]) -> TxI16Samples<'a> {
        let bits = nrzi::encode_iter(hdlc::frame_bits(
            frame,
            self.config.preamble_flags,
            self.config.tail_flags,
        ));
        #[cfg(feature = "g3ruh")]
        if let Some(baseband) = self.config.baseband {
            return TxI16Samples {
                inner: TxI16Inner::Baseband(
                    baseband.i16_samples(Scrambler::default().scramble_iter(bits)),
                ),
            };
        }
        TxI16Samples {
            inner: TxI16Inner::Tone(Modulator::new(self.config.modulator).i16_samples(bits)),
        }
    }

    /// The `f32` twin of [`TncTransmitter::frame_samples_i16`].
    #[must_use]
    pub fn frame_samples_f32<'a>(&self, frame: &'a [u8]) -> TxF32Samples<'a> {
        let bits = nrzi::encode_iter(hdlc::frame_bits(
            frame,
            self.config.preamble_flags,
            self.config.tail_flags,
        ));
        #[cfg(feature = "g3ruh")]
        if let Some(baseband) = self.config.baseband {
            return TxF32Samples {
                inner: TxF32Inner::Baseband(
                    baseband.f32_samples(Scrambler::default().scramble_iter(bits)),
                ),
            };
        }
        TxF32Samples {
            inner: TxF32Inner::Tone(Modulator::new(self.config.modulator).f32_samples(bits)),
        }
    }

    /// One-call transmit: builds the UI frame for `packet` into
    /// `frame_buf` (via `info_buf`), then returns the lazy `i16` sample
    /// iterator over it.
    ///
    /// # Errors
    ///
    /// As [`TncTransmitter::build_frame`].
    pub fn transmit_i16<'a>(
        &self,
        packet: &AprsPacket<'_>,
        dest: Address,
        src: Address,
        path: &[Address],
        info_buf: &mut [u8],
        frame_buf: &'a mut [u8],
    ) -> Result<TxI16Samples<'a>, TncError> {
        let len = self.build_frame(packet, dest, src, path, info_buf, frame_buf)?;
        let frame = frame_buf
            .get(..len)
            .ok_or(TncError::Ax25(Ax25Error::FrameTooLarge {
                len,
                max: frame_buf.len(),
            }))?;
        Ok(self.frame_samples_i16(frame))
    }

    /// The `f32` twin of [`TncTransmitter::transmit_i16`].
    ///
    /// # Errors
    ///
    /// As [`TncTransmitter::build_frame`].
    pub fn transmit_f32<'a>(
        &self,
        packet: &AprsPacket<'_>,
        dest: Address,
        src: Address,
        path: &[Address],
        info_buf: &mut [u8],
        frame_buf: &'a mut [u8],
    ) -> Result<TxF32Samples<'a>, TncError> {
        let len = self.build_frame(packet, dest, src, path, info_buf, frame_buf)?;
        let frame = frame_buf
            .get(..len)
            .ok_or(TncError::Ax25(Ax25Error::FrameTooLarge {
                len,
                max: frame_buf.len(),
            }))?;
        Ok(self.frame_samples_f32(frame))
    }

    /// Lower-level entry: wraps raw information bytes (any payload, not
    /// necessarily APRS) into a UI frame in `frame_buf`, returning the
    /// frame body length.
    ///
    /// # Errors
    ///
    /// [`TncError::Ax25`] when the path is too long or `frame_buf` is too
    /// small.
    pub fn build_frame_raw(
        &self,
        dest: Address,
        src: Address,
        path: &[Address],
        info: &[u8],
        frame_buf: &mut [u8],
    ) -> Result<usize, TncError> {
        let frame = UiFrame::with_path(dest, src, path, info)?;
        Ok(frame.build(frame_buf)?)
    }

    /// Collects the `i16` transmission of `packet` into a fresh vector.
    ///
    /// # Errors
    ///
    /// As [`TncTransmitter::build_frame`].
    #[cfg(feature = "alloc")]
    pub fn transmit_to_vec_i16(
        &self,
        packet: &AprsPacket<'_>,
        dest: Address,
        src: Address,
        path: &[Address],
    ) -> Result<alloc::vec::Vec<i16>, TncError> {
        let mut info_buf = [0u8; MAX_FRAME_BYTES];
        let mut frame_buf = [0u8; MAX_FRAME_BYTES];
        let samples = self.transmit_i16(packet, dest, src, path, &mut info_buf, &mut frame_buf)?;
        Ok(samples.collect())
    }

    /// The `f32` twin of [`TncTransmitter::transmit_to_vec_i16`].
    ///
    /// # Errors
    ///
    /// As [`TncTransmitter::build_frame`].
    #[cfg(feature = "alloc")]
    pub fn transmit_to_vec_f32(
        &self,
        packet: &AprsPacket<'_>,
        dest: Address,
        src: Address,
        path: &[Address],
    ) -> Result<alloc::vec::Vec<f32>, TncError> {
        let mut info_buf = [0u8; MAX_FRAME_BYTES];
        let mut frame_buf = [0u8; MAX_FRAME_BYTES];
        let samples = self.transmit_f32(packet, dest, src, path, &mut info_buf, &mut frame_buf)?;
        Ok(samples.collect())
    }
}
