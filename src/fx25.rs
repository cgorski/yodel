//! FX.25 framing: correlation tags plus a Reed-Solomon codeblock around
//! an ordinary HDLC/AX.25 frame.
//!
//! # The wire format
//!
//! FX.25 is an *additive* forward-error-correction wrapper. The
//! transmitter takes a complete, already bit-stuffed HDLC frame — opening
//! flag, stuffed contents and FCS, closing flag — exactly as it would go
//! on the air, and sends instead:
//!
//! ```text
//! correlation tag (8 bytes, LSB-first) ‖ data block ‖ RS parity
//! ```
//!
//! The **correlation tag** is one of eleven published 64-bit constants
//! ([`CorrelationTag`]), each naming a Reed-Solomon code from the
//! `RS(255, k)` family of [`crate::rs`] and an on-air block size. The
//! **data block** is the stuffed HDLC frame padded with HDLC flag octets
//! (`0x7E`) up to the tag's data length; the **parity** is computed over
//! the data block extended with a zero suffix to the code's full data
//! length (the zero suffix is never transmitted). Because the embedded
//! frame keeps its own flags, stuffing and FCS intact, a legacy non-FX.25
//! receiver decodes the embedded frame and ignores the rest.
//!
//! On receive, a bit-level tag hunter ([`Fx25Receiver`], `ax25` feature)
//! correlates the post-NRZI-decode bit stream against all eleven tags.
//! The tags are pairwise Hamming distance 32 apart, so a hard-decision
//! threshold correlator works: a match within [`TAG_TOLERANCE`] bit
//! errors locks the receiver, which then collects the codeblock,
//! RS-decodes it (correcting up to `t = parity / 2` byte errors anywhere
//! in the block), and feeds the corrected embedded frame to the ordinary
//! HDLC deframer. Bits that arrive while no tag is locked flow through a
//! parallel plain HDLC path, so non-FX.25 traffic still decodes.
//!
//! # Integration seam (design note)
//!
//! This module is a standalone encoder/decoder pair at the byte/bit
//! boundary of the existing modem — [`wrap`] slots between
//! `TncTransmitter::build_frame` and the NRZI/AFSK bit stages on
//! transmit, and [`Fx25Receiver`] is the parallel tag-hunting path beside
//! `HdlcDeframer` on receive (see `docs/ARCHITECTURE.md`, "The
//! frame-wrapper seam: FX.25 and IL2P"). The
//! default TNC paths are untouched and remain byte-identical: FX.25 is
//! not a `TncConfig` setting and no `TncConfig` field reaches this
//! module. Callers opt in by composing the stages themselves, which is
//! what the `warble` CLI does behind its `--fx25` flag. That
//! composition gives up the receive-side tuning `TncReceiver` carries —
//! see [`Fx25Receiver`] for the measured cost.
//!
//! # Beginner: wrap a stuffed HDLC frame
//!
//! The smallest tag whose data block fits the frame is picked
//! automatically:
//!
//! ```
//! use warble::fx25::{CorrelationTag, TAG_BYTES, WRAP_MAX, wrap};
//!
//! // A (toy) stuffed HDLC frame: flag, contents, flag.
//! let stuffed = [0x7E, 0x82, 0xA0, 0xB4, 0x60, 0x61, 0x76, 0x7E];
//! // WRAP_MAX always fits, whichever tag `wrap` selects. Size it
//! // smaller and `wrap` returns `Fx25Error::BufferTooSmall { needed }`
//! // rather than truncating.
//! let mut out = [0u8; WRAP_MAX];
//! let wrapped = wrap(&stuffed, &mut out)?;
//!
//! assert_eq!(wrapped.tag(), CorrelationTag::Rs48_32);
//! assert_eq!(wrapped.len(), TAG_BYTES + 32 + 16); // tag + data + parity
//! # Ok::<(), warble::fx25::Fx25Error>(())
//! ```
//!
//! # Practitioner: layout and flag padding
//!
//! The data block starts with the stuffed frame and is padded with HDLC
//! flag octets; the RS parity protects data and padding alike:
//!
//! ```
//! use warble::fx25::{TAG_BYTES, WRAP_MAX, wrap};
//! use warble::rs::{BLOCK_MAX, RsCodec, RsParity};
//!
//! let stuffed = [0x7E, 0x03, 0xF0, 0x55, 0xAA, 0x7E];
//! let mut out = [0u8; WRAP_MAX];
//! let wrapped = wrap(&stuffed, &mut out)?;
//!
//! // Tag bytes are the 64-bit constant, LSB first.
//! let value = wrapped.tag().tag_value();
//! for (k, &byte) in out.iter().enumerate().take(TAG_BYTES) {
//!     assert_eq!(byte, (value >> (8 * k)) as u8);
//! }
//! // Data region: frame then flag padding.
//! assert_eq!(&out[TAG_BYTES..TAG_BYTES + 6], &stuffed);
//! assert!(out[TAG_BYTES + 6..TAG_BYTES + 32].iter().all(|&b| b == 0x7E));
//!
//! // The codeblock (data + zero suffix + parity) is a clean RS codeword.
//! let mut block = [0u8; BLOCK_MAX];
//! block[..32].copy_from_slice(&out[TAG_BYTES..TAG_BYTES + 32]);
//! block[239..].copy_from_slice(&out[TAG_BYTES + 32..TAG_BYTES + 48]);
//! assert_eq!(RsCodec::new(RsParity::Sixteen).decode(&mut block)?, 0);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```
//!
//! # Where this comes from
//!
//! The wire format, the tag family and the FEC assignments are defined
//! by:
//!
//! > Jim McGuire (KB3MPL), "FX.25 FEC Extension to AX.25 Link Protocol
//! > for Amateur Packet Radio", Stensat Group LLC, document version
//! > 0.01.06 DRAFT, 2006.
//!
//! Note it is a **draft**, and the only edition: the numbering sometimes
//! seen as "v1.0" refers to the same document. Its original home
//! (`stensat.org/docs/FX-25_01_06.pdf`) was withdrawn; web-archive
//! captures and third-party mirrors survive, all byte-identical to the
//! 2006 original (SHA-256 `8e7d1e6f...a2e2b85f`).
//!
//! The specification is **copyright © 2006 Stensat Group LLC and grants
//! no redistribution licence**, so unlike the FT8 tables in
//! [`crate::ft8`] it is not vendored into this repository. It does not
//! need to be: §"Correlation Tag Details" publishes the *construction*
//! of the tag family rather than only its values, and
//! `tests/fx25.rs::tags_regenerate_from_the_published_gold_code`
//! rebuilds all eleven constants from those published polynomials. The
//! provenance is therefore reproducible from the paper description
//! alone.
//!
//! Three details this crate implements are **not** in the specification,
//! and are called out where they are defined rather than left to look
//! normative: the Reed-Solomon field parameters (see [`crate::rs`]), the
//! zero-suffix shortening convention (see [`wrap_with`]), and the tag
//! matching tolerance ([`TAG_TOLERANCE`]).
//!
//! # Expert: the tag set's error margin
//!
//! Every pair of the eleven defined tags differs in exactly 32 of 64
//! bits, so a hunter accepting matches within [`TAG_TOLERANCE`] = 8 bit
//! errors can never confuse two tags (8 < 32 / 2) and false-locks on
//! random noise with probability ≈ `11 · Σ C(64, 0..=8) / 2⁶⁴ < 10⁻⁹`
//! per bit:
//!
//! ```
//! use warble::fx25::{CorrelationTag, TAG_TOLERANCE};
//!
//! for a in CorrelationTag::ALL {
//!     for b in CorrelationTag::ALL {
//!         if a != b {
//!             let dist = (a.tag_value() ^ b.tag_value()).count_ones();
//!             assert_eq!(dist, 32);
//!             assert!(TAG_TOLERANCE < dist / 2);
//!         }
//!     }
//! }
//! ```

use core::fmt;

#[cfg(feature = "ax25")]
use crate::ax25::{Ax25Error, HdlcDeframer, crc16_x25, hdlc};
use crate::rs::{RsCodec, RsError, RsParity};
use crate::types::Bit;

/// Length of the correlation tag on the air, in bytes.
pub const TAG_BYTES: usize = 8;

/// Largest FX.25 transmission unit: tag plus the biggest on-air block
/// (255 bytes of data + parity).
pub const WRAP_MAX: usize = TAG_BYTES + 255;

/// Maximum Hamming distance at which the tag hunter accepts a match.
///
/// **This crate's choice, not a specified value.** The FX.25
/// specification defines the tag family and its correlation properties
/// but says nothing about how close a match must be; implementations
/// pick their own threshold, and they differ (the authors' own reference
/// decoder used an autocorrelation threshold equivalent to `d <= 14`).
///
/// 8 is chosen because the eleven defined tags are pairwise distance 32
/// apart, so any threshold below 16 is unambiguous, and 8 leaves a wide
/// margin while keeping the random-noise false-lock rate negligible (see
/// the module docs). Raising it trades false locks for tolerance of tag
/// damage.
///
/// A caveat if the enum is ever extended: distance 32 is a property of
/// [`CorrelationTag::ALL`], not of the whole 65-member Gold-code family.
/// The two reserved zero-seed codes sit at distance 24 from some
/// members, which is still safe at this tolerance but leaves less room.
pub const TAG_TOLERANCE: u32 = 8;

/// The HDLC flag octet used to pad the data block.
const FLAG_FILL: u8 = 0x7E;

/// One of the eleven defined FX.25 correlation tags.
///
/// Each tag is a 64-bit correlation value (transmitted LSB-first) plus
/// the Reed-Solomon code it selects. The variant name `RsN_K` gives the
/// on-air block size `N` (data + parity bytes) and the on-air data
/// capacity `K`; shortened codes derive from the full
/// `RS(255, 255 - parity)` code by a zero suffix on the data.
///
/// Provenance: Table 1 of the FX.25 specification (see the module docs)
/// assigns `Tag_01`..`Tag_0B` to these eleven FEC modes. The values
/// themselves are members of a Gold-code family the specification
/// *constructs* rather than just tabulates — two 6-stage LFSRs over the
/// published polynomials `I(x) = x⁶ + x⁵` and
/// `Q(x) = x⁶ + x⁵ + x³ + x²`, the first seeded `0x3F` and the second with
/// the tag index, XORed and prefixed with a leading zero to fill 64 bits.
/// `tests/fx25.rs::tags_regenerate_from_the_published_gold_code` rebuilds
/// every constant below from exactly that description.
///
/// ```
/// use warble::fx25::CorrelationTag;
/// use warble::rs::RsParity;
///
/// let tag = CorrelationTag::Rs64_32;
/// assert_eq!(tag.tag_value(), 0xDBF8_69BD_2DBB_1776);
/// assert_eq!(tag.data_len(), 32);
/// assert_eq!(tag.parity(), RsParity::ThirtyTwo);
/// assert_eq!(tag.block_len(), 64);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorrelationTag {
    /// `RS(255, 239)`: 239 data bytes, 16 parity bytes.
    Rs255_239,
    /// `RS(144, 128)`: shortened `RS(255, 239)`, 128 data bytes.
    Rs144_128,
    /// `RS(80, 64)`: shortened `RS(255, 239)`, 64 data bytes.
    Rs80_64,
    /// `RS(48, 32)`: shortened `RS(255, 239)`, 32 data bytes.
    Rs48_32,
    /// `RS(255, 223)`: 223 data bytes, 32 parity bytes.
    Rs255_223,
    /// `RS(160, 128)`: shortened `RS(255, 223)`, 128 data bytes.
    Rs160_128,
    /// `RS(96, 64)`: shortened `RS(255, 223)`, 64 data bytes.
    Rs96_64,
    /// `RS(64, 32)`: shortened `RS(255, 223)`, 32 data bytes.
    Rs64_32,
    /// `RS(255, 191)`: 191 data bytes, 64 parity bytes.
    Rs255_191,
    /// `RS(192, 128)`: shortened `RS(255, 191)`, 128 data bytes.
    Rs192_128,
    /// `RS(128, 64)`: shortened `RS(255, 191)`, 64 data bytes.
    Rs128_64,
}

impl CorrelationTag {
    /// Every published tag, in specification order.
    pub const ALL: [Self; 11] = [
        Self::Rs255_239,
        Self::Rs144_128,
        Self::Rs80_64,
        Self::Rs48_32,
        Self::Rs255_223,
        Self::Rs160_128,
        Self::Rs96_64,
        Self::Rs64_32,
        Self::Rs255_191,
        Self::Rs192_128,
        Self::Rs128_64,
    ];

    /// Tag preference for [`wrap`]: ascending on-air block size (least
    /// airtime first); among the three full-length codes, the strongest
    /// parity that still fits wins.
    const PREFERRED: [Self; 11] = [
        Self::Rs48_32,
        Self::Rs64_32,
        Self::Rs80_64,
        Self::Rs96_64,
        Self::Rs128_64,
        Self::Rs144_128,
        Self::Rs160_128,
        Self::Rs192_128,
        Self::Rs255_191,
        Self::Rs255_223,
        Self::Rs255_239,
    ];

    /// The published 64-bit correlation value (transmitted LSB-first).
    #[must_use]
    pub const fn tag_value(self) -> u64 {
        match self {
            Self::Rs255_239 => 0xB74D_B7DF_8A53_2F3E,
            Self::Rs144_128 => 0x26FF_60A6_00CC_8FDE,
            Self::Rs80_64 => 0xC7DC_0508_F3D9_B09E,
            Self::Rs48_32 => 0x8F05_6EB4_3696_60EE,
            Self::Rs255_223 => 0x6E26_0B1A_C583_5FAE,
            Self::Rs160_128 => 0xFF94_DC63_4F1C_FF4E,
            Self::Rs96_64 => 0x1EB7_B9CD_BC09_C00E,
            Self::Rs64_32 => 0xDBF8_69BD_2DBB_1776,
            Self::Rs255_191 => 0x3ADB_0C13_DEAE_2836,
            Self::Rs192_128 => 0xAB69_DB6A_5431_88D6,
            Self::Rs128_64 => 0x4A4A_BEC4_A724_B796,
        }
    }

    /// On-air data capacity in bytes (the stuffed frame plus flag fill).
    #[must_use]
    pub const fn data_len(self) -> usize {
        match self {
            Self::Rs48_32 | Self::Rs64_32 => 32,
            Self::Rs80_64 | Self::Rs96_64 | Self::Rs128_64 => 64,
            Self::Rs144_128 | Self::Rs160_128 | Self::Rs192_128 => 128,
            Self::Rs255_191 => 191,
            Self::Rs255_223 => 223,
            Self::Rs255_239 => 239,
        }
    }

    /// The Reed-Solomon parity this tag selects.
    #[must_use]
    pub const fn parity(self) -> RsParity {
        match self {
            Self::Rs255_239 | Self::Rs144_128 | Self::Rs80_64 | Self::Rs48_32 => RsParity::Sixteen,
            Self::Rs255_223 | Self::Rs160_128 | Self::Rs96_64 | Self::Rs64_32 => {
                RsParity::ThirtyTwo
            }
            Self::Rs255_191 | Self::Rs192_128 | Self::Rs128_64 => RsParity::SixtyFour,
        }
    }

    /// Data length of the underlying full `RS(255, k)` code
    /// (`k = 255 - parity`); the gap above [`Self::data_len`] is the
    /// implicit zero suffix, never transmitted.
    #[must_use]
    pub const fn rs_data_len(self) -> usize {
        255 - self.parity().len()
    }

    /// On-air block length in bytes: data plus parity (tag excluded).
    #[must_use]
    pub const fn block_len(self) -> usize {
        self.data_len() + self.parity().len()
    }

    /// The smallest-airtime tag whose data block holds `len` bytes;
    /// `None` when `len` exceeds the largest capacity (239 bytes).
    #[must_use]
    pub fn smallest_for(len: usize) -> Option<Self> {
        Self::PREFERRED.into_iter().find(|t| t.data_len() >= len)
    }
}

/// Errors reported by the FX.25 framing layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fx25Error {
    /// The stuffed frame exceeds the largest tag's data capacity.
    FrameTooLong {
        /// Length of the offending frame in bytes.
        got: usize,
        /// Largest supported data length (239 bytes).
        max: usize,
    },
    /// The output buffer cannot hold the wrapped transmission.
    BufferTooSmall {
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        got: usize,
    },
    /// The Reed-Solomon layer rejected the codeblock.
    Rs(RsError),
    /// The embedded HDLC frame was rejected by the AX.25 layer.
    #[cfg(feature = "ax25")]
    Ax25(Ax25Error),
}

impl fmt::Display for Fx25Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Fx25Error::FrameTooLong { got, max } => {
                write!(
                    f,
                    "stuffed frame of {got} bytes exceeds FX.25 capacity {max}"
                )
            }
            Fx25Error::BufferTooSmall { needed, got } => {
                write!(f, "output buffer of {got} bytes, need {needed}")
            }
            Fx25Error::Rs(ref e) => write!(f, "Reed-Solomon layer: {e}"),
            #[cfg(feature = "ax25")]
            Fx25Error::Ax25(ref e) => write!(f, "AX.25 layer: {e}"),
        }
    }
}

impl core::error::Error for Fx25Error {}

impl From<RsError> for Fx25Error {
    fn from(e: RsError) -> Self {
        Fx25Error::Rs(e)
    }
}

/// Description of one wrapped FX.25 transmission produced by [`wrap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fx25Frame {
    tag: CorrelationTag,
    len: usize,
}

impl Fx25Frame {
    /// The correlation tag selected for this transmission.
    #[must_use]
    pub const fn tag(self) -> CorrelationTag {
        self.tag
    }

    /// Total transmission length in bytes: tag + data block + parity.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Always `false`: a wrapped transmission is never empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }
}

/// Wraps a complete stuffed HDLC frame (on-air bytes, flags included)
/// into an FX.25 transmission in `out`, returning the selected tag and
/// total length.
///
/// Selects the smallest fitting tag ([`CorrelationTag::smallest_for`]),
/// pads the data block with HDLC flag octets, and computes the
/// Reed-Solomon parity over the data extended by the implicit zero
/// suffix. The output — tag bytes (LSB-first), data block, parity — is
/// ready for the ordinary NRZI/AFSK bit stages (see [`byte_bits`]).
///
/// # Errors
///
/// [`Fx25Error::FrameTooLong`] when `stuffed` exceeds 239 bytes;
/// [`Fx25Error::BufferTooSmall`] when `out` cannot hold the result.
///
/// ```
/// use warble::fx25::{TAG_BYTES, WRAP_MAX, wrap};
///
/// let stuffed = [0x7E, 0x01, 0x02, 0x03, 0x7E];
/// let mut out = [0u8; WRAP_MAX];
/// let wrapped = wrap(&stuffed, &mut out)?;
/// assert_eq!(wrapped.len(), TAG_BYTES + wrapped.tag().block_len());
/// # Ok::<(), warble::fx25::Fx25Error>(())
/// ```
pub fn wrap(stuffed: &[u8], out: &mut [u8]) -> Result<Fx25Frame, Fx25Error> {
    let tag = CorrelationTag::smallest_for(stuffed.len()).ok_or(Fx25Error::FrameTooLong {
        got: stuffed.len(),
        max: 239,
    })?;
    wrap_with(tag, stuffed, out)
}

/// [`wrap`] with an explicit correlation tag instead of the automatic
/// smallest-fit selection — e.g. to buy stronger parity at the same
/// data capacity ([`CorrelationTag::Rs64_32`] corrects 16 symbol errors
/// where [`CorrelationTag::Rs48_32`] corrects 8).
///
/// # Shortening convention (not specified)
///
/// The shortened codes are computed by extending the data block with an
/// implicit **zero suffix** out to the full `k = 255 - parity`, then
/// transmitting only the real data and the parity. The FX.25
/// specification names the shortened codes but does not say where the
/// implicit zeros go, and a zero *prefix* is the more common convention
/// elsewhere. The suffix is what interoperates, so it is pinned by the
/// differential suite rather than by the document — the same status as
/// the Reed-Solomon field parameters in [`crate::rs`].
///
/// # Errors
///
/// [`Fx25Error::FrameTooLong`] when `stuffed` exceeds the tag's data
/// capacity; [`Fx25Error::BufferTooSmall`] when `out` cannot hold the
/// result.
///
/// ```
/// use warble::fx25::{CorrelationTag, WRAP_MAX, wrap_with};
///
/// let stuffed = [0x7E, 0x03, 0xF0, 0x7E];
/// let mut out = [0u8; WRAP_MAX];
/// let wrapped = wrap_with(CorrelationTag::Rs64_32, &stuffed, &mut out)?;
/// assert_eq!(wrapped.tag(), CorrelationTag::Rs64_32);
/// # Ok::<(), warble::fx25::Fx25Error>(())
/// ```
pub fn wrap_with(
    tag: CorrelationTag,
    stuffed: &[u8],
    out: &mut [u8],
) -> Result<Fx25Frame, Fx25Error> {
    let data_len = tag.data_len();
    if stuffed.len() > data_len {
        return Err(Fx25Error::FrameTooLong {
            got: stuffed.len(),
            max: data_len,
        });
    }
    let parity_len = tag.parity().len();
    let total = TAG_BYTES + data_len + parity_len;
    if out.len() < total {
        return Err(Fx25Error::BufferTooSmall {
            needed: total,
            got: out.len(),
        });
    }
    let value = tag.tag_value();
    for (k, slot) in out.iter_mut().enumerate().take(TAG_BYTES) {
        *slot = (value >> (8 * k)) as u8;
    }
    // Data block: the stuffed frame, then flag fill to the tag's size.
    let mut padded = [0u8; 255];
    for (dst, src) in padded.iter_mut().zip(stuffed.iter()) {
        *dst = *src;
    }
    for slot in padded.iter_mut().take(data_len).skip(stuffed.len()) {
        *slot = FLAG_FILL;
    }
    // The zero suffix padded[data_len..rs_data_len] shortens the code.
    let rs_data_len = tag.rs_data_len();
    let codec = RsCodec::new(tag.parity());
    let (data_out, rest) = out
        .get_mut(TAG_BYTES..total)
        .ok_or(Fx25Error::BufferTooSmall {
            needed: total,
            got: 0,
        })?
        .split_at_mut(data_len);
    codec.encode(padded.get(..rs_data_len).unwrap_or(&[]), rest)?;
    data_out.copy_from_slice(padded.get(..data_len).unwrap_or(&[]));
    Ok(Fx25Frame { tag, len: total })
}

/// Serializes an AX.25 frame body (without FCS) into stuffed on-air HDLC
/// bytes: one opening flag, the stuffed contents and FCS, one closing
/// flag, with any final partial byte completed by leading flag bits.
///
/// This is the byte-level companion of [`crate::ax25::hdlc::frame_bits`]
/// for feeding [`wrap`]: FX.25 transports the frame as bytes, so the bit
/// stream is packed LSB-first here instead of going straight to the
/// modulator.
///
/// # Errors
///
/// [`Fx25Error::BufferTooSmall`] when `out` cannot hold the stuffed
/// frame.
///
/// ```
/// use warble::fx25::{WRAP_MAX, stuff_frame, wrap};
///
/// // A minimal 16-byte AX.25 header as the frame body.
/// let body: [u8; 16] = core::array::from_fn(|i| i as u8);
/// let mut stuffed = [0u8; 64];
/// let len = stuff_frame(&body, &mut stuffed)?;
/// assert_eq!(stuffed[0], 0x7E);
///
/// let mut out = [0u8; WRAP_MAX];
/// let wrapped = wrap(&stuffed[..len], &mut out)?;
/// assert!(wrapped.tag().data_len() >= len);
/// # Ok::<(), warble::fx25::Fx25Error>(())
/// ```
#[cfg(feature = "ax25")]
pub fn stuff_frame(frame: &[u8], out: &mut [u8]) -> Result<usize, Fx25Error> {
    let mut nbits = 0usize;
    for bit in hdlc::frame_bits(frame, 1, 1) {
        let Some(slot) = out.get_mut(nbits / 8) else {
            return Err(Fx25Error::BufferTooSmall {
                needed: nbits / 8 + 1,
                got: out.len(),
            });
        };
        if nbits.is_multiple_of(8) {
            *slot = 0;
        }
        if let Bit::One = bit {
            *slot |= 1 << (nbits % 8);
        }
        nbits += 1;
    }
    // Complete any partial final byte with the leading bits of a flag,
    // so padding never breaks the flag idle pattern mid-run.
    let mut flag_bit = 0u32;
    while !nbits.is_multiple_of(8) {
        let Some(slot) = out.get_mut(nbits / 8) else {
            return Err(Fx25Error::BufferTooSmall {
                needed: nbits / 8 + 1,
                got: out.len(),
            });
        };
        if (FLAG_FILL >> flag_bit) & 1 != 0 {
            *slot |= 1 << (nbits % 8);
        }
        flag_bit += 1;
        nbits += 1;
    }
    Ok(nbits / 8)
}

/// Lazy LSB-first bit iterator over a byte slice.
///
/// The FX.25 transmission bytes produced by [`wrap`] feed the same bit
/// stages as an ordinary frame: `byte_bits(..)` → NRZI encode →
/// modulator.
#[derive(Debug, Clone)]
pub struct ByteBits<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// Creates an LSB-first [`ByteBits`] iterator over `bytes`.
#[must_use]
pub fn byte_bits(bytes: &[u8]) -> ByteBits<'_> {
    ByteBits { bytes, pos: 0 }
}

impl Iterator for ByteBits<'_> {
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        let byte = self.bytes.get(self.pos / 8)?;
        let bit = Bit::from((byte >> (self.pos % 8)) & 1 != 0);
        self.pos += 1;
        Some(bit)
    }
}

/// Receive state of [`Fx25Receiver`].
#[cfg(feature = "ax25")]
#[derive(Debug, Clone, Copy)]
enum RxState {
    /// Correlating the bit stream against the tag set.
    Hunt,
    /// Collecting the codeblock announced by a matched tag.
    Collect {
        /// The matched tag.
        tag: CorrelationTag,
        /// Complete bytes collected so far (data, then parity).
        count: usize,
        /// Bits accumulated into `cur`, `0..8`.
        nbits: u8,
        /// Byte currently being assembled, LSB-first.
        cur: u8,
    },
}

/// FX.25-aware frame receiver: post-NRZI bits in, decoded frames out.
///
/// Runs two paths over the same bit stream:
///
/// * a **tag hunter** correlating a sliding 64-bit window against every
///   published tag (accepting matches within [`TAG_TOLERANCE`] bit
///   errors); a lock collects the announced codeblock, Reed-Solomon
///   decodes it in place, and pushes the corrected embedded HDLC frame
///   through the deframer;
/// * a **plain HDLC path**: every bit also feeds a deframer directly, so
///   ordinary non-FX.25 frames still decode.
///
/// The two are **parallel taps**, not alternatives: the tag hunter never
/// withholds a bit. That matters because a lock lasts a whole codeblock
/// — up to 255 bytes, about 1.7 s at 1200 baud — and tag matching
/// accepts up to [`TAG_TOLERANCE`] bit errors at every bit offset, so a
/// blanking design loses every plain frame that overlaps a block, and
/// every plain frame at all after a false lock. (Earlier revisions did
/// blank, and measured **zero** frames on the plain path whenever FX.25
/// traffic was present.)
///
/// Because both paths see every bit, the same frame can surface twice:
/// the plain path closes at the embedded frame's trailing flag, while
/// the FX.25 path cannot deliver until the last parity byte arrives, a
/// gap of up to `255 × 8` bit times. Deliveries are therefore
/// deduplicated by `(FCS, length)` over a window of one maximum
/// codeblock; see [`Fx25Receiver::push`].
///
/// Block extraction uses its **own** deframer, never the plain path's.
/// Sharing one would let a corrected block's bytes splice onto a
/// half-received plain frame from before the lock and produce a frame
/// that was never transmitted — which a 16-bit FCS would catch only
/// 65535 times in 65536.
///
/// The deframers are built with [`HdlcDeframer::new`], i.e.
/// [`crate::ax25::RecoveryPolicy::None`], and are fed by one bit
/// stream. So this receiver does **not** get what `TncReceiver`
/// brings to the plain path: the multi-chain space-gain sweep, the
/// input band-pass / pre-emphasis chain diversity, single-bit-flip FCS
/// recovery, or cross-chain voting. Measured on plain AX.25 traffic
/// decoded through this path, 60 frames per run: at -2 dB SNR the
/// default `TncReceiver` recovered 51 and this receiver 26; at -3 dB,
/// 25 versus 2. FX.25's own Reed-Solomon layer more than repays that on
/// FX.25-wrapped traffic, but a receiver that mostly hears plain
/// AX.25 is better served by `TncReceiver`.
///
/// `N` is the receive buffer capacity in bytes, covering the embedded
/// frame's contents *plus* its two FCS bytes, so the largest embedded
/// frame contents that can be received is `N - 2`. Everything is
/// fixed-size: a 255-byte block buffer plus the deframer — no
/// allocation.
///
/// ```
/// use warble::fx25::{Fx25Receiver, WRAP_MAX, byte_bits, stuff_frame, wrap};
///
/// let body: [u8; 16] = core::array::from_fn(|i| (i as u8) << 1);
/// let mut stuffed = [0u8; 64];
/// let stuffed_len = stuff_frame(&body, &mut stuffed)?;
/// let mut tx = [0u8; WRAP_MAX];
/// let wrapped = wrap(&stuffed[..stuffed_len], &mut tx)?;
///
/// let mut rx = Fx25Receiver::<330>::new();
/// let mut got = None;
/// for bit in byte_bits(&tx[..wrapped.len()]) {
///     if let Some(Ok(frame)) = rx.push(bit) {
///         got = Some(frame.to_vec());
///     }
/// }
/// assert_eq!(got.as_deref(), Some(&body[..]));
/// # Ok::<(), warble::fx25::Fx25Error>(())
/// ```
#[cfg(feature = "ax25")]
#[derive(Debug, Clone)]
pub struct Fx25Receiver<const N: usize> {
    /// Sliding 64-bit correlation window (newest bit in the MSB, so the
    /// LSB-first tag lines up after 64 pushes).
    accum: u64,
    /// Bits pushed since the last reset (saturating; gates matching
    /// until the window is full).
    seen: u32,
    state: RxState,
    /// Codeblock laid out as the full RS word: data, zero suffix, parity.
    block: [u8; 255],
    /// Plain-path deframer, fed by every received bit.
    deframer: HdlcDeframer<N>,
    /// Deframer used *only* to destuff a Reed-Solomon-corrected block.
    /// Kept separate from the plain path so block bytes can never splice
    /// onto a partially received plain frame.
    block_deframer: HdlcDeframer<N>,
    /// Copy of the frame being emitted (owned so the borrow survives).
    out_buf: [u8; N],
    /// Recently delivered frames, for cross-path deduplication.
    recent: [Recent; RECENT_SLOTS],
    /// Next slot to overwrite (simple round-robin).
    recent_next: usize,
    /// Bits received, used to age [`Fx25Receiver::recent`].
    clock: u32,
}

/// How many recent deliveries are remembered for deduplication. A block
/// can hold only a couple of frames, and the dedup window is one
/// codeblock, so a handful of slots is ample.
const RECENT_SLOTS: usize = 4;

/// Dedup window in bit times: one maximum codeblock (255 bytes).
///
/// The plain path closes an embedded frame at its trailing flag; the
/// FX.25 path cannot deliver the same frame until the block's last
/// parity byte arrives. That gap is bounded by the block length, so a
/// window of one maximum block covers every case exactly, without
/// needing to guess.
const RECENT_WINDOW_BITS: u32 = 255 * 8;

/// One remembered delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Recent {
    fcs: u16,
    len: u16,
    /// Value of [`Fx25Receiver::clock`] when it was delivered.
    at: u32,
    used: bool,
}

#[cfg(feature = "ax25")]
impl<const N: usize> Fx25Receiver<N> {
    /// Creates an empty receiver, hunting for tags and flags.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            accum: 0,
            seen: 0,
            state: RxState::Hunt,
            block: [0; 255],
            deframer: HdlcDeframer::new(),
            block_deframer: HdlcDeframer::new(),
            out_buf: [0; N],
            recent: [Recent {
                fcs: 0,
                len: 0,
                at: 0,
                used: false,
            }; RECENT_SLOTS],
            recent_next: 0,
            clock: 0,
        }
    }

    /// Records a delivery and reports whether it is a duplicate of one
    /// already delivered within [`RECENT_WINDOW_BITS`].
    ///
    /// Keyed on the frame's own FCS plus its length. Two different
    /// frames colliding needs both a 16-bit CRC collision and equal
    /// lengths inside a 1.7 s window, and the cost of that is one
    /// suppressed frame — far cheaper than emitting every FX.25 frame
    /// twice, which is what the alternative does.
    fn is_duplicate(&mut self, fcs: u16, frame_len: usize) -> bool {
        let len = frame_len as u16;
        let now = self.clock;
        for slot in &self.recent {
            if slot.used
                && slot.fcs == fcs
                && slot.len == len
                && now.wrapping_sub(slot.at) <= RECENT_WINDOW_BITS
            {
                return true;
            }
        }
        self.recent[self.recent_next] = Recent {
            fcs,
            len,
            at: now,
            used: true,
        };
        self.recent_next = (self.recent_next + 1) % RECENT_SLOTS;
        false
    }

    /// Pushes one post-NRZI-decode line bit.
    ///
    /// Returns `Some(Ok(frame))` when a frame completes on either path —
    /// the FCS-validated contents, borrowed from the internal buffer
    /// until the next push. `Some(Err(_))` reports a diagnosable
    /// rejection: [`Fx25Error::Rs`] for an uncorrectable codeblock,
    /// [`Fx25Error::Ax25`] for a plain-path deframer error. Garbage is
    /// discarded silently.
    ///
    /// Every bit reaches **both** the plain deframer and the tag hunter;
    /// neither withholds bits from the other. A frame that both paths
    /// recover is emitted once: the plain path closes it at its trailing
    /// flag, the FX.25 path only after the block's last parity byte, and
    /// the later copy is suppressed by an `(FCS, length)` dedup over a
    /// window of one maximum codeblock (255 bytes of bit times), which
    /// bounds that gap exactly.
    ///
    /// If a block completes on the very same bit that closes a plain
    /// frame, the block's frame is the one returned. That costs nothing
    /// in practice: the two would be the same frame, and the plain copy
    /// would have been suppressed as a duplicate anyway.
    pub fn push(&mut self, bit: Bit) -> Option<Result<&[u8], Fx25Error>> {
        self.clock = self.clock.wrapping_add(1);

        // --- Plain path: fed unconditionally, in every state. ---
        let mut plain: Option<Result<usize, Ax25Error>> = None;
        if let Some(event) = self.deframer.push(bit) {
            plain = Some(match event {
                Ok(frame) => {
                    let len = frame.len().min(N);
                    for (dst, src) in self.out_buf.iter_mut().zip(frame.iter()) {
                        *dst = *src;
                    }
                    Ok(len)
                }
                Err(e) => Err(e),
            });
        }

        // --- FX.25 path: hunt for a tag, or collect a locked block. ---
        let mut block: Option<Result<usize, Fx25Error>> = None;
        match self.state {
            RxState::Hunt => {
                self.accum >>= 1;
                if let Bit::One = bit {
                    self.accum |= 1 << 63;
                }
                self.seen = self.seen.saturating_add(1);
                if self.seen >= 64
                    && let Some(tag) = Self::match_tag(self.accum)
                {
                    self.block = [0; 255];
                    self.state = RxState::Collect {
                        tag,
                        count: 0,
                        nbits: 0,
                        cur: 0,
                    };
                }
            }
            RxState::Collect {
                tag,
                mut count,
                mut nbits,
                mut cur,
            } => {
                if let Bit::One = bit {
                    cur |= 1 << nbits;
                }
                nbits += 1;
                if nbits == 8 {
                    // Data bytes land at the front of the RS word,
                    // parity bytes after the implicit zero suffix.
                    let index = if count < tag.data_len() {
                        count
                    } else {
                        tag.rs_data_len() + (count - tag.data_len())
                    };
                    if let Some(slot) = self.block.get_mut(index) {
                        *slot = cur;
                    }
                    count += 1;
                    nbits = 0;
                    cur = 0;
                }
                if count == tag.block_len() {
                    self.accum = 0;
                    self.seen = 0;
                    self.state = RxState::Hunt;
                    block = self.decode_block(tag);
                } else {
                    self.state = RxState::Collect {
                        tag,
                        count,
                        nbits,
                        cur,
                    };
                }
            }
        }

        // --- Deliver, deduplicated. FX.25 wins a tie (see above). ---
        match block {
            Some(Ok(len)) => {
                let fcs = crc16_x25(self.out_buf.get(..len).unwrap_or(&[]));
                if self.is_duplicate(fcs, len) {
                    return None;
                }
                return Some(Ok(self.out_buf.get(..len).unwrap_or(&[])));
            }
            Some(Err(e)) => return Some(Err(e)),
            None => {}
        }
        match plain {
            Some(Ok(len)) => {
                let fcs = crc16_x25(self.out_buf.get(..len).unwrap_or(&[]));
                if self.is_duplicate(fcs, len) {
                    None
                } else {
                    Some(Ok(self.out_buf.get(..len).unwrap_or(&[])))
                }
            }
            Some(Err(e)) => Some(Err(Fx25Error::Ax25(e))),
            None => None,
        }
    }

    /// The tag matching `accum` within [`TAG_TOLERANCE`] bit errors.
    fn match_tag(accum: u64) -> Option<CorrelationTag> {
        CorrelationTag::ALL
            .into_iter()
            .find(|t| (accum ^ t.tag_value()).count_ones() <= TAG_TOLERANCE)
    }

    /// Reed-Solomon decodes the collected block and destuffs the
    /// corrected embedded frame, returning its staged length.
    ///
    /// Extraction runs through [`Self::block_deframer`], which is reset
    /// first and is **not** the plain path's deframer, so the frame a
    /// block yields is a function of the block alone and never of what
    /// happened to precede it on the air.
    ///
    /// Honesty about how strong that guarantee needs to be: the original
    /// motivation was that sharing one deframer could splice these bytes
    /// onto a plain frame left half-received when the tag locked,
    /// manufacturing a frame that was never transmitted with only the
    /// 16-bit FCS in the way. That argument was sound when a tag lock
    /// *blanked* the plain path, because the deframer was then frozen
    /// mid-frame for the whole block. Now that every bit reaches the
    /// plain deframer, it is no longer frozen, and HDLC resynchronises
    /// on flags — **no failing case is currently known**, and an attempt
    /// to construct one did not produce a behavioural difference. The
    /// separate deframer is kept as defence in depth and for the
    /// independence property, not because a live defect is being fixed.
    ///
    /// An uncorrectable codeblock surfaces as [`Fx25Error::Rs`], but a
    /// block whose RS decode **succeeded** and still produced no frame
    /// returns plain `None` — the same thing [`Fx25Receiver::push`]
    /// returns for every ordinary bit. There is no error and no
    /// counter, so a caller cannot distinguish "a repaired FX.25 block
    /// held nothing the deframer would accept" (padding only, a runt, a
    /// truncated embedded frame) from "no FX.25 was ever here" (the tag
    /// lock was spurious).
    ///
    /// Only the **first** frame in a block is returned. A 255-byte block
    /// can legitimately carry two short frames; the second is dropped,
    /// because `push` has one output slot per bit and the whole block
    /// completes on a single bit. Real APRS traffic wraps one frame per
    /// block, so this has never been observed to cost anything.
    fn decode_block(&mut self, tag: CorrelationTag) -> Option<Result<usize, Fx25Error>> {
        let codec = RsCodec::new(tag.parity());
        if let Err(e) = codec.decode(&mut self.block) {
            return Some(Err(Fx25Error::Rs(e)));
        }
        self.block_deframer = HdlcDeframer::new();
        let mut found: Option<usize> = None;
        for i in 0..tag.data_len() {
            let byte = self.block.get(i).copied().unwrap_or(0);
            for k in 0..8 {
                let data = Bit::from((byte >> k) & 1 != 0);
                if let Some(Ok(frame)) = self.block_deframer.push(data)
                    && found.is_none()
                {
                    let len = frame.len().min(N);
                    for (dst, src) in self.out_buf.iter_mut().zip(frame.iter()) {
                        *dst = *src;
                    }
                    found = Some(len);
                }
            }
        }
        Some(Ok(found?))
    }
}

#[cfg(feature = "ax25")]
impl<const N: usize> Default for Fx25Receiver<N> {
    /// Same as [`Fx25Receiver::new`].
    fn default() -> Self {
        Self::new()
    }
}
