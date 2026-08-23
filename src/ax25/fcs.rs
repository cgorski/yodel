//! CRC-16/X.25 frame check sequence.
//!
//! AX.25 protects each frame with the HDLC FCS, known as CRC-16/X.25:
//! width 16, polynomial `0x1021` reflected (`0x8408`), initial value
//! `0xFFFF`, input and output reflected, final XOR `0xFFFF`. The check
//! value for `b"123456789"` is `0x906E`. On the wire the FCS is appended
//! little-endian (low byte first).
//!
//! # Specification
//!
//! AX.25 2.2 §3.7 specifies the FCS and cites ISO 3309 for it:
//!
//! > ISO/IEC 3309:1993, "Information technology — Telecommunications and
//! > information exchange between systems — High-level data link control
//! > (HDLC) procedures — Frame structure", 5th edition, December 1993.
//! > (Withdrawn; superseded by ISO/IEC 13239:2002.)
//!
//! The same CRC is specified by ITU-T Recommendation X.25 §2.2.7.4, and
//! RFC 1662 ("PPP in HDLC-like Framing", §3.1 and Appendix C.2) prints a
//! freely readable implementation of it — the practical reference, since
//! the ISO and ITU documents are paywalled.
//!
//! "Appended little-endian" and AX.25 §3.8's "FCS transmitted MSB first"
//! describe the same wire bytes at different layers; the byte order here
//! is the one that makes the receiver's HDLC residue come out `0xF0B8`.

/// Streaming CRC-16/X.25 accumulator.
///
/// Feed bytes with [`Fcs::update`] / [`Fcs::update_slice`], then read the
/// final value with [`Fcs::finish`]. For one-shot use see [`crc16_x25`].
///
/// # Known-answer check value
///
/// The canonical CRC-16/X.25 check value for `b"123456789"` is `0x906E`;
/// streaming and one-shot use agree, and on the wire the result is
/// appended low byte first:
///
/// ```
/// use warble::ax25::fcs::{Fcs, crc16_x25};
///
/// assert_eq!(crc16_x25(b"123456789"), 0x906E);
///
/// let mut fcs = Fcs::new();
/// fcs.update_slice(b"1234");
/// fcs.update_slice(b"56789"); // arbitrary chunking, same result
/// assert_eq!(fcs.finish(), 0x906E);
///
/// // AX.25 appends the FCS little-endian: 0x6E then 0x90.
/// assert_eq!(0x906E_u16.to_le_bytes(), [0x6E, 0x90]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fcs {
    crc: u16,
}

impl Fcs {
    /// Creates a fresh accumulator (initial value `0xFFFF`).
    #[must_use]
    pub const fn new() -> Self {
        Self { crc: 0xFFFF }
    }

    /// Folds one byte into the running CRC.
    pub const fn update(&mut self, byte: u8) {
        let mut crc = self.crc ^ (byte as u16);
        let mut i = 0;
        while i < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
            i += 1;
        }
        self.crc = crc;
    }

    /// Folds a slice of bytes into the running CRC.
    pub const fn update_slice(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            self.update(bytes[i]);
            i += 1;
        }
    }

    /// The finalized FCS (running value XOR `0xFFFF`).
    ///
    /// Does not consume the accumulator; more bytes may still be fed.
    #[must_use]
    pub const fn finish(&self) -> u16 {
        self.crc ^ 0xFFFF
    }
}

impl Default for Fcs {
    /// Same as [`Fcs::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Computes the CRC-16/X.25 of `bytes` in one shot.
#[must_use]
pub const fn crc16_x25(bytes: &[u8]) -> u16 {
    let mut fcs = Fcs::new();
    fcs.update_slice(bytes);
    fcs.finish()
}

/// Location of a single-bit error identified by
/// [`locate_single_bit_error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleBitError {
    /// The flipped bit is in the transmitted FCS itself; the frame
    /// contents are intact and need no change.
    InFcs,
    /// The flipped bit is in the frame contents: XOR `mask` into the
    /// byte at `index` to repair the frame.
    InContent {
        /// Byte offset of the corrupted byte within the frame contents.
        index: usize,
        /// Single-bit mask to XOR into that byte.
        mask: u8,
    },
}

/// Advances a CRC register *difference* through one zero-input byte.
///
/// The CRC is linear over GF(2): if two register states differ by
/// `delta`, absorbing the same byte into both leaves them differing by
/// `delta` run through the eight shift/feedback steps with no input —
/// the input contribution cancels. This lets a whole-frame syndrome be
/// matched against every possible single-bit flip in O(1) per bit.
const fn propagate_zero_byte(mut delta: u16) -> u16 {
    let mut i = 0;
    while i < 8 {
        delta = if delta & 1 != 0 {
            (delta >> 1) ^ 0x8408
        } else {
            delta >> 1
        };
        i += 1;
    }
    delta
}

/// Locates the single bit flip, if any, that explains an FCS mismatch.
///
/// `content_len` is the frame content length in bytes (FCS excluded),
/// `expected` the FCS carried by the frame, `computed` the FCS computed
/// over the received contents. By CRC linearity, flipping input bit `k`
/// of content byte `j` XORs the final CRC by a syndrome that depends only
/// on the bit's distance from the end of the frame; flipping a bit of the
/// transmitted FCS XORs `expected` by a single bit. This scans all
/// `8 * content_len + 16` candidate positions in O(content_len) total
/// work and returns the first match, or `None` when no single-bit flip
/// explains the mismatch (or the FCS already matches).
#[must_use]
pub fn locate_single_bit_error(
    content_len: usize,
    expected: u16,
    computed: u16,
) -> Option<SingleBitError> {
    // The initial value and final XOR (both 0xFFFF) cancel in the
    // difference, leaving a pure linear syndrome.
    let syndrome = expected ^ computed;
    if syndrome == 0 {
        return None;
    }
    // A flipped bit of the transmitted FCS changes `expected` by exactly
    // one bit and leaves the contents intact.
    if syndrome.count_ones() == 1 {
        return Some(SingleBitError::InFcs);
    }
    // Content bits, scanned from the last byte backwards: the syndrome of
    // bit k in byte j is `1 << k` propagated through the `content_len - j`
    // zero-input byte updates from that byte (inclusive) to the end.
    let mut deltas: [u16; 8] = [0; 8];
    for (k, delta) in deltas.iter_mut().enumerate() {
        *delta = propagate_zero_byte(1u16 << k);
    }
    let mut j = content_len;
    while j > 0 {
        j -= 1;
        for (k, delta) in deltas.iter_mut().enumerate() {
            if *delta == syndrome {
                return Some(SingleBitError::InContent {
                    index: j,
                    mask: 1u8 << k,
                });
            }
            *delta = propagate_zero_byte(*delta);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        // The canonical CRC-16/X.25 check value.
        assert_eq!(crc16_x25(b"123456789"), 0x906E);
    }

    #[test]
    fn known_vectors() {
        assert_eq!(crc16_x25(b""), 0x0000);
        assert_eq!(crc16_x25(&[0x00]), 0xF078);
        assert_eq!(crc16_x25(&[0xFF]), 0xFF00);
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"the quick brown fox";
        let mut fcs = Fcs::new();
        for &b in &data[..7] {
            fcs.update(b);
        }
        fcs.update_slice(&data[7..]);
        assert_eq!(fcs.finish(), crc16_x25(data));
    }

    #[test]
    fn finish_is_non_consuming() {
        let mut fcs = Fcs::new();
        fcs.update(0x12);
        let first = fcs.finish();
        assert_eq!(fcs.finish(), first);
        fcs.update(0x34);
        assert_eq!(fcs.finish(), crc16_x25(&[0x12, 0x34]));
    }

    #[test]
    fn default_equals_new() {
        assert_eq!(Fcs::default(), Fcs::new());
    }
}
