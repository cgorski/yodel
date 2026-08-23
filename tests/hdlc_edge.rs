//! HDLC dense-traffic edge cases: shared flags, minimal preambles,
//! aborts, FCS failures, and runt garbage must never disturb an
//! adjacent valid frame.
#![cfg(feature = "ax25")]

use warble::Bit;
use warble::ax25::{Address, Ax25Error, HdlcDeframer, UiFrame, hdlc};

/// A valid UI frame body.
fn ui_body(info: &[u8]) -> Vec<u8> {
    let dest = Address::new(b"APRS", 0).unwrap();
    let src = Address::new(b"N0CALL", 7).unwrap();
    let frame = UiFrame::new(dest, src, info);
    let mut buf = [0u8; 128];
    let len = frame.build(&mut buf).unwrap();
    buf[..len].to_vec()
}

fn flag_bits(n: usize) -> Vec<Bit> {
    let mut out = Vec::new();
    for _ in 0..n {
        for i in 0..8 {
            out.push(Bit::from((hdlc::FLAG >> i) & 1 != 0));
        }
    }
    out
}

/// Line bits of one frame with the given preamble/tail flag counts.
fn frame_line_bits(body: &[u8], pre: usize, tail: usize) -> Vec<Bit> {
    hdlc::frame_bits(body, pre, tail).collect()
}

fn deframe_all(bits: &[Bit]) -> Vec<Result<Vec<u8>, Ax25Error>> {
    let mut d = HdlcDeframer::<330>::new();
    let mut out = Vec::new();
    for &b in bits {
        if let Some(r) = d.push(b) {
            out.push(r.map(<[u8]>::to_vec));
        }
    }
    out
}

fn ok_frames(results: &[Result<Vec<u8>, Ax25Error>]) -> Vec<Vec<u8>> {
    results
        .iter()
        .filter_map(|r| r.as_ref().ok().cloned())
        .collect()
}

/// (a) Two frames sharing a single 7E flag decode both.
#[test]
fn shared_flag_decodes_both_frames() {
    let a = ui_body(b"frame A payload");
    let b = ui_body(b"frame B payload");
    let mut bits = frame_line_bits(&a, 2, 0);
    bits.extend(flag_bits(1)); // the single shared flag
    bits.extend(frame_line_bits(&b, 0, 2));
    let frames = deframe_all(&bits);
    assert_eq!(ok_frames(&frames), [a, b]);
}

/// (b) A frame after a one-flag preamble decodes.
#[test]
fn one_flag_preamble_decodes() {
    let body = ui_body(b"short preamble");
    // Some idle ones (line idle), then exactly one flag, then the frame.
    let mut bits = vec![Bit::One; 24];
    bits.extend(frame_line_bits(&body, 1, 1));
    let frames = deframe_all(&bits);
    assert_eq!(ok_frames(&frames), [body]);
}

/// (c) An abort (7+ ones) mid-frame, then a flag, then a valid frame:
/// the valid frame decodes.
#[test]
fn abort_then_flag_then_valid_frame() {
    let aborted = ui_body(b"this one is aborted");
    let good = ui_body(b"this one is good");
    let partial = frame_line_bits(&aborted, 2, 0);
    let mut bits: Vec<Bit> = partial[..partial.len() - 40].to_vec();
    bits.extend(vec![Bit::One; 9]); // abort
    bits.extend(frame_line_bits(&good, 1, 1));
    let frames = deframe_all(&bits);
    assert_eq!(ok_frames(&frames), [good]);
}

/// (d) An FCS-fail frame followed immediately by a shared-flag valid
/// frame decodes the valid one: the failed close must not eat the
/// opening flag.
#[test]
fn fcs_fail_then_shared_flag_valid_frame() {
    let bad = ui_body(b"gets corrupted");
    let good = ui_body(b"stays intact");
    let mut bits = frame_line_bits(&bad, 2, 0);
    // Flip a content bit well inside the first frame's data section.
    let idx = 2 * 8 + 30;
    bits[idx] = match bits[idx] {
        Bit::Zero => Bit::One,
        Bit::One => Bit::Zero,
    };
    bits.extend(flag_bits(1)); // shared flag
    bits.extend(frame_line_bits(&good, 0, 1));
    let frames = deframe_all(&bits);
    assert!(
        frames
            .iter()
            .any(|r| matches!(r, Err(Ax25Error::FcsMismatch { .. }))),
        "corrupted frame must be reported: {frames:?}"
    );
    assert_eq!(ok_frames(&frames), [good]);
}

/// (e) Runt (< 17 byte) garbage between flags does not disturb the
/// next frame.
#[test]
fn runt_garbage_between_flags_ignored() {
    let good = ui_body(b"after the runt");
    let mut bits = flag_bits(2);
    // 8 bytes of garbage (byte-aligned, so it closes as a runt).
    for &byte in &[0xA5u8, 0x3C, 0x00, 0xFF, 0x12, 0x99, 0x42, 0x77] {
        let mut ones = 0u32;
        for i in 0..8 {
            let bit = Bit::from((byte >> i) & 1 != 0);
            bits.push(bit);
            if bit == Bit::One {
                ones += 1;
                if ones == 5 {
                    bits.push(Bit::Zero);
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    bits.extend(flag_bits(1)); // close the runt, open the frame
    bits.extend(frame_line_bits(&good, 0, 2));
    let frames = deframe_all(&bits);
    assert_eq!(ok_frames(&frames), std::slice::from_ref(&good));

    // Non-byte-aligned runt salvage too: a handful of stray bits.
    let mut bits = flag_bits(1);
    bits.extend([Bit::One, Bit::Zero, Bit::One, Bit::One, Bit::Zero]);
    bits.extend(flag_bits(1));
    bits.extend(frame_line_bits(&good, 0, 2));
    let frames = deframe_all(&bits);
    assert_eq!(ok_frames(&frames), [good]);
}
