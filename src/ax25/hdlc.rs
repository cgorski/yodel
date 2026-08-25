//! HDLC bit-level framing: flags, zero-bit stuffing, and deframing.
//!
//! HDLC delimits frames with the flag octet `0x7E` (`01111110`). To keep
//! the flag unique, the transmitter inserts a `Zero` after any five
//! consecutive `One`s of frame content (*bit stuffing*); the receiver
//! removes it. Octets are serialized least-significant bit first; flags
//! themselves are never stuffed. Bit stuffing also guarantees an NRZI
//! transition at least every six bit periods, keeping the demodulator's
//! clock recovery fed (see [`crate::nrzi`]).
//!
//! Transmit side: [`frame_bits`] / [`FrameBits`], a lazy allocation-free
//! iterator that appends the FCS, stuffs, and adds preamble/tail flags.
//! Receive side: [`HdlcDeframer`], a push-one-bit state machine with a
//! fixed const-generic byte buffer.

use super::Ax25Error;
use super::fcs::{Fcs, SingleBitError, crc16_x25, locate_single_bit_error};
use super::frame::{MIN_FRAME_LEN, UiFrame};
use crate::types::Bit;

/// The HDLC flag octet delimiting frames.
pub const FLAG: u8 = 0x7E;

/// Default number of preamble flags sent before a frame.
///
/// 32 flags (~213 ms at 1200 baud) is a known-good interoperability value:
/// it gives real receivers ample time for squelch opening, AGC settling,
/// and clock acquisition before the frame starts.
pub const DEFAULT_PREAMBLE_FLAGS: usize = 32;

/// Default number of tail flags sent after a frame.
///
/// Two closing flags are a known-good interoperability value: the first
/// terminates the frame, the second guards against a receiver missing the
/// boundary by a bit.
pub const DEFAULT_TAIL_FLAGS: usize = 2;

/// Serializes a frame into HDLC line bits.
///
/// The returned iterator yields, lazily and without allocating:
/// `preamble_flags` flag octets, then the frame octets followed by the
/// computed CRC-16/X.25 FCS (little-endian) — all LSB-first with a `Zero`
/// stuffed after five consecutive `One`s — then `tail_flags` flag octets.
/// Flags are never stuffed.
///
/// # Stuffing behavior
///
/// With no flags, the stream is the pure stuffed data: `0x1F` is
/// `1 1 1 1 1 0 0 0` LSB-first, so a `Zero` is stuffed right after the
/// five ones — and no data run of six consecutive ones can ever appear,
/// keeping the six-ones flag pattern unique on the line:
///
/// ```
/// use yodel::Bit;
/// use yodel::ax25::hdlc::frame_bits;
///
/// let bits: Vec<Bit> = frame_bits(&[0x1F], 0, 0).collect();
/// // Five ones, then the stuffed Zero (not a data bit), then bit 5 of 0x1F.
/// use Bit::{One, Zero};
/// assert_eq!(&bits[..7], &[One, One, One, One, One, Zero, Zero]);
/// // 8 data bits + 16 FCS bits + exactly one stuffed zero.
/// assert_eq!(bits.len(), 8 + 16 + 1);
///
/// // The invariant, even for an all-ones payload: never six ones in a row.
/// let worst: Vec<Bit> = frame_bits(&[0xFF; 8], 0, 0).collect();
/// assert!(worst.windows(6).all(|w| w.contains(&Zero)));
/// ```
pub fn frame_bits(frame: &[u8], preamble_flags: usize, tail_flags: usize) -> FrameBits<'_> {
    let fcs = crc16_x25(frame);
    FrameBits {
        frame,
        fcs: fcs.to_le_bytes(),
        tail_flags,
        state: TxState::Preamble {
            flags_left: preamble_flags,
            bit: 0,
        },
        ones: 0,
        stuff_pending: false,
    }
}

/// Transmit state of [`FrameBits`].
#[derive(Debug, Clone, Copy)]
enum TxState {
    /// Emitting preamble flags.
    Preamble {
        /// Flags still to send, including the current one.
        flags_left: usize,
        /// Bit position within the current flag, `0..8`.
        bit: u8,
    },
    /// Emitting stuffed frame + FCS octets.
    Data {
        /// Byte position: `0..frame.len()` is the frame, then two FCS bytes.
        pos: usize,
        /// Bit position within the current octet, `0..8`.
        bit: u8,
    },
    /// Emitting tail flags.
    Tail {
        /// Flags still to send, including the current one.
        flags_left: usize,
        /// Bit position within the current flag, `0..8`.
        bit: u8,
    },
    /// All bits emitted.
    Done,
}

/// Lazy iterator of HDLC line bits for one frame.
///
/// Created by [`frame_bits`]. Yields preamble flags, the stuffed frame and
/// FCS, and tail flags; see [`frame_bits`] for details.
#[derive(Debug, Clone)]
pub struct FrameBits<'a> {
    frame: &'a [u8],
    fcs: [u8; 2],
    tail_flags: usize,
    state: TxState,
    ones: u8,
    stuff_pending: bool,
}

impl FrameBits<'_> {
    /// The data byte at `pos` (frame, then the two FCS bytes), if any.
    fn data_byte(&self, pos: usize) -> Option<u8> {
        match self.frame.get(pos) {
            Some(&b) => Some(b),
            None => self.fcs.get(pos.wrapping_sub(self.frame.len())).copied(),
        }
    }
}

impl Iterator for FrameBits<'_> {
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        loop {
            match self.state {
                TxState::Preamble { flags_left, bit } => {
                    if flags_left == 0 {
                        self.state = TxState::Data { pos: 0, bit: 0 };
                        continue;
                    }
                    let out = Bit::from((FLAG >> bit) & 1 != 0);
                    self.state = if bit == 7 {
                        TxState::Preamble {
                            flags_left: flags_left - 1,
                            bit: 0,
                        }
                    } else {
                        TxState::Preamble {
                            flags_left,
                            bit: bit + 1,
                        }
                    };
                    return Some(out);
                }
                TxState::Data { pos, bit } => {
                    if self.stuff_pending {
                        self.stuff_pending = false;
                        self.ones = 0;
                        return Some(Bit::Zero);
                    }
                    let Some(byte) = self.data_byte(pos) else {
                        self.state = TxState::Tail {
                            flags_left: self.tail_flags,
                            bit: 0,
                        };
                        continue;
                    };
                    let out = Bit::from((byte >> bit) & 1 != 0);
                    match out {
                        Bit::One => {
                            self.ones += 1;
                            if self.ones == 5 {
                                self.stuff_pending = true;
                            }
                        }
                        Bit::Zero => self.ones = 0,
                    }
                    self.state = if bit == 7 {
                        TxState::Data {
                            pos: pos + 1,
                            bit: 0,
                        }
                    } else {
                        TxState::Data { pos, bit: bit + 1 }
                    };
                    return Some(out);
                }
                TxState::Tail { flags_left, bit } => {
                    if flags_left == 0 {
                        self.state = TxState::Done;
                        return None;
                    }
                    let out = Bit::from((FLAG >> bit) & 1 != 0);
                    self.state = if bit == 7 {
                        TxState::Tail {
                            flags_left: flags_left - 1,
                            bit: 0,
                        }
                    } else {
                        TxState::Tail {
                            flags_left,
                            bit: bit + 1,
                        }
                    };
                    return Some(out);
                }
                TxState::Done => return None,
            }
        }
    }
}

/// Policy for recovering frames that fail the FCS check.
///
/// The CRC-16/X.25 is linear, so a frame corrupted by exactly one bit
/// flip (in the contents or the FCS itself) can be identified and
/// repaired from the FCS mismatch syndrome alone, in O(frame bytes)
/// total work. Repair carries a small but real false-accept risk — a
/// multi-bit corruption can alias to a single-bit syndrome — so repaired
/// frames are additionally required to parse as sane AX.25 UI frames
/// (valid address characters, UI control, no-layer-3 PID) before being
/// accepted, and the whole pass is opt-in: the default is
/// [`RecoveryPolicy::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryPolicy {
    /// No recovery: frames failing the FCS check are rejected (default).
    #[default]
    None,
    /// Attempt single-bit-flip repair on FCS failures, gated by a UI
    /// frame sanity parse of the repaired contents.
    SingleBitFlip,
    /// [`RecoveryPolicy::SingleBitFlip`] plus, when the syndrome repair
    /// fails, a bounded brute-force retry from the **raw pre-destuff**
    /// bit window: each single line bit between the flags is flipped in
    /// turn, the window is re-destuffed, and the result re-checked
    /// against the FCS. This repairs errors that hit a stuffing bit (or
    /// create/destroy one), which shift the whole destuffed tail and are
    /// unreachable from the post-destuff bytes. Candidates must destuff
    /// to a byte-aligned, in-range frame with a valid FCS **and** parse
    /// as a sane AX.25 UI frame before being accepted.
    PreDestuffFlip,
}

/// Capacity of the raw pre-destuff bit window, in bits (bit-packed into
/// [`RAW_BYTES`] bytes). Frames whose stuffed length outgrows this skip
/// pre-destuff recovery rather than allocating.
pub(crate) const RAW_BITS: usize = 4096;
/// Byte size of the raw bit window buffer.
pub(crate) const RAW_BYTES: usize = RAW_BITS / 8;

/// Streaming HDLC deframer: line bits in, validated frames out.
///
/// Push one destuffed-candidate bit at a time with [`HdlcDeframer::push`].
/// The deframer hunts for flags, removes stuffed zeros, accumulates octets
/// (LSB-first) into a fixed `[u8; N]` buffer, and on each closing flag
/// validates length and FCS. It tolerates a single flag shared between
/// back-to-back frames, silently discards runt or misaligned bit salvage,
/// aborts on seven or more consecutive ones, and never panics on garbage
/// input.
///
/// `N` is the byte capacity of the accumulation buffer, and the frame's
/// two FCS bytes are accumulated into it alongside the contents: the
/// largest frame *contents* that can be received is therefore `N - 2`.
/// Anything longer is reported as [`Ax25Error::FrameTooLarge`], whose
/// `len` is the content-plus-FCS length while `max` is `N` (so with
/// `N = 330`, 328 content bytes are accepted and 329 are rejected as
/// `len: 331, max: 330`).
#[derive(Debug, Clone)]
pub struct HdlcDeframer<const N: usize> {
    /// Byte buffer for the frame being accumulated (content + FCS).
    buf: [u8; N],
    /// Complete bytes stored in `buf` (or seen, when overflowed).
    len: usize,
    /// Bits accumulated into `cur_byte`, `0..8`.
    nbits: u8,
    /// Octet currently being assembled, LSB-first.
    cur_byte: u8,
    /// Consecutive `One`s seen (for destuffing / flag / abort detection).
    ones: u8,
    /// Whether an opening flag has been seen (accumulating frame content).
    in_frame: bool,
    /// Whether the current frame outgrew `buf`.
    overflowed: bool,
    /// FCS-failure recovery policy (default [`RecoveryPolicy::None`]).
    recovery: RecoveryPolicy,
    /// Raw pre-destuff line bits recorded since the opening flag
    /// (bit-packed LSB-first), for [`RecoveryPolicy::PreDestuffFlip`].
    raw: [u8; RAW_BYTES],
    /// Bits recorded in `raw` (saturating; past [`RAW_BITS`] the window
    /// has overflowed and pre-destuff recovery is skipped).
    raw_len: usize,
    /// `raw_len` snapshot taken at the most recent closing flag, before
    /// the per-frame reset: lets the receiver bank fetch the raw window
    /// of a frame that just failed the FCS (cross-chain voting).
    #[cfg_attr(not(feature = "tnc"), allow(dead_code))]
    last_raw_len: usize,
}

impl<const N: usize> HdlcDeframer<N> {
    /// Creates an empty deframer, hunting for an opening flag.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            nbits: 0,
            cur_byte: 0,
            ones: 0,
            in_frame: false,
            overflowed: false,
            recovery: RecoveryPolicy::None,
            raw: [0; RAW_BYTES],
            raw_len: 0,
            last_raw_len: 0,
        }
    }

    /// Creates an empty deframer with an explicit FCS recovery policy.
    #[must_use]
    pub const fn with_recovery(recovery: RecoveryPolicy) -> Self {
        let mut d = Self::new();
        d.recovery = recovery;
        d
    }

    /// Resets the per-frame accumulation state.
    const fn reset_frame(&mut self) {
        self.len = 0;
        self.nbits = 0;
        self.cur_byte = 0;
        self.overflowed = false;
        self.raw_len = 0;
    }

    /// Records one raw pre-destuff line bit into the bit-packed window.
    const fn record_raw(&mut self, bit: Bit) {
        if self.raw_len < RAW_BITS {
            let byte = self.raw_len / 8;
            let mask = 1u8 << (self.raw_len % 8);
            match bit {
                Bit::One => self.raw[byte] |= mask,
                Bit::Zero => self.raw[byte] &= !mask,
            }
        }
        self.raw_len = self.raw_len.saturating_add(1);
    }

    /// Reads raw bit `index` from the window (`false` when out of range).
    const fn raw_bit(&self, index: usize) -> bool {
        if index >= RAW_BITS {
            return false;
        }
        (self.raw[index / 8] >> (index % 8)) & 1 != 0
    }

    /// Pushes one line bit (post-NRZI-decode).
    ///
    /// Returns `Some(Ok(frame))` when a closing flag completes a valid
    /// frame — the slice is the frame contents with the (verified) FCS
    /// stripped, borrowed from the internal buffer until the next push.
    /// Returns `Some(Err(_))` for a frame rejected with a diagnosable
    /// cause ([`Ax25Error::FcsMismatch`], [`Ax25Error::FrameTooLarge`]).
    /// Runts, misaligned salvage, and aborts are discarded silently
    /// (`None`).
    pub fn push(&mut self, bit: Bit) -> Option<Result<&[u8], Ax25Error>> {
        if self.in_frame {
            // Keep the raw pre-destuff window in sync with the frame in
            // progress (used by pre-destuff bit-flip recovery). The
            // closing flag's own eight bits end up recorded too; the
            // recovery pass excludes them.
            self.record_raw(bit);
        }
        match bit {
            Bit::One => {
                self.ones += 1;
                if self.ones >= 7 {
                    // Abort sequence: drop any frame in progress and hunt.
                    self.ones = 7; // saturate so long idle can't overflow
                    self.in_frame = false;
                    self.reset_frame();
                    return None;
                }
                if self.in_frame {
                    self.push_frame_bit(Bit::One);
                }
                None
            }
            Bit::Zero => {
                let ones = self.ones;
                self.ones = 0;
                match ones {
                    5 => {
                        // Stuffed zero: discard.
                        None
                    }
                    6 => {
                        // Flag. Close any frame in progress, then treat the
                        // same flag as the opener of the next frame.
                        let was_in_frame = self.in_frame;
                        self.in_frame = true;
                        if was_in_frame {
                            let result = self.close_frame();
                            self.last_raw_len = self.raw_len;
                            self.reset_frame();
                            // Re-borrow to satisfy the borrow checker: map
                            // the close result through fresh borrows.
                            return match result {
                                CloseResult::Frame(len) => {
                                    Some(Ok(self.buf.get(..len).unwrap_or(&[])))
                                }
                                CloseResult::Error(e) => Some(Err(e)),
                                CloseResult::Discard => None,
                            };
                        }
                        self.reset_frame();
                        None
                    }
                    _ => {
                        if self.in_frame {
                            self.push_frame_bit(Bit::Zero);
                        }
                        None
                    }
                }
            }
        }
    }

    /// Accumulates one frame-content bit (LSB-first into octets).
    const fn push_frame_bit(&mut self, bit: Bit) {
        if let Bit::One = bit {
            self.cur_byte |= 1 << self.nbits;
        }
        self.nbits += 1;
        if self.nbits == 8 {
            if self.len < N {
                self.buf[self.len] = self.cur_byte;
            } else {
                self.overflowed = true;
            }
            // Track the attempted length even past capacity (saturating so
            // endless garbage cannot overflow the counter).
            self.len = self.len.saturating_add(1);
            self.cur_byte = 0;
            self.nbits = 0;
        }
    }

    /// Validates the accumulated content at a closing flag.
    ///
    /// The last seven bits pushed were the flag's own opening zero and six
    /// ones; a byte-aligned frame therefore ends with exactly `nbits == 7`.
    fn close_frame(&mut self) -> CloseResult {
        if self.nbits != 7 {
            // Bit salvage that is not byte-aligned: with pre-destuff
            // recovery enabled this is the signature of a corrupted
            // stuffing bit (the destuffed tail shifted); try the bounded
            // raw-window repair before discarding.
            if let RecoveryPolicy::PreDestuffFlip = self.recovery
                && let Some(len) = self.try_predestuff()
            {
                return CloseResult::Frame(len);
            }
            return CloseResult::Discard;
        }
        if self.overflowed || self.len > N {
            return CloseResult::Error(Ax25Error::FrameTooLarge {
                len: self.len,
                max: N,
            });
        }
        // Runt: shorter than the smallest UI frame plus FCS.
        if self.len < MIN_FRAME_LEN + 2 {
            return CloseResult::Discard;
        }
        let Some(bytes) = self.buf.get(..self.len) else {
            return CloseResult::Discard;
        };
        let content_len = self.len - 2;
        let (content, fcs_bytes) = bytes.split_at(content_len);
        let expected = u16::from_le_bytes([
            fcs_bytes.first().copied().unwrap_or(0),
            fcs_bytes.get(1).copied().unwrap_or(0),
        ]);
        let mut fcs = Fcs::new();
        fcs.update_slice(content);
        let computed = fcs.finish();
        if computed == expected {
            CloseResult::Frame(content_len)
        } else {
            match self.recovery {
                RecoveryPolicy::None => {
                    CloseResult::Error(Ax25Error::FcsMismatch { expected, computed })
                }
                RecoveryPolicy::SingleBitFlip => self.try_repair(content_len, expected, computed),
                RecoveryPolicy::PreDestuffFlip => {
                    match self.try_repair(content_len, expected, computed) {
                        CloseResult::Frame(len) => CloseResult::Frame(len),
                        other => match self.try_predestuff() {
                            Some(len) => CloseResult::Frame(len),
                            None => other,
                        },
                    }
                }
            }
        }
    }

    /// Attempts single-bit-flip repair of a frame that failed the FCS.
    ///
    /// Uses CRC linearity to locate the unique single-bit flip (if any)
    /// explaining the mismatch syndrome, applies it, and accepts the
    /// frame only if the repaired contents parse as a sane AX.25 UI
    /// frame; otherwise the flip is reverted and the mismatch reported.
    fn try_repair(&mut self, content_len: usize, expected: u16, computed: u16) -> CloseResult {
        let mismatch = CloseResult::Error(Ax25Error::FcsMismatch { expected, computed });
        let Some(location) = locate_single_bit_error(content_len, expected, computed) else {
            return mismatch;
        };
        match location {
            SingleBitError::InFcs => {
                // The flip is in the transmitted FCS itself. Rejected by
                // policy: accepting would surface contents that no
                // checksum corroborates (the repair only shows the FCS
                // field was hit), the weakest-evidence case for false
                // accepts. Content repairs below are corroborated by the
                // intact 16-bit FCS. Measured on the benchmark corpus,
                // accepting this case recovered zero frames.
                mismatch
            }
            SingleBitError::InContent { index, mask } => {
                let Some(byte) = self.buf.get_mut(index) else {
                    return mismatch;
                };
                *byte ^= mask;
                match self.buf.get(..content_len) {
                    Some(content) if UiFrame::parse(content).is_ok() => {
                        CloseResult::Frame(content_len)
                    }
                    _ => {
                        // Revert: not a plausible frame after repair.
                        if let Some(byte) = self.buf.get_mut(index) {
                            *byte ^= mask;
                        }
                        mismatch
                    }
                }
            }
        }
    }

    /// Attempts bounded pre-destuff single-bit-flip recovery.
    ///
    /// The raw window holds every line bit recorded since the opening
    /// flag: the stuffed frame content followed by the closing flag's
    /// eight bits. A bit error in or near a stuffed zero changes where
    /// the receiver removes stuffing, shifting the whole destuffed tail —
    /// unreachable from the post-destuff bytes. Retry from the raw bits:
    /// flip each single content bit in turn, re-destuff, and accept the
    /// first candidate that destuffs to a byte-aligned, in-range frame
    /// with a **valid FCS** that also parses as a sane AX.25 UI frame.
    /// On success the repaired content is copied into `buf` and its
    /// length returned. Frames whose stuffed length outgrew the fixed
    /// window are skipped (never allocates).
    fn try_predestuff(&mut self) -> Option<usize> {
        let total = self.raw_len;
        if !(8..=RAW_BITS).contains(&total) {
            return None;
        }
        // Exclude the closing flag's eight bits from flipping/destuffing.
        let content_bits = total - 8;
        // Too short to ever destuff into a minimum frame + FCS: skip the
        // sweep entirely (common for inter-frame noise salvage).
        if content_bits < (MIN_FRAME_LEN + 2) * 8 {
            return None;
        }
        let mut candidate = [0u8; N];
        for flip in 0..content_bits {
            let Some(len) = self.destuff_candidate(flip, content_bits, &mut candidate) else {
                continue;
            };
            if len < MIN_FRAME_LEN + 2 || len > N {
                continue;
            }
            let Some(bytes) = candidate.get(..len) else {
                continue;
            };
            let content_len = len - 2;
            let (content, fcs_bytes) = bytes.split_at(content_len);
            let expected = u16::from_le_bytes([
                fcs_bytes.first().copied().unwrap_or(0),
                fcs_bytes.get(1).copied().unwrap_or(0),
            ]);
            if crc16_x25(content) != expected || UiFrame::parse(content).is_err() {
                continue;
            }
            // Same policy as the post-destuff `InFcs` case: when the
            // repaired content is identical to the original destuffed
            // content, the flip only "fixed" the transmitted FCS field —
            // no checksum corroborates the contents. Reject.
            if self.len >= 2
                && content_len == self.len - 2
                && matches!(self.buf.get(..content_len), Some(orig) if orig == content)
            {
                continue;
            }
            for (dst, src) in self.buf.iter_mut().zip(content.iter()) {
                *dst = *src;
            }
            return Some(content_len);
        }
        None
    }

    /// The raw pre-destuff bit window of the frame that just closed
    /// (bit-packed LSB-first) and its recorded length in bits, including
    /// the closing flag's eight bits. Only meaningful immediately after
    /// [`HdlcDeframer::push`] returned a frame event; `None` when the
    /// window overflowed or is too short to matter.
    #[cfg(feature = "tnc")]
    pub(crate) fn failed_window(&self) -> Option<(&[u8; RAW_BYTES], usize)> {
        let total = self.last_raw_len;
        if !(8..=RAW_BITS).contains(&total) {
            return None;
        }
        Some((&self.raw, total))
    }

    /// Validates a majority-voted raw bit window (cross-chain candidate
    /// voting): destuffs it, checks length, FCS and UI-frame sanity, and
    /// — when the plain destuff fails — retries with the bounded
    /// single-bit-flip sweep. On success the frame content is left in
    /// the internal buffer (fetch it with [`HdlcDeframer::frame_bytes`])
    /// and its length returned. `total` counts the window bits including
    /// the closing flag's eight bits, exactly as recorded.
    #[cfg(feature = "tnc")]
    pub(crate) fn try_voted_window(
        &mut self,
        bits: &[u8; RAW_BYTES],
        total: usize,
    ) -> Option<usize> {
        if !(8..=RAW_BITS).contains(&total) {
            return None;
        }
        let content_bits = total - 8;
        if content_bits < (MIN_FRAME_LEN + 2) * 8 {
            return None;
        }
        self.raw = *bits;
        self.raw_len = total;
        let found = self.check_voted(content_bits);
        // The deframer is mid-hunt for the next frame (its per-frame
        // state was reset at the closing flag); restore that invariant.
        self.raw_len = 0;
        self.len = 0;
        found
    }

    /// [`HdlcDeframer::try_voted_window`] body: plain destuff + FCS + UI
    /// sanity first, then one single-bit-flip pass on the voted window.
    #[cfg(feature = "tnc")]
    fn check_voted(&mut self, content_bits: usize) -> Option<usize> {
        // Plain destuff of the voted window (a flip index equal to
        // `content_bits` is out of range, so no bit is flipped).
        let mut candidate = [0u8; N];
        if let Some(len) = self.destuff_candidate(content_bits, content_bits, &mut candidate)
            && (MIN_FRAME_LEN + 2..=N).contains(&len)
            && let Some(bytes) = candidate.get(..len)
        {
            let content_len = len - 2;
            let (content, fcs_bytes) = bytes.split_at(content_len);
            let expected = u16::from_le_bytes([
                fcs_bytes.first().copied().unwrap_or(0),
                fcs_bytes.get(1).copied().unwrap_or(0),
            ]);
            if crc16_x25(content) == expected && UiFrame::parse(content).is_ok() {
                for (dst, src) in self.buf.iter_mut().zip(content.iter()) {
                    *dst = *src;
                }
                return Some(content_len);
            }
            // Seed `buf`/`len` with the plain destuff so the flip pass's
            // identity guard (rejecting repairs that only "fix" the
            // transmitted FCS field) can compare against it.
            for (dst, src) in self.buf.iter_mut().zip(bytes.iter()) {
                *dst = *src;
            }
            self.len = len;
        }
        // Voting fixed most but not quite all damage: allow one
        // single-bit-flip pass on the voted result.
        self.try_predestuff()
    }

    /// The first `len` bytes of the internal frame buffer (the frame
    /// content left by [`HdlcDeframer::try_voted_window`]).
    #[cfg(feature = "tnc")]
    pub(crate) fn frame_bytes(&self, len: usize) -> &[u8] {
        self.buf.get(..len).unwrap_or(&[])
    }

    /// Destuffs the raw window with bit `flip` inverted into `out`.
    ///
    /// Returns the destuffed byte length, or `None` when the candidate is
    /// implausible: a flag or abort pattern appears mid-window, the
    /// result is not byte-aligned, it outgrows `out`, or it ends in five
    /// or more ones (which would have swallowed the closing flag's
    /// opening zero as a stuffing bit).
    fn destuff_candidate(
        &self,
        flip: usize,
        content_bits: usize,
        out: &mut [u8; N],
    ) -> Option<usize> {
        let mut ones = 0u8;
        let mut nbits = 0u8;
        let mut cur = 0u8;
        let mut len = 0usize;
        for i in 0..content_bits {
            let bit = self.raw_bit(i) != (i == flip);
            if bit {
                ones += 1;
                if ones >= 7 {
                    return None; // abort pattern
                }
            } else {
                let run = ones;
                ones = 0;
                if run == 5 {
                    continue; // stuffed zero: discard
                }
                if run == 6 {
                    return None; // flag pattern mid-window
                }
            }
            cur |= u8::from(bit) << nbits;
            nbits += 1;
            if nbits == 8 {
                if len >= N {
                    return None;
                }
                out[len] = cur;
                len += 1;
                cur = 0;
                nbits = 0;
            }
        }
        if nbits != 0 || ones >= 5 {
            return None;
        }
        Some(len)
    }
}

impl<const N: usize> Default for HdlcDeframer<N> {
    /// Same as [`HdlcDeframer::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of validating a frame at its closing flag.
#[derive(Debug, Clone, Copy)]
enum CloseResult {
    /// A valid frame of this many content bytes (FCS stripped) is in `buf`.
    Frame(usize),
    /// A diagnosable rejection.
    Error(Ax25Error),
    /// Not a frame at all; drop silently.
    Discard,
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// Collects the stuffed data bits (no flags) for `bytes`.
    fn stuffed_bits(bytes: &[u8]) -> Vec<Bit> {
        let mut ones = 0u32;
        let mut out = Vec::new();
        for &b in bytes {
            for i in 0..8 {
                let bit = Bit::from((b >> i) & 1 != 0);
                out.push(bit);
                if bit == Bit::One {
                    ones += 1;
                    if ones == 5 {
                        out.push(Bit::Zero);
                        ones = 0;
                    }
                } else {
                    ones = 0;
                }
            }
        }
        out
    }

    fn bits_of(iter: FrameBits<'_>) -> Vec<Bit> {
        iter.collect()
    }

    fn flag_bits(n: usize) -> Vec<Bit> {
        let mut out = Vec::new();
        for _ in 0..n {
            for i in 0..8 {
                out.push(Bit::from((FLAG >> i) & 1 != 0));
            }
        }
        out
    }

    /// Runs a bit sequence through a deframer, collecting owned results.
    fn deframe_all<const N: usize>(bits: &[Bit]) -> Vec<Result<Vec<u8>, Ax25Error>> {
        let mut d = HdlcDeframer::<N>::new();
        let mut out = Vec::new();
        for &b in bits {
            if let Some(r) = d.push(b) {
                out.push(r.map(<[u8]>::to_vec));
            }
        }
        out
    }

    /// A minimum-size valid payload (16 bytes, like dest+src+ctrl+pid).
    fn min_payload() -> Vec<u8> {
        (0u8..16).collect()
    }

    #[test]
    fn frame_bits_layout_matches_reference_stuffing() {
        let payload = min_payload();
        let bits = bits_of(frame_bits(&payload, 3, 2));
        let mut expected = flag_bits(3);
        let fcs = crc16_x25(&payload);
        let mut with_fcs = payload.clone();
        with_fcs.extend_from_slice(&fcs.to_le_bytes());
        expected.extend(stuffed_bits(&with_fcs));
        expected.extend(flag_bits(2));
        assert_eq!(bits, expected);
    }

    #[test]
    fn stuffing_after_exactly_five_ones() {
        // 0b0001_1111 -> five ones then a stuffed zero.
        let bits = bits_of(frame_bits(&[0x1F], 0, 0));
        // Data portion is 8 payload bits + 1 stuffed + 16 FCS bits (+ any
        // FCS stuffing). Check the first nine bits.
        let head: Vec<Bit> = bits.iter().copied().take(9).collect();
        assert_eq!(
            head,
            [
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::Zero, // stuffed
                Bit::Zero,
                Bit::Zero,
                Bit::Zero,
            ]
        );
    }

    #[test]
    fn five_ones_at_end_of_data_still_stuffed() {
        // Craft input whose FINAL FCS bits end in five ones? Simpler: rely
        // on the iterator: 0xF8 = 0001_1111 read LSB-first is 0,0,0,1,1,1,1,1
        // — five ones at the very end of the byte must be followed by a
        // stuffed zero before the next (FCS) byte's bits.
        let bits = bits_of(frame_bits(&[0xF8], 0, 0));
        let head: Vec<Bit> = bits.iter().copied().take(9).collect();
        assert_eq!(
            head,
            [
                Bit::Zero,
                Bit::Zero,
                Bit::Zero,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::Zero, // stuffed straddling the byte boundary
            ]
        );
    }

    #[test]
    fn payload_flag_byte_is_stuffed_away() {
        // 0x7E in the payload must never appear as eight consecutive
        // unstuffed flag bits in the data section.
        let payload = [0x7E, 0x7E, 0xFF, 0x7E];
        let bits = bits_of(frame_bits(&payload, 1, 1));
        // Strip the single leading and trailing flag; the interior must not
        // contain the flag pattern 0,1,1,1,1,1,1,0.
        let interior = &bits[8..bits.len() - 8];
        let flag: Vec<Bit> = flag_bits(1);
        assert!(
            !interior.windows(8).any(|w| w == flag.as_slice()),
            "flag pattern leaked into stuffed data"
        );
    }

    #[test]
    fn roundtrip_through_deframer() {
        let payload = min_payload();
        let bits = bits_of(frame_bits(&payload, 4, 2));
        let frames = deframe_all::<64>(&bits);
        assert_eq!(frames, [Ok(payload)]);
    }

    #[test]
    fn roundtrip_stuffing_heavy_payload() {
        let mut payload = min_payload();
        payload.extend_from_slice(&[0xFF, 0xFF, 0x7E, 0xFF, 0x1F, 0xF8, 0x00]);
        let bits = bits_of(frame_bits(&payload, 2, 2));
        let frames = deframe_all::<64>(&bits);
        assert_eq!(frames, [Ok(payload)]);
    }

    #[test]
    fn back_to_back_frames_share_a_flag() {
        let a = min_payload();
        let mut b = min_payload();
        b.push(0xA5);
        let mut bits = bits_of(frame_bits(&a, 2, 0));
        // Single shared flag, then the second frame with no preamble.
        bits.extend(flag_bits(1));
        bits.extend(bits_of(frame_bits(&b, 0, 1)));
        let frames = deframe_all::<64>(&bits);
        assert_eq!(frames, [Ok(a), Ok(b)]);
    }

    #[test]
    fn corrupted_fcs_is_reported() {
        let payload = min_payload();
        let mut bits = bits_of(frame_bits(&payload, 2, 1));
        // Flip one payload bit inside the data section (after the 2 flags).
        let idx = 2 * 8 + 3;
        bits[idx] = match bits[idx] {
            Bit::Zero => Bit::One,
            Bit::One => Bit::Zero,
        };
        let frames = deframe_all::<64>(&bits);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0], Err(Ax25Error::FcsMismatch { .. })));
    }

    #[test]
    fn oversize_frame_is_reported() {
        let payload: Vec<u8> = (0..40).map(|i| i as u8).collect();
        let bits = bits_of(frame_bits(&payload, 1, 1));
        let frames = deframe_all::<24>(&bits);
        assert_eq!(frames, [Err(Ax25Error::FrameTooLarge { len: 42, max: 24 })]);
    }

    #[test]
    fn runt_frames_discarded_silently() {
        // 4 bytes of content + FCS: valid FCS but below the AX.25 minimum.
        let payload = [1u8, 2, 3, 4];
        let bits = bits_of(frame_bits(&payload, 2, 2));
        assert_eq!(deframe_all::<64>(&bits), []);
    }

    #[test]
    fn garbage_bits_never_panic_and_yield_nothing_valid() {
        // Deterministic pseudo-random garbage (xorshift32).
        let mut state = 0x0BAD_5EED_u32 | 1;
        let mut d = HdlcDeframer::<32>::new();
        for _ in 0..100_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let bit = Bit::from(state & 1 != 0);
            if let Some(Ok(frame)) = d.push(bit) {
                // Astronomically unlikely; would require a valid FCS.
                assert!(frame.len() >= MIN_FRAME_LEN);
            }
        }
        // A frame following the garbage must still decode.
        let payload = min_payload();
        let bits = bits_of(frame_bits(&payload, 8, 2));
        let mut got = Vec::new();
        for b in bits {
            if let Some(Ok(frame)) = d.push(b) {
                got.push(frame.to_vec());
            }
        }
        assert_eq!(got, [payload]);
    }

    #[test]
    fn abort_sequence_discards_frame_in_progress() {
        let payload = min_payload();
        let bits = bits_of(frame_bits(&payload, 2, 0));
        let mut d = HdlcDeframer::<64>::new();
        // Feed all but the closing region, then an abort (7+ ones), then a
        // fresh complete frame.
        for &b in &bits[..bits.len() - 4] {
            assert!(d.push(b).is_none());
        }
        for _ in 0..10 {
            assert!(d.push(Bit::One).is_none());
        }
        let fresh = bits_of(frame_bits(&payload, 2, 1));
        let mut got = Vec::new();
        for b in fresh {
            if let Some(r) = d.push(b) {
                got.push(r.map(<[u8]>::to_vec));
            }
        }
        assert_eq!(got, [Ok(payload)]);
    }

    #[test]
    fn defaults_documented_values() {
        assert_eq!(DEFAULT_PREAMBLE_FLAGS, 32);
        assert_eq!(DEFAULT_TAIL_FLAGS, 2);
        assert_eq!(FLAG, 0x7E);
        assert_eq!(RecoveryPolicy::default(), RecoveryPolicy::None);
    }

    /// A valid UI frame body (parses through the repair sanity gate).
    fn ui_frame_body() -> Vec<u8> {
        use crate::ax25::Address;
        let dest = Address::new(b"APRS", 0).unwrap();
        let src = Address::new(b"N0CALL", 7).unwrap();
        let frame = UiFrame::new(dest, src, b"!4903.50N/07201.75W-test");
        let mut buf = [0u8; 64];
        let len = frame.build(&mut buf).unwrap();
        buf[..len].to_vec()
    }

    /// Deframes `bits` with the given recovery policy.
    fn deframe_with_recovery<const N: usize>(
        bits: &[Bit],
        recovery: RecoveryPolicy,
    ) -> Vec<Result<Vec<u8>, Ax25Error>> {
        let mut d = HdlcDeframer::<N>::with_recovery(recovery);
        let mut out = Vec::new();
        for &b in bits {
            if let Some(r) = d.push(b) {
                out.push(r.map(<[u8]>::to_vec));
            }
        }
        out
    }

    /// Corrupts the pre-stuffing frame+FCS bytes by XORing bit masks,
    /// then serializes to line bits (2 preamble, 2 tail flags).
    fn corrupted_bits(body: &[u8], flips: &[(usize, u8)]) -> Vec<Bit> {
        let fcs = crc16_x25(body);
        let mut raw = body.to_vec();
        raw.extend_from_slice(&fcs.to_le_bytes());
        for &(i, mask) in flips {
            raw[i] ^= mask;
        }
        let mut bits = flag_bits(2);
        bits.extend(stuffed_bits(&raw));
        bits.extend(flag_bits(2));
        bits
    }

    #[test]
    fn single_bit_content_corruption_is_repaired() {
        let body = ui_frame_body();
        // Flip one bit in the info field (past the 16-byte header).
        let bits = corrupted_bits(&body, &[(18, 0x10)]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::SingleBitFlip);
        assert_eq!(frames, [Ok(body)]);
    }

    #[test]
    fn single_bit_fcs_field_corruption_is_rejected() {
        let body = ui_frame_body();
        // Flip one bit of the transmitted FCS itself: rejected by policy
        // (no checksum evidence would corroborate the contents).
        let bits = corrupted_bits(&body, &[(body.len() + 1, 0x04)]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::SingleBitFlip);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0], Err(Ax25Error::FcsMismatch { .. })));
    }

    #[test]
    fn every_single_bit_position_is_repaired() {
        let body = ui_frame_body();
        for i in 0..body.len() {
            for k in 0..8 {
                let bits = corrupted_bits(&body, &[(i, 1 << k)]);
                let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::SingleBitFlip);
                assert_eq!(frames, [Ok(body.clone())], "byte {i} bit {k}");
            }
        }
    }

    #[test]
    fn two_bit_corruption_is_not_accepted() {
        let body = ui_frame_body();
        // Two flips in different bytes: no single-bit syndrome matches
        // (or if one aliases, the UI sanity gate must reject it). Either
        // way nothing may be emitted as Ok with wrong contents.
        let bits = corrupted_bits(&body, &[(3, 0x02), (20, 0x40)]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::SingleBitFlip);
        assert_eq!(frames.len(), 1);
        assert!(
            matches!(frames[0], Err(Ax25Error::FcsMismatch { .. })),
            "two-bit corruption must be rejected, got {:?}",
            frames[0]
        );
    }

    #[test]
    fn policy_none_rejects_single_bit_corruption() {
        let body = ui_frame_body();
        let bits = corrupted_bits(&body, &[(18, 0x10)]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::None);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0], Err(Ax25Error::FcsMismatch { .. })));
    }

    #[test]
    fn clean_frames_unchanged_under_recovery() {
        let body = ui_frame_body();
        let bits = corrupted_bits(&body, &[]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::SingleBitFlip);
        assert_eq!(frames, [Ok(body)]);
    }

    /// A UI frame body whose info field forces bit stuffing (runs of
    /// five+ ones).
    fn stuffy_ui_frame_body() -> Vec<u8> {
        use crate::ax25::Address;
        let dest = Address::new(b"APRS", 0).unwrap();
        let src = Address::new(b"N0CALL", 7).unwrap();
        let frame = UiFrame::new(dest, src, b"!test\xff\xff\x1f\xf8data");
        let mut buf = [0u8; 64];
        let len = frame.build(&mut buf).unwrap();
        buf[..len].to_vec()
    }

    #[test]
    fn predestuff_repairs_corrupted_stuffing_run() {
        // Flip a ONE inside the run of five ones that precedes a stuffed
        // zero: the receiver then fails to discard the stuffed zero and
        // the whole destuffed tail shifts — unreachable for the
        // post-destuff syndrome repair, recovered by the pre-destuff
        // retry.
        let body = stuffy_ui_frame_body();
        let fcs = crc16_x25(&body);
        let mut raw = body.clone();
        raw.extend_from_slice(&fcs.to_le_bytes());
        let data = stuffed_bits(&raw);
        // Locate the first stuffed zero: five ones then a zero inserted.
        let mut ones = 0usize;
        let mut stuffed_at = None;
        for (i, &b) in data.iter().enumerate() {
            if b == Bit::One {
                ones += 1;
                if ones == 5 {
                    stuffed_at = Some(i + 1);
                    break;
                }
            } else {
                ones = 0;
            }
        }
        let stuffed_at = stuffed_at.expect("payload must force stuffing");
        // Corrupt the last ONE of the run (line index: after 2 flags).
        let mut bits = flag_bits(2);
        bits.extend(data);
        bits.extend(flag_bits(2));
        let idx = 16 + stuffed_at - 1;
        assert_eq!(bits[idx], Bit::One);
        bits[idx] = Bit::Zero;

        // Post-destuff-only repair cannot fix it...
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::SingleBitFlip);
        assert!(
            !frames.contains(&Ok(body.clone())),
            "syndrome repair unexpectedly fixed a stuffing error"
        );
        // ...the pre-destuff retry does.
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::PreDestuffFlip);
        assert!(
            frames.contains(&Ok(body)),
            "pre-destuff repair failed: {frames:?}"
        );
    }

    #[test]
    fn predestuff_clean_frames_unchanged() {
        let body = ui_frame_body();
        let bits = corrupted_bits(&body, &[]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::PreDestuffFlip);
        assert_eq!(frames, [Ok(body)]);
    }

    #[test]
    fn predestuff_rejects_fcs_field_flip() {
        // A flip confined to the transmitted FCS bytes must stay
        // rejected: repairing it would leave contents no checksum
        // corroborates.
        let body = ui_frame_body();
        let bits = corrupted_bits(&body, &[(body.len() + 1, 0x04)]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::PreDestuffFlip);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0], Err(Ax25Error::FcsMismatch { .. })));
    }

    #[test]
    fn predestuff_repairs_single_content_bit_too() {
        // Plain content-bit damage is also reachable from the raw window.
        let body = ui_frame_body();
        let bits = corrupted_bits(&body, &[(18, 0x10)]);
        let frames = deframe_with_recovery::<64>(&bits, RecoveryPolicy::PreDestuffFlip);
        assert_eq!(frames, [Ok(body)]);
    }
}
