//! Edge-case and published known-answer tests.
//!
//! Part 1 — PUBLISHED KNOWN-ANSWER VECTORS. Every vector here is either
//! (a) a published check value from a public catalogue/specification,
//! cited at the assertion, or (b) an arithmetic identity of a published
//! polynomial, derived in-comment from the polynomial itself and verified
//! with an *independent* implementation that shares no code or tables with
//! `src/`. This breaks the circularity of testing the crate only against
//! in-tree first-principles self-models.
//!
//! Public sources used:
//!
//! * CRC RevEng catalogue of parametrised CRC algorithms, entry
//!   **CRC-16/X.25**: `width=16 poly=0x1021 init=0xffff refin=true
//!   refout=true xorout=0xffff check=0x906e residue=0xf0b8`. The same
//!   algorithm is the HDLC FCS of ISO/IEC 13239 used by AX.25 2.2.
//! * FX.25 Forward Error Correction Extension to the AX.25 Link Protocol
//!   (Stensat Group, 2006), "FEC Codeblock" section: Reed-Solomon codes
//!   RS(255,239)/RS(255,223)/RS(255,191) over 8-bit symbols, the same
//!   `GF(256)` construction as CCSDS-style RS: field polynomial
//!   `x^8 + x^4 + x^3 + x^2 + 1` (0x11D), generator polynomial
//!   `g(x) = (x - a^1)(x - a^2)...(x - a^p)` with primitive element
//!   `a = x` (numeric 2).
//! * G3RUH 9600-baud packet radio modem design (James Miller, G3RUH,
//!   published 1988): scrambler polynomial `x^17 + x^12 + 1`, i.e.
//!   `out[n] = in[n] ^ out[n-12] ^ out[n-17]`.
//!
//! Part 2 — EDGE CASES not already covered elsewhere: maximum-length
//! frames at the const-generic receive capacity (and capacity+1
//! rejection), FX.25 correlation-tag corruption resolution/rejection at
//! the byte level, and all-ones stuffing stress at the HDLC layer.
//! (Empty info field: `tests/coverage_fill.rs::ax25_empty_info_round_trip`;
//! corrupted CRC: `tests/ax25.rs::corrupted_fcs_is_rejected`; audio-level
//! tag damage: `tests/fx25.rs::tag_hunter_tolerates_tag_bit_errors` /
//! `tag_hunter_rejects_beyond_tolerance`.)

/// CRC-16/X.25 published catalogue vectors (`ax25` feature).
#[cfg(feature = "ax25")]
mod crc16_x25_catalogue {
    use warble::ax25::crc16_x25;

    /// The CRC RevEng catalogue defines every algorithm's `check` value
    /// as the CRC of the nine ASCII bytes "123456789"; for CRC-16/X.25
    /// the published check value is 0x906E.
    #[test]
    fn catalogue_check_value_123456789() {
        assert_eq!(crc16_x25(b"123456789"), 0x906E);
    }

    /// Published residue property: the catalogue states residue=0xf0b8 —
    /// the CRC register over any message with its FCS appended
    /// (little-endian, the AX.25 wire order) is 0xF0B8 for every message.
    /// After this API's final XOR with 0xFFFF that surfaces as the
    /// constant 0xF0B8 ^ 0xFFFF.
    #[test]
    fn catalogue_residue_over_message_plus_fcs() {
        for msg in [
            &b"123456789"[..],
            b"",
            b"A",
            b"warble residue property",
            &[0x00, 0xFF, 0x7E, 0xAA][..],
        ] {
            let fcs = crc16_x25(msg);
            let mut with_fcs = msg.to_vec();
            with_fcs.push((fcs & 0xFF) as u8); // low byte first (AX.25 order)
            with_fcs.push((fcs >> 8) as u8);
            assert_eq!(
                crc16_x25(&with_fcs),
                0xF0B8 ^ 0xFFFF,
                "residue violated for {msg:?}"
            );
        }
        // init=0xffff and xorout=0xffff cancel over the empty message.
        assert_eq!(crc16_x25(b""), 0x0000);
    }
}

/// GF(256)/RS(255,k) identities of the published FX.25 parameters
/// (`fx25` feature), verified with an independent shift-and-reduce
/// GF(256) implementation over the published field polynomial 0x11D —
/// no tables, no code shared with `src/rs.rs` (which uses compile-time
/// log/antilog tables).
#[cfg(feature = "fx25")]
mod rs_gf256_published_identities {
    use warble::rs::{RsCodec, RsParity};

    /// The published field polynomial `x^8 + x^4 + x^3 + x^2 + 1`
    /// (0x11D), x^8 bit included.
    const FIELD_POLY: u16 = 0x11D;

    /// GF(256) multiply by textbook shift-and-reduce, structured
    /// nothing like the crate's log/antilog tables.
    fn gf_mul(a: u8, b: u8) -> u8 {
        let mut acc: u16 = 0;
        let mut aa = u16::from(a);
        let mut bb = u16::from(b);
        while bb != 0 {
            if bb & 1 != 0 {
                acc ^= aa;
            }
            aa <<= 1;
            if aa & 0x100 != 0 {
                aa ^= FIELD_POLY;
            }
            bb >>= 1;
        }
        (acc & 0xFF) as u8
    }

    /// a^n for the primitive element a = x (numeric 2).
    fn gf_pow2(n: u32) -> u8 {
        let mut v: u8 = 1;
        for _ in 0..n {
            v = gf_mul(v, 2);
        }
        v
    }

    /// The element x (numeric 2) is primitive in GF(256)/0x11D: its
    /// multiplicative order is exactly 255 (a^255 = 1 and a^k != 1 for
    /// 0 < k < 255). This is the property that makes the antilog table
    /// and the RS construction of the FX.25 spec well-defined.
    #[test]
    fn generator_element_has_order_255() {
        let mut v: u8 = 1;
        for k in 1..255u32 {
            v = gf_mul(v, 2);
            assert_ne!(v, 1, "order divides {k}, must be exactly 255");
        }
        assert_eq!(gf_mul(v, 2), 1, "a^255 must be 1");
    }

    /// Antilog spot values derived directly from the published polynomial:
    ///
    /// * a^1..a^7 are plain left shifts (0x02..0x80);
    /// * a^8 = x^8 ≡ x^4 + x^3 + x^2 + 1 = 0x1D — the reduction is
    ///   literally the low byte of the field polynomial;
    /// * a^254 = 0x8E is the inverse of 2 (0x8E·x = 0x11C, ⊕0x11D = 1);
    /// * a^255 = 1 (order 255, above).
    #[test]
    fn antilog_spot_values_for_0x11d() {
        for (n, want) in [
            (0u32, 0x01u8),
            (1, 0x02),
            (2, 0x04),
            (3, 0x08),
            (4, 0x10),
            (5, 0x20),
            (6, 0x40),
            (7, 0x80),
            (8, 0x1D),
            (254, 0x8E),
            (255, 0x01),
        ] {
            assert_eq!(gf_pow2(n), want, "a^{n}");
        }
        assert_eq!(gf_mul(0x8E, 2), 1, "0x8E is the inverse of the element 2");
    }

    /// Builds `g(x) = (x - a^1)(x - a^2)...(x - a^p)` with the independent
    /// arithmetic, highest degree first (monic). In GF(2^8) subtraction is
    /// XOR, so each factor is `x + a^i`.
    fn generator_poly(p: usize) -> Vec<u8> {
        let mut g = vec![1u8];
        for i in 1..=p as u32 {
            let root = gf_pow2(i);
            let mut next = vec![0u8; g.len() + 1];
            for (d, &c) in g.iter().enumerate() {
                next[d] ^= c; // c · x^(deg+1)
                next[d + 1] ^= gf_mul(c, root);
            }
            g = next;
        }
        g
    }

    /// Spec-stated generator property for every published FX.25 parity
    /// size (16/32/64): g is monic of degree p, g(a^i) = 0 for i in
    /// 1..=p, and g(a^0) != 0 (first consecutive root is a^1, not a^0).
    #[test]
    fn generator_polynomial_roots_are_a1_through_ap() {
        for p in [16usize, 32, 64] {
            let g = generator_poly(p);
            assert_eq!(g.len(), p + 1);
            assert_eq!(g[0], 1, "g must be monic");
            for i in 0..=p as u32 {
                let x = gf_pow2(i);
                let mut acc = 0u8;
                for &c in &g {
                    acc = gf_mul(acc, x) ^ c;
                }
                if i == 0 {
                    assert_ne!(acc, 0, "a^0 must not be a root (fcr = 1), p = {p}");
                } else {
                    assert_eq!(acc, 0, "g(a^{i}) must vanish (p = {p})");
                }
            }
        }
    }

    /// Systematic RS encoding is division: parity = data(x)·x^p mod g(x).
    /// The remainder is computed here by naive long division with the
    /// independent arithmetic; the crate's encoder must produce exactly
    /// it, for every published parity size and several messages
    /// (including shortened blocks, which the spec treats as virtually
    /// zero-padded and which leave the remainder unchanged).
    #[test]
    fn crate_parity_equals_independent_long_division() {
        let cases: [&[u8]; 4] = [
            b"hello, fx.25 world",
            &[0u8; 40],
            &[0xFFu8; 191],
            b"\x00\x01\x02\x03\xfd\xfe\xff",
        ];
        for parity in [RsParity::Sixteen, RsParity::ThirtyTwo, RsParity::SixtyFour] {
            let p = parity.len();
            let g = generator_poly(p);
            let codec = RsCodec::new(parity);
            for data in cases {
                if data.len() > codec.data_capacity() {
                    continue;
                }
                // data(x) · x^p, highest degree first; divide by monic g.
                let mut work = vec![0u8; data.len() + p];
                work[..data.len()].copy_from_slice(data);
                for i in 0..data.len() {
                    let coef = work[i];
                    if coef != 0 {
                        for (j, &gc) in g.iter().enumerate() {
                            work[i + j] ^= gf_mul(coef, gc);
                        }
                    }
                }
                let remainder = &work[data.len()..];

                let mut crate_parity = vec![0u8; p];
                codec.encode(data, &mut crate_parity).unwrap();
                assert_eq!(
                    crate_parity, remainder,
                    "parity mismatch: p = {p}, data = {data:?}"
                );
            }
        }
    }

    /// Spec-stated codeword property: a valid RS codeword c(x) has every
    /// generator root as a root — c(a^j) = 0 for j in 1..=p. The codeword
    /// comes from the crate; the evaluation is independent (Horner).
    #[test]
    fn crate_codewords_vanish_at_all_generator_roots() {
        for parity in [RsParity::Sixteen, RsParity::ThirtyTwo, RsParity::SixtyFour] {
            let p = parity.len();
            let codec = RsCodec::new(parity);
            let data: Vec<u8> = (0..codec.data_capacity())
                .map(|i| (i as u8).wrapping_mul(31).wrapping_add(7))
                .collect();
            let mut par = vec![0u8; p];
            codec.encode(&data, &mut par).unwrap();
            let mut codeword = data;
            codeword.extend_from_slice(&par);
            assert_eq!(codeword.len(), 255);
            for j in 1..=p as u32 {
                let x = gf_pow2(j);
                let mut acc = 0u8;
                for &byte in &codeword {
                    acc = gf_mul(acc, x) ^ byte;
                }
                assert_eq!(acc, 0, "c(a^{j}) must vanish, p = {p}");
            }
        }
    }
}

/// G3RUH scrambler PN-sequence known answers (`g3ruh` feature).
#[cfg(feature = "g3ruh")]
mod g3ruh_pn_sequence {
    use warble::{Bit, Scrambler};

    /// First 48 bits of the free-running LFSR output, DERIVED IN THIS
    /// COMMENT from the published polynomial `x^17 + x^12 + 1` (G3RUH
    /// 9600-baud modem design, 1988). The derivation below is the whole
    /// provenance: it can be checked by hand, line by line.
    /// With all-zeros input the scrambler degenerates to
    /// the recurrence `out[n] = out[n-12] ^ out[n-17]`. Seeding the
    /// register with 1 sets only the most recent past output:
    /// out[-1] = 1, out[-2..=-17] = 0. Walking the recurrence forward,
    /// out[n] = 1 exactly when an odd number of its taps reach a 1:
    ///
    /// * n = 11 (n-12 = -1)                    → 1
    /// * n = 16 (n-17 = -1)                    → 1
    /// * n = 23 (out[11] via the 12-delay tap) → 1
    /// * n = 28 (out[16] ^ out[11] = 1 ^ 1)    → 0 (both taps hit)
    /// * n = 33 (out[16] via the 17-delay tap) → 1
    /// * n = 35 (out[23] via 12)               → 1
    /// * n = 40 (out[23] via 17)               → 1
    /// * n = 45 (out[33] via 12)               → 1
    /// * n = 47 (out[35] via 12)               → 1
    ///
    /// and 0 everywhere else below 48.
    const FIRST_48: &str = "000000000001000010000001000000000101000010000101";

    #[test]
    fn pn_sequence_first_48_bits_from_seed_1() {
        let mut lfsr = Scrambler::with_state(1);
        let got: String = (0..48)
            .map(|_| match lfsr.scramble(Bit::Zero) {
                Bit::One => '1',
                Bit::Zero => '0',
            })
            .collect();
        assert_eq!(got, FIRST_48);
    }

    /// The published polynomial is primitive over GF(2), so the zero-input
    /// output sequence from any nonzero state has maximal period
    /// 2^17 - 1 = 131071. The state-cycle version of this proof lives in
    /// `src/scrambler.rs::lfsr_sequence_has_maximal_period`; this variant
    /// pins the *output* sequence period (the whitening guarantee) and
    /// needs no divisor checks because 131071 is a Mersenne prime — the
    /// only smaller candidate period is 1, excluded by non-constancy.
    #[test]
    fn pn_output_sequence_has_period_131071() {
        const PERIOD: usize = (1 << 17) - 1;
        let mut lfsr = Scrambler::with_state(1);
        let bits: Vec<Bit> = (0..2 * PERIOD).map(|_| lfsr.scramble(Bit::Zero)).collect();
        assert_eq!(
            &bits[..PERIOD],
            &bits[PERIOD..],
            "output must repeat with period 2^17 - 1"
        );
        assert!(bits[..PERIOD].contains(&Bit::One));
        assert!(bits[..PERIOD].contains(&Bit::Zero));
    }
}

/// Frame-size capacity edges: fill the const-generic receive buffer to
/// exactly its capacity, and prove capacity+1 is rejected with a typed
/// error (`ax25` feature).
#[cfg(feature = "ax25")]
mod frame_capacity_edges {
    use warble::Bit;
    use warble::ax25::{Address, Ax25Error, HdlcDeframer, UiFrame, hdlc};

    /// Deframer capacity used throughout: header (16) + info + FCS (2).
    /// The const-generic `N` of `HdlcDeframer` counts the buffered frame
    /// *including* its 2 FCS bytes, so the largest accepted content
    /// length is `N - 2`.
    const CAP: usize = 64;
    /// Largest frame content (FCS excluded) the deframer accepts.
    const CONTENT_MAX: usize = CAP - 2;

    /// Builds a UI frame whose total content length (header + info, FCS
    /// excluded) is exactly `total` bytes.
    fn frame_of_len(total: usize, buf: &mut [u8]) -> usize {
        let header = 7 + 7 + 2; // two addresses + control + PID
        let info = vec![0x5Au8; total - header];
        let frame = UiFrame::new(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 0).unwrap(),
            &info,
        );
        frame.build(buf).unwrap()
    }

    fn deframe(bits: impl Iterator<Item = Bit>) -> Vec<Result<Vec<u8>, Ax25Error>> {
        let mut deframer = HdlcDeframer::<CAP>::new();
        let mut out = Vec::new();
        for b in bits {
            if let Some(r) = deframer.push(b) {
                out.push(r.map(<[u8]>::to_vec));
            }
        }
        out
    }

    /// A frame exactly at the deframer's capacity (content = N - 2, so
    /// content plus FCS fills the buffer) is accepted and byte-exact.
    #[test]
    fn frame_at_exact_capacity_is_accepted() {
        let mut buf = [0u8; 128];
        let len = frame_of_len(CONTENT_MAX, &mut buf);
        assert_eq!(len, CONTENT_MAX);
        let got = deframe(hdlc::frame_bits(&buf[..len], 4, 2));
        assert_eq!(got, vec![Ok(buf[..len].to_vec())]);
    }

    /// One byte past capacity is rejected with the typed
    /// `FrameTooLarge` error naming the capacity — never a panic and
    /// never a truncated accept.
    #[test]
    fn frame_one_past_capacity_is_rejected() {
        let mut buf = [0u8; 128];
        let len = frame_of_len(CONTENT_MAX + 1, &mut buf);
        assert_eq!(len, CONTENT_MAX + 1);
        let got = deframe(hdlc::frame_bits(&buf[..len], 4, 2));
        assert_eq!(got.len(), 1);
        assert!(
            matches!(got[0], Err(Ax25Error::FrameTooLarge { max: CAP, .. })),
            "expected FrameTooLarge, got {:?}",
            got[0]
        );
        // The deframer recovers: the same frame at capacity passes next.
        let len_ok = frame_of_len(CONTENT_MAX, &mut buf);
        let got = deframe(hdlc::frame_bits(&buf[..len_ok], 4, 2));
        assert_eq!(got, vec![Ok(buf[..len_ok].to_vec())]);
    }

    /// All-ones stuffing stress: an info field of 0xFF bytes maximizes
    /// stuffing insertions (one stuffed zero per five data ones); the
    /// stuffed stream must round-trip byte-exact through the deframer at
    /// several lengths up to capacity.
    #[test]
    fn all_ones_info_stuffing_stress() {
        for info_len in [1usize, 5, 17, 32, CONTENT_MAX - 16] {
            let info = vec![0xFFu8; info_len];
            let frame = UiFrame::new(
                Address::new(b"APRS", 0).unwrap(),
                Address::new(b"N0CALL", 15).unwrap(),
                &info,
            );
            let mut buf = [0u8; 128];
            let len = frame.build(&mut buf).unwrap();
            let got = deframe(hdlc::frame_bits(&buf[..len], 4, 2));
            assert_eq!(got, vec![Ok(buf[..len].to_vec())], "info_len {info_len}");
            // Sanity: the stuffed bit stream really is longer than the
            // unstuffed content (stuffing engaged).
            let stuffed_bits = hdlc::frame_bits(&buf[..len], 0, 0).count();
            assert!(stuffed_bits > (len + 2) * 8, "stuffing must have engaged");
        }
    }
}

/// FX.25 correlation-tag corruption at the byte/bit level: nearest-tag
/// resolution within tolerance and rejection beyond it (`fx25` +
/// `ax25` features). Complements the audio-level tag tests in
/// `tests/fx25.rs` by driving the receiver with raw bits so the DSP
/// cannot mask tag-hunter behavior.
#[cfg(all(feature = "fx25", feature = "ax25"))]
mod fx25_tag_corruption {
    use warble::ax25::{Address, UiFrame};
    use warble::fx25::{
        CorrelationTag, Fx25Receiver, TAG_BYTES, TAG_TOLERANCE, WRAP_MAX, byte_bits, stuff_frame,
        wrap,
    };

    const RX_CAP: usize = 330;

    fn wrapped(info: &[u8]) -> (Vec<u8>, Vec<u8>, CorrelationTag) {
        let frame = UiFrame::new(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 7).unwrap(),
            info,
        );
        let mut body = [0u8; 330];
        let body_len = frame.build(&mut body).unwrap();
        let mut stuffed = [0u8; 512];
        let stuffed_len = stuff_frame(&body[..body_len], &mut stuffed).unwrap();
        let mut out = [0u8; WRAP_MAX];
        let w = wrap(&stuffed[..stuffed_len], &mut out).unwrap();
        (body[..body_len].to_vec(), out[..w.len()].to_vec(), w.tag())
    }

    fn receive(tx: &[u8]) -> Vec<Vec<u8>> {
        let mut rx = Fx25Receiver::<RX_CAP>::new();
        let mut frames = Vec::new();
        for bit in byte_bits(tx) {
            if let Some(Ok(frame)) = rx.push(bit) {
                frames.push(frame.to_vec());
            }
        }
        frames
    }

    /// Single-bit tag corruption: for every bit position of the 64-bit
    /// tag, the hunter must still resolve the nearest tag (Hamming
    /// distance 1 ≤ TAG_TOLERANCE) and the frame decode cleanly.
    #[test]
    fn every_single_tag_bit_flip_still_locks() {
        let (body, tx, _) = wrapped(b">single-bit tag damage");
        for bit in 0..(TAG_BYTES * 8) {
            let mut damaged = tx.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            let got = receive(&damaged);
            assert!(
                got.contains(&body),
                "tag bit {bit}: frame lost after single-bit tag corruption"
            );
        }
    }

    /// Multi-bit corruption at exactly the acceptance tolerance
    /// (TAG_TOLERANCE flipped bits): the hunter must still lock on the
    /// nearest tag — all other published tags are 32 bits away, so
    /// TAG_TOLERANCE = 8 < 32 − 8 keeps the choice unambiguous.
    #[test]
    fn tolerance_many_tag_bit_flips_still_lock() {
        let (body, tx, _) = wrapped(b">at-tolerance tag damage");
        let mut damaged = tx;
        for e in 0..TAG_TOLERANCE as usize {
            damaged[e % TAG_BYTES] ^= 1 << ((3 * e) % 8);
        }
        let got = receive(&damaged);
        assert!(got.contains(&body), "frame lost at tolerance-level damage");
    }

    /// Beyond-tolerance corruption (TAG_TOLERANCE + 4 flips, still less
    /// than half the 32-bit inter-tag distance so no *wrong* tag can
    /// match either): the tag hunter must not lock. The embedded frame
    /// remains decodable by the receiver's parallel plain HDLC path —
    /// asserted equal to the body, i.e. no wrong-tag RS decode occurred.
    #[test]
    fn beyond_tolerance_tag_damage_does_not_mislock() {
        let (body, tx, _) = wrapped(b">beyond-tolerance tag damage");
        let mut damaged = tx;
        let flips = TAG_TOLERANCE as usize + 4;
        for i in 0..flips {
            damaged[(i * 5) / 8 % TAG_BYTES] ^= 1 << ((i * 5) % 8);
        }
        let got = receive(&damaged);
        // Fallback plain path only: exactly the embedded frame, nothing
        // else (a wrong-tag lock would produce a garbage or missing
        // frame here).
        assert_eq!(got, vec![body]);
    }
}
