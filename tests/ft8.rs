//! FT8 TX tests: payload packing, CRC-14, LDPC(174,91), Gray/Costas
//! mapping, and audio synthesis.
//!
//! Provenance note (mirrors tests/wspr.rs): no full 79-symbol vector
//! from an independent published source is asserted here.
//!
//! Not because none exists — one does, and it was located, checked
//! against this implementation, and found to match at all four stages
//! (77 payload bits, CRC-14, 83 parity bits, 79 symbols). It was then
//! **not used**: it ships inside a GPL-3.0 manual page,
//! and `CONTRIBUTING.md` forbids copying GPL test fixtures into this
//! crate "not even just the constants". See the note there on why that
//! rule holds even when the vector is arguably an uncopyrightable
//! fact. The tier-4 comparison in `tests/ft8_differential.rs` reaches
//! the same conclusion by running the other implementation rather than
//! quoting it, which is the sanctioned route.
//!
//! So this file's composed-encoding coverage is weaker than it could
//! be, and that is a licence decision rather than an oversight.
//!
//! What *is* pinned to an external published source is the channel
//! coding itself: `generator_bits_match_public_domain_file` and
//! `check_rows_match_public_domain_parity_file` check both LDPC matrices
//! against `third_party/ft4_ft8_public/`, vendored from the protocol
//! authors' own public-domain resource package. Those are tier-1 and
//! gate CI, so the constants most likely to be silently wrong — and the
//! ones whose provenance was previously only asserted in a comment — are
//! now mechanically verified.
//!
//! Beyond that this suite carries (a) invariant
//! proofs at every stage — c28/g15 packing edge cases, a CRC-14
//! known-answer vector computed by long division inside the test
//! (independent code path), H·c = 0 for every codeword over many
//! random payloads plus single-bit-flip detection, Gray-map bijection
//! and adjacency, Costas placement — and (b) a frozen regression
//! snapshot of this implementation's 79 symbols for "CQ K1ABC FN42",
//! labeled as self-derived. The RX slice will close the loop end to end.
#![cfg(feature = "ft8")]

use warble::ft8::{
    self, CODEWORD_LEN, COSTAS, CRC_POLY, Ft8Config, Ft8Error, Ft8Message, Ft8Modulator, Ft8Tail,
    GRAY_MAP, MAXGRID4, MESSAGE_LEN, PAYLOAD_LEN, SYMBOL_COUNT, add_crc, crc14, gfsk_pulse,
    ldpc_check, ldpc_encode, pack_c28, pack_g15, symbols_from_codeword, unpack_free_text,
};
use warble::geo::GeoError;
use warble::{MaidenheadGrid, SampleRate};

/// A grid trailer from locator text, panicking on invalid input: these
/// tests are about the FT8 encoding, and locator parsing has its own
/// suite in `warble::geo`.
fn grid(text: &str) -> Ft8Tail {
    Ft8Tail::grid(text).expect("valid locator")
}

// ---------------------------------------------------------------- c28

#[test]
fn c28_special_tokens() {
    assert_eq!(pack_c28("DE").unwrap(), 0);
    assert_eq!(pack_c28("QRZ").unwrap(), 1);
    assert_eq!(pack_c28("CQ").unwrap(), 2);
    // Case-insensitive.
    assert_eq!(pack_c28("cq").unwrap(), 2);
}

#[test]
fn c28_standard_calls_pack_positionally() {
    // " A0A  " packs indices (0, 10, 0, 1, 0, 0) over the published
    // positional sets, offset by NTOKENS + MAX22. Independent
    // hand-computation: ((((0*36+10)*10+0)*27+1)*27+0)*27+0.
    let expected_index: u32 = (((10 * 10) * 27 + 1) * 27) * 27;
    assert_eq!(
        pack_c28("A0A").unwrap(),
        ft8::NTOKENS + ft8::MAX22 + expected_index
    );
    // Aligned differently: K1ABC aligns to " K1ABC" (digit third),
    // KA1ABC stays put.
    let k1abc = pack_c28("K1ABC").unwrap();
    let ka1abc = pack_c28("KA1ABC").unwrap();
    assert!(k1abc >= ft8::NTOKENS + ft8::MAX22);
    assert_ne!(k1abc, ka1abc);
    // Lowercase folds to the same value.
    assert_eq!(pack_c28("k1abc").unwrap(), k1abc);
}

#[test]
fn c28_field_partition_is_exact() {
    // NTOKENS + MAX22 + 37*36*10*27^3 == 2^28: the c28 space is full,
    // so the maximum standard callsign fits in 28 bits.
    let max_call = 37u64 * 36 * 10 * 27 * 27 * 27 - 1;
    assert_eq!(
        u64::from(ft8::NTOKENS) + u64::from(ft8::MAX22) + max_call,
        (1u64 << 28) - 1
    );
    // ZZ9ZZZ is that maximum.
    assert_eq!(pack_c28("ZZ9ZZZ").unwrap(), (1u32 << 28) - 1);
}

#[test]
fn c28_rejections_are_specific() {
    assert_eq!(pack_c28("K1/ABC"), Err(Ft8Error::CallsignCompound));
    assert_eq!(pack_c28("CQ DX"), Err(Ft8Error::DirectedCqUnsupported));
    assert_eq!(pack_c28(""), Err(Ft8Error::CallsignLength { len: 0 }));
    assert_eq!(
        pack_c28("ABCDEFG"),
        Err(Ft8Error::CallsignLength { len: 7 })
    );
    // No digit in position 2 (or 1 after shifting): unalignable.
    assert_eq!(pack_c28("ABCDEF"), Err(Ft8Error::CallsignShape));
    // '-' lands at aligned position 5 after the shift-by-one
    // ("K1AB-" → " K1AB-").
    assert_eq!(
        pack_c28("K1AB-"),
        Err(Ft8Error::CallsignChar { ch: '-', index: 5 })
    );
}

// ---------------------------------------------------------------- g15

#[test]
fn g15_grid_packs_positionally() {
    // AA00 -> 0; RR99 -> 32399 (the published grid range).
    assert_eq!(pack_g15(grid("AA00")).unwrap(), 0);
    assert_eq!(pack_g15(grid("RR99")).unwrap(), MAXGRID4 - 1);
    // FN42 = ((5*18 + 13)*10 + 4)*10 + 2.
    assert_eq!(
        pack_g15(grid("FN42")).unwrap(),
        ((5 * 18 + 13) * 10 + 4) * 10 + 2
    );
    // Lowercase folds (in `MaidenheadGrid`, before FT8 ever sees it).
    assert_eq!(
        pack_g15(grid("fn42")).unwrap(),
        pack_g15(grid("FN42")).unwrap()
    );
    // The trailer can also be built from an already-parsed locator.
    assert_eq!(
        pack_g15(Ft8Tail::Grid(MaidenheadGrid::new("FN42").unwrap())).unwrap(),
        pack_g15(grid("FN42")).unwrap()
    );
}

#[test]
fn g15_specials_sit_above_maxgrid4() {
    assert_eq!(pack_g15(Ft8Tail::None).unwrap(), MAXGRID4 + 1);
    assert_eq!(pack_g15(Ft8Tail::Rrr).unwrap(), MAXGRID4 + 2);
    assert_eq!(pack_g15(Ft8Tail::Seventy3).unwrap(), MAXGRID4 + 4);
    // Reports: r + 35 above MAXGRID4 (so -30 -> +5, 0 -> +35, +49 -> +84).
    assert_eq!(pack_g15(Ft8Tail::Report(-30)).unwrap(), MAXGRID4 + 5);
    assert_eq!(pack_g15(Ft8Tail::Report(0)).unwrap(), MAXGRID4 + 35);
    assert_eq!(pack_g15(Ft8Tail::Report(49)).unwrap(), MAXGRID4 + 84);
    // Everything fits 15 bits.
    const { assert!(MAXGRID4 + 84 < (1 << 15)) };
}

/// `RR73` is the one trailer that is **not** a reserved token on the
/// air: it packs as the Maidenhead square of the same name.
///
/// `MAXGRID4 + 3` is what the reserved-token table reserves for it and
/// every decoder accepts it — including this one, asserted below. But
/// the dominant implementation's packer tests "is this a valid
/// four-character locator?" before consulting the token list, and
/// `RR73` is one, so that is what real traffic carries. Matching it
/// makes a warble transmission bit-identical rather than just
/// intelligible; `tests/ft8_differential.rs` is what proves it against
/// an independent encoder, and this test is the tier-2 statement of
/// the same fact so a contributor without that binary still sees it.
#[test]
fn rr73_packs_as_a_grid_square_not_as_a_token() {
    // Why the overload is safe, computed rather than asserted: RR73 is
    // 83.5 N, 175 E -- the Arctic Ocean north of Siberia, where no
    // station transmits. An earlier version of this comment said "the
    // empty mid-Pacific", which is the right argument about the wrong
    // ocean, and is precisely the kind of unchecked claim this project
    // keeps finding in its own code.
    let square = warble::geo::Coordinates::from_maidenhead(MaidenheadGrid::new("RR73").unwrap());
    assert_eq!(square.latitude.to_degrees(), 83.5);
    assert_eq!(square.longitude.to_degrees(), 175.0);

    let grid = pack_g15(Ft8Tail::grid("RR73").unwrap()).unwrap();
    assert_eq!(grid, 32_373, "the published four-character grid layout");
    assert_eq!(
        pack_g15(Ft8Tail::Rr73).unwrap(),
        grid,
        "RR73 must go out as the grid the network uses, not MAXGRID4 + 3"
    );
    assert!(grid < MAXGRID4, "and it is therefore below MAXGRID4");

    // Both spellings still decode to the same text, which is why
    // choosing between them costs nothing.
    let token = Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Rr73).unwrap();
    assert_eq!(
        ft8::unpack_message(&token.payload()).unwrap().as_str(),
        "K1ABC W9XYZ RR73"
    );
    let mut payload = token.payload();
    // Overwrite g15 (bits 59..74) with the reserved token value.
    for (i, bit) in (0..15).map(|i| (i, (MAXGRID4 + 3) >> (14 - i) & 1)) {
        let pos = 59 + i;
        payload[pos / 8] &= !(1 << (7 - pos % 8));
        payload[pos / 8] |= (bit as u8) << (7 - pos % 8);
    }
    assert_eq!(
        ft8::unpack_message(&payload).unwrap().as_str(),
        "K1ABC W9XYZ RR73",
        "the reserved-token spelling must still decode"
    );
}

#[test]
fn g15_rejections_are_specific() {
    // Malformed locator text never becomes a trailer at all: the same
    // three rejections the module's own validator used to make, now
    // typed by `MaidenheadGrid` (short, field letter past R, letter
    // where a square digit belongs).
    assert_eq!(
        Ft8Tail::grid("FN4"),
        Err(GeoError::BadGridLength { got: 3 })
    );
    assert_eq!(
        Ft8Tail::grid("SN42"),
        Err(GeoError::BadGridChar {
            got: b'S',
            position: 0
        })
    );
    assert_eq!(
        Ft8Tail::grid("FNA2"),
        Err(GeoError::BadGridChar {
            got: b'A',
            position: 2
        })
    );
    // A well-formed locator finer than a square is rejected, not
    // truncated: `g15` holds four characters, and dropping the
    // subsquare would transmit a place the operator did not name.
    assert_eq!(
        pack_g15(grid("FN42ab")),
        Err(Ft8Error::GridLength { len: 6 })
    );
    assert_eq!(
        pack_g15(grid("FN42ab12")),
        Err(Ft8Error::GridLength { len: 8 })
    );
    assert_eq!(
        pack_g15(Ft8Tail::Report(-31)),
        Err(Ft8Error::ReportOutOfRange { got: -31 })
    );
    assert_eq!(
        pack_g15(Ft8Tail::Report(50)),
        Err(Ft8Error::ReportOutOfRange { got: 50 })
    );
}

// ------------------------------------------------------ standard msgs

#[test]
fn standard_message_payload_layout() {
    // Verify the 77-bit field layout bit by bit against an
    // independently assembled reference.
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let c28a = u128::from(pack_c28("CQ").unwrap());
    let c28b = u128::from(pack_c28("K1ABC").unwrap());
    let g15 = u128::from(pack_g15(grid("FN42")).unwrap());
    // c28a|r1a|c28b|r1b|R1|g15|i3, MSB-first.
    let value: u128 = (((((c28a << 1) << 28 | c28b) << 1) << 1) << 15 | g15) << 3 | 1;
    let mut expected = [0u8; PAYLOAD_LEN];
    for pos in 0..77 {
        let bit = (value >> (76 - pos)) & 1;
        expected[pos / 8] |= (bit as u8) << (7 - pos % 8);
    }
    assert_eq!(msg.payload(), expected);
}

#[test]
fn standard_message_r_flag_rules() {
    // R with grid or report: fine.
    assert!(Ft8Message::standard("K1ABC", "W9XYZ", true, grid("EN37")).is_ok());
    assert!(Ft8Message::standard("K1ABC", "W9XYZ", true, Ft8Tail::Report(-8)).is_ok());
    // R with RRR/RR73/73/none: rejected.
    for tail in [
        Ft8Tail::Rrr,
        Ft8Tail::Rr73,
        Ft8Tail::Seventy3,
        Ft8Tail::None,
    ] {
        assert_eq!(
            Ft8Message::standard("K1ABC", "W9XYZ", true, tail),
            Err(Ft8Error::AckFlagInvalid)
        );
    }
}

#[test]
fn standard_message_second_call_must_be_a_call() {
    for token in ["CQ", "QRZ", "DE"] {
        assert_eq!(
            Ft8Message::standard("K1ABC", token, false, Ft8Tail::Seventy3),
            Err(Ft8Error::TokenNotAllowedHere)
        );
    }
}

// ------------------------------------------------------- free text

#[test]
fn free_text_roundtrip() {
    let msg = Ft8Message::free_text("TNX BOB 73 GL").unwrap();
    let chars = unpack_free_text(&msg.payload()).unwrap();
    assert_eq!(&chars, b"TNX BOB 73 GL");
    // Short text is **right**-justified: the 13 characters are one
    // base-42 integer, so which end the padding goes on changes every
    // bit of the payload, and the network pads on the left. MEASURED
    // against an independent encoder; `tests/ft8_differential.rs` is
    // the proof, this is the tier-2 statement of it.
    let msg = Ft8Message::free_text("HI").unwrap();
    assert_eq!(&unpack_free_text(&msg.payload()).unwrap(), b"           HI");
    // Empty is legal (all spaces).
    let msg = Ft8Message::free_text("").unwrap();
    assert_eq!(&unpack_free_text(&msg.payload()).unwrap(), b"             ");
    // Full alphabet coverage in chunks.
    for text in [" 0123456789AB", "CDEFGHIJKLMNO", "PQRSTUVWXYZ+-", "./?"] {
        let msg = Ft8Message::free_text(text).unwrap();
        let chars = unpack_free_text(&msg.payload()).unwrap();
        let offset = 13 - text.len();
        assert_eq!(&chars[offset..], text.as_bytes());
        assert!(chars[..offset].iter().all(|&c| c == b' '));
    }
    // The displayed form trims both ends, so the padding side is
    // invisible to a caller who only reads text.
    for text in ["HI", "TNX BOB 73 GL", "./?", ""] {
        let msg = Ft8Message::free_text(text).unwrap();
        assert_eq!(ft8::unpack_message(&msg.payload()).unwrap().as_str(), text);
    }
}

#[test]
fn free_text_rejections() {
    assert_eq!(
        Ft8Message::free_text("FOURTEEN CHARS"),
        Err(Ft8Error::FreeTextLength { len: 14 })
    );
    assert_eq!(
        Ft8Message::free_text("HI!"),
        Err(Ft8Error::FreeTextChar { ch: '!', index: 2 })
    );
}

#[test]
fn free_text_sets_i3_n3_zero() {
    let payload = Ft8Message::free_text("TEST").unwrap().payload();
    // i3 = bits 74..77 (byte 9 bits 5..3), n3 = bits 71..74.
    assert_eq!((payload[9] >> 3) & 0x7, 0);
    assert_eq!(((payload[8] & 1) << 2) | ((payload[9] >> 6) & 0x3), 0);
    // Low 3 bits of the last byte are padding, always zero.
    assert_eq!(payload[9] & 0x7, 0);
}

// ------------------------------------------------------------- CRC-14

/// Independent CRC-14 reference: bitwise long division over the 77
/// payload bits + 5 zeros, written from the definition (not shared
/// with the implementation).
fn crc14_reference(payload: &[u8; PAYLOAD_LEN]) -> u16 {
    // Build the 82-bit dividend, then append 14 zero bits and divide.
    let mut bits = [0u8; 96];
    for (pos, slot) in bits.iter_mut().enumerate().take(77) {
        *slot = (payload[pos / 8] >> (7 - pos % 8)) & 1;
    }
    let poly: u32 = 0x4000 | u32::from(CRC_POLY); // x^14 term explicit
    let mut rem: u32 = 0;
    for &bit in &bits {
        rem = (rem << 1) | u32::from(bit);
        if rem & 0x4000 != 0 {
            rem ^= poly;
        }
    }
    (rem & 0x3FFF) as u16
}

#[test]
fn crc14_matches_independent_long_division() {
    let payloads = [
        [0u8; PAYLOAD_LEN],
        [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF8],
        Ft8Message::standard("CQ", "K1ABC", false, grid("FN42"))
            .unwrap()
            .payload(),
        Ft8Message::free_text("CRC14 KAT").unwrap().payload(),
    ];
    for p in payloads {
        assert_eq!(crc14(&p), crc14_reference(&p), "payload {p:02x?}");
    }
}

#[test]
fn crc14_known_answer() {
    // Frozen known-answer vector, cross-checked in this suite by the
    // independent long division above: a payload of a single 1 in bit
    // 76 (last payload bit set). The dividend is that bit followed by
    // 5 zero-extension bits, i.e. the polynomial x^19 mod the CRC
    // polynomial.
    let mut payload = [0u8; PAYLOAD_LEN];
    payload[9] = 0x08; // bit 76
    let expected = crc14_reference(&payload);
    assert_eq!(crc14(&payload), expected);
    // And the all-zero payload has CRC 0 (zero init, no final XOR).
    assert_eq!(crc14(&[0u8; PAYLOAD_LEN]), 0);
}

#[test]
fn add_crc_layout() {
    let payload = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42"))
        .unwrap()
        .payload();
    let message = add_crc(&payload);
    // Payload bits preserved.
    assert_eq!(&message[..9], &payload[..9]);
    assert_eq!(message[9] & 0xF8, payload[9]);
    // CRC occupies bits 77..91; bits 91..96 are zero.
    let crc = crc14(&payload);
    let mut extracted: u16 = 0;
    for pos in 77..91 {
        extracted = (extracted << 1) | u16::from((message[pos / 8] >> (7 - pos % 8)) & 1);
    }
    assert_eq!(extracted, crc);
    assert_eq!(message[MESSAGE_LEN - 1] & 0x1F, 0);
}

// ---------------------------------------------------------------- LDPC

/// Tiny deterministic xorshift for reproducible random payloads.
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn random_payload(rng: &mut XorShift) -> [u8; PAYLOAD_LEN] {
    let mut p = [0u8; PAYLOAD_LEN];
    for b in p.iter_mut() {
        *b = (rng.next() >> 32) as u8;
    }
    p[9] &= 0xF8; // keep the 3 padding bits clear
    p
}

#[test]
fn ldpc_every_codeword_satisfies_all_checks() {
    // The strongest dependency-free proof: H·c = 0 over GF(2) for the
    // codeword of every one of 500 random payloads (plus edges).
    let mut rng = XorShift(0x1234_5678_9ABC_DEF0);
    for i in 0..500 {
        let payload = random_payload(&mut rng);
        let codeword = ldpc_encode(&add_crc(&payload));
        assert_eq!(ldpc_check(&codeword), 0, "payload #{i} {payload:02x?}");
    }
    for payload in [
        [0u8; PAYLOAD_LEN],
        [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF8],
    ] {
        assert_eq!(ldpc_check(&ldpc_encode(&add_crc(&payload))), 0);
    }
}

#[test]
fn ldpc_single_bit_flip_fails_checks() {
    // Generator/H consistency: flipping any single codeword bit must
    // violate at least one parity check (columns of H are nonzero).
    let payload = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42"))
        .unwrap()
        .payload();
    let codeword = ldpc_encode(&add_crc(&payload));
    for pos in 0..174 {
        let mut corrupted = codeword;
        corrupted[pos / 8] ^= 1 << (7 - pos % 8);
        assert!(ldpc_check(&corrupted) > 0, "flip at bit {pos} undetected");
    }
}

#[test]
fn ldpc_is_linear_and_systematic() {
    // Systematic: the first 91 codeword bits are the message (byte 11
    // additionally carries parity bits 91..96 in its low 5 bits).
    let payload = Ft8Message::free_text("LINEARITY").unwrap().payload();
    let message = add_crc(&payload);
    let codeword = ldpc_encode(&message);
    assert_eq!(&codeword[..MESSAGE_LEN - 1], &message[..MESSAGE_LEN - 1]);
    assert_eq!(codeword[MESSAGE_LEN - 1] & 0xE0, message[MESSAGE_LEN - 1]);
    // Linear: encode(a) XOR encode(b) == encode(a XOR b).
    let mut rng = XorShift(42);
    let a = add_crc(&random_payload(&mut rng));
    let b = add_crc(&random_payload(&mut rng));
    let mut ab = [0u8; MESSAGE_LEN];
    for i in 0..MESSAGE_LEN {
        ab[i] = a[i] ^ b[i];
    }
    let ca = ldpc_encode(&a);
    let cb = ldpc_encode(&b);
    let cab = ldpc_encode(&ab);
    for i in 0..CODEWORD_LEN {
        assert_eq!(ca[i] ^ cb[i], cab[i]);
    }
    // Zero encodes to zero.
    assert_eq!(ldpc_encode(&[0u8; MESSAGE_LEN]), [0u8; CODEWORD_LEN]);
}

// --------------------------------------------------- symbols / Costas

#[test]
fn gray_map_is_a_bijection_with_adjacency() {
    let mut seen = [false; 8];
    for &tone in &GRAY_MAP {
        assert!(tone <= 7);
        assert!(!seen[usize::from(tone)], "tone {tone} mapped twice");
        seen[usize::from(tone)] = true;
    }
    assert!(seen.iter().all(|&s| s));
    // Gray property: consecutive 3-bit inputs map to tones... no —
    // the FT8 map is bits->tone such that ADJACENT TONES differ in one
    // bit: check the inverse map is a Gray sequence over tones 0..=7.
    let mut inverse = [0u8; 8];
    for (bits, &tone) in GRAY_MAP.iter().enumerate() {
        inverse[usize::from(tone)] = bits as u8;
    }
    for t in 0..7 {
        let diff = inverse[t] ^ inverse[t + 1];
        assert_eq!(diff.count_ones(), 1, "tones {t},{} differ in >1 bit", t + 1);
    }
}

#[test]
fn costas_placement_and_data_layout() {
    let payload = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42"))
        .unwrap()
        .payload();
    let codeword = ldpc_encode(&add_crc(&payload));
    let symbols = symbols_from_codeword(&codeword);
    assert_eq!(symbols.len(), SYMBOL_COUNT);
    assert!(symbols.iter().all(|&s| s <= 7));
    // Costas arrays at 0..7, 36..43, 72..79.
    assert_eq!(&symbols[0..7], &COSTAS);
    assert_eq!(&symbols[36..43], &COSTAS);
    assert_eq!(&symbols[72..79], &COSTAS);
    // The Costas sequence itself is the published 3,1,4,0,6,5,2 and is
    // a true Costas array (all difference vectors distinct).
    assert_eq!(COSTAS, [3, 1, 4, 0, 6, 5, 2]);
    for d in 1..7 {
        let mut diffs = [false; 15];
        for i in 0..(7 - d) {
            let diff = i16::from(COSTAS[i + d]) - i16::from(COSTAS[i]) + 7;
            assert!(!diffs[diff as usize], "repeated difference at lag {d}");
            diffs[diff as usize] = true;
        }
    }
    // Data symbols: position 7 + j carries Gray(bits 3j..3j+3).
    for j in 0..58usize {
        let mut bits = 0u8;
        for b in 0..3 {
            let pos = 3 * j + b;
            bits = (bits << 1) | ((codeword[pos / 8] >> (7 - pos % 8)) & 1);
        }
        let position = if j < 29 { 7 + j } else { 43 + (j - 29) };
        assert_eq!(symbols[position], GRAY_MAP[usize::from(bits)]);
    }
}

#[test]
fn frozen_symbol_snapshot_cq_k1abc_fn42() {
    // Frozen regression snapshot of THIS implementation's output for
    // "CQ K1ABC FN42" — labeled as self-derived: generated by this
    // code, so it detects regressions in any pipeline stage, not
    // transcription errors against the reference implementation. The RX
    // slice will close that loop.
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let symbols = msg.channel_symbols();
    let frozen = FROZEN_CQ_K1ABC_FN42;
    assert_eq!(symbols, frozen);
}

/// Self-derived snapshot; see `frozen_symbol_snapshot_cq_k1abc_fn42`.
#[rustfmt::skip]
const FROZEN_CQ_K1ABC_FN42: [u8; SYMBOL_COUNT] = [
    3, 1, 4, 0, 6, 5, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 5, 4, 7, 6, 7, 0, 4, 6, 0, 6, 0, 2,
    1, 5, 3, 3, 4, 3, 3, 1, 4, 0, 6, 5, 2, 7, 3, 6, 0, 1, 1, 0, 4, 7, 5, 1, 7, 0, 0, 7, 3, 3,
    4, 7, 4, 5, 4, 5, 5, 1, 3, 3, 5, 4, 3, 1, 4, 0, 6, 5, 2,
];

// ---------------------------------------------------------------- audio

#[test]
fn config_validation() {
    let sr12k = SampleRate::new(12_000).unwrap();
    let cfg = Ft8Config::new(1_500, sr12k).unwrap();
    assert_eq!(cfg.samples_per_symbol(), 1_920);
    assert_eq!(cfg.base_hz(), 1_500);
    // 44.1 kHz is a multiple of 25 Hz (44100/25 = 1764) — exact.
    let cfg441 = Ft8Config::new(1_500, SampleRate::new(44_100).unwrap()).unwrap();
    assert_eq!(cfg441.samples_per_symbol(), 7_056);
    // Not a multiple of 25.
    assert_eq!(
        Ft8Config::new(1_500, SampleRate::new(12_010).unwrap()),
        Err(Ft8Error::SampleRateInexact { got: 12_010 })
    );
    // Nyquist / zero-base rejections. Highest tone = base + 43.75 Hz;
    // 5956 + 43.75 = 5999.75 < 6000 still fits, 5957 does not.
    assert_eq!(
        Ft8Config::new(0, sr12k),
        Err(Ft8Error::ToneOutOfRange {
            base_hz: 0,
            sample_rate: 12_000
        })
    );
    assert!(Ft8Config::new(5_957, sr12k).is_err());
    assert!(Ft8Config::new(5_956, sr12k).is_ok());
}

#[test]
fn audio_sample_count_and_timing() {
    let cfg = Ft8Config::new(1_500, SampleRate::new(12_000).unwrap()).unwrap();
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let mut tx = Ft8Modulator::for_message(cfg, &msg);
    assert_eq!(tx.total_samples(), 79 * 1_920);
    // 79 × 0.16 s = 12.64 s at 12 kHz.
    assert_eq!(tx.total_samples(), 151_680);
    let mut count = 0u64;
    let mut buf = [0i16; 4_096];
    loop {
        let n = tx.fill_i16(&mut buf);
        count += n as u64;
        if n < buf.len() {
            break;
        }
    }
    assert_eq!(count, 151_680);
    assert_eq!(tx.next_i16(), None);
}

/// Goertzel power of `samples` at `hz` (12 kHz rate).
fn goertzel(samples: &[i16], hz: f64) -> f64 {
    let w = 2.0 * core::f64::consts::PI * hz / 12_000.0;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = f64::from(x) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

#[test]
fn per_symbol_dominant_tone() {
    let cfg = Ft8Config::new(1_500, SampleRate::new(12_000).unwrap()).unwrap();
    let msg = Ft8Message::standard("K1ABC", "W9XYZ", true, Ft8Tail::Report(-8)).unwrap();
    let symbols = msg.channel_symbols();
    let mut tx = Ft8Modulator::new(cfg, symbols);
    let mut sym = [0i16; 1_920];
    for (i, &expected) in symbols.iter().enumerate() {
        assert_eq!(tx.fill_i16(&mut sym), 1_920, "symbol {i}");
        // GFSK smoothing spans symbol boundaries: measure the middle
        // half of the symbol where the frequency has settled.
        let mid = &sym[480..1_440];
        let mut best = (0u8, f64::MIN);
        for tone in 0..8u8 {
            let p = goertzel(mid, 1_500.0 + 6.25 * f64::from(tone));
            if p > best.1 {
                best = (tone, p);
            }
        }
        assert_eq!(best.0, expected, "symbol {i}");
    }
}

#[test]
fn phase_continuity() {
    // No discontinuity: successive samples never jump more than the
    // maximum per-sample phase step allows. At tone 7 (1543.75 Hz at
    // 12 kHz) the waveform advances < 0.81 rad/sample, so |Δ| between
    // consecutive i16 samples is bounded well below full scale.
    let cfg = Ft8Config::new(1_500, SampleRate::new(12_000).unwrap()).unwrap();
    let msg = Ft8Message::free_text("PHASE TEST").unwrap();
    let tx = Ft8Modulator::for_message(cfg, &msg);
    let mut prev: Option<i16> = None;
    // Max |Δsin| per sample = max phase step (2π·1543.75/12000 ≈ 0.808
    // rad) → |Δ| ≤ 0.808 × 32767 ≈ 26 500. Use a small margin.
    for s in tx {
        if let Some(p) = prev {
            let delta = (i32::from(s) - i32::from(p)).unsigned_abs();
            assert!(delta < 27_000, "phase jump: {p} -> {s}");
        }
        prev = Some(s);
    }
}

#[test]
fn gfsk_pulse_properties() {
    // The BT=2.0 Gaussian frequency pulse: unit area partition — the
    // shifted pulses sum to 1 at any instant (so the instantaneous
    // frequency is always a convex blend of neighboring tones).
    for k in 0..100 {
        let t = -0.5 + f64::from(k) / 100.0;
        let sum = gfsk_pulse(t - 1.0) + gfsk_pulse(t) + gfsk_pulse(t + 1.0);
        assert!((sum - 1.0).abs() < 1e-6, "t={t}: sum={sum}");
    }
    // Symmetric, peaked at 0, effectively zero beyond |t| = 1.5.
    assert!((gfsk_pulse(0.3) - gfsk_pulse(-0.3)).abs() < 1e-12);
    assert!(gfsk_pulse(0.0) > gfsk_pulse(0.4));
    assert!(gfsk_pulse(1.6).abs() < 1e-6);
    // And the shaping is PRESENT in the waveform: at a boundary between
    // different tones the instantaneous frequency is mid-way, which
    // Goertzel sees as reduced power at both tones compared to the
    // symbol centers (checked implicitly by per_symbol_dominant_tone
    // measuring only the settled middle).
    assert!(gfsk_pulse(0.5) > 0.4 && gfsk_pulse(0.5) < 0.6);
}

// Derivation proof for `ft8::CHECK_ROWS`: re-derives the sparse
// parity-check rows (weight <= 7) of the dual code spanned by
// [G | I83] via randomized Gaussian elimination and asserts the
// embedded table is exactly that set. This is how CHECK_ROWS was
// produced — the test keeps the derivation reproducible.
#[test]
fn check_rows_match_derivation_from_generator() {
    use std::collections::BTreeSet;
    const N: usize = 174;
    const M: usize = 83;
    const K: usize = 91;
    type Row = [u64; 3];
    fn get(r: &Row, j: usize) -> bool {
        (r[j / 64] >> (j % 64)) & 1 == 1
    }
    fn setb(r: &mut Row, j: usize) {
        r[j / 64] |= 1 << (j % 64);
    }
    fn xor(a: &mut Row, b: &Row) {
        for i in 0..3 {
            a[i] ^= b[i];
        }
    }
    fn weight(r: &Row) -> u32 {
        r.iter().map(|w| w.count_ones()).sum()
    }
    let mut basis: Vec<Row> = Vec::new();
    for (i, &gen_row) in ft8::GENERATOR_BITS.iter().enumerate() {
        let mut row: Row = [0; 3];
        // Matrix column `j` is bit `90 - j` of the 91-bit row.
        for j in 0..K {
            if (gen_row >> (K - 1 - j)) & 1 == 1 {
                setb(&mut row, j);
            }
        }
        setb(&mut row, K + i);
        basis.push(row);
    }
    let mut found: BTreeSet<Row> = BTreeSet::new();
    let mut rng = XorShift(0x5EED_5EED_1234_ABCD);
    let mut iters = 0u32;
    while found.len() < M && iters < 20_000 {
        iters += 1;
        let mut perm: Vec<usize> = (0..N).collect();
        for i in (1..N).rev() {
            let j = (rng.next() % (i as u64 + 1)) as usize;
            perm.swap(i, j);
        }
        let mut rows = basis.clone();
        let mut pivot = 0usize;
        for &col in &perm {
            if pivot >= M {
                break;
            }
            let Some(p) = (pivot..M).find(|&r| get(&rows[r], col)) else {
                continue;
            };
            rows.swap(pivot, p);
            let pr = rows[pivot];
            for (r, row) in rows.iter_mut().enumerate() {
                if r != pivot && get(row, col) {
                    xor(row, &pr);
                }
            }
            pivot += 1;
        }
        for r in &rows {
            if weight(r) <= 7 {
                found.insert(*r);
            }
        }
    }
    println!("iterations: {iters}, found: {}", found.len());
    assert_eq!(found.len(), M);
    let mut cover = [0u32; N];
    for r in &found {
        for (j, c) in cover.iter_mut().enumerate() {
            if get(r, j) {
                *c += 1;
            }
        }
    }
    assert!(cover.iter().all(|&c| c == 3), "cover: {cover:?}");
    // The embedded table is exactly the derived set (as sorted
    // index-lists; 255 pads weight-6 rows).
    let derived: std::collections::BTreeSet<Vec<u8>> = found
        .iter()
        .map(|r| {
            let mut idx: Vec<u8> = (0..N).filter(|&j| get(r, j)).map(|j| j as u8).collect();
            idx.resize(7, 255);
            idx
        })
        .collect();
    let embedded: std::collections::BTreeSet<Vec<u8>> =
        ft8::CHECK_ROWS.iter().map(|r| r.to_vec()).collect();
    assert_eq!(derived, embedded);
}

// --- Provenance: the two channel-coding matrices against their
// --- public-domain source ----------------------------------------
//
// `third_party/ft4_ft8_public/` vendors `generator.dat` and `parity.dat`
// from the FT4/FT8 protocol resource package -- reference [14] of the
// QEX paper -- which section 9 of that paper places in the PUBLIC DOMAIN
// and explicitly carves out of WSJT-X's GPLv3. See the README there.
//
// These two tests are the reason the embedded constants can claim a
// provenance at all: they check it mechanically on every CI run instead
// of asserting it in a comment. A comment claiming provenance is worth
// very little -- an earlier version of `src/ft8.rs` carried one that was
// false -- so the check is executable.

/// Locates a vendored public-domain data file.
fn public_domain_file(name: &str) -> String {
    let path = format!(
        "{}/third_party/ft4_ft8_public/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// `ft8::GENERATOR_BITS` must equal `generator.dat`, whose 83 rows are
/// 91 ASCII binary digits each. This is the whole provenance claim for
/// the generator matrix, reduced to an assertion.
#[test]
fn generator_bits_match_public_domain_file() {
    let text = public_domain_file("generator.dat");
    // The file opens with a prose header; rows are the lines that are
    // exactly 91 binary digits.
    let rows: Vec<u128> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.len() == 91 && l.bytes().all(|b| b == b'0' || b == b'1'))
        .map(|l| u128::from_str_radix(l, 2).expect("91 binary digits"))
        .collect();
    assert_eq!(rows.len(), 83, "generator.dat should hold 83 rows");
    assert_eq!(
        rows,
        ft8::GENERATOR_BITS.to_vec(),
        "GENERATOR_BITS must equal the public-domain generator.dat"
    );
}

/// `ft8::CHECK_ROWS` must equal the transpose of `parity.dat`, which
/// lists for each of the 174 columns the three one-based rows holding a
/// one.
///
/// Compared as a multiset of rows, not row-for-row: the row *order* of a
/// parity-check matrix is arbitrary (permuting rows permutes the order
/// the checks are evaluated in and nothing else), and `CHECK_ROWS` is in
/// the order its derivation search happened to find them. What must hold
/// is that the two describe the same 83 checks.
#[test]
fn check_rows_match_public_domain_parity_file() {
    let text = public_domain_file("parity.dat");
    let columns: Vec<Vec<usize>> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|f| f.len() == 3 && f.iter().all(|t| t.bytes().all(|b| b.is_ascii_digit())))
        .map(|f| f.iter().map(|t| t.parse().expect("index")).collect())
        .collect();
    assert_eq!(columns.len(), 174, "parity.dat should hold 174 columns");

    // Transpose: row (r - 1) covers column j when j's triple names r.
    let mut rows: Vec<Vec<u8>> = vec![Vec::new(); 83];
    for (j, triple) in columns.iter().enumerate() {
        for &r in triple {
            assert!((1..=83).contains(&r), "row index {r} out of range");
            rows[r - 1].push(u8::try_from(j).expect("column fits u8"));
        }
    }
    let mut from_file: Vec<Vec<u8>> = rows
        .into_iter()
        .map(|mut idx| {
            assert!(
                idx.len() == 6 || idx.len() == 7,
                "row weight must be 6 or 7, got {}",
                idx.len()
            );
            idx.sort_unstable();
            idx.resize(7, 255); // pad weight-6 rows as CHECK_ROWS does
            idx
        })
        .collect();
    let mut embedded: Vec<Vec<u8>> = ft8::CHECK_ROWS.iter().map(|r| r.to_vec()).collect();
    from_file.sort();
    embedded.sort();
    assert_eq!(
        embedded, from_file,
        "CHECK_ROWS must be the transpose of the public-domain parity.dat"
    );
}

/// The source-encoding alphabets must match the `data` statements of the
/// public-domain reference programs: `free_text_to_f71.f90` (its `c`) and
/// `std_call_to_c28.f90` (its `a1`/`a2`/`a3`/`a4`).
///
/// These are short strings and an eyeball would catch a wrong one, but a
/// *reordered* one is invisible to review and would corrupt every
/// callsign silently — positionally packed alphabets encode meaning in
/// their order, not just their membership.
#[test]
fn alphabets_match_public_domain_files() {
    /// Extracts the Fortran `data <name>/'<value>'/` initializers.
    fn data_statements(src: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for line in src.lines() {
            let rest = match line.trim().strip_prefix("data ") {
                Some(r) => r,
                None => continue,
            };
            let (name, tail) = match rest.split_once('/') {
                Some(p) => p,
                None => continue,
            };
            // The value is single-quoted; take what is between the first
            // pair of quotes.
            let mut parts = tail.splitn(3, '\'');
            let _before = parts.next();
            if let Some(value) = parts.next() {
                out.push((name.trim().to_string(), value.to_string()));
            }
        }
        out
    }

    let free = data_statements(&public_domain_file("free_text_to_f71.f90"));
    let call = data_statements(&public_domain_file("std_call_to_c28.f90"));
    let lookup = |set: &[(String, String)], name: &str| -> String {
        set.iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("no `data {name}/` statement found"))
    };

    assert_eq!(
        lookup(&free, "c").as_bytes(),
        &ft8::FREE_TEXT_ALPHABET[..],
        "FREE_TEXT_ALPHABET must match free_text_to_f71.f90"
    );
    for (i, name) in ["a1", "a2", "a3", "a4"].iter().enumerate() {
        assert_eq!(
            lookup(&call, name).as_bytes(),
            ft8::C28_SETS[i],
            "C28_SETS[{i}] must match std_call_to_c28.f90's `{name}`"
        );
    }
}

#[test]
fn f32_path_matches_i16_phase() {
    let cfg = Ft8Config::new(1_000, SampleRate::new(12_000).unwrap()).unwrap();
    let msg = Ft8Message::free_text("F32").unwrap();
    let mut a = Ft8Modulator::for_message(cfg, &msg);
    let mut b = Ft8Modulator::for_message(cfg, &msg);
    for _ in 0..10_000 {
        let x = a.next_i16().unwrap();
        let y = b.next_f32().unwrap();
        assert!((f64::from(x) / 32_767.0 - f64::from(y)).abs() < 0.01);
    }
}
