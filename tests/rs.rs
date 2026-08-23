//! Integration tests for the `fx25` Reed-Solomon `RS(255,k)` codec.
#![cfg(feature = "fx25")]

use warble::rs::{RsCodec, RsError, RsParity};

const PARITIES: [RsParity; 3] = [RsParity::Sixteen, RsParity::ThirtyTwo, RsParity::SixtyFour];

/// Tiny deterministic PRNG (xorshift64*) so error-injection tests are
/// reproducible without a dev-dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
}

/// Builds an encoded block of `data_len` pseudo-random data bytes followed
/// by parity; returns (block, block_len).
fn encoded_block(codec: &RsCodec, rng: &mut Rng, data_len: usize) -> ([u8; 255], usize) {
    let p = codec.parity_len();
    let mut block = [0u8; 255];
    for slot in block.iter_mut().take(data_len) {
        *slot = rng.byte();
    }
    let (data, rest) = block.split_at_mut(data_len);
    codec.encode(data, &mut rest[..p]).expect("encode");
    (block, data_len + p)
}

/// Flips `count` distinct symbols of `block[..len]` with nonzero deltas.
fn inject_errors(block: &mut [u8], len: usize, count: usize, rng: &mut Rng) {
    let mut hit = [false; 255];
    let mut injected = 0;
    while injected < count {
        let pos = rng.below(len);
        if hit[pos] {
            continue;
        }
        hit[pos] = true;
        let delta = loop {
            let d = rng.byte();
            if d != 0 {
                break d;
            }
        };
        block[pos] ^= delta;
        injected += 1;
    }
}

#[test]
fn clean_round_trip_all_parities() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for parity in PARITIES {
        let codec = RsCodec::new(parity);
        let data_len = codec.data_capacity();
        let (mut block, len) = encoded_block(&codec, &mut rng, data_len);
        let original = block;
        let corrected = codec.decode(&mut block[..len]).expect("clean decode");
        assert_eq!(corrected, 0);
        assert_eq!(block, original);
    }
}

#[test]
fn corrects_up_to_t_random_errors() {
    let mut rng = Rng(0xC0FF_EE00_DEAD_BEEF);
    for parity in PARITIES {
        let codec = RsCodec::new(parity);
        let t = codec.correctable();
        for errors in 1..=t {
            // Mix full-length and shortened blocks.
            let data_len = if errors % 2 == 0 {
                codec.data_capacity()
            } else {
                errors + 1 + rng.below(64)
            };
            let (block, len) = encoded_block(&codec, &mut rng, data_len);
            let mut corrupted = block;
            inject_errors(&mut corrupted, len, errors, &mut rng);
            let corrected = codec
                .decode(&mut corrupted[..len])
                .expect("within-capacity decode");
            assert_eq!(corrected, errors, "parity {parity:?}, {errors} errors");
            assert_eq!(corrupted[..len], block[..len]);
        }
    }
}

#[test]
fn beyond_t_errors_fail_or_miscorrect_without_panic() {
    let mut rng = Rng(0x0BAD_F00D_1357_9BDF);
    for parity in PARITIES {
        let codec = RsCodec::new(parity);
        let t = codec.correctable();
        for trial in 0..20 {
            let data_len = 100 + trial;
            let (block, len) = encoded_block(&codec, &mut rng, data_len);
            let mut corrupted = block;
            inject_errors(&mut corrupted, len, t + 1, &mut rng);
            match codec.decode(&mut corrupted[..len]) {
                // Detected: the common outcome.
                Err(RsError::Uncorrectable) => {}
                Err(other) => panic!("unexpected error kind: {other:?}"),
                // Bounded-distance decoding may land on a different valid
                // codeword; it must at least differ from the original.
                Ok(_) => assert_ne!(corrupted[..len], block[..len]),
            }
        }
    }
}

#[test]
fn shortened_round_trips_at_various_lengths() {
    let mut rng = Rng(0xFEED_FACE_CAFE_D00D);
    for parity in PARITIES {
        let codec = RsCodec::new(parity);
        let t = codec.correctable();
        for data_len in [1, 2, 5, 17, 64, 128, codec.data_capacity()] {
            if data_len > codec.data_capacity() {
                continue;
            }
            let (block, len) = encoded_block(&codec, &mut rng, data_len);
            let mut corrupted = block;
            let errors = t.min(len);
            inject_errors(&mut corrupted, len, errors, &mut rng);
            let corrected = codec.decode(&mut corrupted[..len]).expect("decode");
            assert_eq!(corrected, errors);
            assert_eq!(corrupted[..len], block[..len]);
        }
    }
}

/// GF(256) helpers re-derived from first principles for the known-answer
/// test: schoolbook carry-less multiply reduced by x^8+x^4+x^3+x^2+1.
fn gf_mul_slow(mut a: u8, mut b: u8) -> u8 {
    let mut acc = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            acc ^= a;
        }
        let carry = a & 0x80 != 0;
        a <<= 1;
        if carry {
            a ^= 0x1D;
        }
        b >>= 1;
    }
    acc
}

#[test]
fn known_answer_parity_matches_first_principles_division() {
    // Generator g(x) = prod_{i=1..=16} (x - a^i), built with the slow
    // multiply, coefficients lowest degree first.
    let mut root = 1u8;
    let mut generator = [0u8; 17];
    generator[0] = 1;
    for degree in 0..16 {
        root = gf_mul_slow(root, 2); // a^(degree+1)
        for j in (1..=degree + 1).rev() {
            generator[j] = generator[j - 1] ^ gf_mul_slow(root, generator[j]);
        }
        generator[0] = gf_mul_slow(root, generator[0]);
    }

    // Fixed message; compute the remainder of msg(x) * x^16 mod g(x) by
    // plain polynomial long division over GF(256).
    let msg: [u8; 24] = *b"The quick brown fox 1234";
    let mut work = [0u8; 24 + 16];
    work[..24].copy_from_slice(&msg);
    for i in 0..24 {
        let coef = work[i];
        if coef == 0 {
            continue;
        }
        // g is monic, so the quotient coefficient is `coef` itself.
        for (j, &g) in generator.iter().enumerate() {
            work[i + 16 - j] ^= gf_mul_slow(coef, g);
        }
    }
    let expected_parity = &work[24..];

    let codec = RsCodec::new(RsParity::Sixteen);
    let mut parity = [0u8; 16];
    codec.encode(&msg, &mut parity).expect("encode");
    assert_eq!(&parity, expected_parity);

    // And the block decodes cleanly.
    let mut block = [0u8; 40];
    block[..24].copy_from_slice(&msg);
    block[24..].copy_from_slice(&parity);
    assert_eq!(codec.decode(&mut block), Ok(0));
}

#[test]
fn typed_errors_on_bad_slice_lengths() {
    let codec = RsCodec::new(RsParity::ThirtyTwo);
    let data = [0u8; 250];
    let mut parity = [0u8; 32];
    assert_eq!(
        codec.encode(&data, &mut parity),
        Err(RsError::DataTooLong { got: 250, max: 223 })
    );
    let mut short_parity = [0u8; 16];
    assert_eq!(
        codec.encode(&data[..10], &mut short_parity),
        Err(RsError::ParityLengthMismatch {
            got: 16,
            expected: 32
        })
    );
    let mut tiny = [0u8; 32];
    assert_eq!(
        codec.decode(&mut tiny),
        Err(RsError::BlockLengthInvalid {
            got: 32,
            min: 33,
            max: 255
        })
    );
    let mut huge = [0u8; 256];
    assert_eq!(
        codec.decode(&mut huge),
        Err(RsError::BlockLengthInvalid {
            got: 256,
            min: 33,
            max: 255
        })
    );
}
