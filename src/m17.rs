//! M17 packet-mode data: framing, FEC, and the 4-level baseband modem.
//!
//! # What M17 is
//!
//! [M17](https://spec.m17project.org/) is an open, royalty-free digital
//! radio protocol built around 4-level frequency modulation at 4800
//! symbols (9600 bits) per second. Every over-the-air element — the
//! base-40 callsign addressing, the Link Setup Frame (LSF), the
//! convolutional FEC with puncturing, the interleaver, the randomizer
//! and the sync bursts — is published in the M17 specification.
//!
//! Implemented from the **published M17 specification**
//! (spec.m17project.org, an openly published document). Constants are
//! annotated with the spec part they come from; where a constant was
//! transcribed from the published tables rather than derived, that
//! provenance is noted on the item so a correction is a one-line change.
//!
//! # Scope: packet data only (voice absent)
//!
//! M17 has two payload modes:
//!
//! * **Packet mode** — connectionless data packets (this module:
//!   complete TX and RX).
//! * **Stream mode** — continuous voice (Codec2) and/or data. Voice
//!   requires a Codec2 vocoder, which is an external dependency this
//!   crate does not take without operator approval; see the proposal
//!   in `docs/ARCHITECTURE.md`, "Codec2 voice for M17 stream mode".
//!   Stream-mode *framing* is therefore not
//!   assembled here either, but its one nontrivial code — the extended
//!   **Golay(24,12)** protecting the LICH chunks — ships as a public,
//!   fully tested building block ([`golay24_encode`] /
//!   [`golay24_decode`]) so a future stream slice starts from a proven
//!   codec.
//!
//! # Baseband
//!
//! An audio modem ends at baseband: this module produces and consumes
//! the **RRC-shaped 4-level PAM baseband waveform** (the signal an FM
//! exciter's modulator input would be fed, or an FM discriminator
//! output delivers). The 4FSK RF modulation itself — ±0.8/±2.4 kHz
//! deviation of an RF carrier — happens in the radio and is outside an
//! audio modem's scope, exactly as the crate's G3RUH 9600-baud path
//! stops at its scrambled baseband.
//!
//! The modem mirrors the G3RUH baseband family's structure (fixed-tap
//! integer FIR, feed/pull streaming, alloc-free) but is a standalone
//! pair rather than a third `ModulationScheme` variant: the existing
//! seam is binary-symbol and HDLC-centric, and M17's 4-level symbols,
//! sync-word framing and block FEC share none of that chain. Keeping it
//! standalone leaves the shipped G3RUH/AFSK paths byte-identical.
//!
//! # TX pipeline (spec: Data Link Layer + Physical Layer)
//!
//! 1. [`Lsf`]: DST/SRC [`Address`] (base-40 callsign), TYPE, META,
//!    CRC-16 → 240 bits.
//! 2. Packet superframe: payload + CRC-16, chunked into 25-byte frames
//!    each carrying a 6-bit frame-number/EOF field → 206 bits.
//! 3. Convolutional encoder (K=5, rate 1/2, G1/G2), 4 flush bits.
//! 4. Puncturing: P1 (LSF, 488→368) or P3 (packet, 420→368).
//! 5. Interleaving: quadratic permutation polynomial over 368 bits.
//! 6. Randomizing: XOR with the published 46-byte sequence.
//! 7. Sync burst (16 bits) + 368 bits = 192 dibit symbols per frame.
//! 8. [`M17Modulator`]: symbols → root-raised-cosine (α = 0.5) shaped
//!    PAM at 4800 symbols/s (48 kHz canonical, 10 samples/symbol).
//!
//! [`M17PacketTx`] runs the whole chain (preamble → LSF → packet frames
//! → EOT → filter flush) as one alloc-free sample iterator; the
//! receiving side is [`M17Receiver`] (matched filter, sync-correlation
//! timing recovery, 4-level slicer, Viterbi) plus [`PacketAssembler`].
//!
//! # Example
//!
//! ```
//! use yodel::SampleRate;
//! use yodel::m17::{Address, Lsf, M17PacketTx, M17Receiver, PacketAssembler, M17FrameEvent};
//!
//! let lsf = Lsf::packet_data(Address::broadcast(), Address::from_callsign("N0CALL")?, 0);
//! let payload = b"Hello, M17!";
//! let sr = SampleRate::new(48_000)?;
//! let mut tx = M17PacketTx::new(sr, lsf, payload)?;
//! let mut rx = M17Receiver::new(sr)?;
//! let mut asm = PacketAssembler::new();
//! let mut got = None;
//! while let Some(sample) = tx.next_i16() {
//!     match rx.push_i16(sample) {
//!         Some(M17FrameEvent::Lsf(l)) => asm.start(l),
//!         Some(M17FrameEvent::PacketFrame(f)) => {
//!             if let Some(p) = asm.feed(&f) {
//!                 got = Some(p.to_vec());
//!             }
//!         }
//!         None => {}
//!     }
//! }
//! assert_eq!(got.as_deref(), Some(&payload[..]));
//! # Ok::<(), yodel::m17::M17Error>(())
//! ```

use core::fmt;

use crate::error::ConfigError;
use crate::types::SampleRate;

mod fec;
mod modem;

// The submodules are an internal split of this module, so everything
// they define is re-exported here and every public path is unchanged.
pub use fec::*;
pub use modem::*;

// Shared internals: the bit helpers serve packet framing as well as the
// channel coder, and the RRC design serves the receiver's matched
// filter as well as the modulator.
use fec::{get_bit, set_bit};
use modem::{MAX_TAPS, checked_sps, design_rrc};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong constructing or decoding M17 entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum M17Error {
    /// Callsign is empty or longer than 9 characters.
    CallsignLength {
        /// The offending length in characters.
        len: usize,
    },
    /// Callsign contains a character outside the base-40 alphabet
    /// (`A`–`Z`, `0`–`9`, `-`, `/`, `.`).
    CallsignChar {
        /// The offending character.
        ch: char,
    },
    /// A 48-bit address value in the reserved range (0, or above every
    /// encodable callsign but below broadcast).
    ReservedAddress {
        /// The raw 48-bit value.
        value: u64,
    },
    /// A 48-bit address value that does not fit in 48 bits.
    AddressRange {
        /// The raw value.
        value: u64,
    },
    /// Packet payload longer than [`MAX_PACKET_PAYLOAD`] bytes.
    PayloadTooLong {
        /// The offending length.
        len: usize,
    },
    /// CRC-16 mismatch while decoding an LSF or a packet.
    Crc,
    /// Sample rate is not a multiple of 4800 Hz (the symbol rate), so
    /// samples-per-symbol would not be an integer.
    SampleRateInexact {
        /// The offending rate in Hz.
        got: u32,
    },
    /// A configuration error from the shared sample-rate validator.
    Config(ConfigError),
}

impl fmt::Display for M17Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CallsignLength { len } => {
                write!(f, "callsign must be 1..=9 characters, got {len}")
            }
            Self::CallsignChar { ch } => {
                write!(f, "character {ch:?} is outside the M17 base-40 alphabet")
            }
            Self::ReservedAddress { value } => {
                write!(f, "address {value:#014x} is in the reserved range")
            }
            Self::AddressRange { value } => {
                write!(f, "address {value:#x} does not fit in 48 bits")
            }
            Self::PayloadTooLong { len } => write!(
                f,
                "packet payload is {len} bytes, maximum is {MAX_PACKET_PAYLOAD}"
            ),
            Self::Crc => write!(f, "CRC-16 mismatch"),
            Self::SampleRateInexact { got } => write!(
                f,
                "sample rate {got} Hz is not a multiple of the 4800 Hz symbol rate"
            ),
            Self::Config(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for M17Error {}

impl From<ConfigError> for M17Error {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}

// ---------------------------------------------------------------------------
// CRC-16 (M17 spec, "CRC" section of the Data Link Layer)
// ---------------------------------------------------------------------------

/// The M17 CRC-16 polynomial, `x^16 + x^14 + x^12 + x^11 + x^8 + x^5 +
/// x^4 + x^2 + 1` → 0x5935 (M17 spec, Data Link Layer, CRC: a custom
/// polynomial chosen from the Koopman tables).
pub const CRC16_POLY: u16 = 0x5935;

/// CRC-16 initial value (M17 spec: 0xFFFF, no reflection, no final XOR).
pub const CRC16_INIT: u16 = 0xFFFF;

/// Computes the M17 CRC-16 over `data`.
///
/// Parameters per the M17 spec CRC section: polynomial [`CRC16_POLY`],
/// init [`CRC16_INIT`], MSB-first, no input/output reflection, no final
/// XOR. The spec publishes check values (empty → 0xFFFF, `"A"` →
/// 0x206E, `"123456789"` → 0x772B); `tests/m17.rs` pins them.
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = CRC16_INIT;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ CRC16_POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// Base-40 callsign addresses (M17 spec, "Address Encoding")
// ---------------------------------------------------------------------------

/// The M17 base-40 alphabet, index 0..=39 (M17 spec, Address Encoding).
/// Index 0 is the pad/termination character and never appears inside an
/// encoded callsign produced by this module.
const BASE40: &[u8; 40] = b" ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-/.";

/// The all-ones broadcast destination address (M17 spec: 0xFFFFFFFFFFFF).
pub const BROADCAST_ADDRESS: u64 = 0xFFFF_FFFF_FFFF;

/// Largest 48-bit value that decodes to a base-40 callsign: 40⁹ − 1.
/// Values above it (up to but excluding [`BROADCAST_ADDRESS`]) are
/// reserved by the spec for future use.
pub const MAX_CALLSIGN_ADDRESS: u64 = 40u64.pow(9) - 1;

/// A validated 48-bit M17 address (base-40 callsign or broadcast).
///
/// The spec encodes a callsign of up to 9 characters as a base-40
/// number: processing characters from the **last to the first**, each
/// step multiplies the accumulator by 40 and adds the character's
/// alphabet index, so the first character ends up in the least
/// significant digit (M17 spec, Address Encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(u64);

impl Address {
    /// The broadcast address (valid as a destination only, per spec;
    /// this module does not police direction).
    #[must_use]
    pub const fn broadcast() -> Self {
        Self(BROADCAST_ADDRESS)
    }

    /// Returns true for the broadcast address.
    #[must_use]
    pub const fn is_broadcast(self) -> bool {
        self.0 == BROADCAST_ADDRESS
    }

    /// Encodes a callsign (1..=9 characters from `A`–`Z`, `0`–`9`,
    /// `-`, `/`, `.`; lowercase accepted and folded).
    ///
    /// # Errors
    ///
    /// [`M17Error::CallsignLength`] or [`M17Error::CallsignChar`].
    pub fn from_callsign(callsign: &str) -> Result<Self, M17Error> {
        let bytes = callsign.as_bytes();
        if bytes.is_empty() || bytes.len() > 9 {
            return Err(M17Error::CallsignLength { len: bytes.len() });
        }
        let mut value: u64 = 0;
        for &b in bytes.iter().rev() {
            let up = b.to_ascii_uppercase();
            let idx = BASE40[1..]
                .iter()
                .position(|&c| c == up)
                .ok_or(M17Error::CallsignChar { ch: char::from(b) })?;
            value = value * 40 + (idx as u64 + 1);
        }
        Ok(Self(value))
    }

    /// Wraps a raw 48-bit address value, rejecting values outside 48
    /// bits and the spec's reserved ranges (0, and everything between
    /// [`MAX_CALLSIGN_ADDRESS`] and [`BROADCAST_ADDRESS`] exclusive).
    ///
    /// # Errors
    ///
    /// [`M17Error::AddressRange`] or [`M17Error::ReservedAddress`].
    pub const fn from_raw(value: u64) -> Result<Self, M17Error> {
        if value > BROADCAST_ADDRESS {
            return Err(M17Error::AddressRange { value });
        }
        if value == 0 || (value > MAX_CALLSIGN_ADDRESS && value != BROADCAST_ADDRESS) {
            return Err(M17Error::ReservedAddress { value });
        }
        Ok(Self(value))
    }

    /// The raw 48-bit value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Decodes the address back to callsign characters, writing into
    /// `buf` and returning the used prefix (`"@ALL"` for broadcast,
    /// mirroring common M17 tooling display convention).
    pub fn callsign(self, buf: &mut [u8; 9]) -> &str {
        if self.is_broadcast() {
            buf[..4].copy_from_slice(b"@ALL");
            return core::str::from_utf8(&buf[..4]).unwrap_or("@ALL");
        }
        let mut v = self.0;
        let mut n = 0;
        while v > 0 && n < 9 {
            buf[n] = BASE40[(v % 40) as usize];
            v /= 40;
            n += 1;
        }
        core::str::from_utf8(&buf[..n]).unwrap_or("")
    }
}

// ---------------------------------------------------------------------------
// Link Setup Frame (M17 spec, Data Link Layer, "Link Setup Frame")
// ---------------------------------------------------------------------------

/// Bytes in an LSF: DST(6) + SRC(6) + TYPE(2) + META(14) + CRC(2).
pub const LSF_BYTES: usize = 30;

/// A Link Setup Frame: the 240-bit header opening every M17
/// transmission (M17 spec, Data Link Layer).
///
/// TYPE field bit layout (LSB first, per spec):
/// bit 0 packet/stream (0 = packet), bits 1–2 data type (`0b01` =
/// data), bits 3–4 encryption type (0 = none), bits 5–6 encryption
/// subtype, bits 7–10 Channel Access Number, bits 11–15 reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lsf {
    /// Destination address.
    pub dst: Address,
    /// Source address.
    pub src: Address,
    /// The 16-bit TYPE field (see the struct docs for the layout).
    pub lsf_type: u16,
    /// The 112-bit META field (zeroed unless the caller provides one;
    /// packet data mode uses no META, per spec it is then all zero).
    pub meta: [u8; 14],
}

impl Lsf {
    /// Builds a packet-mode, data-payload LSF (TYPE bits: packet,
    /// data-subtype `0b01`, no encryption) with the given 4-bit Channel
    /// Access Number and zeroed META.
    #[must_use]
    pub const fn packet_data(dst: Address, src: Address, can: u8) -> Self {
        // TYPE: bit0 = 0 (packet), bits1-2 = 0b01 (data), bits7-10 = CAN.
        let lsf_type = (0b01 << 1) | ((can as u16 & 0xF) << 7);
        Self {
            dst,
            src,
            lsf_type,
            meta: [0; 14],
        }
    }

    /// Serializes to the 30-byte wire form, computing the CRC-16 over
    /// the first 28 bytes (spec: CRC covers DST..META).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; LSF_BYTES] {
        let mut out = [0u8; LSF_BYTES];
        out[..6].copy_from_slice(&self.dst.raw().to_be_bytes()[2..]);
        out[6..12].copy_from_slice(&self.src.raw().to_be_bytes()[2..]);
        out[12..14].copy_from_slice(&self.lsf_type.to_be_bytes());
        out[14..28].copy_from_slice(&self.meta);
        let crc = crc16(&out[..28]);
        out[28..].copy_from_slice(&crc.to_be_bytes());
        out
    }

    /// Parses the 30-byte wire form, verifying the CRC and validating
    /// both addresses.
    ///
    /// # Errors
    ///
    /// [`M17Error::Crc`] on checksum mismatch; address errors from
    /// [`Address::from_raw`].
    pub fn from_bytes(bytes: &[u8; LSF_BYTES]) -> Result<Self, M17Error> {
        let crc = u16::from_be_bytes([bytes[28], bytes[29]]);
        if crc != crc16(&bytes[..28]) {
            return Err(M17Error::Crc);
        }
        let mut d = [0u8; 8];
        d[2..].copy_from_slice(&bytes[..6]);
        let dst = Address::from_raw(u64::from_be_bytes(d))?;
        let mut s = [0u8; 8];
        s[2..].copy_from_slice(&bytes[6..12]);
        let src = Address::from_raw(u64::from_be_bytes(s))?;
        let lsf_type = u16::from_be_bytes([bytes[12], bytes[13]]);
        let mut meta = [0u8; 14];
        meta.copy_from_slice(&bytes[14..28]);
        Ok(Self {
            dst,
            src,
            lsf_type,
            meta,
        })
    }
}

// ---------------------------------------------------------------------------
// Sync bursts (M17 spec, Physical Layer, "Synchronization Burst")
// ---------------------------------------------------------------------------

/// LSF sync burst (M17 spec, Physical Layer: 0x55F7).
pub const SYNC_LSF: u16 = 0x55F7;
/// Stream-frame sync burst (M17 spec: 0xFF5D). Stream framing itself is
/// out of scope this slice (voice pending); the constant ships for
/// completeness and future use.
pub const SYNC_STREAM: u16 = 0xFF5D;
/// Packet-frame sync burst (M17 spec: 0x75FF).
pub const SYNC_PACKET: u16 = 0x75FF;
/// BERT sync burst (M17 spec: 0xDF55).
pub const SYNC_BERT: u16 = 0xDF55;
/// End-of-transmission marker, sent repeated for one 40 ms frame
/// (M17 spec, Physical Layer: 0x555D).
pub const EOT_MARKER: u16 = 0x555D;

// ---------------------------------------------------------------------------
// Packet-mode framing (M17 spec, "Packet Superframes")
// ---------------------------------------------------------------------------

/// Payload bytes carried by one packet frame.
pub const PACKET_FRAME_PAYLOAD: usize = 25;

/// Maximum raw packet payload in bytes: with the 2-byte CRC appended,
/// the superframe fills at most 33 × 25 = 825 bytes (frame counter is 5
/// bits, so 32 numbered frames plus the final EOF frame; M17 spec,
/// Packet Superframes).
pub const MAX_PACKET_PAYLOAD: usize = 33 * PACKET_FRAME_PAYLOAD - 2;

/// One received (or to-be-sent) packet frame: 25 payload bytes plus the
/// 6-bit frame-number/EOF field (M17 spec, Packet Superframes: the
/// field follows the payload; its MSB is the EOF flag, the next 5 bits
/// are the frame number, or — when EOF is set — the count of valid
/// bytes in this final frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketFrame {
    /// The 25 payload bytes (trailing bytes of a short final frame are
    /// zero).
    pub data: [u8; PACKET_FRAME_PAYLOAD],
    /// EOF flag (true on the final frame of a superframe).
    pub eof: bool,
    /// Frame number (EOF false) or valid-byte count (EOF true).
    pub counter: u8,
}

impl PacketFrame {
    /// Packs into the 206-bit frame content (25 bytes + 6-bit field in
    /// the top of byte 25; the last two bit positions are unused/zero).
    #[must_use]
    pub fn to_content(&self) -> [u8; 26] {
        let mut out = [0u8; 26];
        out[..PACKET_FRAME_PAYLOAD].copy_from_slice(&self.data);
        out[25] = (u8::from(self.eof) << 7) | ((self.counter & 0x1F) << 2);
        out
    }

    /// Unpacks from the 206-bit frame content.
    #[must_use]
    pub fn from_content(content: &[u8; 26]) -> Self {
        let mut data = [0u8; PACKET_FRAME_PAYLOAD];
        data.copy_from_slice(&content[..PACKET_FRAME_PAYLOAD]);
        Self {
            data,
            eof: content[25] & 0x80 != 0,
            counter: (content[25] >> 2) & 0x1F,
        }
    }
}

/// Encodes 206 bits of packet-frame content through the shared channel
/// coding: conv encode (+4 flush) → P3 puncture (420 → 368) →
/// interleave → randomize.
#[must_use]
pub fn packet_frame_encode(frame: &PacketFrame) -> [u8; FRAME_BYTES] {
    let content = frame.to_content();
    let mut coded = [0u8; 53]; // 420 bits
    let n = convolutional_encode(&content, 206, &mut coded);
    debug_assert_eq!(n, 420);
    let mut punctured = [0u8; FRAME_BYTES];
    let kept = puncture(&coded, n, &PUNCTURE_P3, &mut punctured);
    debug_assert_eq!(kept, FRAME_BITS);
    let mut out = interleave(&punctured);
    randomize(&mut out);
    out
}

/// Decodes a received 368-bit packet frame (inverse of
/// [`packet_frame_encode`]). Returns the frame and the Viterbi path
/// metric (0 = error-free).
#[must_use]
pub fn packet_frame_decode(frame: &[u8; FRAME_BYTES]) -> (PacketFrame, u32) {
    let mut f = *frame;
    randomize(&mut f);
    let deint = deinterleave(&f);
    let mut bits = [0u8; 420];
    let mut known = [false; 420];
    depuncture(&deint, FRAME_BITS, &PUNCTURE_P3, 420, &mut bits, &mut known);
    let mut content = [0u8; 26];
    let metric = viterbi_decode(&bits, &known, 206, &mut content);
    (PacketFrame::from_content(&content), metric)
}

/// Encodes an LSF through the channel coding: conv encode (240 + 4
/// flush = 488 bits) → P1 puncture (488 → 368) → interleave →
/// randomize (M17 spec, Channel Coding for the LSF).
#[must_use]
pub fn lsf_encode(lsf: &Lsf) -> [u8; FRAME_BYTES] {
    let bytes = lsf.to_bytes();
    let mut coded = [0u8; 61]; // 488 bits
    let n = convolutional_encode(&bytes, 240, &mut coded);
    debug_assert_eq!(n, 488);
    let mut punctured = [0u8; FRAME_BYTES];
    let kept = puncture(&coded, n, &PUNCTURE_P1, &mut punctured);
    debug_assert_eq!(kept, FRAME_BITS);
    let mut out = interleave(&punctured);
    randomize(&mut out);
    out
}

/// Decodes a received 368-bit LSF frame (inverse of [`lsf_encode`]),
/// verifying the CRC.
///
/// # Errors
///
/// [`M17Error::Crc`] or address validation errors.
pub fn lsf_decode(frame: &[u8; FRAME_BYTES]) -> Result<Lsf, M17Error> {
    let mut f = *frame;
    randomize(&mut f);
    let deint = deinterleave(&f);
    let mut bits = [0u8; 488];
    let mut known = [false; 488];
    depuncture(&deint, FRAME_BITS, &PUNCTURE_P1, 488, &mut bits, &mut known);
    let mut content = [0u8; LSF_BYTES];
    let _metric = viterbi_decode(&bits, &known, 240, &mut content);
    Lsf::from_bytes(&content)
}

// ---------------------------------------------------------------------------
// One-shot packet transmitter
// ---------------------------------------------------------------------------

/// Symbols in the preamble and in the EOT burst (one 40 ms frame each).
pub const PREAMBLE_SYMBOLS: usize = FRAME_SYMBOLS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxSection {
    Preamble,
    Frame,
    Eot,
    Flush,
    Done,
}

/// One-shot alloc-free packet-mode transmitter: preamble + LSF frame +
/// packet frames + EOT marker + filter flush, as a pull-based i16
/// sample source (mirrors the crate's `WsprModulator`/`Ft8Modulator`
/// one-burst style).
///
/// The payload is borrowed; the 2-byte CRC and the frame chunking are
/// produced on the fly, so the only buffers are one 368-bit frame and
/// the modulator's filter state.
#[derive(Debug, Clone)]
pub struct M17PacketTx<'a> {
    modulator: M17Modulator,
    lsf: Lsf,
    payload: &'a [u8],
    crc: u16,
    section: TxSection,
    /// Current frame's coded bits (sync word handled separately).
    frame_bits: [u8; FRAME_BYTES],
    sync: u16,
    /// Symbol cursor within the current section.
    symbol: usize,
    /// Next packet frame index; `usize::MAX` marks "LSF pending".
    next_frame: usize,
    frames_total: usize,
}

impl<'a> M17PacketTx<'a> {
    /// Prepares a transmission of `payload` under the given LSF.
    ///
    /// # Errors
    ///
    /// [`M17Error::PayloadTooLong`] beyond [`MAX_PACKET_PAYLOAD`];
    /// sample-rate errors from [`M17Modulator::new`].
    pub fn new(sample_rate: SampleRate, lsf: Lsf, payload: &'a [u8]) -> Result<Self, M17Error> {
        if payload.len() > MAX_PACKET_PAYLOAD {
            return Err(M17Error::PayloadTooLong { len: payload.len() });
        }
        let total = payload.len() + 2;
        let frames_total = total.div_ceil(PACKET_FRAME_PAYLOAD);
        Ok(Self {
            modulator: M17Modulator::new(sample_rate)?,
            lsf,
            payload,
            crc: crc16(payload),
            section: TxSection::Preamble,
            frame_bits: [0; FRAME_BYTES],
            sync: SYNC_LSF,
            symbol: 0,
            next_frame: usize::MAX,
            frames_total,
        })
    }

    /// Superframe byte at stream index `i` (payload then big-endian CRC).
    fn stream_byte(&self, i: usize) -> u8 {
        if i < self.payload.len() {
            self.payload[i]
        } else if i == self.payload.len() {
            (self.crc >> 8) as u8
        } else {
            (self.crc & 0xFF) as u8
        }
    }

    fn load_next_frame(&mut self) -> bool {
        if self.next_frame == usize::MAX {
            self.frame_bits = lsf_encode(&self.lsf);
            self.sync = SYNC_LSF;
            self.next_frame = 0;
            return true;
        }
        let k = self.next_frame;
        if k >= self.frames_total {
            return false;
        }
        let total = self.payload.len() + 2;
        let start = k * PACKET_FRAME_PAYLOAD;
        let take = (total - start).min(PACKET_FRAME_PAYLOAD);
        let mut data = [0u8; PACKET_FRAME_PAYLOAD];
        for (j, d) in data.iter_mut().enumerate().take(take) {
            *d = self.stream_byte(start + j);
        }
        let eof = k + 1 == self.frames_total;
        let frame = PacketFrame {
            data,
            eof,
            counter: if eof { take as u8 } else { (k & 0x1F) as u8 },
        };
        self.frame_bits = packet_frame_encode(&frame);
        self.sync = SYNC_PACKET;
        self.next_frame = k + 1;
        true
    }

    /// Feeds the modulator the next symbol of the burst; false when done.
    fn feed_next_symbol(&mut self) -> bool {
        loop {
            match self.section {
                TxSection::Preamble => {
                    if self.symbol < PREAMBLE_SYMBOLS {
                        // Preamble: alternating +3/−3 (M17 spec, Physical
                        // Layer: the 0x77 preamble byte pattern).
                        let s = if self.symbol.is_multiple_of(2) { 3 } else { -3 };
                        self.modulator.feed(s);
                        self.symbol += 1;
                        return true;
                    }
                    if self.load_next_frame() {
                        self.section = TxSection::Frame;
                        self.symbol = 0;
                    } else {
                        self.section = TxSection::Eot;
                        self.symbol = 0;
                    }
                }
                TxSection::Frame => {
                    if self.symbol < FRAME_SYMBOLS {
                        let s = if self.symbol < 8 {
                            sync_symbols(self.sync)[self.symbol]
                        } else {
                            let bit_idx = (self.symbol - 8) * 2;
                            let dibit = (get_bit(&self.frame_bits, bit_idx) << 1)
                                | get_bit(&self.frame_bits, bit_idx + 1);
                            dibit_to_symbol(dibit)
                        };
                        self.modulator.feed(s);
                        self.symbol += 1;
                        return true;
                    }
                    if self.load_next_frame() {
                        self.symbol = 0;
                    } else {
                        self.section = TxSection::Eot;
                        self.symbol = 0;
                    }
                }
                TxSection::Eot => {
                    if self.symbol < FRAME_SYMBOLS {
                        // EOT: the 0x555D marker repeated (M17 spec).
                        let i = self.symbol % 8;
                        let dibit = ((EOT_MARKER >> (14 - 2 * i)) & 0b11) as u8;
                        self.modulator.feed(dibit_to_symbol(dibit));
                        self.symbol += 1;
                        return true;
                    }
                    self.section = TxSection::Flush;
                    self.symbol = 0;
                }
                TxSection::Flush => {
                    if self.symbol < RRC_SPAN_SYMBOLS + 1 {
                        self.modulator.feed(0);
                        self.symbol += 1;
                        return true;
                    }
                    self.section = TxSection::Done;
                }
                TxSection::Done => return false,
            }
        }
    }

    /// Pulls the next PCM sample, or `None` when the burst is complete.
    pub fn next_i16(&mut self) -> Option<i16> {
        loop {
            if let Some(s) = self.modulator.next_i16() {
                return Some(s);
            }
            if !self.feed_next_symbol() {
                return None;
            }
        }
    }
}

impl Iterator for M17PacketTx<'_> {
    type Item = i16;
    fn next(&mut self) -> Option<i16> {
        self.next_i16()
    }
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// A deframed event from the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum M17FrameEvent {
    /// A Link Setup Frame that passed FEC + CRC.
    Lsf(Lsf),
    /// A packet frame as decoded by the Viterbi FEC — **not** a
    /// frame that is known good. The decoder always returns its best
    /// guess and the receiver discards the path metric, so a frame
    /// carrying garbage (wrong counter, wrong payload bytes) is
    /// emitted just like a clean one; corrupted input reliably
    /// produces `PacketFrame` events with wrong contents. Correctness
    /// is gated downstream by [`PacketAssembler`], which checks the
    /// frame-counter sequence and the superframe CRC-16 before
    /// yielding a payload. (The [`Lsf`](Self::Lsf) variant *is*
    /// CRC-checked here.)
    PacketFrame(PacketFrame),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RxState {
    Hunt,
    /// Sync threshold crossed: refine the correlation peak for up to
    /// one symbol period.
    Confirm,
    Collect,
}

/// Streaming M17 baseband receiver: i16 PCM in, deframed
/// [`M17FrameEvent`]s out.
///
/// Per sample it runs the RRC matched filter, then hunts for a sync
/// burst by correlating the last eight symbol centers against the
/// LSF/packet sync symbol patterns — the correlation peak fixes both
/// the frame alignment and the symbol timing at once (the burst is
/// re-acquired every 40 ms frame, so a fixed per-frame timing estimate
/// suffices at audio-loopback clock accuracy; a Gardner-style tracker
/// can be layered later for off-frequency soundcards). The sync peak
/// also calibrates the 4-level slicer thresholds (±2 units around
/// zero) — once per burst, from the acquiring sync burst, and never
/// updated afterwards, so amplitude drift or fading within a burst
/// biases the ±2·unit decision thresholds. Sliced dibits are
/// derandomized, deinterleaved, depunctured and Viterbi-decoded; LSFs
/// are CRC-checked here, packet superframe CRCs by
/// [`PacketAssembler`].
///
/// # Level handling (no AGC)
///
/// Acquisition is squelched by a hard-coded floor: a sync correlation
/// only counts as plausible when the matched-filter output peaks above
/// 500. There is no automatic gain control, so this fixes the usable
/// dynamic range below nominal drive. Measured against the modulator's
/// nominal ~29 337 peak output, packets recover perfectly down to
/// −43 dB of attenuation; at −44 dB the receiver emits no LSF events
/// at all. Scale weak captures up before feeding them in.
#[derive(Debug, Clone)]
pub struct M17Receiver {
    /// Matched-filter taps (unit-energy RRC, Q13).
    taps: [i32; MAX_TAPS],
    ntaps: usize,
    sps: usize,
    /// Raw-sample delay line for the FIR (ring; length `ntaps`).
    delay: [i16; MAX_TAPS],
    dpos: usize,
    /// Filtered-sample history for sync correlation (ring).
    hist: [i32; MAX_TAPS],
    hpos: usize,
    state: RxState,
    /// Best correlation seen while confirming, its sync word, its age
    /// in samples, and the remaining confirmation window.
    best_corr: i64,
    best_sync: u16,
    best_age: usize,
    confirm_left: usize,
    /// Slicer unit amplitude (filtered-domain value of a ±1 symbol).
    unit: i32,
    /// Samples until the next symbol center while collecting.
    countdown: usize,
    /// True when the next 8 collected symbols are a sync burst (frame
    /// continuation in lockstep after a completed frame).
    expect_sync: bool,
    /// Sync dibits accumulated while `expect_sync`.
    sync_acc: u16,
    sync_count: u8,
    /// Collected payload symbols (as bits) and count.
    frame_bits: [u8; FRAME_BYTES],
    nsyms: usize,
    frame_sync: u16,
}

impl M17Receiver {
    /// Creates a receiver for the given sample rate (same multiple-of-
    /// 4800 Hz rule as the modulator).
    ///
    /// # Errors
    ///
    /// [`M17Error::SampleRateInexact`].
    pub fn new(sample_rate: SampleRate) -> Result<Self, M17Error> {
        let sps = checked_sps(sample_rate)?;
        let mut f = [0.0f64; MAX_TAPS];
        let ntaps = design_rrc(sps, &mut f);
        let mut taps = [0i32; MAX_TAPS];
        for (q, &h) in taps.iter_mut().zip(f.iter()).take(ntaps) {
            *q = (h * 8_192.0 + if h >= 0.0 { 0.5 } else { -0.5 }) as i32;
        }
        Ok(Self {
            taps,
            ntaps,
            sps,
            delay: [0; MAX_TAPS],
            dpos: 0,
            hist: [0; MAX_TAPS],
            hpos: 0,
            state: RxState::Hunt,
            best_corr: 0,
            best_sync: 0,
            best_age: 0,
            confirm_left: 0,
            unit: 0,
            countdown: 0,
            expect_sync: false,
            sync_acc: 0,
            sync_count: 0,
            frame_bits: [0; FRAME_BYTES],
            nsyms: 0,
            frame_sync: 0,
        })
    }

    /// Filtered sample `back` samples ago (0 = newest).
    #[inline]
    fn hist_at(&self, back: usize) -> i32 {
        let len = self.hist.len();
        self.hist[(self.hpos + len - 1 - back) % len]
    }

    /// Correlation of the last 8 symbol centers against `sync`'s
    /// symbols. Returns `(corr, plausible)`: `plausible` demands the
    /// sign of every center match the sync symbol, every center carry
    /// comparable energy (min ≥ max/2 — sync symbols are all ±3 and
    /// the combined TX+RX RRC is a raised cosine, so center ISI is
    /// negligible) and a silence floor. The energy-uniformity test is
    /// what rejects the filter ramp-in during the preamble, where a
    /// mostly-empty history can correlate deceptively well.
    ///
    /// The silence floor is the fixed `max_mag > 500` below, an
    /// acquisition squelch with no AGC behind it: measured against the
    /// modulator's nominal ~29 337 peak, that is ≈ 43 dB of usable
    /// range before acquisition stops entirely (see [`M17Receiver`]).
    fn sync_correlate(&self, sync: u16) -> (i64, bool) {
        let syms = sync_symbols(sync);
        let mut corr: i64 = 0;
        let mut min_mag = i64::MAX;
        let mut max_mag = 0i64;
        let mut signs_ok = true;
        for (k, &s) in syms.iter().enumerate() {
            let y = i64::from(self.hist_at((7 - k) * self.sps));
            corr += i64::from(s) * y;
            let mag = y.abs();
            min_mag = min_mag.min(mag);
            max_mag = max_mag.max(mag);
            if (s > 0) != (y > 0) {
                signs_ok = false;
            }
        }
        let plausible = signs_ok && min_mag * 2 >= max_mag && max_mag > 500;
        (corr, plausible)
    }

    /// Pushes one PCM sample; returns a deframed event when a frame
    /// completes and survives FEC (+ CRC for LSFs).
    pub fn push_i16(&mut self, sample: i16) -> Option<M17FrameEvent> {
        // Matched filter.
        self.delay[self.dpos] = sample;
        self.dpos = (self.dpos + 1) % self.ntaps;
        let mut acc: i64 = 0;
        for i in 0..self.ntaps {
            let x = self.delay[(self.dpos + i) % self.ntaps];
            acc += i64::from(x) * i64::from(self.taps[self.ntaps - 1 - i]);
        }
        let y = (acc >> 13).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let hlen = self.hist.len();
        self.hist[self.hpos] = y;
        self.hpos = (self.hpos + 1) % hlen;

        match self.state {
            RxState::Hunt | RxState::Confirm => {
                let (c_lsf, ok_lsf) = self.sync_correlate(SYNC_LSF);
                let (c_pkt, ok_pkt) = self.sync_correlate(SYNC_PACKET);
                let (corr, sync, strong) = if ok_lsf && (!ok_pkt || c_lsf >= c_pkt) {
                    (c_lsf, SYNC_LSF, true)
                } else if ok_pkt {
                    (c_pkt, SYNC_PACKET, true)
                } else {
                    (0, SYNC_LSF, false)
                };
                match self.state {
                    RxState::Hunt if strong => {
                        self.state = RxState::Confirm;
                        self.best_corr = corr;
                        self.best_sync = sync;
                        self.best_age = 0;
                        self.confirm_left = self.sps;
                    }
                    RxState::Confirm => {
                        self.best_age += 1;
                        if strong && corr > self.best_corr {
                            self.best_corr = corr;
                            self.best_sync = sync;
                            self.best_age = 0;
                        }
                        self.confirm_left -= 1;
                        if self.confirm_left == 0 {
                            // Last sync symbol centered `best_age` ago;
                            // the first payload symbol follows one
                            // period later.
                            self.unit = (self.best_corr / (8 * 9)).max(1) as i32;
                            self.countdown = self.sps.saturating_sub(self.best_age).max(1);
                            self.frame_sync = self.best_sync;
                            self.frame_bits = [0; FRAME_BYTES];
                            self.nsyms = 0;
                            self.expect_sync = false;
                            self.state = RxState::Collect;
                        }
                    }
                    _ => {}
                }
                None
            }
            RxState::Collect => {
                self.countdown -= 1;
                if self.countdown > 0 {
                    return None;
                }
                self.countdown = self.sps;
                let t2 = 2 * self.unit;
                let symbol: i8 = if y > t2 {
                    3
                } else if y > 0 {
                    1
                } else if y > -t2 {
                    -1
                } else {
                    -3
                };
                let dibit = symbol_to_dibit(symbol);
                if self.expect_sync {
                    // Lockstep continuation: within one burst frames
                    // are contiguous, so the next frame's sync burst
                    // occupies exactly the next 8 symbols — read it
                    // here instead of re-hunting (which could false-
                    // trigger on the frame boundary). Tolerate up to
                    // 3 flipped sync bits.
                    self.sync_acc = (self.sync_acc << 2) | u16::from(dibit);
                    self.sync_count += 1;
                    if self.sync_count < 8 {
                        return None;
                    }
                    self.expect_sync = false;
                    self.sync_count = 0;
                    let d_lsf = (self.sync_acc ^ SYNC_LSF).count_ones();
                    let d_pkt = (self.sync_acc ^ SYNC_PACKET).count_ones();
                    if d_lsf <= 3 && d_lsf <= d_pkt {
                        self.frame_sync = SYNC_LSF;
                    } else if d_pkt <= 3 {
                        self.frame_sync = SYNC_PACKET;
                    } else {
                        // EOT marker or carrier drop: burst over.
                        self.state = RxState::Hunt;
                        return None;
                    }
                    self.frame_bits = [0; FRAME_BYTES];
                    self.nsyms = 0;
                    return None;
                }
                let idx = self.nsyms * 2;
                set_bit(&mut self.frame_bits, idx, (dibit >> 1) & 1);
                set_bit(&mut self.frame_bits, idx + 1, dibit & 1);
                self.nsyms += 1;
                if self.nsyms < FRAME_SYMBOLS - 8 {
                    return None;
                }
                // Frame complete — stay in lockstep for the next one.
                self.expect_sync = true;
                self.sync_acc = 0;
                let bits = self.frame_bits;
                if self.frame_sync == SYNC_LSF {
                    lsf_decode(&bits).ok().map(M17FrameEvent::Lsf)
                } else {
                    let (frame, _metric) = packet_frame_decode(&bits);
                    Some(M17FrameEvent::PacketFrame(frame))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Packet superframe assembly
// ---------------------------------------------------------------------------

/// Reassembles packet superframes from deframed [`PacketFrame`]s and
/// verifies the packet CRC-16 (M17 spec, Packet Superframes: the CRC is
/// appended to the payload before chunking).
///
/// Fixed 825-byte buffer; no allocation. Frames with out-of-sequence
/// counters reset the assembly (a fresh [`PacketAssembler::start`] with
/// the next LSF recovers).
#[derive(Debug, Clone)]
pub struct PacketAssembler {
    buf: [u8; 33 * PACKET_FRAME_PAYLOAD],
    len: usize,
    frames: usize,
    active: bool,
    lsf: Option<Lsf>,
}

impl Default for PacketAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketAssembler {
    /// Creates an idle assembler.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; 825],
            len: 0,
            frames: 0,
            active: false,
            lsf: None,
        }
    }

    /// Begins a new superframe (called on each received LSF).
    pub fn start(&mut self, lsf: Lsf) {
        self.len = 0;
        self.frames = 0;
        self.active = true;
        self.lsf = Some(lsf);
    }

    /// The LSF that opened the current/most recent superframe.
    #[must_use]
    pub const fn lsf(&self) -> Option<Lsf> {
        self.lsf
    }

    /// Feeds one deframed packet frame. Returns the complete, CRC-
    /// verified payload (without the trailing CRC) when the EOF frame
    /// lands and checks out.
    pub fn feed(&mut self, frame: &PacketFrame) -> Option<&[u8]> {
        if !self.active {
            return None;
        }
        if frame.eof {
            let take = usize::from(frame.counter).clamp(1, PACKET_FRAME_PAYLOAD);
            if self.len + take > self.buf.len() {
                self.active = false;
                return None;
            }
            self.buf[self.len..self.len + take].copy_from_slice(&frame.data[..take]);
            let total = self.len + take;
            self.active = false;
            if total < 2 {
                return None;
            }
            let payload_len = total - 2;
            let want = u16::from_be_bytes([self.buf[payload_len], self.buf[payload_len + 1]]);
            if crc16(&self.buf[..payload_len]) == want {
                Some(&self.buf[..payload_len])
            } else {
                None
            }
        } else {
            if usize::from(frame.counter) != self.frames % 32
                || self.len + PACKET_FRAME_PAYLOAD >= self.buf.len()
            {
                self.active = false;
                return None;
            }
            self.buf[self.len..self.len + PACKET_FRAME_PAYLOAD].copy_from_slice(&frame.data);
            self.len += PACKET_FRAME_PAYLOAD;
            self.frames += 1;
            None
        }
    }
}
