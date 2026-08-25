//! NRZI (non-return-to-zero inverted) line coding.
//!
//! # Why NRZI?
//!
//! AX.25/HDLC over Bell 202 AFSK does not transmit data bits directly as
//! mark/space tones. Instead it inserts a *differential* layer — NRZI —
//! between the raw AFSK bit stream and the HDLC framing above it: the
//! information is carried in tone *transitions* rather than in the tones
//! themselves. A data `Zero` is sent as a **toggle** of the line level; a
//! data `One` **holds** the current level (this variant is sometimes called
//! NRZI-S, "space"). The receiver therefore only needs to detect whether two
//! consecutive bit periods used the same tone, and never has to agree with
//! the transmitter about which tone means what — an inverted or
//! arbitrarily-assigned mark/space mapping decodes identically.
//!
//! # The all-ones stall property
//!
//! Because `One` holds the line, a run of `One`s produces **no transitions
//! at all**: the encoded line stays at a constant level and the receiver's
//! clock recovery would starve. Conversely a run of `Zero`s toggles the line
//! every bit, giving maximal transition density. This is exactly why HDLC
//! performs zero-bit stuffing (inserting a `Zero` after five consecutive
//! `One`s): after NRZI, the stuffed stream is guaranteed a transition at
//! least every six bit periods, keeping the demodulator's timing locked.
//!
//! # Streaming API
//!
//! [`NrziEncoder`] and [`NrziDecoder`] are one-bit-at-a-time state machines
//! holding a single [`Bit`] of state. They are pure, infallible, and
//! allocation-free. The initial line state is configurable via
//! [`NrziEncoder::new`] / [`NrziDecoder::new`]; the [`Default`] for both is
//! an initial level of [`Bit::One`] (the idle/mark line convention used by
//! AX.25). A decoder whose assumed initial level disagrees with the
//! transmitter can corrupt at most its *first* output bit — from the second
//! bit on, its state is the previously received line bit, so it is
//! self-synchronizing.
//!
//! For whole streams, wrap any `Iterator<Item = Bit>` with
//! [`NrziEncoder::encode_iter`] / [`NrziDecoder::decode_iter`] (or the free
//! functions [`encode_iter`] / [`decode_iter`], which start from the default
//! initial state), mirroring the modulator/demodulator adapter style.
//!
//! ```
//! use yodel::{Bit, NrziEncoder, NrziDecoder};
//!
//! let data = [Bit::One, Bit::Zero, Bit::Zero, Bit::One];
//! let line: Vec<Bit> = NrziEncoder::default()
//!     .encode_iter(data.iter().copied())
//!     .collect();
//! let back: Vec<Bit> = NrziDecoder::default()
//!     .decode_iter(line.iter().copied())
//!     .collect();
//! assert_eq!(back, data);
//! ```

use crate::types::Bit;

/// Returns the opposite bit.
const fn toggle(bit: Bit) -> Bit {
    match bit {
        Bit::Zero => Bit::One,
        Bit::One => Bit::Zero,
    }
}

/// Streaming NRZI encoder: data bits in, line-level bits out.
///
/// A data [`Bit::Zero`] toggles the current line level; a data [`Bit::One`]
/// holds it. The state is the current line level.
///
/// The [`Default`] encoder starts with the line at [`Bit::One`], matching
/// the idle/mark convention of AX.25 links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NrziEncoder {
    level: Bit,
}

impl NrziEncoder {
    /// Creates an encoder whose line starts at `initial` (the level the
    /// first encoded bit will be derived from).
    #[must_use]
    pub const fn new(initial: Bit) -> Self {
        Self { level: initial }
    }

    /// Encodes one data bit, returning the new line level.
    ///
    /// `Zero` toggles the line, `One` holds it.
    pub const fn encode(&mut self, bit: Bit) -> Bit {
        self.level = match bit {
            Bit::Zero => toggle(self.level),
            Bit::One => self.level,
        };
        self.level
    }

    /// Adapts a data-bit iterator into an iterator of line-level bits.
    pub fn encode_iter<I>(self, bits: I) -> EncodeIter<I>
    where
        I: Iterator<Item = Bit>,
    {
        EncodeIter {
            encoder: self,
            bits,
        }
    }
}

impl Default for NrziEncoder {
    /// An encoder with the line initially at [`Bit::One`] (idle/mark).
    fn default() -> Self {
        Self::new(Bit::One)
    }
}

/// Streaming NRZI decoder: line-level bits in, data bits out.
///
/// Emits [`Bit::One`] when the incoming line bit equals the previous line
/// bit (no transition) and [`Bit::Zero`] when it differs (a transition).
/// The state is the previously seen line level, so a wrong initial state can
/// corrupt only the first output bit — the decoder self-synchronizes.
///
/// The [`Default`] decoder assumes the line was previously at [`Bit::One`],
/// matching the idle/mark convention of AX.25 links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NrziDecoder {
    prev: Bit,
}

impl NrziDecoder {
    /// Creates a decoder assuming the line level before the first input bit
    /// was `initial`.
    #[must_use]
    pub const fn new(initial: Bit) -> Self {
        Self { prev: initial }
    }

    /// Decodes one line-level bit, returning the recovered data bit.
    ///
    /// Returns `One` iff `line` equals the previous line bit.
    pub const fn decode(&mut self, line: Bit) -> Bit {
        let out = match (line, self.prev) {
            (Bit::Zero, Bit::Zero) | (Bit::One, Bit::One) => Bit::One,
            (Bit::Zero, Bit::One) | (Bit::One, Bit::Zero) => Bit::Zero,
        };
        self.prev = line;
        out
    }

    /// Adapts a line-level-bit iterator into an iterator of data bits.
    pub fn decode_iter<I>(self, bits: I) -> DecodeIter<I>
    where
        I: Iterator<Item = Bit>,
    {
        DecodeIter {
            decoder: self,
            bits,
        }
    }
}

impl Default for NrziDecoder {
    /// A decoder assuming the line was initially at [`Bit::One`] (idle/mark).
    fn default() -> Self {
        Self::new(Bit::One)
    }
}

/// NRZI-encodes a data-bit iterator using the default initial line state
/// ([`Bit::One`]). See [`NrziEncoder::encode_iter`].
pub fn encode_iter<I>(bits: I) -> EncodeIter<I>
where
    I: Iterator<Item = Bit>,
{
    NrziEncoder::default().encode_iter(bits)
}

/// NRZI-decodes a line-level-bit iterator using the default initial line
/// state ([`Bit::One`]). See [`NrziDecoder::decode_iter`].
pub fn decode_iter<I>(bits: I) -> DecodeIter<I>
where
    I: Iterator<Item = Bit>,
{
    NrziDecoder::default().decode_iter(bits)
}

/// Iterator of NRZI line-level bits over a data-bit iterator.
///
/// Created by [`NrziEncoder::encode_iter`] or [`encode_iter`].
#[derive(Debug, Clone)]
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct EncodeIter<I> {
    encoder: NrziEncoder,
    bits: I,
}

impl<I> Iterator for EncodeIter<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        self.bits.next().map(|bit| self.encoder.encode(bit))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.bits.size_hint()
    }
}

/// Iterator of recovered data bits over an NRZI line-level-bit iterator.
///
/// Created by [`NrziDecoder::decode_iter`] or [`decode_iter`].
#[derive(Debug, Clone)]
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct DecodeIter<I> {
    decoder: NrziDecoder,
    bits: I,
}

impl<I> Iterator for DecodeIter<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        self.bits.next().map(|bit| self.decoder.decode(bit))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.bits.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random bit sequence (xorshift32).
    fn pseudo_random_bits(seed: u32, len: usize) -> impl Iterator<Item = Bit> {
        let mut state = seed | 1;
        core::iter::repeat_with(move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            Bit::from(state & 1 != 0)
        })
        .take(len)
    }

    fn roundtrip(data: &[Bit], initial: Bit) {
        let mut enc = NrziEncoder::new(initial);
        let mut dec = NrziDecoder::new(initial);
        for &bit in data {
            let line = enc.encode(bit);
            assert_eq!(dec.decode(line), bit);
        }
    }

    #[test]
    fn roundtrip_structured_sequences_both_initial_states() {
        let sequences: &[&[Bit]] = &[
            &[],
            &[Bit::Zero],
            &[Bit::One],
            &[Bit::Zero, Bit::One, Bit::One, Bit::Zero, Bit::Zero],
            &[Bit::One; 16],
            &[Bit::Zero; 16],
            &[
                Bit::Zero,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::One,
                Bit::Zero, // HDLC flag 0x7E, LSB first
            ],
        ];
        for &seq in sequences {
            roundtrip(seq, Bit::Zero);
            roundtrip(seq, Bit::One);
        }
    }

    #[test]
    fn roundtrip_pseudo_random_sequences() {
        for seed in [1, 0xDEAD_BEEF, 0x1234_5678] {
            for initial in [Bit::Zero, Bit::One] {
                let mut enc = NrziEncoder::new(initial);
                let mut dec = NrziDecoder::new(initial);
                for bit in pseudo_random_bits(seed, 4096) {
                    assert_eq!(dec.decode(enc.encode(bit)), bit);
                }
            }
        }
    }

    #[test]
    fn all_ones_stall_encoding_holds_line() {
        for initial in [Bit::Zero, Bit::One] {
            let mut enc = NrziEncoder::new(initial);
            for _ in 0..64 {
                assert_eq!(enc.encode(Bit::One), initial);
            }
        }
    }

    #[test]
    fn constant_line_decodes_to_all_ones_after_first_bit() {
        for level in [Bit::Zero, Bit::One] {
            for initial in [Bit::Zero, Bit::One] {
                let mut dec = NrziDecoder::new(initial);
                let first = dec.decode(level);
                // First bit depends on the assumed initial state.
                assert_eq!(first, Bit::from(level == initial));
                for _ in 0..64 {
                    assert_eq!(dec.decode(level), Bit::One);
                }
            }
        }
    }

    #[test]
    fn all_zeros_yields_alternating_line() {
        for initial in [Bit::Zero, Bit::One] {
            let mut enc = NrziEncoder::new(initial);
            let mut expected = initial;
            for _ in 0..64 {
                expected = toggle(expected);
                assert_eq!(enc.encode(Bit::Zero), expected);
            }
        }
    }

    #[test]
    fn decoder_self_synchronizes_after_one_bit() {
        // Encode with one initial state, decode with the *other*: only the
        // first data bit may differ.
        for enc_initial in [Bit::Zero, Bit::One] {
            let dec_initial = toggle(enc_initial);
            let mut enc = NrziEncoder::new(enc_initial);
            let mut dec = NrziDecoder::new(dec_initial);
            for (index, bit) in pseudo_random_bits(42, 1024).enumerate() {
                let out = dec.decode(enc.encode(bit));
                if index == 0 {
                    assert_eq!(out, toggle(bit), "first bit must be inverted");
                } else {
                    assert_eq!(out, bit, "must match from the second bit on");
                }
            }
        }
    }

    #[test]
    fn iterator_adapters_agree_with_push_api() {
        let data: [Bit; 8] = [
            Bit::One,
            Bit::Zero,
            Bit::Zero,
            Bit::One,
            Bit::One,
            Bit::One,
            Bit::Zero,
            Bit::One,
        ];
        for initial in [Bit::Zero, Bit::One] {
            let mut enc = NrziEncoder::new(initial);
            let mut dec = NrziDecoder::new(initial);
            let it_enc = NrziEncoder::new(initial).encode_iter(data.iter().copied());
            for (bit, line_from_iter) in data.iter().copied().zip(it_enc) {
                let line = enc.encode(bit);
                assert_eq!(line, line_from_iter);
                assert_eq!(dec.decode(line), bit);
            }
            // Decode adapter over the encode adapter roundtrips too.
            let round = NrziDecoder::new(initial)
                .decode_iter(NrziEncoder::new(initial).encode_iter(data.iter().copied()));
            let mut count = 0usize;
            for (out, expected) in round.zip(data.iter().copied()) {
                assert_eq!(out, expected);
                count += 1;
            }
            assert_eq!(count, data.len());
        }
    }

    #[test]
    fn free_functions_use_default_initial_state() {
        let data = [Bit::Zero, Bit::One, Bit::Zero];
        let via_free: [Option<Bit>; 3] = {
            let mut it = encode_iter(data.iter().copied());
            [it.next(), it.next(), it.next()]
        };
        let via_struct: [Option<Bit>; 3] = {
            let mut it = NrziEncoder::default().encode_iter(data.iter().copied());
            [it.next(), it.next(), it.next()]
        };
        assert_eq!(via_free, via_struct);

        let line = [Bit::Zero, Bit::Zero, Bit::One];
        let d_free: [Option<Bit>; 3] = {
            let mut it = decode_iter(line.iter().copied());
            [it.next(), it.next(), it.next()]
        };
        let d_struct: [Option<Bit>; 3] = {
            let mut it = NrziDecoder::default().decode_iter(line.iter().copied());
            [it.next(), it.next(), it.next()]
        };
        assert_eq!(d_free, d_struct);
    }

    #[test]
    fn default_initial_state_is_one() {
        assert_eq!(NrziEncoder::default(), NrziEncoder::new(Bit::One));
        assert_eq!(NrziDecoder::default(), NrziDecoder::new(Bit::One));
    }

    #[test]
    fn size_hints_pass_through() {
        let data = [Bit::One, Bit::Zero];
        assert_eq!(encode_iter(data.iter().copied()).size_hint(), (2, Some(2)));
        assert_eq!(decode_iter(data.iter().copied()).size_hint(), (2, Some(2)));
    }
}
