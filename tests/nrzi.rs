//! Integration tests for the `nrzi` feature: NRZI encode/decode roundtrip,
//! the all-ones stall property, zero-run toggling, decoder self-sync, and
//! agreement between the push API and the iterator adapters.
#![cfg(feature = "nrzi")]

use yodel::{Bit, NrziDecoder, NrziEncoder};

fn toggle(bit: Bit) -> Bit {
    match bit {
        Bit::Zero => Bit::One,
        Bit::One => Bit::Zero,
    }
}

/// Deterministic pseudo-random bit sequence (xorshift32).
fn pseudo_random_bits(seed: u32, len: usize) -> Vec<Bit> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            Bit::from(state & 1 != 0)
        })
        .collect()
}

fn bytes_to_bits_lsb(bytes: &[u8]) -> Vec<Bit> {
    bytes
        .iter()
        .flat_map(|&b| (0..8).map(move |i| Bit::from((b >> i) & 1 != 0)))
        .collect()
}

fn encode_all(data: &[Bit], initial: Bit) -> Vec<Bit> {
    let mut enc = NrziEncoder::new(initial);
    data.iter().map(|&b| enc.encode(b)).collect()
}

fn decode_all(line: &[Bit], initial: Bit) -> Vec<Bit> {
    let mut dec = NrziDecoder::new(initial);
    line.iter().map(|&b| dec.decode(b)).collect()
}

#[test]
fn roundtrip_assorted_sequences_both_initial_states() {
    let sequences: Vec<Vec<Bit>> = vec![
        vec![],
        vec![Bit::Zero],
        vec![Bit::One],
        vec![Bit::One, Bit::Zero, Bit::One, Bit::Zero],
        bytes_to_bits_lsb(&[0x7E, 0x00, 0xFF, 0xA5, 0x3C]),
        pseudo_random_bits(0xC0FF_EE00, 2048),
        pseudo_random_bits(7, 333),
    ];
    for seq in &sequences {
        for initial in [Bit::Zero, Bit::One] {
            let line = encode_all(seq, initial);
            assert_eq!(&decode_all(&line, initial), seq);
        }
    }
}

#[test]
fn all_ones_stall_constant_line_level() {
    for initial in [Bit::Zero, Bit::One] {
        let line = encode_all(&[Bit::One; 100], initial);
        assert!(line.iter().all(|&b| b == initial));
    }
}

#[test]
fn constant_line_decodes_to_all_ones_after_first_bit() {
    for level in [Bit::Zero, Bit::One] {
        for initial in [Bit::Zero, Bit::One] {
            let out = decode_all(&[level; 100], initial);
            assert_eq!(out[0], Bit::from(level == initial));
            assert!(out[1..].iter().all(|&b| b == Bit::One));
        }
    }
}

#[test]
fn all_zeros_yields_alternating_line() {
    for initial in [Bit::Zero, Bit::One] {
        let line = encode_all(&[Bit::Zero; 100], initial);
        let mut expected = initial;
        for &bit in &line {
            expected = toggle(expected);
            assert_eq!(bit, expected);
        }
    }
}

#[test]
fn decoder_self_synchronizes_after_at_most_one_bit() {
    let data = pseudo_random_bits(0xABCD_1234, 512);
    for enc_initial in [Bit::Zero, Bit::One] {
        let line = encode_all(&data, enc_initial);
        let out = decode_all(&line, toggle(enc_initial));
        // Only the first bit may be corrupted by the mismatched state.
        assert_eq!(out[0], toggle(data[0]));
        assert_eq!(&out[1..], &data[1..]);
    }
}

#[test]
fn iterator_adapters_agree_with_push_api() {
    let data = pseudo_random_bits(99, 777);
    for initial in [Bit::Zero, Bit::One] {
        let via_push = encode_all(&data, initial);
        let via_iter: Vec<Bit> = NrziEncoder::new(initial)
            .encode_iter(data.iter().copied())
            .collect();
        assert_eq!(via_iter, via_push);

        let dec_push = decode_all(&via_push, initial);
        let dec_iter: Vec<Bit> = NrziDecoder::new(initial)
            .decode_iter(via_push.iter().copied())
            .collect();
        assert_eq!(dec_iter, dec_push);
        assert_eq!(dec_iter, data);
    }
}

#[test]
fn free_function_adapters_roundtrip_with_default_state() {
    let data = pseudo_random_bits(5, 256);
    let back: Vec<Bit> =
        yodel::nrzi::decode_iter(yodel::nrzi::encode_iter(data.iter().copied())).collect();
    assert_eq!(back, data);
}
