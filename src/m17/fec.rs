//! M17 channel coding: convolutional code, puncturing, Viterbi
//! decoding, the QPP interleaver, the randomizer, and Golay(24,12).
//!
//! Pure bit manipulation over fixed-size buffers. Nothing here knows
//! about addresses, frames or audio, so it is testable in isolation and
//! reusable by both the packet and any future stream path. Re-exported
//! from [`crate::m17`], which is where callers reach it.

// ---------------------------------------------------------------------------
// Convolutional code + puncturing (M17 spec, "Channel Coding")
// ---------------------------------------------------------------------------

/// Convolutional generator G1 = 0x19 (`x⁴ + x³ + 1`, K = 5, rate 1/2;
/// M17 spec, Channel Coding). The newest input bit is the **LSB** of
/// the 5-bit window (`window = (window << 1) | bit`), so bit 0 taps
/// the current input and bit 4 the oldest — matching the spec's
/// G1(D) = 1 + D³ + D⁴.
pub const CONV_G1: u8 = 0x19;
/// Convolutional generator G2 = 0x17 (`x⁴ + x² + x + 1`; M17 spec) —
/// the spec's G2(D) = 1 + D + D² + D⁴, with the same LSB-is-newest
/// window convention as [`CONV_G1`].
pub const CONV_G2: u8 = 0x17;

/// Number of encoder flush (tail) bits: K − 1 = 4 zeros.
pub const CONV_FLUSH_BITS: usize = 4;

/// Bits in every channel-coded M17 frame payload after puncturing.
pub const FRAME_BITS: usize = 368;
/// Bytes holding [`FRAME_BITS`].
pub const FRAME_BYTES: usize = FRAME_BITS / 8;

/// P1 puncturing pattern for the LSF (M17 spec, Channel Coding):
/// a 61-entry pattern with 46 ones — one leading `1` followed by
/// fifteen repetitions of `1,0,1,1` — taking 488 coded bits to 368.
/// Transcribed from the published spec table; internal roundtrips are
/// pattern-agnostic, interop should be confirmed against reference
/// vectors.
pub const PUNCTURE_P1: [u8; 61] = {
    let mut p = [0u8; 61];
    p[0] = 1;
    let mut i = 0;
    while i < 15 {
        p[1 + 4 * i] = 1;
        p[1 + 4 * i + 1] = 0;
        p[1 + 4 * i + 2] = 1;
        p[1 + 4 * i + 3] = 1;
        i += 1;
    }
    p
};

/// P3 puncturing pattern for packet frames (M17 spec, Channel Coding):
/// 8 entries, 7 ones (`1,1,1,1,1,1,1,0`), taking 420 coded bits to 368.
/// Transcribed from the published spec table (same caveat as
/// [`PUNCTURE_P1`]).
pub const PUNCTURE_P3: [u8; 8] = [1, 1, 1, 1, 1, 1, 1, 0];

#[inline]
pub(super) fn get_bit(buf: &[u8], i: usize) -> u8 {
    (buf[i / 8] >> (7 - (i % 8))) & 1
}

#[inline]
pub(super) fn set_bit(buf: &mut [u8], i: usize, bit: u8) {
    let mask = 1u8 << (7 - (i % 8));
    if bit != 0 {
        buf[i / 8] |= mask;
    } else {
        buf[i / 8] &= !mask;
    }
}

/// Convolutionally encodes `nbits` bits of `data` (MSB-first) plus
/// [`CONV_FLUSH_BITS`] zero tail bits with the K = 5, rate-1/2 code
/// ([`CONV_G1`]/[`CONV_G2`]; M17 spec, Channel Coding). Writes
/// `2 × (nbits + 4)` bits into `out` (MSB-first) and returns that count.
///
/// Per step, the G1 parity is emitted first, then the G2 parity.
///
/// # Panics
///
/// Panics if `out` is too small for the encoded bits.
pub fn convolutional_encode(data: &[u8], nbits: usize, out: &mut [u8]) -> usize {
    let total = nbits + CONV_FLUSH_BITS;
    let mut window: u8 = 0; // 5-bit sliding window, newest bit in LSB position 0.
    let mut o = 0;
    for i in 0..total {
        let bit = if i < nbits { get_bit(data, i) } else { 0 };
        window = ((window << 1) | bit) & 0x1F;
        set_bit(out, o, (window & CONV_G1).count_ones() as u8 & 1);
        set_bit(out, o + 1, (window & CONV_G2).count_ones() as u8 & 1);
        o += 2;
    }
    o
}

/// Punctures `nbits` bits of `data` with the cyclic `pattern` (1 =
/// keep), writing the survivors MSB-first into `out` and returning the
/// surviving bit count.
///
/// # Panics
///
/// Panics if `out` is too small.
pub fn puncture(data: &[u8], nbits: usize, pattern: &[u8], out: &mut [u8]) -> usize {
    let mut o = 0;
    for i in 0..nbits {
        if pattern[i % pattern.len()] != 0 {
            set_bit(out, o, get_bit(data, i));
            o += 1;
        }
    }
    o
}

/// Reverses [`puncture`]: expands `nbits` received bits back to
/// `out_bits` positions, filling punctured positions with erasures.
/// `bits`/`known` receive one entry per output position (`known[i] ==
/// false` marks an erasure). Returns the number of input bits consumed.
///
/// # Panics
///
/// Panics if `bits`/`known` are shorter than `out_bits`.
pub fn depuncture(
    data: &[u8],
    nbits: usize,
    pattern: &[u8],
    out_bits: usize,
    bits: &mut [u8],
    known: &mut [bool],
) -> usize {
    let mut consumed = 0;
    for i in 0..out_bits {
        if pattern[i % pattern.len()] != 0 && consumed < nbits {
            bits[i] = get_bit(data, consumed);
            known[i] = true;
            consumed += 1;
        } else {
            bits[i] = 0;
            known[i] = false;
        }
    }
    consumed
}

/// Hard-decision Viterbi decoder for the M17 K = 5 rate-1/2 code with
/// erasure support (16 states; M17 spec, Channel Coding).
///
/// `bits`/`known` hold `2 × (out_nbits + 4)` positions from
/// [`depuncture`]. Decoded data bits (the flush tail stripped) are
/// written MSB-first into `out`. Returns the surviving path metric
/// (count of mismatched known bits) — 0 for an error-free frame.
///
/// # Panics
///
/// Panics if `out_nbits` exceeds [`VITERBI_MAX_BITS`] or the buffers
/// are inconsistent with it.
pub fn viterbi_decode(bits: &[u8], known: &[bool], out_nbits: usize, out: &mut [u8]) -> u32 {
    assert!(out_nbits <= VITERBI_MAX_BITS);
    let steps = out_nbits + CONV_FLUSH_BITS;
    const INF: u32 = u32::MAX / 2;
    let mut pm = [INF; 16];
    pm[0] = 0;
    let mut decisions = [0u16; VITERBI_MAX_BITS + CONV_FLUSH_BITS];
    for (t, decision) in decisions.iter_mut().enumerate().take(steps) {
        let (r0, k0) = (bits[2 * t], known[2 * t]);
        let (r1, k1) = (bits[2 * t + 1], known[2 * t + 1]);
        let mut next = [INF; 16];
        let mut word = 0u16;
        for ns in 0..16u8 {
            let bit = ns & 1;
            let mut best = INF;
            let mut best_h = 0u16;
            for h in 0..2u8 {
                let ps = (ns >> 1) | (h << 3);
                if pm[ps as usize] >= INF {
                    continue;
                }
                let window = ((ps << 1) | bit) & 0x1F;
                let o0 = (window & CONV_G1).count_ones() as u8 & 1;
                let o1 = (window & CONV_G2).count_ones() as u8 & 1;
                let mut cost = pm[ps as usize];
                if k0 && r0 != o0 {
                    cost += 1;
                }
                if k1 && r1 != o1 {
                    cost += 1;
                }
                if cost < best {
                    best = cost;
                    best_h = u16::from(h);
                }
            }
            next[ns as usize] = best;
            word |= best_h << ns;
        }
        *decision = word;
        pm = next;
    }
    // The flush bits force the encoder back to state 0: trace from there.
    let metric = pm[0];
    let mut state: u8 = 0;
    for t in (0..steps).rev() {
        let bit = state & 1;
        if t < out_nbits {
            set_bit(out, t, bit);
        }
        let h = ((decisions[t] >> state) & 1) as u8;
        state = (state >> 1) | (h << 3);
    }
    metric
}

/// Largest data-bit count [`viterbi_decode`] supports (the LSF's 240).
pub const VITERBI_MAX_BITS: usize = 240;

// ---------------------------------------------------------------------------
// Interleaver + randomizer (M17 spec, "Interleaving" / "Randomizer")
// ---------------------------------------------------------------------------

/// The quadratic permutation polynomial interleaver index map:
/// `π(i) = (45·i + 92·i²) mod 368` (M17 spec, Interleaving).
#[must_use]
pub const fn interleave_index(i: usize) -> usize {
    (45 * i + 92 * i * i) % FRAME_BITS
}

/// Interleaves 368 bits: output bit `π(i)` = input bit `i` (M17 spec,
/// Interleaving; applied after puncturing, before randomizing).
#[must_use]
pub fn interleave(data: &[u8; FRAME_BYTES]) -> [u8; FRAME_BYTES] {
    let mut out = [0u8; FRAME_BYTES];
    for i in 0..FRAME_BITS {
        set_bit(&mut out, interleave_index(i), get_bit(data, i));
    }
    out
}

/// Reverses [`interleave`].
#[must_use]
pub fn deinterleave(data: &[u8; FRAME_BYTES]) -> [u8; FRAME_BYTES] {
    let mut out = [0u8; FRAME_BYTES];
    for i in 0..FRAME_BITS {
        set_bit(&mut out, i, get_bit(data, interleave_index(i)));
    }
    out
}

/// The 46-byte decorrelator (randomizer) sequence XORed onto every
/// 368-bit frame payload (M17 spec, Randomizer). Transcribed from the
/// published spec table; the operation is self-inverse regardless, but
/// interop depends on these exact bytes — confirm against reference
/// vectors before fielding.
pub const RAND_SEQ: [u8; FRAME_BYTES] = [
    0xD6, 0xB5, 0xE2, 0x30, 0x82, 0xFF, 0x84, 0x62, 0xBA, 0x4E, 0x96, 0x90, 0xD8, 0x98, 0xDD, 0x5D,
    0x0C, 0xC8, 0x52, 0x43, 0x91, 0x1D, 0xF8, 0x6E, 0x68, 0x2F, 0x35, 0xDA, 0x14, 0xEA, 0xCD, 0x76,
    0x19, 0x8D, 0xD5, 0x80, 0xD1, 0x33, 0x87, 0x13, 0x57, 0x18, 0x2D, 0x29, 0x78, 0xC3,
];

/// XORs the frame with [`RAND_SEQ`] in place (self-inverse: applying it
/// twice is the identity).
pub fn randomize(frame: &mut [u8; FRAME_BYTES]) {
    for (b, r) in frame.iter_mut().zip(RAND_SEQ.iter()) {
        *b ^= r;
    }
}

// ---------------------------------------------------------------------------
// Golay(24,12) (M17 spec, "Golay Encoder" — used by stream-mode LICH)
// ---------------------------------------------------------------------------

/// Generator polynomial of the (23,12) Golay code as used by the M17
/// spec's Golay(24,12) construction: 0xC75 (13-bit polynomial with the
/// implicit leading 1: `x¹¹ + x¹⁰ + x⁶ + x⁵ + x⁴ + x² + 1`).
/// Transcribed from the published spec; the codec is self-consistent
/// either way, interop matters only for stream-mode LICH chunks (out of
/// scope this slice).
pub const GOLAY_POLY: u32 = 0xC75;

/// Encodes 12 data bits into an extended Golay(24,12) codeword:
/// `data(12) | check(11) | parity(1)`, data in the most significant
/// bits (M17 spec, Golay Encoder).
///
/// Shipped as a public building block: stream mode's LICH uses four
/// Golay(24,12) words per frame, but stream framing awaits the Codec2
/// voice proposal (see `docs/ARCHITECTURE.md`), so no framing here calls
/// it yet.
#[must_use]
pub const fn golay24_encode(data: u16) -> u32 {
    let data = (data & 0xFFF) as u32;
    // Polynomial division of data·x^11 by GOLAY_POLY (degree 11).
    let mut rem = data << 11;
    let mut i = 22;
    while i >= 11 {
        if rem & (1 << i) != 0 {
            rem ^= GOLAY_POLY << (i - 11);
        }
        i -= 1;
    }
    let cw23 = (data << 11) | (rem & 0x7FF);
    let parity = cw23.count_ones() & 1;
    (cw23 << 1) | parity
}

/// Decodes an extended Golay(24,12) codeword, correcting up to 3 bit
/// errors. Returns `(data, corrected_bits)`, or `None` when the word is
/// 4 or more bit flips from every codeword (uncorrectable — the
/// extended code's minimum distance 8 guarantees ≤3 errors decode
/// uniquely).
///
/// The decoder is an exhaustive minimum-distance search over all 4096
/// codewords: 12-bit encode per candidate, no tables, no allocation —
/// simplicity for an obviously-correct no_std building block (LICH
/// rate would be 100 words/s; ~50k cheap ops per word).
#[must_use]
pub fn golay24_decode(word: u32) -> Option<(u16, u32)> {
    let word = word & 0xFF_FFFF;
    let mut best_data = 0u16;
    let mut best_dist = u32::MAX;
    for data in 0..4096u16 {
        let dist = (golay24_encode(data) ^ word).count_ones();
        if dist < best_dist {
            best_dist = dist;
            best_data = data;
        }
    }
    if best_dist <= 3 {
        Some((best_data, best_dist))
    } else {
        None
    }
}
