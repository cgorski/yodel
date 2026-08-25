//! Test-only AX.25 / HDLC protocol helpers.
//!
//! Written from the public AX.25 v2.2, HDLC (ISO 13239), and CRC-CCITT
//! specifications. Used by integration tests that validate the modem
//! against a reference implementation, and unit-tested below.

#![allow(dead_code)]

use yodel::Bit;

/// NRZI encode (NRZI-S as used by AX.25): a `Zero` data bit produces a
/// transition of the line level, a `One` keeps the previous level.
/// The line starts at `One` (idle/mark).
pub fn nrzi_encode(bits: &[Bit]) -> Vec<Bit> {
    let mut level = Bit::One;
    bits.iter()
        .map(|b| {
            if *b == Bit::Zero {
                level = match level {
                    Bit::Zero => Bit::One,
                    Bit::One => Bit::Zero,
                };
            }
            level
        })
        .collect()
}

/// NRZI decode: a transition means data bit `Zero`, no transition means `One`.
/// The initial line level is assumed to be `One`.
pub fn nrzi_decode(bits: &[Bit]) -> Vec<Bit> {
    let mut prev = Bit::One;
    bits.iter()
        .map(|b| {
            let out = if *b == prev { Bit::One } else { Bit::Zero };
            prev = *b;
            out
        })
        .collect()
}

/// CRC-16/X.25 frame check sequence: reflected polynomial 0x1021,
/// init 0xFFFF, final XOR 0xFFFF. Implemented bitwise from the CRC
/// definition (LSB-first over each byte, reflected polynomial 0x8408).
pub fn fcs_crc16_x25(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x8408;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ 0xFFFF
}

/// Convert bytes to bits, LSB first per byte (HDLC bit order).
pub fn bytes_to_bits_lsb(bytes: &[u8]) -> Vec<Bit> {
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for &b in bytes {
        for i in 0..8 {
            bits.push(if (b >> i) & 1 != 0 {
                Bit::One
            } else {
                Bit::Zero
            });
        }
    }
    bits
}

/// Convert bits (LSB first per byte) back to bytes. Extra trailing bits
/// that do not fill a whole byte are discarded.
pub fn bits_to_bytes_lsb(bits: &[Bit]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|chunk| {
            let mut b = 0u8;
            for (i, bit) in chunk.iter().enumerate() {
                if *bit == Bit::One {
                    b |= 1 << i;
                }
            }
            b
        })
        .collect()
}

/// HDLC-frame a payload (which must already include the FCS): apply bit
/// stuffing (insert a `Zero` after five consecutive `One`s) and surround
/// with opening/closing 0x7E flags.
pub fn hdlc_frame(
    frame_bytes_with_fcs: &[u8],
    leading_flags: usize,
    trailing_flags: usize,
) -> Vec<Bit> {
    let flag = bytes_to_bits_lsb(&[0x7E]);
    let mut out = Vec::new();
    for _ in 0..leading_flags {
        out.extend_from_slice(&flag);
    }
    let mut ones = 0u32;
    for bit in bytes_to_bits_lsb(frame_bytes_with_fcs) {
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
    for _ in 0..trailing_flags {
        out.extend_from_slice(&flag);
    }
    out
}

/// Scan a decoded (post-NRZI) bitstream for HDLC frames delimited by 0x7E
/// flags, destuff the contents, and return every candidate whose FCS
/// verifies. Returned frames exclude the 2-byte FCS.
pub fn hdlc_deframe(bits: &[Bit]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    // Track last 8 bits to detect the flag pattern 0b01111110 (LSB first
    // on the wire: 0,1,1,1,1,1,1,0).
    let mut window: u8 = 0;
    let mut collecting: Option<Vec<Bit>> = None;
    let mut ones = 0u32;
    for (idx, bit) in bits.iter().enumerate() {
        window = (window >> 1) | if *bit == Bit::One { 0x80 } else { 0 };
        let is_flag_boundary = idx >= 7 && window == 0x7E;
        if is_flag_boundary {
            if let Some(mut content) = collecting.take() {
                // Remove the 7 flag bits that were pushed into content.
                let keep = content.len().saturating_sub(7);
                content.truncate(keep);
                if let Some(frame) = destuff_and_check(&content) {
                    frames.push(frame);
                }
            }
            collecting = Some(Vec::new());
            ones = 0;
            continue;
        }
        if let Some(content) = collecting.as_mut() {
            content.push(*bit);
            if *bit == Bit::One {
                ones += 1;
                if ones > 6 {
                    // Abort sequence: discard current frame.
                    collecting = None;
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    frames
}

/// Destuff a stuffed bit sequence, convert to bytes, and verify the FCS.
/// Returns the frame bytes without the FCS if valid.
fn destuff_and_check(stuffed: &[Bit]) -> Option<Vec<u8>> {
    let mut bits = Vec::with_capacity(stuffed.len());
    let mut ones = 0u32;
    let mut skip_next_zero = false;
    for bit in stuffed {
        if skip_next_zero {
            skip_next_zero = false;
            if *bit == Bit::Zero {
                ones = 0;
                continue;
            }
            // Five ones followed by a one would be a flag/abort; invalid here.
            return None;
        }
        bits.push(*bit);
        if *bit == Bit::One {
            ones += 1;
            if ones == 5 {
                skip_next_zero = true;
            }
        } else {
            ones = 0;
        }
    }
    if bits.len() % 8 != 0 {
        return None;
    }
    let bytes = bits_to_bytes_lsb(&bits);
    if bytes.len() < 4 {
        return None;
    }
    let (frame, fcs_bytes) = bytes.split_at(bytes.len() - 2);
    let expect = u16::from(fcs_bytes[0]) | (u16::from(fcs_bytes[1]) << 8);
    if fcs_crc16_x25(frame) == expect {
        Some(frame.to_vec())
    } else {
        None
    }
}

/// Encode one AX.25 address field entry: callsign left-shifted one bit,
/// space padded to six characters, plus an SSID byte. `last` sets the
/// address-extension bit that terminates the address field.
pub fn ax25_address(callsign: &str, ssid: u8, last: bool) -> [u8; 7] {
    assert!(callsign.len() <= 6, "callsign too long");
    assert!(ssid <= 15, "ssid out of range");
    let mut out = [b' ' << 1; 7];
    for (i, c) in callsign.bytes().enumerate() {
        out[i] = c << 1;
    }
    // SSID byte: 011 SSID x, with bit 0 the extension bit.
    out[6] = 0b0110_0000 | (ssid << 1) | u8::from(last);
    out
}

/// Build an unnumbered-information (UI) frame: destination, source,
/// control 0x03, PID 0xF0, then the information field. FCS not included.
pub fn ax25_ui_frame(dest: &str, dest_ssid: u8, src: &str, src_ssid: u8, info: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&ax25_address(dest, dest_ssid, false));
    frame.extend_from_slice(&ax25_address(src, src_ssid, true));
    frame.push(0x03);
    frame.push(0xF0);
    frame.extend_from_slice(info);
    frame
}

/// Parse the addresses and info field out of a UI frame (no FCS).
/// Returns (dest, source, info) with callsigns rendered as "CALL-SSID"
/// (or just "CALL" when the SSID is zero).
pub fn ax25_parse_ui(frame: &[u8]) -> Option<(String, String, Vec<u8>)> {
    if frame.len() < 16 {
        return None;
    }
    let dest = decode_address(&frame[0..7]);
    let src = decode_address(&frame[7..14]);
    // Skip any digipeater addresses until the extension bit is set.
    let mut idx = 14;
    if frame[13] & 1 == 0 {
        while idx + 7 <= frame.len() {
            let ext = frame[idx + 6] & 1;
            idx += 7;
            if ext == 1 {
                break;
            }
        }
    }
    if idx + 2 > frame.len() {
        return None;
    }
    if frame[idx] != 0x03 || frame[idx + 1] != 0xF0 {
        return None;
    }
    Some((dest, src, frame[idx + 2..].to_vec()))
}

fn decode_address(field: &[u8]) -> String {
    let call: String = field[..6]
        .iter()
        .map(|b| (b >> 1) as char)
        .collect::<String>()
        .trim_end()
        .to_string();
    let ssid = (field[6] >> 1) & 0x0F;
    if ssid == 0 {
        call
    } else {
        format!("{call}-{ssid}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vectors() {
        // CRC-16/X.25 check value for "123456789" is 0x906E.
        assert_eq!(fcs_crc16_x25(b"123456789"), 0x906E);
        assert_eq!(fcs_crc16_x25(b""), 0x0000);
        assert_eq!(fcs_crc16_x25(&[0x00]), 0xF078);
    }

    #[test]
    fn nrzi_round_trip() {
        let data: Vec<Bit> = bytes_to_bits_lsb(&[0x7E, 0x00, 0xFF, 0xA5, 0x3C]);
        let encoded = nrzi_encode(&data);
        assert_eq!(nrzi_decode(&encoded), data);
    }

    #[test]
    fn stuff_destuff_round_trip() {
        let payload = [0xFF, 0xFF, 0x7E, 0x00, 0xAA, 0xFF];
        let mut with_fcs = payload.to_vec();
        let fcs = fcs_crc16_x25(&payload);
        with_fcs.push((fcs & 0xFF) as u8);
        with_fcs.push((fcs >> 8) as u8);
        let framed = hdlc_frame(&with_fcs, 3, 3);
        let frames = hdlc_deframe(&framed);
        assert_eq!(frames, vec![payload.to_vec()]);
    }

    #[test]
    fn ui_frame_round_trip() {
        let frame = ax25_ui_frame("APRS", 0, "N0CALL", 7, b"hello");
        let (dest, src, info) = ax25_parse_ui(&frame).unwrap();
        assert_eq!(dest, "APRS");
        assert_eq!(src, "N0CALL-7");
        assert_eq!(info, b"hello");
    }

    #[test]
    fn full_bitstream_round_trip() {
        let frame = ax25_ui_frame("APRS", 0, "N0CALL", 1, b"test payload 123");
        let mut with_fcs = frame.clone();
        let fcs = fcs_crc16_x25(&frame);
        with_fcs.push((fcs & 0xFF) as u8);
        with_fcs.push((fcs >> 8) as u8);
        let framed = hdlc_frame(&with_fcs, 8, 2);
        let line = nrzi_encode(&framed);
        let back = nrzi_decode(&line);
        let frames = hdlc_deframe(&back);
        assert_eq!(frames, vec![frame]);
    }
}

// ---------------------------------------------------------------------
// Rebuild asymmetry: what kind of difference is this?
// ---------------------------------------------------------------------

/// How a re-serialized packet compares with the bytes it was parsed
/// from.
///
/// # Why this is a classification and not a boolean
///
/// The obvious question to ask of a decoder is "does it write back what
/// it read", and the obvious way to score it is to count byte-identical
/// rebuilds. That number is misleading in both directions, so this
/// enum splits it into the four cases that mean different things.
///
/// Pushing the byte-identical count toward 100% is not the goal, and
/// treating it as the goal actively damages the crate: it would mean
/// re-emitting whatever the sender sent, including the forms the
/// specification forbids, so that the builder transmits malformed
/// packets in order to improve a diagnostic. It would also blind the
/// diagnostic. The reason a rebuild comparison can detect a *misread*
/// value at all is that the builder writes what the parser understood:
/// when telemetry above 255 was clamped, the rebuild said 255 where the
/// wire said 510 and the defect was visible. A builder that echoed its
/// input would have reproduced 510 and hidden it.
///
/// So the rule this enum encodes is: **build canonically, and classify
/// the differences.** Symmetry is the goal wherever both spellings are
/// legal, and asymmetry is correct wherever the sender's was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Asymmetry {
    /// Byte for byte identical. The ideal.
    Exact,
    /// Differs only by a line terminator the sender should not have
    /// sent.
    ///
    /// Chapter 14: "Do not put any carriage return (0x0d) or line feed
    /// (0x0a) at the end", and it adds that igates strip them
    /// "resulting in slightly different contents", so the
    /// specification expects this difference to exist. Correct.
    NormalisedTerminator,
    /// Differs only in the case of a letter.
    ///
    /// Chapter 6 specifies "the upper case letter N for north or S for
    /// south". Lower case is accepted on receive because rejecting a
    /// position over the case of one letter would discard a good fix,
    /// and upper case is what goes back out. Correct.
    NormalisedCase,
    /// Differs while both spellings are legal, so this crate rewrote a
    /// valid packet into a different valid packet.
    ///
    /// A defect. Chapter 12 permits the weather parameters in any
    /// order, so emitting a fixed order changes bytes that nobody
    /// asked to have changed.
    Rewritten,
    /// The rebuild does not parse back to the value it was built from.
    ///
    /// The worst outcome, and the only one that loses information
    /// rather than re-spelling it. Distinct from `Rewritten` because a
    /// rewrite is cosmetic and this is not.
    ValueChanged,
    /// The packet could not be re-serialized at all.
    BuildFailed,
}

impl Asymmetry {
    /// Whether this outcome is correct behaviour.
    ///
    /// Exact and both normalisations are; the rest are defects.
    #[must_use]
    pub fn is_acceptable(self) -> bool {
        matches!(
            self,
            Asymmetry::Exact | Asymmetry::NormalisedTerminator | Asymmetry::NormalisedCase
        )
    }
}

/// Classifies `built` against the `wire` bytes it should reproduce.
///
/// `reparses_equal` says whether the built bytes parse back to the same
/// typed value; the caller supplies it because only the caller knows
/// the packet type. Pass `true` when the value survived.
#[must_use]
pub fn classify(wire: &[u8], built: &[u8], reparses_equal: bool) -> Asymmetry {
    if !reparses_equal {
        return Asymmetry::ValueChanged;
    }
    if wire == built {
        return Asymmetry::Exact;
    }
    // Case before terminator, and each checked with and without the
    // other, because the two normalisations are independent and either
    // may apply alone. A position report is the case that forces this:
    // its comment carries the trailing CR through to the rebuild, so
    // the terminator is NOT stripped there, and comparing a
    // terminator-stripped wire against a terminator-carrying rebuild
    // reports a case difference as a rewrite.
    if wire.eq_ignore_ascii_case(built) {
        return Asymmetry::NormalisedCase;
    }
    let stripped = strip_terminator(wire);
    if stripped == built {
        return Asymmetry::NormalisedTerminator;
    }
    if stripped.eq_ignore_ascii_case(built) {
        return Asymmetry::NormalisedCase;
    }
    Asymmetry::Rewritten
}

/// Drops up to one trailing CR, LF or CR LF.
#[must_use]
pub fn strip_terminator(bytes: &[u8]) -> &[u8] {
    let bytes = match bytes {
        [rest @ .., b'\r' | b'\n'] => rest,
        all => all,
    };
    match bytes {
        [rest @ .., b'\r'] => rest,
        all => all,
    }
}

#[cfg(test)]
mod asymmetry_tests {
    use super::*;

    #[test]
    fn each_class_is_reachable_and_distinct() {
        assert_eq!(classify(b"abc", b"abc", true), Asymmetry::Exact);
        assert_eq!(
            classify(b"abc\r", b"abc", true),
            Asymmetry::NormalisedTerminator
        );
        assert_eq!(
            classify(b"4903.50n", b"4903.50N", true),
            Asymmetry::NormalisedCase
        );
        // Both normalisations at once still reads as a normalisation.
        assert_eq!(
            classify(b"4903.50n\r", b"4903.50N", true),
            Asymmetry::NormalisedCase
        );
        assert_eq!(
            classify(b"h38b10161", b"b10161h38", true),
            Asymmetry::Rewritten
        );
        // A value that did not survive outranks any spelling question,
        // including a rebuild that happens to be byte-identical.
        assert_eq!(classify(b"abc", b"abc", false), Asymmetry::ValueChanged);
    }

    #[test]
    fn only_the_correct_outcomes_are_acceptable() {
        assert!(Asymmetry::Exact.is_acceptable());
        assert!(Asymmetry::NormalisedTerminator.is_acceptable());
        assert!(Asymmetry::NormalisedCase.is_acceptable());
        assert!(!Asymmetry::Rewritten.is_acceptable());
        assert!(!Asymmetry::ValueChanged.is_acceptable());
        assert!(!Asymmetry::BuildFailed.is_acceptable());
    }

    #[test]
    fn terminators_are_stripped_at_most_once_each() {
        assert_eq!(strip_terminator(b"x"), b"x");
        assert_eq!(strip_terminator(b"x\r"), b"x");
        assert_eq!(strip_terminator(b"x\n"), b"x");
        assert_eq!(strip_terminator(b"x\r\n"), b"x");
        // Not a terminator run: only the trailing pair is removed, so a
        // body that legitimately ends in several is not eaten whole.
        assert_eq!(strip_terminator(b"x\r\r\r"), b"x\r");
    }
}
