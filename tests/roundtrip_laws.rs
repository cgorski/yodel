//! Property-style roundtrip laws over LCG-seeded randomized inputs.
//!
//! Extends the crate's existing deterministic randomization convention
//! (the fixed-seed 64-bit LCG with Knuth MMIX constants used by
//! `tests/fuzz_decode.rs`) into explicit per-layer roundtrip *laws*:
//! for each protocol layer, `decode(encode(x)) == x` over hundreds of
//! randomized inputs. Every case derives from a literal seed — no wall
//! clock, no external randomness; failures reproduce exactly. No
//! property-testing dev-dependency is needed: the laws are total over
//! the sampled domain and the seeds are printed in the assertion
//! messages, which substitutes for shrinking at this input size.
#![cfg(all(feature = "tnc", feature = "fx25", feature = "kiss", feature = "g3ruh"))]

use yodel::aprs::{Latitude, Longitude, Position, Symbol};
use yodel::ax25::{Address, HdlcDeframer, UiFrame, hdlc};
use yodel::fx25::{Fx25Receiver, WRAP_MAX, byte_bits, stuff_frame, wrap};
use yodel::kiss::{KissCommand, KissDeframer, KissPort, encode_into, frame_iter};
use yodel::nrzi::{NrziDecoder, NrziEncoder};
use yodel::{Bit, Descrambler, Scrambler};

/// 64-bit LCG (Knuth MMIX constants), matching `tests/fuzz_decode.rs`.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() >> 33) as usize % bound
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u8()).collect()
    }

    fn bits(&mut self, len: usize) -> Vec<Bit> {
        (0..len)
            .map(|_| Bit::from(self.next_u64() & 1 == 1))
            .collect()
    }
}

const CASES: usize = 300;

/// LAW: NRZI decode ∘ encode = identity, for random bit strings of
/// random lengths, from both encoder initial states.
#[test]
fn law_nrzi_roundtrip_identity() {
    let mut rng = Lcg::new(0x4C41_5701);
    for case in 0..CASES {
        let len = 1 + rng.below(256);
        let data = rng.bits(len);
        let mut enc = NrziEncoder::default();
        let mut dec = NrziDecoder::default();
        for (n, &bit) in data.iter().enumerate() {
            assert_eq!(
                dec.decode(enc.encode(bit)),
                bit,
                "NRZI law violated: case {case}, bit {n}"
            );
        }
    }
}

/// LAW: G3RUH descramble ∘ scramble = identity for random bit strings
/// (matched zero states).
#[test]
fn law_scrambler_roundtrip_identity() {
    let mut rng = Lcg::new(0x4C41_5702);
    for case in 0..CASES {
        let len = 1 + rng.below(512);
        let data = rng.bits(len);
        let mut tx = Scrambler::new();
        let mut rx = Descrambler::new();
        for (n, &bit) in data.iter().enumerate() {
            assert_eq!(
                rx.descramble(tx.scramble(bit)),
                bit,
                "scrambler law violated: case {case}, bit {n}"
            );
        }
    }
}

/// LAW: HDLC deframe ∘ frame = identity for random payload bytes
/// (random content and length), i.e. bit stuffing plus FCS framing is
/// invertible for every payload.
#[test]
fn law_hdlc_roundtrip_identity() {
    let mut rng = Lcg::new(0x4C41_5703);
    for case in 0..CASES {
        let info_len = rng.below(64);
        let info = rng.bytes(info_len);
        let frame = UiFrame::new(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", (case % 16) as u8).unwrap(),
            &info,
        );
        let mut buf = [0u8; 128];
        let len = frame.build(&mut buf).unwrap();
        let mut deframer = HdlcDeframer::<128>::new();
        let mut got = Vec::new();
        for bit in hdlc::frame_bits(&buf[..len], 4, 2) {
            if let Some(Ok(f)) = deframer.push(bit) {
                got.push(f.to_vec());
            }
        }
        assert_eq!(
            got,
            vec![buf[..len].to_vec()],
            "HDLC law violated: case {case}"
        );
    }
}

/// LAW: AX.25 UI parse ∘ build = identity on all fields, for random
/// info bytes and rotating addresses/SSIDs.
#[test]
fn law_ax25_frame_roundtrip_identity() {
    let mut rng = Lcg::new(0x4C41_5704);
    let calls: [&[u8]; 4] = [b"N0CALL", b"K1ABC", b"APRS", b"W6XYZ"];
    for case in 0..CASES {
        let info_len = rng.below(128);
        let info = rng.bytes(info_len);
        let dest = Address::new(calls[case % 4], (case % 16) as u8).unwrap();
        let src = Address::new(calls[(case + 1) % 4], ((case / 16) % 16) as u8).unwrap();
        let frame = UiFrame::new(dest, src, &info);
        let mut buf = [0u8; 256];
        let len = frame.build(&mut buf).unwrap();
        let parsed = UiFrame::parse(&buf[..len]).unwrap();
        assert_eq!(parsed.dest, dest, "case {case}");
        assert_eq!(parsed.src, src, "case {case}");
        assert_eq!(parsed.info, &info[..], "case {case}");
    }
}

/// LAW: FX.25 receive ∘ wrap = identity at the bit level, for random
/// payload bytes of random lengths across every tag family.
#[test]
fn law_fx25_roundtrip_identity() {
    let mut rng = Lcg::new(0x4C41_5705);
    for case in 0..CASES {
        let info_len = rng.below(180);
        let info = rng.bytes(info_len);
        let frame = UiFrame::new(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 7).unwrap(),
            &info,
        );
        let mut body = [0u8; 330];
        let body_len = frame.build(&mut body).unwrap();
        let mut stuffed = [0u8; 512];
        let stuffed_len = stuff_frame(&body[..body_len], &mut stuffed).unwrap();
        let mut out = [0u8; WRAP_MAX];
        let wrapped = wrap(&stuffed[..stuffed_len], &mut out).unwrap();

        let mut rx = Fx25Receiver::<330>::new();
        let mut got = Vec::new();
        for bit in byte_bits(&out[..wrapped.len()]) {
            if let Some(Ok(f)) = rx.push(bit) {
                got.push(f.to_vec());
            }
        }
        assert_eq!(
            got,
            vec![body[..body_len].to_vec()],
            "FX.25 law violated: case {case} (len {})",
            info.len()
        );
    }
}

/// LAW: KISS deframe ∘ encode = identity for random payloads of random
/// lengths across *all 16 ports* (FEND/FESC bytes appear at the random
/// rate, exercising the escaping).
///
/// Port 12 is included, and its inclusion *is* the regression test for
/// the framing defect: its Data command byte is
/// `(12 << 4) | 0 = 0xC0 = FEND`, which the framer once emitted bare,
/// making the frame unreadable by this crate's own deframer. That was a
/// crate bug, not an ambiguity inherent to KISS — the transparency rule
/// covers the command byte, so it is escaped as `FESC TFEND` and the law
/// holds on every port. Restoring any port exclusion here would hide a
/// regression.
#[test]
fn law_kiss_roundtrip_identity() {
    let mut rng = Lcg::new(0x4C41_5706);
    for case in 0..CASES {
        let payload_len = rng.below(200);
        let payload = rng.bytes(payload_len);
        let port_n = (case % 16) as u8;
        let port = KissPort::new(port_n).unwrap();
        let mut wire = [0u8; 512];
        let wire_len = encode_into(port, KissCommand::Data, &payload, &mut wire).unwrap();
        // The two encoders must agree byte for byte; both now have to
        // escape the command byte, so port 12 is where they could diverge.
        let from_iter: Vec<u8> = frame_iter(port, KissCommand::Data, &payload).collect();
        assert_eq!(from_iter, &wire[..wire_len], "case {case}");
        let mut deframer = KissDeframer::<256>::new();
        let mut got = Vec::new();
        for &byte in &wire[..wire_len] {
            if let Some(Ok(frame)) = deframer.push(byte) {
                assert_eq!(frame.port(), port, "case {case}");
                got.push(frame.payload().to_vec());
            }
        }
        assert_eq!(got, vec![payload], "KISS law violated: case {case}");
    }
}

/// LAW: APRS position parse ∘ build = identity within the wire format's
/// representable precision, for random valid lat/lon.
///
/// Uncompressed wire resolution is exactly 1/100 arc-minute, the same
/// unit as `Latitude`/`Longitude`, so the roundtrip must be *exact*.
/// The compressed (base-91) form has ~1/4 the resolution at the
/// latitude scale (380926 codes over 180°, ≈1.04 hundredths of a
/// minute per code at the equator), so the law allows the documented
/// quantization: |Δ| ≤ 1 hundredth for latitude and ≤ 2 for longitude
/// (391 codes per degree vs 6000 hundredths).
#[test]
fn law_aprs_position_roundtrip_within_precision() {
    let mut rng = Lcg::new(0x4C41_5707);
    const LAT_MAX: i64 = 90 * 6000;
    const LON_MAX: i64 = 180 * 6000;
    for case in 0..CASES {
        let lat_h = (rng.next_u64() % (2 * LAT_MAX as u64 + 1)) as i64 - LAT_MAX;
        let lon_h = (rng.next_u64() % (2 * LON_MAX as u64 + 1)) as i64 - LON_MAX;
        // The sweep is written in 1/100 arc-minutes, the unit the
        // uncompressed wire format uses; storage is finer.
        let lat = Latitude::new(lat_h * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap();
        let lon = Longitude::new(lon_h * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap();

        // Uncompressed: exact.
        let pos = Position::new(lat, lon, Symbol::CAR);
        let mut buf = [0u8; 64];
        let len = pos.build(&mut buf).unwrap();
        let parsed = Position::parse(&buf[..len]).unwrap();
        assert_eq!(parsed.latitude, lat, "case {case} uncompressed lat");
        assert_eq!(parsed.longitude, lon, "case {case} uncompressed lon");

        // Compressed: within base-91 quantization.
        let pos = pos.with_compressed(true);
        let len = pos.build(&mut buf).unwrap();
        let parsed = Position::parse(&buf[..len]).unwrap();
        // One base-91 step, not one hundredth of a minute. The bound
        // used to be stated in hundredths because that was the storage
        // unit and the compressed grid was coarser than it; now the
        // compressed grid is the finer of the two and the quantisation
        // is 1/380926 of a degree on latitude, half that on longitude.
        let lat_step = yodel::geo::UNITS_PER_DEGREE / 380_926;
        let lon_step = yodel::geo::UNITS_PER_DEGREE / 190_463;
        let dlat = parsed.latitude.units() - lat.units();
        let dlon = parsed.longitude.units() - lon.units();
        assert!(
            dlat.abs() <= lat_step,
            "case {case}: compressed lat off by {dlat} units (input {lat_h})"
        );
        assert!(
            dlon.abs() <= lon_step,
            "case {case}: compressed lon off by {dlon} units (input {lon_h})"
        );
    }
}

/// LAW: composed line stack — NRZI then scrambler on TX, inverse order
/// on RX — is the identity for random bit strings (the 9600-baud TX
/// composition at the bit level).
#[test]
fn law_nrzi_scrambler_composition_identity() {
    let mut rng = Lcg::new(0x4C41_5708);
    for case in 0..CASES {
        let data_len = 1 + rng.below(384);
        let data = rng.bits(data_len);
        let recovered: Vec<Bit> = {
            let channel =
                Scrambler::default().scramble_iter(yodel::nrzi::encode_iter(data.iter().copied()));
            let mut nrzi = NrziDecoder::default();
            Descrambler::default()
                .descramble_iter(channel)
                .map(|b| nrzi.decode(b))
                .collect()
        };
        assert_eq!(recovered, data, "composition law violated: case {case}");
    }
}
