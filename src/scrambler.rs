//! G3RUH multiplicative (self-synchronizing) LFSR scrambler.
//!
//! # Specification
//!
//! The scrambler polynomial and the 9600-baud baseband design it belongs
//! to are from:
//!
//! > Miller, J. (G3RUH), "9600 Baud Packet Radio Modem Design",
//! > Proceedings of the ARRL 7th Computer Networking Conference,
//! > October 1988, pp. 135-140. (Also Proc. 1st RSGB Data Symposium,
//! > Harrow, England, July 1988.)
//! > <https://www.amsat.org/amsat/articles/g3ruh/109.html>
//!
//! That design in turn builds on Steve Goode (K9NG), "Modifying the
//! Hamtronics FM-5 for 9600 bps Packet Operation", Proc. 4th ARRL
//! Amateur Radio Computer Networking Conference, 1985, pp. 45-51 — the
//! earlier direct-FM approach G3RUH refined.
//!
//! # Why scramble?
//!
//! 9600-baud G3RUH packet transmits baseband pulses directly through the
//! radio, so the spectrum of the transmitted signal *is* the spectrum of the
//! bit stream. Long runs of identical bits would concentrate energy near DC
//! and starve the receiver's clock recovery. G3RUH therefore whitens the
//! stream with a 17-stage **multiplicative scrambler** built from the
//! polynomial
//!
//! ```text
//! x^17 + x^12 + 1
//! ```
//!
//! The non-unity terms `x^17` and `x^12` name the two shift-register taps:
//! delays of 17 and 12 bit periods. On the transmit side the tap feedback
//! comes from the scrambler's **own output** history:
//!
//! ```text
//! out[n] = in[n] XOR out[n-12] XOR out[n-17]
//! ```
//!
//! On the receive side the descrambler is the exact feed-*forward* inverse,
//! tapping the **received** bit history:
//!
//! ```text
//! out[n] = in[n] XOR in[n-12] XOR in[n-17]
//! ```
//!
//! Because the descrambler's state is just the last 17 *channel* bits, it is
//! **self-synchronizing**: whatever state it starts in, after 17 received
//! bits its register matches the channel history exactly and every
//! subsequent output bit is correct. No preamble or state agreement between
//! the ends is required. The price is **error multiplication ×3**: one
//! flipped channel bit passes through all three descrambler taps, corrupting
//! exactly the output bits at offsets 0, 12 and 17 after the flip.
//!
//! [`Scrambler`] and [`Descrambler`] are one-bit-at-a-time state machines
//! holding a single `u32` register — pure, infallible, allocation-free, and
//! `no_std`, mirroring [`NrziEncoder`](crate::nrzi::NrziEncoder) /
//! [`NrziDecoder`](crate::nrzi::NrziDecoder) including the iterator
//! adapters.
//!
//! ```
//! use warble::{Bit, Descrambler, Scrambler};
//!
//! let data = [Bit::One, Bit::Zero, Bit::Zero, Bit::One];
//! let channel: Vec<Bit> = Scrambler::default()
//!     .scramble_iter(data.iter().copied())
//!     .collect();
//! let back: Vec<Bit> = Descrambler::default()
//!     .descramble_iter(channel.iter().copied())
//!     .collect();
//! assert_eq!(back, data);
//! ```

use crate::types::Bit;

/// Number of shift-register stages: the degree of `x^17 + x^12 + 1`.
const STAGES: u32 = 17;

/// Mask keeping exactly the [`STAGES`] register bits.
const STATE_MASK: u32 = (1 << STAGES) - 1;

/// Tap delays, read directly off the polynomial's non-unity terms.
const TAP_A: u32 = 12;
const TAP_B: u32 = 17;

/// XOR of the two polynomial taps over a shift-register history.
///
/// Register bit `d - 1` holds the bit from `d` shifts ago, so the `x^12`
/// and `x^17` taps live at bit positions 11 and 16.
const fn taps(state: u32) -> u32 {
    ((state >> (TAP_A - 1)) ^ (state >> (TAP_B - 1))) & 1
}

/// Shifts `bit` into the low end of the 17-bit history register.
const fn shift_in(state: u32, bit: Bit) -> u32 {
    ((state << 1) | bit as u32) & STATE_MASK
}

/// Streaming G3RUH scrambler (transmit side): data bits in, whitened
/// channel bits out.
///
/// Implements `out[n] = in[n] XOR out[n-12] XOR out[n-17]` from the
/// polynomial `x^17 + x^12 + 1`; the register holds the last 17 *output*
/// bits. The matching receive-side inverse is [`Descrambler`].
///
/// # Examples
///
/// New to scramblers? A scrambler and descrambler pair behaves like a
/// perfect wire — whatever bits go in come back out:
///
/// ```
/// use warble::{Bit, Descrambler, Scrambler};
///
/// let mut tx = Scrambler::new();
/// let mut rx = Descrambler::new();
/// for bit in [Bit::One, Bit::One, Bit::Zero, Bit::One, Bit::Zero] {
///     assert_eq!(rx.descramble(tx.scramble(bit)), bit);
/// }
/// ```
///
/// In a real transmit chain the scrambler comes last in the bit stream,
/// after the HDLC bit stuffer and the NRZI encoder (stuffer → NRZI →
/// scrambler → waveform), driven bit-at-a-time or via the iterator
/// adapter:
///
/// ```
/// use warble::{Bit, Scrambler};
///
/// let nrzi_bits = [Bit::Zero, Bit::One, Bit::One, Bit::Zero];
/// let channel: Vec<Bit> = Scrambler::default()
///     .scramble_iter(nrzi_bits.iter().copied())
///     .collect();
/// assert_eq!(channel.len(), nrzi_bits.len());
/// ```
///
/// Note for the protocol-minded: with all-zeros input the scrambler
/// degenerates to a free-running LFSR (`out[n] = out[n-12] XOR out[n-17]`),
/// and because `x^17 + x^12 + 1` is primitive over GF(2) that sequence has
/// maximal period 2^17 − 1 = 131071 — this is exactly the whitening
/// guarantee:
///
/// ```
/// use warble::{Bit, Scrambler};
///
/// let mut lfsr = Scrambler::with_state(1);
/// let first: Vec<Bit> = (0..17).map(|_| lfsr.scramble(Bit::Zero)).collect();
/// // Not a constant run: the register contents get spread over the output.
/// assert!(first.contains(&Bit::One));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scrambler {
    /// Last 17 output bits, newest in bit 0.
    state: u32,
}

impl Scrambler {
    /// Creates a scrambler with an all-zeros register (the conventional
    /// starting state).
    #[must_use]
    pub const fn new() -> Self {
        Self { state: 0 }
    }

    /// Creates a scrambler whose register is seeded with the low 17 bits of
    /// `state` (bit 0 = most recent output bit; higher bits are ignored).
    ///
    /// Any seed works: the receiving [`Descrambler`] self-synchronizes
    /// after 17 bits regardless.
    #[must_use]
    pub const fn with_state(state: u32) -> Self {
        Self {
            state: state & STATE_MASK,
        }
    }

    /// Scrambles one data bit, returning the channel bit.
    pub const fn scramble(&mut self, bit: Bit) -> Bit {
        let out = (bit as u32 ^ taps(self.state)) & 1;
        let out = if out == 1 { Bit::One } else { Bit::Zero };
        self.state = shift_in(self.state, out);
        out
    }

    /// Adapts a data-bit iterator into an iterator of channel bits.
    pub fn scramble_iter<I>(self, bits: I) -> ScrambleIter<I>
    where
        I: Iterator<Item = Bit>,
    {
        ScrambleIter {
            scrambler: self,
            bits,
        }
    }
}

/// Streaming G3RUH descrambler (receive side): channel bits in, recovered
/// data bits out.
///
/// Implements `out[n] = in[n] XOR in[n-12] XOR in[n-17]`, the feed-forward
/// inverse of [`Scrambler`]; the register holds the last 17 *received*
/// bits.
///
/// # Examples
///
/// New to scramblers? Feed it what a [`Scrambler`] produced and the
/// original data falls out:
///
/// ```
/// use warble::{Bit, Descrambler, Scrambler};
///
/// let data = [Bit::Zero, Bit::Zero, Bit::One, Bit::One];
/// let mut tx = Scrambler::new();
/// let mut rx = Descrambler::new();
/// for bit in data {
///     assert_eq!(rx.descramble(tx.scramble(bit)), bit);
/// }
/// ```
///
/// In a receive chain it consumes the recovered channel bit stream ahead of
/// the HDLC deframer, most conveniently as an iterator adapter:
///
/// ```
/// use warble::{Bit, Descrambler, Scrambler};
///
/// let channel: Vec<Bit> = Scrambler::default()
///     .scramble_iter([Bit::One, Bit::Zero, Bit::One].into_iter())
///     .collect();
/// let data: Vec<Bit> = Descrambler::default()
///     .descramble_iter(channel.into_iter())
///     .collect();
/// assert_eq!(data, [Bit::One, Bit::Zero, Bit::One]);
/// ```
///
/// Note for the protocol-minded: the descrambler's state is nothing but the
/// last 17 channel bits, so it **self-synchronizes** — start it in *any*
/// state and it is bit-exact once 17 channel bits have flushed the register.
/// The dual property is error multiplication ×3: one flipped channel bit
/// corrupts exactly the output bits at offsets 0, 12 and 17.
///
/// ```
/// use warble::{Bit, Descrambler, Scrambler};
///
/// let mut tx = Scrambler::with_state(0b1_0101_0101_0101_0101);
/// let mut rx = Descrambler::new(); // mismatched state on purpose
/// let mut wrong_after_sync = 0;
/// for (n, bit) in core::iter::repeat(Bit::One).take(64).enumerate() {
///     let out = rx.descramble(tx.scramble(bit));
///     if n >= 17 && out != bit {
///         wrong_after_sync += 1;
///     }
/// }
/// assert_eq!(wrong_after_sync, 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Descrambler {
    /// Last 17 received channel bits, newest in bit 0.
    state: u32,
}

impl Descrambler {
    /// Creates a descrambler with an all-zeros register.
    ///
    /// The initial state is immaterial beyond the first 17 bits: the
    /// descrambler is self-synchronizing.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: 0 }
    }

    /// Creates a descrambler whose register is seeded with the low 17 bits
    /// of `state` (bit 0 = most recent channel bit; higher bits ignored).
    #[must_use]
    pub const fn with_state(state: u32) -> Self {
        Self {
            state: state & STATE_MASK,
        }
    }

    /// Descrambles one channel bit, returning the recovered data bit.
    pub const fn descramble(&mut self, bit: Bit) -> Bit {
        let out = (bit as u32 ^ taps(self.state)) & 1;
        self.state = shift_in(self.state, bit);
        if out == 1 { Bit::One } else { Bit::Zero }
    }

    /// Adapts a channel-bit iterator into an iterator of recovered data
    /// bits.
    pub fn descramble_iter<I>(self, bits: I) -> DescrambleIter<I>
    where
        I: Iterator<Item = Bit>,
    {
        DescrambleIter {
            descrambler: self,
            bits,
        }
    }
}

/// Iterator of scrambled channel bits over a data-bit iterator.
///
/// Created by [`Scrambler::scramble_iter`].
#[derive(Debug, Clone)]
pub struct ScrambleIter<I> {
    scrambler: Scrambler,
    bits: I,
}

impl<I> Iterator for ScrambleIter<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        self.bits.next().map(|bit| self.scrambler.scramble(bit))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.bits.size_hint()
    }
}

/// Iterator of recovered data bits over a channel-bit iterator.
///
/// Created by [`Descrambler::descramble_iter`].
#[derive(Debug, Clone)]
pub struct DescrambleIter<I> {
    descrambler: Descrambler,
    bits: I,
}

impl<I> Iterator for DescrambleIter<I>
where
    I: Iterator<Item = Bit>,
{
    type Item = Bit;

    fn next(&mut self) -> Option<Bit> {
        self.bits.next().map(|bit| self.descrambler.descramble(bit))
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

    /// Drains `bits` into a fixed-size array.
    ///
    /// The alloc-free stand-in for `collect::<Vec<_>>()`: this module is
    /// compiled as the crate's own unit tests, and the crate is `#![no_std]`
    /// with no allocator unless a feature asks for one, so under
    /// `--no-default-features --features g3ruh` there is no `Vec` at all.
    /// Fixed buffers are also what the scrambler itself promises callers, so
    /// the tests now demonstrate the property instead of leaning on a heap.
    ///
    /// Asserts the source yielded exactly `N` bits, so a short iterator
    /// cannot quietly leave zero-filled tail slots in a comparison.
    fn bit_array<const N: usize>(bits: impl Iterator<Item = Bit>) -> [Bit; N] {
        let mut out = [Bit::Zero; N];
        let mut filled = 0usize;
        for (slot, bit) in out.iter_mut().zip(bits) {
            *slot = bit;
            filled += 1;
        }
        assert_eq!(filled, N, "source iterator yielded fewer than {N} bits");
        out
    }

    #[test]
    fn roundtrip_structured_sequences() {
        let sequences: &[&[Bit]] = &[
            &[],
            &[Bit::Zero],
            &[Bit::One],
            &[Bit::One; 64],
            &[Bit::Zero; 64],
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
            let mut tx = Scrambler::new();
            let mut rx = Descrambler::new();
            for &bit in seq {
                assert_eq!(rx.descramble(tx.scramble(bit)), bit);
            }
        }
    }

    #[test]
    fn roundtrip_pseudo_random_sequences() {
        for seed in [1, 0xDEAD_BEEF, 0x1234_5678] {
            let mut tx = Scrambler::new();
            let mut rx = Descrambler::new();
            for bit in pseudo_random_bits(seed, 8192) {
                assert_eq!(rx.descramble(tx.scramble(bit)), bit);
            }
        }
    }

    #[test]
    fn roundtrip_with_matching_nonzero_seed() {
        for seed in [1, 0x1_FFFF, 0x0AAAA, 0x15555] {
            let mut tx = Scrambler::with_state(seed);
            let mut rx = Descrambler::with_state(seed);
            for bit in pseudo_random_bits(seed, 2048) {
                assert_eq!(rx.descramble(tx.scramble(bit)), bit);
            }
        }
    }

    #[test]
    fn descrambler_self_synchronizes_within_17_bits() {
        // Every combination of mismatched TX/RX seeds: outputs may be wrong
        // only within the first 17 bits.
        for tx_seed in [0, 1, 0x1_FFFF, 0x12345] {
            for rx_seed in [0, 0x1_FFFF, 0x0F0F0, 0x1BEEF] {
                let mut tx = Scrambler::with_state(tx_seed);
                let mut rx = Descrambler::with_state(rx_seed);
                for (n, bit) in pseudo_random_bits(7, 1024).enumerate() {
                    let out = rx.descramble(tx.scramble(bit));
                    if n >= 17 {
                        assert_eq!(out, bit, "bit {n} must be correct after sync");
                    }
                }
            }
        }
    }

    /// Independent model of the recurrence out[n] = out[n-12] ^ out[n-17],
    /// computed over a plain history array rather than a packed register.
    fn lfsr_reference(seed: u32, len: usize) -> impl Iterator<Item = Bit> {
        // history[0] = most recent past output, history[16] = 17 ago.
        let mut history = [0u8; 17];
        for (d, slot) in history.iter_mut().enumerate() {
            *slot = ((seed >> d) & 1) as u8;
        }
        core::iter::repeat_with(move || {
            let out = history[11] ^ history[16];
            history.rotate_right(1);
            history[0] = out;
            Bit::from(out != 0)
        })
        .take(len)
    }

    #[test]
    fn all_zeros_input_is_pure_lfsr_sequence() {
        // With zero input the scrambler is a free-running LFSR; compare
        // against an independently coded recurrence.
        for seed in [1, 0x00800, 0x1_FFFF, 0x13579] {
            let mut tx = Scrambler::with_state(seed);
            for (n, expected) in lfsr_reference(seed, 4096).enumerate() {
                assert_eq!(tx.scramble(Bit::Zero), expected, "seed {seed:#x} bit {n}");
            }
        }
    }

    #[test]
    fn all_zeros_state_and_input_stay_zero() {
        let mut tx = Scrambler::new();
        for _ in 0..256 {
            assert_eq!(tx.scramble(Bit::Zero), Bit::Zero);
        }
    }

    #[test]
    fn lfsr_sequence_has_maximal_period() {
        // x^17 + x^12 + 1 is primitive over GF(2): from any nonzero state
        // the zero-input state sequence has period exactly 2^17 - 1.
        let period: u32 = (1 << 17) - 1;
        let start = Scrambler::with_state(1);
        let mut tx = start;
        let mut steps = 0u32;
        loop {
            tx.scramble(Bit::Zero);
            steps += 1;
            if tx == start {
                break;
            }
            assert!(steps <= period, "period must not exceed 2^17 - 1");
        }
        assert_eq!(steps, period);
    }

    #[test]
    fn single_channel_error_corrupts_exactly_offsets_0_12_17() {
        const TOTAL: usize = 256;
        const FLIP_AT: usize = 100;

        let data: [Bit; TOTAL] = bit_array(pseudo_random_bits(99, TOTAL));
        let mut channel: [Bit; TOTAL] =
            bit_array(Scrambler::new().scramble_iter(data.iter().copied()));
        channel[FLIP_AT] = match channel[FLIP_AT] {
            Bit::Zero => Bit::One,
            Bit::One => Bit::Zero,
        };

        // The recurrence out[n] = in[n] ^ out[n-12] ^ out[n-17] feeds each
        // channel bit into exactly three data bits, so a single channel
        // error produces exactly three wrong offsets: 0, +12 and +17. Four
        // slots, so a regression that widens the error burst overflows the
        // buffer and fails loudly rather than being silently truncated.
        let mut wrong = [0usize; 4];
        let mut count = 0usize;
        for (n, (got, want)) in Descrambler::new()
            .descramble_iter(channel.iter().copied())
            .zip(data.iter().copied())
            .enumerate()
        {
            if got != want {
                assert!(count < wrong.len(), "more than {} wrong bits", wrong.len());
                wrong[count] = n;
                count += 1;
            }
        }
        assert_eq!(&wrong[..count], &[FLIP_AT, FLIP_AT + 12, FLIP_AT + 17]);
    }

    #[test]
    fn iterator_adapters_agree_with_push_api() {
        const LEN: usize = 64;

        let data: [Bit; LEN] = bit_array(pseudo_random_bits(5, LEN));
        let mut tx = Scrambler::new();
        let mut rx = Descrambler::new();
        let mut compared = 0usize;
        // Streamed rather than collected: the adapter pair must agree with
        // the push API bit for bit and compose to the identity.
        for (&bit, iter_out) in data.iter().zip(
            Descrambler::new()
                .descramble_iter(Scrambler::new().scramble_iter(data.iter().copied())),
        ) {
            assert_eq!(rx.descramble(tx.scramble(bit)), iter_out);
            assert_eq!(iter_out, bit);
            compared += 1;
        }
        assert_eq!(compared, LEN, "the adapter pair must preserve length");
    }

    #[test]
    fn defaults_match_new() {
        assert_eq!(Scrambler::default(), Scrambler::new());
        assert_eq!(Descrambler::default(), Descrambler::new());
    }

    #[test]
    fn with_state_masks_to_17_bits() {
        assert_eq!(
            Scrambler::with_state(u32::MAX),
            Scrambler::with_state(0x1_FFFF)
        );
        assert_eq!(
            Descrambler::with_state(u32::MAX),
            Descrambler::with_state(0x1_FFFF)
        );
    }

    #[test]
    fn size_hints_pass_through() {
        let data = [Bit::One, Bit::Zero];
        assert_eq!(
            Scrambler::new()
                .scramble_iter(data.iter().copied())
                .size_hint(),
            (2, Some(2))
        );
        assert_eq!(
            Descrambler::new()
                .descramble_iter(data.iter().copied())
                .size_hint(),
            (2, Some(2))
        );
    }
}
