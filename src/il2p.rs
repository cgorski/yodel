//! IL2P (Improved Layer 2 Protocol) frame codec: header translation,
//! LFSR scrambling, and per-block Reed-Solomon FEC.
//!
//! # What IL2P is
//!
//! IL2P, published by Nino Carrillo (KK4HEJ), is a modern replacement
//! for AX.25's HDLC line coding. Where FX.25 *wraps* an unmodified HDLC
//! frame in a Reed-Solomon codeblock (backward compatible, but the
//! embedded frame still relies on fragile bit stuffing), IL2P replaces
//! the framing wholesale:
//!
//! * no HDLC flags or bit stuffing — frames are located by a fixed
//!   3-byte **sync word** `0xF1 0x5E 0x48` ([`SYNC_WORD`]) after a
//!   `0x55` preamble ([`PREAMBLE_BYTE`]);
//! * a compact 13-byte **header** ([`HEADER_LEN`]) that either
//!   *translates* an AX.25 UI frame (type 1: SIXBIT-packed callsigns,
//!   SSIDs, a PID code and the payload byte count) or *transparently*
//!   carries any AX.25 frame as payload (type 0), protected by its own
//!   2 Reed-Solomon parity symbols ([`HEADER_PARITY_LEN`]);
//! * the payload split into blocks of at most [`MAX_BLOCK_DATA`] = 239
//!   bytes, each with its own 16 symbols of RS parity ([`Il2pParity`]);
//! * a multiplicative **LFSR scrambler** (`x^9 + x^4 + 1`,
//!   [`Il2pScrambler`]) whitening header and payload bytes (parity
//!   symbols are transmitted unscrambled).
//!
//! Use IL2P instead of FX.25 when both ends speak it: it spends less
//! overhead for the same protection, its frame length never varies with
//! payload contents (no stuffing), and every transmitted byte is FEC
//! protected. Use FX.25 when legacy AX.25-only receivers must still
//! copy the traffic.
//!
//! # Wire format and bit order
//!
//! ```text
//! preamble (0x55 ..) ‖ sync 0xF1 0x5E 0x48 ‖ scrambled header (13)
//!   ‖ header RS parity (2) ‖ [ scrambled block ‖ block parity ] ..
//! ```
//!
//! IL2P transmits each byte **most-significant bit first** (unlike
//! AX.25's LSB-first order), with NRZI line coding and **no bit
//! stuffing**. [`tx_bits`] serializes an encoded frame (preamble, frame
//! bytes, trailer) in that order for the modulator, and
//! [`Il2pReceiver`] consumes the post-NRZI-decode bit stream on
//! receive, hunting for the 24-bit sync word (within
//! [`SYNC_TOLERANCE`] bit errors) and byte-accumulating the frame the
//! header announces.
//!
//! The Reed-Solomon codes are the shortened `RS(255, k)` family of
//! [`crate::rs`] over `GF(256)` (field polynomial `0x11D`) with first
//! consecutive root `a^0` — IL2P's convention, versus FX.25's `a^1`.
//!
//! # Spec-parameter notes
//!
//! Implemented from the published IL2P specification (Draft v0.6 — see
//! the version note below). Parameters, each kept as a single named
//! constant so a correction is a one-line change:
//!
//! * scrambler polynomial `x^9 + x^4 + 1`, register preset
//!   [`SCRAMBLER_SEED`] (`0x1F0`) at the start of every scrambled unit
//!   (header and each payload block are scrambled independently);
//! * header bit map: see [`Il2pHeader`] — byte 1 bit 7 is the header
//!   type, bit 7 of bytes 2..=11 the 10-bit payload count (MSB first),
//!   byte 0 bit 6 the UI flag, bit 6 of bytes 1..=4 the 4-bit PID code,
//!   byte 12 the destination (high nibble) and source (low nibble)
//!   SSIDs;
//! * maximum payload [`PAYLOAD_MAX`] = 1023 bytes, blocks of at most
//!   [`MAX_BLOCK_DATA`] = 239 data bytes, split as evenly as possible
//!   (the legacy baseline FEC level divides by
//!   [`MAX_BASELINE_BLOCK_DATA`] = 247 instead — see [`block_count_for`],
//!   which is the only correct way to ask);
//! * payload parity fixed at 16 symbols per block ([`Il2pParity`]; the
//!   smaller operating points are v0.4 legacy and do not interoperate).
//!
//! # Conformance
//!
//! This module implements **IL2P Specification Draft v0.6** (16 March
//! 2024). The wire-format constants that a peer must agree on — the
//! scrambler preset, the PID code table, the UI control subfield and
//! the payload block divisor — are pinned by the specification's own
//! "Example Encoded Packets" verification vectors, exercised in
//! `tests/il2p.rs` as the `spec_v06_*` tests.
//!
//! Those vectors are load-bearing. This module previously implemented
//! v0.4 and could not exchange a frame with any other station, while
//! its round-trip tests all passed — an encoder and decoder that are
//! mutual inverses stay mutual inverses when a shared constant is
//! wrong. Do not change a wire constant here without re-running them.
//!
//! Not implemented: the **optional** Trailing CRC (v0.6 states its use
//! "must be coordinated between participating stations"; it is not a
//! default and the reference implementation omits it).
//!
//! Encoder and decoder in this module
//! are exact inverses regardless.
//!
//! # Round trip
//!
//! ```
//! use warble::ax25::{Address, UiFrame};
//! use warble::il2p::{self, Il2pParity, ENCODED_MAX, SYNC_LEN};
//!
//! let frame = UiFrame::new(
//!     Address::new(b"APRS", 0)?,
//!     Address::new(b"N0CALL", 7)?,
//!     b">IL2P test",
//! );
//! let mut tx = [0u8; ENCODED_MAX];
//! let len = il2p::encode_ui_frame(&frame, Il2pParity::Sixteen, &mut tx)?;
//!
//! // Receive side: bytes after the sync word.
//! let mut payload = [0u8; il2p::PAYLOAD_MAX];
//! let decoded = il2p::decode(&tx[SYNC_LEN..len], Il2pParity::Sixteen, &mut payload)?;
//! let back = il2p::to_ui_frame(&decoded.header, &payload[..decoded.payload_len])?;
//! assert_eq!(back, frame);
//! assert_eq!(decoded.corrected(), 0); // clean channel
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

use core::fmt;

use crate::ax25::{Address, Ax25Error, UiFrame};
use crate::rs::{RsCodec, RsError, RsParity};
use crate::types::Bit;

/// The IL2P sync word, transmitted MSB-first right after the preamble.
pub const SYNC_WORD: u32 = 0xF1_5E48;

/// The sync word as on-air bytes.
pub const SYNC_BYTES: [u8; 3] = [0xF1, 0x5E, 0x48];

/// Length of the sync word in bytes.
pub const SYNC_LEN: usize = 3;

/// Maximum Hamming distance at which [`Il2pReceiver`] accepts a sync
/// word match. One bit error is the common practice for IL2P sync
/// hunting: the 24-bit word is long enough that a 1-bit tolerance
/// false-locks on random noise only ≈ 25/2²⁴ ≈ 1.5·10⁻⁶ per bit, and a
/// false lock is harmless (the header FEC rejects garbage).
pub const SYNC_TOLERANCE: u32 = 1;

/// The preamble byte sent (repeatedly) before the sync word: `0x55`
/// gives an alternating bit pattern MSB-first for clock recovery.
pub const PREAMBLE_BYTE: u8 = 0x55;

/// Length of the IL2P header in bytes (before its parity).
pub const HEADER_LEN: usize = 13;

/// Reed-Solomon parity symbols protecting the header (correcting one
/// symbol error anywhere in the 15-byte header codeblock).
pub const HEADER_PARITY_LEN: usize = 2;

/// Largest payload an IL2P frame can carry: the header's byte count
/// field is 10 bits.
pub const PAYLOAD_MAX: usize = 1023;

/// Largest number of payload data bytes per Reed-Solomon block; a
/// maximum-size payload therefore uses `ceil(1023 / 239) = 5` blocks.
///
/// Spec v0.6: "payload_block_count = Ceiling(payload_byte_count /
/// 239)". Earlier drafts used a smaller divisor together with a
/// selectable parity length; v0.6 fixes parity at 16 symbols per block
/// and the divisor at 239.
pub const MAX_BLOCK_DATA: usize = 239;

/// Maximum payload data bytes in one Reed-Solomon block at the legacy
/// **baseline** FEC level, where at most 8 parity symbols are appended
/// rather than 16.
///
/// Draft v0.6 removed baseline FEC, but deployed stations still
/// transmit it (a receiver is told which plan is in use by the header's
/// FEC-level bit), so the receive path has to understand both. See
/// [`Il2pParity::baseline_for_block`].
pub const MAX_BASELINE_BLOCK_DATA: usize = 247;

/// Scrambler register preset applied at the start of every scrambled
/// unit (the header and each payload block): all ones.
///
/// Spec v0.6 draws the scrambler in **Galois** configuration with an
/// explicit five-bit pipeline delay, whose output is "taken after its
/// bit delay has elapsed (5 bits in this case), and flushed at the end
/// of the data block". Propagating the drawn initial register contents
/// through that delay leaves nine ones of history, which is the preset
/// this crate's Fibonacci-form implementation needs. Transcribing the
/// schematic's literal left-to-right contents instead (as this crate
/// did through v0.4) yields `0x1F0` and corrupts every byte.
///
/// Pinned by the published verification vectors in `tests/il2p.rs`;
/// do not change it without them.
pub const SCRAMBLER_SEED: u16 = 0x1FF;

/// Worst-case encoded frame length produced by the encoders here:
/// sync + header + header parity + max payload + 5 blocks × 16 parity.
pub const ENCODED_MAX: usize = SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN + PAYLOAD_MAX + 5 * 16;

/// Largest byte count [`Il2pReceiver`] collects after the sync word:
/// header codeblock plus a maximum frame's payload and block parity.
pub const RX_FRAME_MAX: usize = ENCODED_MAX - SYNC_LEN;

/// The 4-bit IL2P PID code ↔ AX.25 PID byte table, from the "IL2P
/// AX.25 PID Code Mapping" table of spec v0.6.
///
/// Codes `0x0` and `0x1` identify frames that carry **no** PID byte
/// (supervisory, and unnumbered other than UI), so they have no AX.25
/// PID to map and are absent from this table. Codes `0x7`..=`0xA` are
/// marked Future by the spec and are likewise absent. Only
/// [`PID_CODE_NO_LAYER3`] is exercised by the UI-frame translation in
/// this crate.
///
/// Pinned by the published verification vectors in `tests/il2p.rs`.
pub const PID_TABLE: [(u8, u8); 10] = [
    (0x2, 0x20), // AX.25 layer 3 (yy10yyyy / yy01yyyy)
    (0x3, 0x01), // ISO 8208 / CCITT X.25 PLP
    (0x4, 0x06), // Compressed TCP/IP
    (0x5, 0x07), // Uncompressed TCP/IP
    (0x6, 0x08), // Segmentation fragment
    (0xB, 0xCC), // ARPA Internet Protocol
    (0xC, 0xCD), // ARPA Address Resolution
    (0xD, 0xCE), // FlexNet
    (0xE, 0xCF), // TheNET
    (0xF, 0xF0), // No layer 3
];

/// The PID code for AX.25 PID `0xF0` (no layer 3), the value used by
/// every UI frame this crate builds.
pub const PID_CODE_NO_LAYER3: u8 = 0xF;

/// The UI opcode within the IL2P control subfield, with the P/F and C
/// bits clear — the value **receive** compares against.
///
/// Spec v0.6's U-frame control map is `bit 6 = P/F`, `bits 5..=3 =
/// OPCODE`, `bit 2 = C`, `bits 1..=0` unused; UI is opcode `0b101`,
/// giving `0b0101000`. Receive masks P/F and C away before comparing
/// (see [`CONTROL_UI_OPCODE_MASK`]), so a peer's choice of either does
/// not cause a rejection.
///
/// Kept **separate** from [`CONTROL_UI_COMMAND`], the value
/// transmitted. One constant serving both roles is what previously
/// made the transmitted C bit impossible to correct without breaking
/// our own receive path.
pub const CONTROL_UI_OPCODE: u8 = 0b010_1000;

/// The IL2P control subfield this crate **transmits** for a translated
/// UI frame: the UI opcode with the Command bit set.
///
/// IL2P compresses AX.25's command/response indication — which AX.25
/// spreads across the C bits of the destination and source SSID octets
/// — into this single bit, copied from the **destination** address's C
/// bit. [`UiFrame::build`](crate::ax25::UiFrame::build) writes that bit
/// set and the source's clear for every frame it produces, so every UI
/// frame this crate can translate is a command and the bit is
/// constant here rather than derived.
///
/// The mapping is inherently lossy in the other direction: four AX.25
/// C-bit combinations collapse onto one IL2P bit, so the two legacy
/// "both bits equal" cases cannot round-trip through IL2P at all.
pub const CONTROL_UI_COMMAND: u8 = 0b010_1100;

/// Mask selecting the U-frame control subfield's opcode bits (5..=3).
///
/// Receive compares `control & CONTROL_UI_OPCODE_MASK` against
/// [`CONTROL_UI_OPCODE`], so a peer's P/F or C bit does not cause a
/// rejection.
/// Those two bits are **not preserved** through translation: an AX.25
/// UI frame decoded out of IL2P always comes back with P/F and C
/// clear, which is lossless for APRS and lossy for nothing else this
/// crate builds.
pub const CONTROL_UI_OPCODE_MASK: u8 = 0b011_1000;

/// Number of shift-register stages: the degree of `x^9 + x^4 + 1`.
const LFSR_STAGES: u16 = 9;

/// Mask keeping exactly the [`LFSR_STAGES`] register bits.
const LFSR_MASK: u16 = (1 << LFSR_STAGES) - 1;

/// Tap delays, read directly off the polynomial's non-unity terms.
const LFSR_TAP_A: u16 = 4;
const LFSR_TAP_B: u16 = 9;

/// XOR of the two polynomial taps over a shift-register history
/// (newest bit in bit 0, so a delay of `d` lives at bit `d - 1`).
const fn lfsr_taps(state: u16) -> u16 {
    ((state >> (LFSR_TAP_A - 1)) ^ (state >> (LFSR_TAP_B - 1))) & 1
}

/// IL2P multiplicative scrambler/descrambler (`x^9 + x^4 + 1`).
///
/// The transmit direction feeds the register from its own **output**
/// (`out[n] = in[n] ^ out[n-4] ^ out[n-9]`); the receive direction is
/// the self-synchronizing feed-forward inverse tapping the received
/// history (`out[n] = in[n] ^ in[n-4] ^ in[n-9]`). Bytes are processed
/// most-significant bit first, matching IL2P's transmit bit order.
///
/// ```
/// use warble::il2p::Il2pScrambler;
///
/// let mut data = *b"il2p known answer";
/// let original = data;
/// Il2pScrambler::new().scramble(&mut data);
/// assert_ne!(data, original);
/// Il2pScrambler::new().descramble(&mut data);
/// assert_eq!(data, original);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Il2pScrambler {
    /// Last 9 history bits, newest in bit 0.
    state: u16,
}

impl Il2pScrambler {
    /// Creates a scrambler with the register preset to
    /// [`SCRAMBLER_SEED`], the state at the start of every scrambled
    /// unit.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: SCRAMBLER_SEED & LFSR_MASK,
        }
    }

    /// Scrambles `bytes` in place (transmit direction), MSB-first.
    pub const fn scramble(&mut self, bytes: &mut [u8]) {
        let mut i = 0;
        while i < bytes.len() {
            let mut byte = bytes[i];
            let mut k = 8;
            while k > 0 {
                k -= 1;
                let bit = (byte >> k) & 1;
                let out = (bit as u16 ^ lfsr_taps(self.state)) & 1;
                self.state = ((self.state << 1) | out) & LFSR_MASK;
                byte = (byte & !(1 << k)) | ((out as u8) << k);
            }
            bytes[i] = byte;
            i += 1;
        }
    }

    /// Descrambles `bytes` in place (receive direction), MSB-first.
    pub const fn descramble(&mut self, bytes: &mut [u8]) {
        let mut i = 0;
        while i < bytes.len() {
            let mut byte = bytes[i];
            let mut k = 8;
            while k > 0 {
                k -= 1;
                let bit = (byte >> k) & 1;
                let out = (bit as u16 ^ lfsr_taps(self.state)) & 1;
                self.state = ((self.state << 1) | bit as u16) & LFSR_MASK;
                byte = (byte & !(1 << k)) | ((out as u8) << k);
            }
            bytes[i] = byte;
            i += 1;
        }
    }
}

impl Default for Il2pScrambler {
    /// Same as [`Il2pScrambler::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Payload Reed-Solomon parity per block.
///
/// **Only [`Il2pParity::Sixteen`] is interoperable.** Spec v0.6 is
/// unambiguous: "The encoder will always append 16 parity symbols per
/// payload block, regardless of block size." The smaller points are
/// the v0.4 "Baseline FEC" ladder, which v0.6 deleted along with the
/// header bit that used to select it (now RESERVED). Because the
/// parity length is **not signalled on the wire**, any setting other
/// than `Sixteen` is un-negotiable: both ends must be configured
/// identically out of band, and no other implementation will agree.
///
/// They are retained because they are useful for experiments and for
/// links where both ends are yours — the shorter parity trades
/// correction strength (`t = p / 2` symbols per block) for overhead —
/// but do not expect a NinoTNC or any other station to decode them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Il2pParity {
    /// 2 parity symbols per block: corrects 1 symbol error.
    Two,
    /// 4 parity symbols per block: corrects up to 2 symbol errors.
    Four,
    /// 6 parity symbols per block: corrects up to 3 symbol errors.
    Six,
    /// 8 parity symbols per block: corrects up to 4 symbol errors.
    Eight,
    /// 16 parity symbols per block (baseline): corrects up to 8.
    Sixteen,
}

impl Il2pParity {
    /// Every operating point, weakest to strongest.
    pub const ALL: [Self; 5] = [Self::Two, Self::Four, Self::Six, Self::Eight, Self::Sixteen];

    /// The baseline-FEC parity for a payload block of `size` bytes.
    ///
    /// When the header's FEC-level bit is clear, the parity is not
    /// carried on the wire at all — it is derived from the block size,
    /// stepping up every ~62 bytes so that the symbol-error rate a
    /// block can absorb stays roughly constant:
    ///
    /// | small block size | parity symbols |
    /// |---|---|
    /// | `..=61` | 2 |
    /// | `62..=123` | 4 |
    /// | `124..=185` | 6 |
    /// | `186..` | 8 |
    ///
    /// Draft v0.4 also printed a formula, `size / 32 + 2`, which
    /// disagrees with its own table and can yield 3, 5 or 7 — values
    /// the code does not define. The table is what deployed
    /// implementations follow, so it is what this uses.
    #[must_use]
    pub const fn baseline_for_block(size: usize) -> Self {
        if size <= 61 {
            Self::Two
        } else if size <= 123 {
            Self::Four
        } else if size <= 185 {
            Self::Six
        } else {
            Self::Eight
        }
    }

    /// Whether this operating point is "maximum FEC", which is what
    /// header byte 0 bit 7 announces.
    ///
    /// # Why this bit matters more than the specification suggests
    ///
    /// Draft v0.4 defined that bit as the **FEC level**: set means a
    /// constant 16 parity symbols per payload block, clear means the
    /// variable 2/4/6/8-symbol "baseline" scheme, whose block sizes are
    /// derived differently as well. Draft v0.6 deleted baseline FEC,
    /// mandated 16 symbols everywhere, and redefined the bit as
    /// RESERVED.
    ///
    /// Deployed receivers did not follow. They still read the bit and
    /// use it to compute **how many bytes to take off the air** for the
    /// payload. A frame that clears it while carrying 16-symbol parity
    /// tells such a receiver to collect far too few bytes; the block
    /// then fails its RS decode and the whole frame is discarded. A
    /// strictly v0.6-conforming encoder is therefore silently
    /// non-interoperable, which is what this crate was until an
    /// on-air differential caught it.
    ///
    /// So the bit is set for [`Il2pParity::Sixteen`] and clear for the
    /// legacy operating points, which is both what the wire needs and
    /// what v0.4 says.
    #[must_use]
    pub const fn is_max_fec(self) -> bool {
        matches!(self, Self::Sixteen)
    }

    /// The parity length in symbols (bytes) per payload block.
    #[must_use]
    pub const fn len(self) -> usize {
        match self {
            Self::Two => 2,
            Self::Four => 4,
            Self::Six => 6,
            Self::Eight => 8,
            Self::Sixteen => 16,
        }
    }

    /// Always `false`: every operating point carries parity.
    /// (Provided because [`Self::len`] exists.)
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Maximum correctable symbol errors per block, `t = p / 2`.
    #[must_use]
    pub const fn correctable(self) -> usize {
        self.len() / 2
    }

    /// The matching [`crate::rs`] parity selector.
    const fn rs(self) -> RsParity {
        match self {
            Self::Two => RsParity::Two,
            Self::Four => RsParity::Four,
            Self::Six => RsParity::Six,
            Self::Eight => RsParity::Eight,
            Self::Sixteen => RsParity::Sixteen,
        }
    }
}

/// Errors reported by the IL2P codec layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Il2pError {
    /// The payload exceeds [`PAYLOAD_MAX`] bytes.
    PayloadTooLong {
        /// Length of the offending payload.
        got: usize,
        /// The 10-bit ceiling, 1023.
        max: usize,
    },
    /// A caller buffer cannot hold the result.
    BufferTooSmall {
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        got: usize,
    },
    /// The received byte slice is shorter than the frame it announces.
    FrameTooShort {
        /// Bytes received.
        got: usize,
        /// Bytes required (header + parity + payload blocks).
        needed: usize,
    },
    /// The header codeblock had more than one symbol error.
    HeaderUncorrectable,
    /// A payload block exceeded its correction capability.
    BlockUncorrectable {
        /// Zero-based index of the failing block.
        block: usize,
    },
    /// A type 1 header carried a PID code outside [`PID_TABLE`].
    UnsupportedPid {
        /// The rejected 4-bit code.
        got: u8,
    },
    /// A type 1 header described a non-UI frame; UI is the only
    /// translated frame type this crate implements.
    UnsupportedControl {
        /// The header's 7-bit control code.
        got: u8,
    },
    /// The translated or transparent AX.25 content was invalid.
    Ax25(Ax25Error),
    /// The Reed-Solomon layer rejected a codec invocation (a length
    /// contract violation; never a channel-error condition).
    Rs(RsError),
}

impl fmt::Display for Il2pError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Il2pError::PayloadTooLong { got, max } => {
                write!(f, "payload of {got} bytes exceeds IL2P capacity {max}")
            }
            Il2pError::BufferTooSmall { needed, got } => {
                write!(f, "buffer of {got} bytes, need {needed}")
            }
            Il2pError::FrameTooShort { got, needed } => {
                write!(f, "received {got} bytes of an IL2P frame needing {needed}")
            }
            Il2pError::HeaderUncorrectable => {
                write!(f, "IL2P header uncorrectable")
            }
            Il2pError::BlockUncorrectable { block } => {
                write!(f, "IL2P payload block {block} uncorrectable")
            }
            Il2pError::UnsupportedPid { got } => {
                write!(f, "unsupported IL2P PID code {got:#x}")
            }
            Il2pError::UnsupportedControl { got } => {
                write!(f, "unsupported IL2P control code {got:#x} (UI only)")
            }
            Il2pError::Ax25(ref e) => write!(f, "AX.25 layer: {e}"),
            Il2pError::Rs(ref e) => write!(f, "Reed-Solomon layer: {e}"),
        }
    }
}

impl core::error::Error for Il2pError {}

impl From<Ax25Error> for Il2pError {
    fn from(e: Ax25Error) -> Self {
        Il2pError::Ax25(e)
    }
}

impl From<RsError> for Il2pError {
    fn from(e: RsError) -> Self {
        Il2pError::Rs(e)
    }
}

/// A parsed (or to-be-packed) 13-byte IL2P header.
///
/// The bit map over the 13 bytes (spec-parameter note in the
/// [module docs](self)):
///
/// ```text
/// bytes 0..=5  bits 0..=5   destination callsign, SIXBIT (char - 0x20)
/// bytes 6..=11 bits 0..=5   source callsign, SIXBIT
/// byte 1       bit 7        header type: 0 transparent, 1 translated
/// bytes 2..=11 bit 7        payload byte count, 10 bits MSB first
/// byte 0       bit 6        UI flag (type 1)
/// bytes 1..=4  bit 6        PID code, 4 bits MSB first (type 1)
/// bytes 5..=11 bit 6        control code, 7 bits MSB first (type 1)
/// byte 12      bits 4..=7   destination SSID (type 1)
/// byte 12      bits 0..=3   source SSID (type 1)
/// byte 0       bit 7        FEC level: 1 maximum FEC, 0 baseline
/// ```
///
/// Byte 0 bit 7 is what draft v0.6 calls RESERVED and what draft v0.4
/// called the FEC level. [`Il2pHeader::pack`] writes the FEC level there
/// and [`decode`] reads it back, because deployed receivers size the
/// payload from it — see [`Il2pParity::is_max_fec`] for why conforming to
/// v0.6 here is silently non-interoperable. (The specification's own
/// example packets have the bit clear, which is why the `spec_v06_*`
/// vectors in `tests/il2p.rs` are encoded at a baseline operating
/// point.)
///
/// UI is the only translated frame type this crate implements: the
/// packed control subfield is always [`CONTROL_UI_COMMAND`] and a decoded
/// type 1 header must carry the UI flag and the UI opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Il2pHeader {
    /// Type 0, transparent: the payload is a complete AX.25 frame body
    /// (addresses through info field, no FCS and no flags — the form
    /// [`UiFrame::build`] emits and [`UiFrame::parse`] accepts).
    Transparent {
        /// Payload byte count, `0..=1023`.
        payload_len: u16,
    },
    /// Type 1, translated AX.25 UI frame: the payload is the bare
    /// information field.
    Translated {
        /// Destination address.
        dest: Address,
        /// Source address.
        src: Address,
        /// The AX.25 PID byte (mapped through [`PID_TABLE`]).
        pid: u8,
        /// Payload (information field) byte count, `0..=1023`.
        payload_len: u16,
        /// Whether this is an AX.25 **command** (as opposed to a
        /// response), carried in the control subfield's C bit.
        ///
        /// AX.25 spreads this across the C bits of the destination and
        /// source SSID octets — destination set with source clear means
        /// command, the reverse means response. IL2P compresses it to
        /// one bit, copied from the destination's.
        ///
        /// It lives on the header rather than being derived because it
        /// is part of what the header encodes, and because this crate's
        /// [`UiFrame`] does not model it: `UiFrame::build` always writes
        /// the command encoding, and `UiFrame::parse` discards which it
        /// saw. So a frame translated from a `UiFrame` is a command by
        /// construction, while a frame decoded off the air may be
        /// either, and only an explicit field can represent both.
        ///
        /// The mapping is lossy in the AX.25 direction: four C-bit
        /// combinations collapse onto this one bit, so the two legacy
        /// "both equal" cases cannot round-trip.
        command: bool,
    },
}

impl Il2pHeader {
    /// The payload byte count this header announces.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        match *self {
            Il2pHeader::Transparent { payload_len }
            | Il2pHeader::Translated { payload_len, .. } => payload_len as usize,
        }
    }

    /// Packs the header into its 13 unscrambled wire bytes.
    ///
    /// `max_fec` sets bit 7 of byte 0, and **must** agree with the
    /// parity applied to the payload blocks — see
    /// [`Il2pParity::is_max_fec`]. Take it from the same
    /// [`Il2pParity`] value the payload is encoded with;
    /// [`encode`] does exactly that, so callers using it cannot get
    /// the two out of step.
    ///
    /// # Errors
    ///
    /// [`Il2pError::UnsupportedPid`] when a translated header's PID has
    /// no [`PID_TABLE`] code.
    pub fn pack(&self, max_fec: bool) -> Result<[u8; HEADER_LEN], Il2pError> {
        let mut h = [0u8; HEADER_LEN];
        // Bit 7 of byte 0: the FEC level. Applies to both header types.
        if max_fec {
            h[0] |= 0x80;
        }
        let count = self.payload_len() as u16;
        // Payload count: bit 7 of bytes 2..=11, MSB first.
        for (k, slot) in h.iter_mut().enumerate().skip(2).take(10) {
            if (count >> (11 - k)) & 1 != 0 {
                *slot |= 0x80;
            }
        }
        match *self {
            Il2pHeader::Transparent { .. } => {}
            Il2pHeader::Translated {
                dest,
                src,
                pid,
                command,
                ..
            } => {
                h[1] |= 0x80; // header type 1
                h[0] |= 0x40; // UI flag (only UI is translated here)
                let code = pid_to_code(pid).ok_or(Il2pError::UnsupportedPid { got: pid })?;
                for (k, slot) in h.iter_mut().enumerate().skip(1).take(4) {
                    if (code >> (4 - k)) & 1 != 0 {
                        *slot |= 0x40;
                    }
                }
                // Control subfield (bytes 5..=11 bit 6), MSB first: the
                // UI opcode with P/F clear and C from the header.
                let control = if command {
                    CONTROL_UI_COMMAND
                } else {
                    CONTROL_UI_OPCODE
                };
                for (k, slot) in h.iter_mut().enumerate().skip(5).take(7) {
                    if (control >> (11 - k)) & 1 != 0 {
                        *slot |= 0x40;
                    }
                }
                pack_callsign(&dest, &mut h, 0);
                pack_callsign(&src, &mut h, 6);
                h[12] = (dest.ssid.value() << 4) | src.ssid.value();
            }
        }
        Ok(h)
    }

    /// Unpacks 13 unscrambled header bytes.
    ///
    /// # Errors
    ///
    /// [`Il2pError::UnsupportedControl`] on a type 1 header without the
    /// UI flag or whose control subfield is not the UI opcode (the P/F
    /// and C bits are ignored);
    /// [`Il2pError::UnsupportedPid`] on an unknown PID code;
    /// [`Il2pError::Ax25`] on an invalid SIXBIT callsign.
    pub fn unpack(h: &[u8; HEADER_LEN]) -> Result<Self, Il2pError> {
        let mut count = 0u16;
        for (k, &byte) in h.iter().enumerate().skip(2).take(10) {
            count = (count << 1) | u16::from(byte >> 7);
            let _ = k;
        }
        if h[1] & 0x80 == 0 {
            return Ok(Il2pHeader::Transparent { payload_len: count });
        }
        let ui = h[0] & 0x40 != 0;
        let mut code = 0u8;
        for &byte in h.iter().skip(1).take(4) {
            code = (code << 1) | ((byte >> 6) & 1);
        }
        let mut control = 0u8;
        for &byte in h.iter().skip(5).take(7) {
            control = (control << 1) | ((byte >> 6) & 1);
        }
        // Accept any P/F and C; only the opcode must say UI.
        if !ui || control & CONTROL_UI_OPCODE_MASK != CONTROL_UI_OPCODE {
            return Err(Il2pError::UnsupportedControl { got: control });
        }
        let pid = code_to_pid(code).ok_or(Il2pError::UnsupportedPid { got: code })?;
        let dest_call = unpack_callsign(h, 0)?;
        let src_call = unpack_callsign(h, 6)?;
        let dest = Address::new(dest_call.text(), h[12] >> 4)?;
        let src = Address::new(src_call.text(), h[12] & 0x0F)?;
        Ok(Il2pHeader::Translated {
            // C is control bit 2; see `CONTROL_UI_COMMAND`.
            command: control & 0b100 != 0,
            dest,
            src,
            pid,
            payload_len: count,
        })
    }
}

/// A SIXBIT-decoded callsign: up to six significant characters.
struct SixbitCall {
    chars: [u8; 6],
    len: usize,
}

impl SixbitCall {
    fn text(&self) -> &[u8] {
        self.chars.get(..self.len).unwrap_or(&[])
    }
}

/// Packs a callsign into bits 0..=5 of six header bytes starting at
/// `at`, SIXBIT-encoded (ASCII − 0x20, space padded).
fn pack_callsign(addr: &Address, h: &mut [u8; HEADER_LEN], at: usize) {
    let text = addr.callsign.as_bytes();
    for k in 0..6 {
        let c = text.get(k).copied().unwrap_or(b' ');
        if let Some(slot) = h.get_mut(at + k) {
            *slot |= (c - 0x20) & 0x3F;
        }
    }
}

/// Unpacks a SIXBIT callsign from six header bytes starting at `at`,
/// trimming trailing spaces.
fn unpack_callsign(h: &[u8; HEADER_LEN], at: usize) -> Result<SixbitCall, Il2pError> {
    let mut chars = [b' '; 6];
    for (k, slot) in chars.iter_mut().enumerate() {
        *slot = (h.get(at + k).copied().unwrap_or(0) & 0x3F) + 0x20;
    }
    let mut len = 6;
    while len > 0 && chars[len - 1] == b' ' {
        len -= 1;
    }
    if len == 0 {
        return Err(Il2pError::Ax25(Ax25Error::CallsignLengthInvalid { got: 0 }));
    }
    // Plausibility, not conformance. The header carries only 2 parity
    // symbols, so it corrects one symbol error and has almost no
    // ability to *detect* two: at that code rate nearly every syndrome
    // pair is consistent with some single error, so a damaged header is
    // "corrected" into a different valid codeword rather than rejected.
    // The result is a frame that was never transmitted.
    //
    // AX.25 callsigns are alphanumeric by definition, while SIXBIT can
    // carry any of 64 characters, so requiring `A-Z`/`0-9` here costs
    // nothing legitimate and throws away most fabrications. This is the
    // same axis as `tests/false_positives.rs` -- specificity, not
    // strictness -- and so does not conflict with the crate's
    // preserve-on-receive rule, which is about accepting unusual but
    // real traffic.
    let mut i = 0;
    while i < len {
        let c = chars[i];
        if !c.is_ascii_uppercase() && !c.is_ascii_digit() {
            return Err(Il2pError::Ax25(Ax25Error::InvalidCallsignChar { got: c }));
        }
        i += 1;
    }
    Ok(SixbitCall { chars, len })
}

/// The AX.25 PID byte for a 4-bit IL2P code.
fn code_to_pid(code: u8) -> Option<u8> {
    PID_TABLE
        .iter()
        .find(|&&(c, _)| c == code)
        .map(|&(_, pid)| pid)
}

/// The 4-bit IL2P code for an AX.25 PID byte.
fn pid_to_code(pid: u8) -> Option<u8> {
    PID_TABLE.iter().find(|&&(_, p)| p == pid).map(|&(c, _)| c)
}

/// Number of payload Reed-Solomon blocks, for either FEC level.
///
/// The per-block data ceiling differs between the two plans because the
/// parity has to fit the same 255-symbol code block: 239 data bytes
/// alongside 16 parity symbols ([`MAX_BLOCK_DATA`]), or 247 alongside at
/// most 8 ([`MAX_BASELINE_BLOCK_DATA`]).
///
/// `max_fec` is therefore not a detail: the two divisors disagree for 80
/// of the 1024 legal payload lengths (`240..=247`, `479..=494`,
/// `718..=741`, `957..=988`), and everything downstream — how many
/// blocks, how large each is, how many parity symbols each carries, and
/// hence the total frame length — follows from the answer. There is no
/// `max_fec`-less convenience wrapper: one existed, the encoder reached
/// for it while the rest of the module used this function, and the two
/// split the same payload differently. Pass the FEC level explicitly,
/// from [`Il2pParity::is_max_fec`] on the transmit side or from the
/// header's bit 7 on receive.
#[must_use]
pub const fn block_count_for(payload_len: usize, max_fec: bool) -> usize {
    if payload_len == 0 {
        0
    } else if max_fec {
        payload_len.div_ceil(MAX_BLOCK_DATA)
    } else {
        payload_len.div_ceil(MAX_BASELINE_BLOCK_DATA)
    }
}

/// The on-air payload length a receiver must collect: the data bytes
/// plus one parity group per block.
#[must_use]
pub const fn payload_wire_len(payload_len: usize, max_fec: bool) -> usize {
    let blocks = block_count_for(payload_len, max_fec);
    payload_len + blocks * payload_parity(payload_len, max_fec).len()
}

/// The parity applied to every block of a payload, given the FEC level
/// the header announces.
///
/// At maximum FEC this is always 16. At the legacy baseline level it is
/// chosen from the **small** block size, so all blocks in one frame
/// share it even though the large blocks are a byte bigger.
#[must_use]
pub const fn payload_parity(payload_len: usize, max_fec: bool) -> Il2pParity {
    if max_fec {
        return Il2pParity::Sixteen;
    }
    let blocks = block_count_for(payload_len, false);
    if blocks == 0 {
        return Il2pParity::Two;
    }
    Il2pParity::baseline_for_block(payload_len / blocks)
}

/// Total encoded frame length (sync word included) for a payload
/// length at an operating point.
#[must_use]
pub const fn encoded_len(payload_len: usize, parity: Il2pParity) -> usize {
    SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN + payload_wire_len(payload_len, parity.is_max_fec())
}

/// The header Reed-Solomon codec (2 parity symbols, IL2P root
/// convention `fcr = 0`).
fn header_codec() -> RsCodec {
    RsCodec::with_fcr(RsParity::Two, 0)
}

/// The payload-block codec for an operating point.
fn block_codec(parity: Il2pParity) -> RsCodec {
    RsCodec::with_fcr(parity.rs(), 0)
}

/// Encodes an IL2P frame — sync word, scrambled+FEC header, scrambled
/// payload blocks with per-block parity — into `out`, returning the
/// total length.
///
/// **Caller obligation, checked only in debug builds:** `header` must
/// announce exactly `payload.len()` bytes. The agreement is enforced by
/// a `debug_assert_eq!`, so a debug build panics but a **release build
/// silently emits a malformed frame** and returns `Ok`. A header saying
/// 2 with a 5-byte payload returns `Ok(39)`; a header saying 2000 with
/// a 5-byte payload also returns `Ok(39)`, with the count truncated to
/// the field's 10 bits (`2000 & 0x3FF == 976`), and the peer rejects it
/// as [`Il2pError::FrameTooShort`] `{ got: 36, needed: 1071 }`. Prefer
/// the [`encode_ui_frame`] / [`encode_raw`] conveniences, which build
/// the header for you and cannot get this wrong.
///
/// `parity` selects the FEC *level*, which **is** signalled on the wire:
/// [`Il2pHeader::pack`] records it in header byte 0 bit 7 and [`decode`]
/// reads it back, so a receiver does not have to be told. At the legacy
/// baseline level the symbol count itself is not transmitted; it is a
/// function of the block size, so any of [`Il2pParity::Two`] ..=
/// [`Il2pParity::Eight`] requests the baseline plan and the count the
/// block size implies (see [`payload_parity`]).
///
/// # Errors
///
/// [`Il2pError::PayloadTooLong`] beyond [`PAYLOAD_MAX`];
/// [`Il2pError::BufferTooSmall`] when `out` cannot hold the frame;
/// header packing errors from [`Il2pHeader::pack`].
///
/// # Panics
///
/// In debug builds only, when `header.payload_len() != payload.len()`.
pub fn encode(
    header: &Il2pHeader,
    payload: &[u8],
    parity: Il2pParity,
    out: &mut [u8],
) -> Result<usize, Il2pError> {
    if payload.len() > PAYLOAD_MAX {
        return Err(Il2pError::PayloadTooLong {
            got: payload.len(),
            max: PAYLOAD_MAX,
        });
    }
    debug_assert_eq!(header.payload_len(), payload.len());
    let total = encoded_len(payload.len(), parity);
    if out.len() < total {
        return Err(Il2pError::BufferTooSmall {
            needed: total,
            got: out.len(),
        });
    }
    let mut pos = 0usize;
    let put = |bytes: &[u8], pos: &mut usize, out: &mut [u8]| {
        if let Some(slot) = out.get_mut(*pos..*pos + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
        *pos += bytes.len();
    };
    put(&SYNC_BYTES, &mut pos, out);

    // Header: pack, scramble, then RS parity over the scrambled bytes.
    // The FEC level comes from the same `parity` the payload blocks are
    // built with, so the header cannot advertise one plan while the
    // payload uses another -- the defect that made this crate's IL2P
    // transmissions undecodable by every other implementation.
    // The parity argument selects the FEC *level*, not a free symbol
    // count. At maximum FEC every block gets 16; at the legacy baseline
    // level the count is a function of the block size, which the
    // receiver recomputes from the payload length -- it is never sent.
    // Honouring a caller's differing baseline value would therefore
    // produce a frame nobody, including this crate, could decode, so
    // the wire's own rule wins.
    let max_fec = parity.is_max_fec();
    let parity = payload_parity(payload.len(), max_fec);
    let mut h = header.pack(max_fec)?;
    // Self-check, at the source. A receiver sizes its payload blocks
    // from this one bit and commits to that length the moment the
    // header decodes, so it must agree with the parity applied below.
    // Their disagreement is what made this crate's IL2P transmissions
    // undecodable by every other implementation while every internal
    // test passed.
    debug_assert_eq!(
        h[0] & 0x80 != 0,
        max_fec,
        "header FEC level must match the payload parity"
    );
    debug_assert_eq!(
        total,
        SYNC_LEN + HDR_BLOCK + payload_wire_len(payload.len(), max_fec),
        "emitted length must equal what the header tells a receiver to expect"
    );
    Il2pScrambler::new().scramble(&mut h);
    let mut hp = [0u8; HEADER_PARITY_LEN];
    header_codec().encode(&h, &mut hp)?;
    put(&h, &mut pos, out);
    put(&hp, &mut pos, out);

    // Payload blocks: as even as possible, first blocks one byte
    // bigger; each block scrambled independently, parity unscrambled.
    //
    // The block count MUST come from the same `max_fec` the length
    // arithmetic above used: `encoded_len` / `payload_wire_len` divide by
    // MAX_BASELINE_BLOCK_DATA (247) at baseline FEC, and `decode`
    // recomputes the split the same way from the header's bit 7. This
    // line previously called a max-FEC-only wrapper, hard-coding the
    // divisor 239 -- the same quantity computed two ways, a few lines
    // apart. For the 80 payload lengths where the divisors disagree the
    // loop below split the payload into one block too many and wrote
    // past the length this function reports, so a caller's `out` sized
    // to `encoded_len()` had the tail silently dropped by `put`'s bounds
    // guard and the frame went out both mis-split and truncated, with
    // `Ok` returned.
    let nblocks = block_count_for(payload.len(), max_fec);
    if let Some(small) = payload.len().checked_div(nblocks) {
        let big_blocks = payload.len() % nblocks;
        let codec = block_codec(parity);
        let mut offset = 0usize;
        // Sized for the LARGER of the two plans' per-block ceilings: a
        // baseline block reaches MAX_BASELINE_BLOCK_DATA (247), against
        // 239 at maximum FEC. Every access below is
        // `.get(..size).unwrap_or(&[])`, so a buffer one byte too narrow
        // fails silently -- unscrambled block, parity over an empty
        // slice, no payload bytes emitted, no error.
        let mut block = [0u8; MAX_BASELINE_BLOCK_DATA];
        // Widest parity group in either plan: 16 symbols at maximum FEC
        // (baseline never exceeds 8), so this one needs no widening.
        let mut bp = [0u8; Il2pParity::Sixteen.len()];
        for i in 0..nblocks {
            let size = small + usize::from(i < big_blocks);
            let chunk = payload.get(offset..offset + size).unwrap_or(&[]);
            for (dst, src) in block.iter_mut().zip(chunk.iter()) {
                *dst = *src;
            }
            offset += size;
            let scrambled = block.get_mut(..size).unwrap_or(&mut []);
            Il2pScrambler::new().scramble(scrambled);
            let bp_slice = bp.get_mut(..parity.len()).unwrap_or(&mut []);
            codec.encode(block.get(..size).unwrap_or(&[]), bp_slice)?;
            put(block.get(..size).unwrap_or(&[]), &mut pos, out);
            put(bp.get(..parity.len()).unwrap_or(&[]), &mut pos, out);
        }
    }
    // Report what was written, not what was predicted. The two
    // must be equal -- the assert says so, and the sweep in
    // `tests/il2p.rs` proves it for every legal length at both FEC
    // levels -- but returning `pos` means a future divergence cannot
    // present itself as a silently truncated frame with an `Ok` length.
    debug_assert_eq!(
        pos, total,
        "emitted byte count must equal the length reported to the caller"
    );
    Ok(pos)
}

/// Encodes an AX.25 UI frame as IL2P.
///
/// A frame without digipeaters (and PID `0xF0`, the only PID
/// [`UiFrame`] produces) uses the compact type 1 translated header with
/// the information field as payload; a frame **with** a digipeater path
/// falls back to the type 0 transparent header carrying the whole
/// serialized frame body, since the translated header has no room for
/// a path.
///
/// # Errors
///
/// [`Il2pError::PayloadTooLong`], [`Il2pError::BufferTooSmall`], or an
/// [`Il2pError::Ax25`] serialization error.
pub fn encode_ui_frame(
    frame: &UiFrame<'_>,
    parity: Il2pParity,
    out: &mut [u8],
) -> Result<usize, Il2pError> {
    if frame.path().is_empty() {
        if frame.info.len() > PAYLOAD_MAX {
            return Err(Il2pError::PayloadTooLong {
                got: frame.info.len(),
                max: PAYLOAD_MAX,
            });
        }
        #[allow(clippy::cast_possible_truncation)] // checked <= 1023
        let header = Il2pHeader::Translated {
            dest: frame.dest,
            src: frame.src,
            pid: 0xF0,
            payload_len: frame.info.len() as u16,
            // `UiFrame::build` writes the destination C bit set and the
            // source's clear, which is the AX.25 command encoding, so
            // every frame reachable through this path is a command.
            // A response can still be expressed by building the header
            // directly and calling `encode`.
            command: true,
        };
        return encode(&header, frame.info, parity, out);
    }
    let needed = frame.encoded_len();
    if needed > PAYLOAD_MAX {
        return Err(Il2pError::PayloadTooLong {
            got: needed,
            max: PAYLOAD_MAX,
        });
    }
    let mut body = [0u8; PAYLOAD_MAX];
    let len = frame.build(&mut body)?;
    encode_raw(body.get(..len).unwrap_or(&[]), parity, out)
}

/// Encodes a raw payload as a type 0 transparent IL2P frame.
///
/// # Errors
///
/// [`Il2pError::PayloadTooLong`] or [`Il2pError::BufferTooSmall`].
pub fn encode_raw(payload: &[u8], parity: Il2pParity, out: &mut [u8]) -> Result<usize, Il2pError> {
    if payload.len() > PAYLOAD_MAX {
        return Err(Il2pError::PayloadTooLong {
            got: payload.len(),
            max: PAYLOAD_MAX,
        });
    }
    #[allow(clippy::cast_possible_truncation)] // checked <= 1023
    let header = Il2pHeader::Transparent {
        payload_len: payload.len() as u16,
    };
    encode(&header, payload, parity, out)
}

/// A successfully decoded IL2P frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Il2pDecoded {
    /// The corrected, descrambled, parsed header.
    pub header: Il2pHeader,
    /// Payload bytes written to the caller's buffer.
    pub payload_len: usize,
    /// Symbols the header codeblock needed corrected (0 or 1).
    pub header_corrected: usize,
    /// Symbols corrected across all payload blocks.
    pub payload_corrected: usize,
}

impl Il2pDecoded {
    /// Total corrected symbols, header and payload combined.
    #[must_use]
    pub const fn corrected(&self) -> usize {
        self.header_corrected + self.payload_corrected
    }
}

/// Decodes a byte-aligned IL2P frame — the bytes **after** the sync
/// word — into `payload_out`.
///
/// Runs the receive pipeline: header RS correction → descramble →
/// header parse → per-block RS correction → per-block descramble.
/// Trailing bytes beyond the frame the header announces are ignored, so
/// a caller may hand over a whole capture buffer. [`Il2pReceiver`] is
/// the bit-level front end for this function: it hunts [`SYNC_WORD`],
/// collects the announced bytes and calls `decode` itself.
///
/// `parity` is **ignored**, and the parameter is named accordingly. The
/// FEC level is read off the wire — header byte 0 bit 7, see
/// [`Il2pParity::is_max_fec`] — and it alone decides the block count,
/// the per-block size and the parity length, so a caller-supplied value
/// could only ever contradict the sender. The parameter is retained for
/// source compatibility and so [`Il2pReceiver`] can hold an operating
/// point without a special case; pass whatever is convenient.
///
/// # Errors
///
/// [`Il2pError::FrameTooShort`] when `bytes` cannot hold the announced
/// frame; [`Il2pError::HeaderUncorrectable`] /
/// [`Il2pError::BlockUncorrectable`] on FEC failure;
/// [`Il2pError::BufferTooSmall`] when `payload_out` is too small;
/// header parse errors from [`Il2pHeader::unpack`].
pub fn decode(
    bytes: &[u8],
    _parity_ignored: Il2pParity,
    payload_out: &mut [u8],
) -> Result<Il2pDecoded, Il2pError> {
    let Some(hdr_bytes) = bytes.get(..HDR_BLOCK) else {
        return Err(Il2pError::FrameTooShort {
            got: bytes.len(),
            needed: HDR_BLOCK,
        });
    };
    let mut hdr_block = [0u8; HDR_BLOCK];
    hdr_block.copy_from_slice(hdr_bytes);
    let header_corrected = header_codec()
        .decode(&mut hdr_block)
        .map_err(|_| Il2pError::HeaderUncorrectable)?;
    let mut h = [0u8; HEADER_LEN];
    h.copy_from_slice(hdr_block.get(..HEADER_LEN).unwrap_or(&[]));
    Il2pScrambler::new().descramble(&mut h);
    let header = Il2pHeader::unpack(&h)?;

    // The FEC level is announced by the header, not chosen by us: it
    // decides both the per-block parity and the block-splitting
    // ceiling, and therefore how many bytes belong to this frame. A
    // caller-supplied value could only ever disagree with the sender,
    // so the parameter is ignored (see the note on this function).
    let max_fec = h[0] & 0x80 != 0;
    let payload_len = header.payload_len();
    let nblocks = block_count_for(payload_len, max_fec);
    let parity = payload_parity(payload_len, max_fec);
    let needed = HDR_BLOCK + payload_len + nblocks * parity.len();
    if bytes.len() < needed {
        return Err(Il2pError::FrameTooShort {
            got: bytes.len(),
            needed,
        });
    }
    if payload_out.len() < payload_len {
        return Err(Il2pError::BufferTooSmall {
            needed: payload_len,
            got: payload_out.len(),
        });
    }

    let mut payload_corrected = 0usize;
    if let Some(small) = payload_len.checked_div(nblocks) {
        let big_blocks = payload_len % nblocks;
        let codec = block_codec(parity);
        let mut pos = HDR_BLOCK;
        let mut written = 0usize;
        // One whole code word: both plans fill it exactly at their
        // widest block (239 data + 16 parity at maximum FEC, 247 + 8 at
        // baseline), so the RS block length is the correct bound here
        // rather than either plan's data ceiling.
        let mut block = [0u8; crate::rs::BLOCK_MAX];
        for i in 0..nblocks {
            let size = small + usize::from(i < big_blocks);
            let coded = size + parity.len();
            let chunk = bytes.get(pos..pos + coded).unwrap_or(&[]);
            for (dst, src) in block.iter_mut().zip(chunk.iter()) {
                *dst = *src;
            }
            pos += coded;
            let word = block.get_mut(..coded).unwrap_or(&mut []);
            payload_corrected += codec
                .decode(word)
                .map_err(|_| Il2pError::BlockUncorrectable { block: i })?;
            let data = block.get_mut(..size).unwrap_or(&mut []);
            Il2pScrambler::new().descramble(data);
            if let Some(slot) = payload_out.get_mut(written..written + size) {
                slot.copy_from_slice(block.get(..size).unwrap_or(&[]));
            }
            written += size;
        }
    }

    Ok(Il2pDecoded {
        header,
        payload_len,
        header_corrected,
        payload_corrected,
    })
}

/// The header codeblock length on the air: header plus its parity.
const HDR_BLOCK: usize = HEADER_LEN + HEADER_PARITY_LEN;

/// Reconstructs the AX.25 UI frame a decoded IL2P frame carries.
///
/// A translated (type 1) header yields a path-free UI frame borrowing
/// `payload` as its information field; a transparent (type 0) header
/// parses `payload` as a complete frame body.
///
/// # Errors
///
/// [`Il2pError::Ax25`] when a transparent payload is not a valid UI
/// frame body.
pub fn to_ui_frame<'a>(header: &Il2pHeader, payload: &'a [u8]) -> Result<UiFrame<'a>, Il2pError> {
    match *header {
        Il2pHeader::Transparent { .. } => Ok(UiFrame::parse(payload)?),
        Il2pHeader::Translated { dest, src, .. } => Ok(UiFrame::new(dest, src, payload)),
    }
}

/// Lazy **MSB-first** transmit bit iterator over an encoded IL2P frame:
/// `preamble_bytes` × [`PREAMBLE_BYTE`], the frame bytes (sync word
/// included — the [`encode`] family emits it first), then `tail_bytes`
/// × [`PREAMBLE_BYTE`] so receive-side filter/slicer latency flushes.
///
/// This is the IL2P twin of [`crate::fx25::byte_bits`], with the byte
/// order IL2P specifies (MSB first, no bit stuffing).
///
/// # Feed these bits to the modulator **directly**
///
/// Unlike AX.25 and FX.25, IL2P is **not** differentially encoded.
/// Specification v0.6, "Interface to Physical Layer", says of the AFSK
/// symbol map: *"A '1' bit is sent as a Bell 202 "mark" tone (1200 Hz),
/// while a '0' bit is sent as a Bell 202 "space" tone (2200 Hz).
/// **Differential encoding is not used.**"* — and repeats the sentence
/// for the FSK map. NRZI *is* differential encoding, so passing these
/// bits through [`crate::nrzi::encode_iter`] produces a signal no other
/// IL2P station can read.
///
/// The scrambler is what supplies the transition density that NRZI plus
/// bit stuffing provides in HDLC, which is why IL2P does not need it.
///
/// The contrast, as a sketch rather than a doctest — the second line is
/// wrong on purpose, so this fence is `text`. (It was `ignore`, which
/// reads as "skip this" but in fact means "a doctest that only runs
/// under `--ignored`", so `cargo test -- --ignored` failed to compile
/// it for everyone, with or without the reference binaries.)
///
/// ```text
/// // Correct: straight into the modulator.
/// modulator.i16_samples(il2p::tx_bits(&encoded[..len], 16, 2))
/// // WRONG: this is what made the crate non-interoperable until the
/// // reference-implementation differential caught it.
/// modulator.i16_samples(nrzi::encode_iter(il2p::tx_bits(..)))
/// ```
#[must_use]
pub fn tx_bits(frame: &[u8], preamble_bytes: usize, tail_bytes: usize) -> Il2pTxBits<'_> {
    Il2pTxBits {
        frame,
        preamble: preamble_bytes,
        tail: tail_bytes,
        pos: 0,
    }
}

/// Iterator type of [`tx_bits`]: MSB-first bits of preamble ‖ frame ‖
/// tail.
#[derive(Debug, Clone)]
pub struct Il2pTxBits<'a> {
    frame: &'a [u8],
    preamble: usize,
    tail: usize,
    pos: usize,
}

impl Iterator for Il2pTxBits<'_> {
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        let index = self.pos / 8;
        let byte = if index < self.preamble {
            PREAMBLE_BYTE
        } else if let Some(&b) = self.frame.get(index - self.preamble) {
            b
        } else if index < self.preamble + self.frame.len() + self.tail {
            PREAMBLE_BYTE
        } else {
            return None;
        };
        let bit = Bit::from((byte >> (7 - self.pos % 8)) & 1 != 0);
        self.pos += 1;
        Some(bit)
    }
}

/// One IL2P frame surfaced by [`Il2pReceiver::push`].
#[derive(Debug, Clone, Copy)]
pub struct Il2pRxFrame<'a> {
    /// The decode summary (header, payload length, corrected symbols).
    pub decoded: Il2pDecoded,
    payload: &'a [u8],
}

impl<'a> Il2pRxFrame<'a> {
    /// The corrected, descrambled payload bytes.
    #[must_use]
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// The decoded header.
    #[must_use]
    pub const fn header(&self) -> &Il2pHeader {
        &self.decoded.header
    }

    /// Total corrected symbols, header and payload combined.
    #[must_use]
    pub const fn corrected(&self) -> usize {
        self.decoded.corrected()
    }

    /// Reconstructs the AX.25 UI frame this IL2P frame carries (see
    /// [`to_ui_frame`]).
    ///
    /// # Errors
    ///
    /// [`Il2pError::Ax25`] when a transparent payload is not a valid UI
    /// frame body.
    pub fn ui_frame(&self) -> Result<UiFrame<'a>, Il2pError> {
        to_ui_frame(&self.decoded.header, self.payload)
    }
}

/// Receive state of [`Il2pReceiver`].
#[derive(Debug, Clone, Copy)]
enum Il2pRxState {
    /// Correlating the bit stream against the 24-bit sync word.
    Hunt,
    /// Byte-accumulating the frame the sync word announced.
    Collect {
        /// Complete bytes collected so far.
        count: usize,
        /// Bits accumulated into `cur`, `0..8`.
        nbits: u8,
        /// Byte currently being assembled, MSB-first.
        cur: u8,
        /// Total bytes to collect; [`HDR_BLOCK`] until the header
        /// decodes and announces the payload.
        needed: usize,
        /// Whether the header has been decoded (so `needed` is final).
        have_header: bool,
    },
}

/// Bit-level IL2P frame receiver: post-NRZI bits in, decoded frames
/// out.
///
/// The receive twin of [`tx_bits`], and the IL2P sibling of
/// [`crate::fx25::Fx25Receiver`]: a sliding 24-bit correlator hunts for
/// [`SYNC_WORD`] (accepting matches within [`SYNC_TOLERANCE`] bit
/// errors), then bytes are accumulated MSB-first — first the header
/// codeblock, whose corrected header announces the total frame length,
/// then the payload blocks — and the whole frame runs through
/// [`decode`]. Everything is fixed-size: the frame buffer and payload
/// buffer together are ≈ 2.1 KiB — no allocation.
///
/// The payload-parity operating point is fixed at construction: IL2P
/// does not signal it in the header, so both ends must be configured
/// identically (see [`Il2pParity`]). A mismatch is not reported as
/// such — the block lengths simply disagree and frames stop decoding.
///
/// ```
/// use warble::ax25::{Address, UiFrame};
/// use warble::il2p::{self, ENCODED_MAX, Il2pParity, Il2pReceiver};
///
/// let frame = UiFrame::new(
///     Address::new(b"APRS", 0)?,
///     Address::new(b"N0CALL", 7)?,
///     b">bit-level round trip",
/// );
/// let mut tx = [0u8; ENCODED_MAX];
/// let len = il2p::encode_ui_frame(&frame, Il2pParity::Sixteen, &mut tx)?;
///
/// let mut rx = Il2pReceiver::new(Il2pParity::Sixteen);
/// let mut got = false;
/// for bit in il2p::tx_bits(&tx[..len], 2, 1) {
///     if let Some(Ok(rxf)) = rx.push(bit) {
///         assert_eq!(rxf.ui_frame()?, frame);
///         assert_eq!(rxf.corrected(), 0);
///         got = true;
///     }
/// }
/// assert!(got);
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct Il2pReceiver {
    /// The payload-parity operating point both ends agreed on.
    parity: Il2pParity,
    /// Sliding 24-bit correlation window (newest bit in bit 0; the
    /// MSB-first sync word lines up directly).
    accum: u32,
    /// Bits pushed since the last reset (saturating; gates matching
    /// until the window is full).
    seen: u32,
    state: Il2pRxState,
    /// The collected frame bytes (everything after the sync word).
    buf: [u8; RX_FRAME_MAX],
    /// Decoded payload of the frame being emitted (owned so the borrow
    /// survives).
    payload: [u8; PAYLOAD_MAX],
}

impl Il2pReceiver {
    /// Creates an empty receiver hunting for the sync word.
    #[must_use]
    pub const fn new(parity: Il2pParity) -> Self {
        Self {
            parity,
            accum: 0,
            seen: 0,
            state: Il2pRxState::Hunt,
            buf: [0; RX_FRAME_MAX],
            payload: [0; PAYLOAD_MAX],
        }
    }

    /// The configured payload-parity operating point.
    #[must_use]
    pub const fn parity(&self) -> Il2pParity {
        self.parity
    }

    /// Resets to the hunt state (correlator cleared).
    const fn reset(&mut self) {
        self.accum = 0;
        self.seen = 0;
        self.state = Il2pRxState::Hunt;
    }

    /// Pushes one post-NRZI-decode line bit (MSB-first byte order, the
    /// order [`tx_bits`] transmits).
    ///
    /// Returns `Some(Ok(frame))` when a complete frame decodes — the
    /// payload borrows the internal buffer until the next push.
    /// `Some(Err(_))` reports a diagnosable rejection (an
    /// uncorrectable header or payload block, or a malformed header);
    /// the receiver returns to hunting either way.
    pub fn push(&mut self, bit: Bit) -> Option<Result<Il2pRxFrame<'_>, Il2pError>> {
        match self.state {
            Il2pRxState::Hunt => {
                self.accum = (self.accum << 1) & 0x00FF_FFFF;
                if let Bit::One = bit {
                    self.accum |= 1;
                }
                self.seen = self.seen.saturating_add(1);
                if self.seen >= 24 && (self.accum ^ SYNC_WORD).count_ones() <= SYNC_TOLERANCE {
                    self.state = Il2pRxState::Collect {
                        count: 0,
                        nbits: 0,
                        cur: 0,
                        needed: HDR_BLOCK,
                        have_header: false,
                    };
                }
                None
            }
            Il2pRxState::Collect {
                mut count,
                mut nbits,
                mut cur,
                mut needed,
                mut have_header,
            } => {
                cur <<= 1;
                if let Bit::One = bit {
                    cur |= 1;
                }
                nbits += 1;
                if nbits == 8 {
                    if let Some(slot) = self.buf.get_mut(count) {
                        *slot = cur;
                    }
                    count += 1;
                    nbits = 0;
                    cur = 0;
                    if count == needed && !have_header {
                        // Header codeblock complete: correct and parse a
                        // copy to learn the total frame length.
                        match Self::peek_payload_len(&self.buf) {
                            Ok((payload_len, max_fec)) => {
                                needed = HDR_BLOCK + payload_wire_len(payload_len, max_fec);
                                have_header = true;
                            }
                            Err(e) => {
                                self.reset();
                                return Some(Err(e));
                            }
                        }
                    }
                    if count == needed && have_header {
                        self.reset();
                        return Some(self.finish(needed));
                    }
                }
                self.state = Il2pRxState::Collect {
                    count,
                    nbits,
                    cur,
                    needed,
                    have_header,
                };
                None
            }
        }
    }

    /// Corrects and parses the collected header codeblock (on a copy —
    /// [`decode`] re-runs the correction on the buffer itself),
    /// returning the announced payload length.
    fn peek_payload_len(buf: &[u8; RX_FRAME_MAX]) -> Result<(usize, bool), Il2pError> {
        let mut hdr_block = [0u8; HDR_BLOCK];
        hdr_block.copy_from_slice(buf.get(..HDR_BLOCK).unwrap_or(&[]));
        header_codec()
            .decode(&mut hdr_block)
            .map_err(|_| Il2pError::HeaderUncorrectable)?;
        let mut h = [0u8; HEADER_LEN];
        h.copy_from_slice(hdr_block.get(..HEADER_LEN).unwrap_or(&[]));
        Il2pScrambler::new().descramble(&mut h);
        // Byte 0 bit 7 is the FEC level, and it is what sizes the rest
        // of the frame -- read it here rather than assuming, because a
        // peer may be transmitting the legacy baseline plan.
        let max_fec = h[0] & 0x80 != 0;
        Ok((Il2pHeader::unpack(&h)?.payload_len(), max_fec))
    }

    /// Runs the codec-layer [`decode`] over the collected frame.
    fn finish(&mut self, needed: usize) -> Result<Il2pRxFrame<'_>, Il2pError> {
        let bytes = self.buf.get(..needed).unwrap_or(&[]);
        // `decode` reads the FEC level from the header itself; the
        // parity we were configured with is not consulted on receive.
        let decoded = decode(bytes, self.parity, &mut self.payload)?;
        Ok(Il2pRxFrame {
            decoded,
            payload: self.payload.get(..decoded.payload_len).unwrap_or(&[]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent bit-level model of the multiplicative scrambler:
    /// history kept as a plain array instead of a packed register.
    fn reference_scramble(bytes: &[u8]) -> impl Iterator<Item = u8> + '_ {
        let mut history = [0u8; 9];
        for (d, slot) in history.iter_mut().enumerate() {
            *slot = ((SCRAMBLER_SEED >> d) & 1) as u8;
        }
        bytes.iter().map(move |&byte| {
            let mut out_byte = 0u8;
            for k in (0..8).rev() {
                let bit = (byte >> k) & 1;
                let out = bit ^ history[3] ^ history[8];
                history.rotate_right(1);
                history[0] = out;
                out_byte |= out << k;
            }
            out_byte
        })
    }

    #[test]
    fn scrambler_matches_reference_recurrence() {
        let data: [u8; 64] = core::array::from_fn(|i| (i as u8).wrapping_mul(37) ^ 0xA5);
        let mut scrambled = data;
        Il2pScrambler::new().scramble(&mut scrambled);
        for (got, want) in scrambled.iter().zip(reference_scramble(&data)) {
            assert_eq!(*got, want);
        }
    }

    #[test]
    fn scrambler_known_answer_vector() {
        // Provenance: self-generated from this module's documented
        // recurrence (out[n] = in[n] ^ out[n-4] ^ out[n-9], seed
        // SCRAMBLER_SEED, MSB first), cross-checked against the
        // independent `reference_scramble` model above. NOT a published
        // spec vector -- for those see the `spec_v06_*` tests in
        // `tests/il2p.rs`, which are what pin the preset.
        let mut data = [0u8; 4];
        Il2pScrambler::new().scramble(&mut data);
        let mut expected = [0u8; 4];
        for (slot, b) in expected.iter_mut().zip(reference_scramble(&[0u8; 4])) {
            *slot = b;
        }
        assert_eq!(data, expected);
        // The all-zeros input exposes the free-running LFSR sequence.
        assert_eq!(data, [0x0F, 0x70, 0xB3, 0x6F]);
    }

    #[test]
    fn scrambler_self_inverse() {
        let data: [u8; 100] = core::array::from_fn(|i| (i as u8).wrapping_mul(151));
        let mut work = data;
        Il2pScrambler::new().scramble(&mut work);
        Il2pScrambler::new().descramble(&mut work);
        assert_eq!(work, data);
    }

    #[test]
    fn header_pack_unpack_type1() {
        let header = Il2pHeader::Translated {
            command: true,
            dest: Address::new(b"APRS", 0).unwrap(),
            src: Address::new(b"N0CALL", 15).unwrap(),
            pid: 0xF0,
            payload_len: 1023,
        };
        let packed = header.pack(true).unwrap();
        assert_eq!(Il2pHeader::unpack(&packed).unwrap(), header);
    }

    #[test]
    fn header_pack_unpack_type0() {
        for len in [0u16, 1, 204, 205, 206, 1023] {
            let header = Il2pHeader::Transparent { payload_len: len };
            let packed = header.pack(true).unwrap();
            assert_eq!(Il2pHeader::unpack(&packed).unwrap(), header);
        }
    }

    #[test]
    fn header_known_answer() {
        // Provenance: self-consistent vector derived from the bit map
        // documented on `Il2pHeader` (NOT a published spec vector).
        let header = Il2pHeader::Translated {
            command: true,
            dest: Address::new(b"AB", 1).unwrap(),
            src: Address::new(b"C", 2).unwrap(),
            pid: 0xF0,
            payload_len: 5,
        };
        let h = header.pack(true).unwrap();
        // Dest 'A','B',' '.. sixbit = 0x21, 0x22, 0x00...
        assert_eq!(h[0] & 0x3F, 0x21);
        assert_eq!(h[1] & 0x3F, 0x22);
        assert_eq!(h[6] & 0x3F, 0x23); // 'C'
        assert_eq!(h[1] & 0x80, 0x80); // type 1
        assert_eq!(h[0] & 0x40, 0x40); // UI
        // PID code 0xF = 1111 across bit 6 of bytes 1..=4.
        assert_eq!(
            [h[1] >> 6 & 1, h[2] >> 6 & 1, h[3] >> 6 & 1, h[4] >> 6 & 1],
            [1, 1, 1, 1]
        );
        // Control subfield 0b0101100 across bit 6 of bytes 5..=11: UI
        // opcode 0b101 in bits 5..=3, P/F 0, and **C 1**. The command
        // bit is set because `UiFrame::build` writes the AX.25
        // destination C bit set and the source's clear, which is the
        // command encoding -- so every UI frame this crate can
        // translate is a command, and emitting C 0 was simply wrong.
        let mut control_bits = [0u8; 7];
        for (k, slot) in control_bits.iter_mut().enumerate() {
            *slot = h[k + 5] >> 6 & 1;
        }
        assert_eq!(control_bits, [0, 1, 0, 1, 1, 0, 0]);
        // Count 5 = 0b0000000101 across bit 7 of bytes 2..=11.
        let mut count_bits = [0u8; 10];
        for (k, slot) in count_bits.iter_mut().enumerate() {
            *slot = h[k + 2] >> 7;
        }
        assert_eq!(count_bits, [0, 0, 0, 0, 0, 0, 0, 1, 0, 1]);
        assert_eq!(h[12], 0x12); // dest SSID 1, source SSID 2
    }

    /// [`PID_CODE_NO_LAYER3`] documents a wire value but is not used by
    /// the encode path (which goes through [`PID_TABLE`] via
    /// `pid_to_code`), so nothing otherwise stops the two drifting
    /// apart. Mutation testing found exactly that hole: changing the
    /// constant alone broke no test, because it is load-bearing for
    /// readers only.
    #[test]
    fn pid_constant_agrees_with_table() {
        assert_eq!(pid_to_code(0xF0), Some(PID_CODE_NO_LAYER3));
        assert_eq!(code_to_pid(PID_CODE_NO_LAYER3), Some(0xF0));
    }

    /// The PID table must be a bijection over the codes and PIDs it
    /// lists: a duplicate on either side would make translation
    /// direction-dependent, and `pid_to_code`/`code_to_pid` return the
    /// first match, so a duplicate would silently shadow an entry.
    #[test]
    fn pid_table_is_a_bijection() {
        for (i, (code, pid)) in PID_TABLE.iter().enumerate() {
            assert!(*code <= 0xF, "code 0x{code:X} exceeds 4 bits");
            for (other_code, other_pid) in PID_TABLE.iter().skip(i + 1) {
                assert_ne!(code, other_code, "duplicate IL2P code 0x{code:X}");
                assert_ne!(pid, other_pid, "duplicate AX.25 PID 0x{pid:02X}");
            }
        }
        // Spec v0.6 reserves 0x0/0x1 for frames carrying no PID byte
        // and marks 0x7..=0xA Future; none may appear here.
        for (code, _) in PID_TABLE {
            assert!(
                !matches!(code, 0x0 | 0x1 | 0x7 | 0x8 | 0x9 | 0xA),
                "code 0x{code:X} is reserved or Future in spec v0.6"
            );
        }
    }

    #[test]
    fn block_layout() {
        // Spec v0.6: block_count = ceil(payload_byte_count / 239).
        assert_eq!(block_count_for(0, true), 0);
        assert_eq!(block_count_for(1, true), 1);
        assert_eq!(block_count_for(239, true), 1);
        assert_eq!(block_count_for(240, true), 2);
        assert_eq!(block_count_for(478, true), 2);
        assert_eq!(block_count_for(479, true), 3);
        assert_eq!(block_count_for(1023, true), 5);
        assert_eq!(encoded_len(1023, Il2pParity::Sixteen), ENCODED_MAX);

        // Baseline FEC: ceil(payload_byte_count / 247). Stated beside the
        // max-FEC line because the two are NOT interchangeable -- the
        // encoder once used the max-FEC divisor for a baseline frame.
        assert_eq!(block_count_for(0, false), 0);
        assert_eq!(block_count_for(1, false), 1);
        assert_eq!(block_count_for(MAX_BASELINE_BLOCK_DATA, false), 1);
        assert_eq!(block_count_for(MAX_BASELINE_BLOCK_DATA + 1, false), 2);
        assert_eq!(block_count_for(494, false), 2);
        assert_eq!(block_count_for(495, false), 3);
        assert_eq!(block_count_for(1023, false), 5);

        // The first length of each disagreeing band (the four bands are
        // swept exhaustively in `tests/il2p.rs`).
        for (len, baseline, max_fec) in [(240, 1, 2), (479, 2, 3), (718, 3, 4), (957, 4, 5)] {
            assert_eq!(block_count_for(len, false), baseline, "{len} baseline");
            assert_eq!(block_count_for(len, true), max_fec, "{len} max FEC");
        }
    }

    #[test]
    fn raw_roundtrip_all_operating_points() {
        let payload: [u8; 300] = core::array::from_fn(|i| (i as u8) ^ 0x3C);
        for parity in Il2pParity::ALL {
            let mut tx = [0u8; ENCODED_MAX];
            let len = encode_raw(&payload, parity, &mut tx).unwrap();
            assert_eq!(len, encoded_len(payload.len(), parity));
            assert_eq!(&tx[..SYNC_LEN], &SYNC_BYTES);
            let mut out = [0u8; PAYLOAD_MAX];
            let decoded = decode(&tx[SYNC_LEN..len], parity, &mut out).unwrap();
            assert_eq!(decoded.payload_len, payload.len());
            assert_eq!(&out[..decoded.payload_len], &payload);
            assert_eq!(decoded.corrected(), 0);
        }
    }

    #[test]
    fn empty_payload_roundtrip() {
        let mut tx = [0u8; ENCODED_MAX];
        let len = encode_raw(&[], Il2pParity::Sixteen, &mut tx).unwrap();
        assert_eq!(len, SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN);
        let mut out = [0u8; 0];
        let decoded = decode(&tx[SYNC_LEN..len], Il2pParity::Sixteen, &mut out).unwrap();
        assert_eq!(decoded.payload_len, 0);
    }
}
