//! Integration tests for the FX.25 correlation-tag framing layer:
//! modem round trips, error correction, tag-hunter tolerance, and the
//! additive backward-compatibility guarantee.
#![cfg(all(feature = "fx25", feature = "ax25", feature = "mod", feature = "demod"))]

use warble::ax25::{FrameReceiver, UiFrame};
use warble::demodulator::{AfskDemodulator, DemodulatorConfig};
use warble::fx25::{
    ByteBits, CorrelationTag, Fx25Receiver, TAG_BYTES, TAG_TOLERANCE, WRAP_MAX, byte_bits,
    stuff_frame, wrap, wrap_with,
};
use warble::modulator::{Modulator, ModulatorConfig};
use warble::nrzi::{self, NrziDecoder};
use warble::rs::{RsCodec, RsParity};
use warble::{Bit, SampleRate};

const RATE: u32 = 48_000;
const RX_CAP: usize = 330;

fn sample_rate() -> SampleRate {
    SampleRate::new(RATE).unwrap()
}

fn modulator() -> Modulator {
    Modulator::new(ModulatorConfig::bell_202(sample_rate()).unwrap())
}

fn demodulator() -> AfskDemodulator {
    AfskDemodulator::new(DemodulatorConfig::bell_202(sample_rate()).unwrap()).unwrap()
}

fn addr(callsign: &[u8], ssid: u8) -> warble::ax25::Address {
    warble::ax25::Address::new(callsign, ssid).unwrap()
}

/// Builds an AX.25 UI frame body with the given info payload.
fn frame_body(info: &[u8]) -> Vec<u8> {
    let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), info);
    let mut buf = [0u8; 330];
    let len = frame.build(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// Wraps a frame body into FX.25 transmission bytes (tag ‖ data ‖ parity).
fn wrapped_bytes(body: &[u8]) -> (Vec<u8>, CorrelationTag) {
    let mut stuffed = [0u8; 512];
    let stuffed_len = stuff_frame(body, &mut stuffed).unwrap();
    let mut out = [0u8; WRAP_MAX];
    let wrapped = wrap(&stuffed[..stuffed_len], &mut out).unwrap();
    (out[..wrapped.len()].to_vec(), wrapped.tag())
}

/// Prepends idle preamble flags and appends tail flags, so the modem's
/// clock recovery has time to lock before the tag arrives.
fn with_idle(tx: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x7Eu8; 32];
    bytes.extend_from_slice(tx);
    bytes.extend_from_slice(&[0x7E, 0x7E]);
    bytes
}

/// Modulates a byte stream into 1200-baud Bell 202 `i16` audio.
fn modulate(bytes: &[u8]) -> Vec<i16> {
    modulator()
        .i16_samples(nrzi::encode_iter(byte_bits(bytes)))
        .collect()
}

/// Runs audio through the FX.25-aware receive path (demod → NRZI →
/// tag hunter / deframer), collecting every recovered frame.
fn receive_fx25(audio: &[i16]) -> Vec<Vec<u8>> {
    let mut demod = demodulator();
    let mut nrzi = NrziDecoder::default();
    let mut rx = Fx25Receiver::<RX_CAP>::new();
    let mut frames = Vec::new();
    for &s in audio {
        let Some(line) = demod.push_sample_i16(s) else {
            continue;
        };
        if let Some(Ok(frame)) = rx.push(nrzi.decode(line)) {
            frames.push(frame.to_vec());
        }
    }
    frames
}

/// Runs audio through the plain (non-FX.25) receive path.
fn receive_plain(audio: &[i16]) -> Vec<Vec<u8>> {
    let mut rx = FrameReceiver::<RX_CAP>::new(demodulator());
    let mut frames = Vec::new();
    for &s in audio {
        if let Some(Ok(frame)) = rx.push_sample_i16(s) {
            frames.push(frame.to_vec());
        }
    }
    frames
}

#[test]
fn tag_constants_are_pairwise_distance_32() {
    for a in CorrelationTag::ALL {
        for b in CorrelationTag::ALL {
            if a != b {
                assert_eq!((a.tag_value() ^ b.tag_value()).count_ones(), 32);
            }
        }
    }
}

#[test]
fn smallest_tag_selection_covers_all_sizes() {
    assert_eq!(
        CorrelationTag::smallest_for(1),
        Some(CorrelationTag::Rs48_32)
    );
    assert_eq!(
        CorrelationTag::smallest_for(32),
        Some(CorrelationTag::Rs48_32)
    );
    assert_eq!(
        CorrelationTag::smallest_for(33),
        Some(CorrelationTag::Rs80_64)
    );
    assert_eq!(
        CorrelationTag::smallest_for(65),
        Some(CorrelationTag::Rs144_128)
    );
    assert_eq!(
        CorrelationTag::smallest_for(129),
        Some(CorrelationTag::Rs255_191)
    );
    assert_eq!(
        CorrelationTag::smallest_for(192),
        Some(CorrelationTag::Rs255_223)
    );
    assert_eq!(
        CorrelationTag::smallest_for(224),
        Some(CorrelationTag::Rs255_239)
    );
    assert_eq!(CorrelationTag::smallest_for(240), None);
}

#[test]
fn wrap_layout_is_tag_data_parity() {
    let body = frame_body(b">layout check");
    let (tx, tag) = wrapped_bytes(&body);
    assert_eq!(tx.len(), TAG_BYTES + tag.block_len());
    // Tag bytes LSB-first.
    for (k, &byte) in tx.iter().enumerate().take(TAG_BYTES) {
        assert_eq!(byte, (tag.tag_value() >> (8 * k)) as u8);
    }
    // The codeblock reassembled as a full RS word decodes cleanly.
    let mut block = [0u8; 255];
    block[..tag.data_len()].copy_from_slice(&tx[TAG_BYTES..TAG_BYTES + tag.data_len()]);
    block[tag.rs_data_len()..].copy_from_slice(&tx[TAG_BYTES + tag.data_len()..]);
    let codec = RsCodec::new(tag.parity());
    assert_eq!(codec.decode(&mut block).unwrap(), 0);
}

#[test]
fn fx25_modem_round_trip_multiple_frames() {
    // TX → 1200-baud Bell 202 audio → FX.25 RX, several frames of
    // assorted sizes (exercising different tags).
    let bodies: Vec<Vec<u8>> = vec![
        frame_body(b">tiny"),
        frame_body(b"!4903.50N/07201.75W-round trip one"),
        frame_body(
            &[b'>']
                .iter()
                .copied()
                .chain((0..80).map(|i| b'a' + i % 26))
                .collect::<Vec<u8>>(),
        ),
        frame_body(b":N1CALL   :hello fx25{42"),
        frame_body(
            &[b'>']
                .iter()
                .copied()
                .chain((0..150).map(|i| b'A' + i % 26))
                .collect::<Vec<u8>>(),
        ),
    ];
    let mut bytes = vec![0x7Eu8; 32];
    for body in &bodies {
        let (tx, _) = wrapped_bytes(body);
        bytes.extend_from_slice(&tx);
        bytes.extend_from_slice(&[0x7E; 8]);
    }
    let audio = modulate(&bytes);
    let got = receive_fx25(&audio);
    assert_eq!(got, bodies);
}

#[test]
fn fx25_corrects_up_to_t_corrupted_symbols() {
    let body = frame_body(b"!4903.50N/07201.75W-correction");
    let (mut tx, tag) = wrapped_bytes(&body);
    let t = tag.parity().correctable();
    // Corrupt t distinct block symbols (data and parity alike), spread
    // deterministically; the tag itself stays intact.
    let block_len = tag.block_len();
    for e in 0..t {
        let pos = TAG_BYTES + (e * block_len) / t;
        tx[pos] ^= 0xA5;
    }
    let audio = modulate(&with_idle(&tx));
    let got = receive_fx25(&audio);
    assert_eq!(got, vec![body]);
}

#[test]
fn fx25_flags_uncorrectable_block() {
    let body = frame_body(b">too much damage");
    let (mut tx, tag) = wrapped_bytes(&body);
    let t = tag.parity().correctable();
    // Saturate well past t: every second data byte corrupted.
    for e in 0..(2 * t + 4) {
        let pos = TAG_BYTES + 2 * e;
        tx[pos] ^= 0x5A;
    }
    // Byte-level (no audio): drive the receiver with the raw bits so
    // the deframer's independent recovery paths cannot mask the RS
    // verdict via the audio channel.
    let mut rx = Fx25Receiver::<RX_CAP>::new();
    let mut correct = Vec::new();
    for bit in byte_bits(&tx) {
        if let Some(Ok(frame)) = rx.push(bit) {
            correct.push(frame.to_vec());
        }
    }
    assert!(!correct.contains(&body));
}

/// A tag lock must not blank the plain path.
///
/// The scenario is a **false lock**: the tag hunter fires on noise or on
/// ordinary traffic, announcing a block that is not really there. The
/// receiver then waits for the whole announced codeblock — up to 255
/// bytes, ~1.7 s at 1200 baud — during which any plain AX.25 frame that
/// arrives must still decode. An earlier design consumed those bits
/// into the block buffer and delivered nothing at all.
///
/// Constructed by emitting the largest tag and *not* following it with a
/// codeblock, so the receiver is mid-`Collect` for the entire frame that
/// follows.
#[test]
fn tag_lock_does_not_blank_the_plain_path() {
    // The largest block (255 bytes) keeps the receiver collecting for
    // the longest possible time.
    let tag = CorrelationTag::ALL
        .into_iter()
        .max_by_key(|t| t.block_len())
        .unwrap();
    assert_eq!(tag.block_len(), 255, "expected a 255-byte codeblock tag");

    let body = frame_body(b"plain frame during a bogus block");
    let mut tx = vec![0x7Eu8; 32];
    tx.extend_from_slice(&tag.tag_value().to_le_bytes());
    // No codeblock follows; instead, ordinary HDLC traffic.
    let mut stuffed = [0u8; 512];
    let stuffed_len = stuff_frame(&body, &mut stuffed).unwrap();
    tx.extend_from_slice(&stuffed[..stuffed_len]);
    tx.extend_from_slice(&[0x7E, 0x7E]);

    let frames = receive_fx25(&modulate(&tx));
    assert!(
        frames.iter().any(|f| f == &body),
        "a plain frame arriving while a (false) tag lock is collecting must still \
         decode; got {} frame(s)",
        frames.len()
    );
}

/// A frame both paths recover must be emitted exactly once.
///
/// Because every bit now reaches the plain deframer *and* the tag
/// hunter, an FX.25-wrapped frame is recoverable twice: the plain path
/// closes it at its trailing flag, the FX.25 path only after the block's
/// last parity byte. Without deduplication every FX.25 frame would be
/// delivered twice — which for a gateway means duplicated traffic.
#[test]
fn a_frame_recoverable_by_both_paths_is_emitted_once() {
    let bodies: [&[u8]; 3] = [b"first", b"second", b"third"];
    let mut tx = Vec::new();
    for body in bodies {
        let (wrapped, _) = wrapped_bytes(&frame_body(body));
        tx.extend_from_slice(&with_idle(&wrapped));
    }
    let frames = receive_fx25(&modulate(&tx));

    for body in bodies {
        let want = frame_body(body);
        let n = frames.iter().filter(|f| **f == want).count();
        assert_eq!(
            n,
            1,
            "frame {:?} delivered {n} times, expected exactly 1",
            core::str::from_utf8(body).unwrap()
        );
    }
    assert_eq!(frames.len(), bodies.len(), "unexpected extra deliveries");
}

/// Block extraction must be independent of plain-path history: the same
/// codeblock must yield the same frame whatever preceded it on the air.
///
/// This is the observable half of the reason extraction runs through its
/// own deframer rather than the plain path's. It is a weaker guard than
/// it looks — see the note on `Fx25Receiver::decode_block` — because
/// HDLC resynchronises on flags, so the two are hard to tell apart
/// behaviourally. It is kept because independence is the property being
/// claimed, and a prefix-sensitive extractor would be a real defect even
/// if the current one is hard to catch failing.
#[test]
fn block_extraction_is_independent_of_what_preceded_it() {
    let carried = frame_body(b"the block's own frame");
    let (wrapped, _) = wrapped_bytes(&carried);

    let interrupted = frame_body(b"this frame never finishes on the air");
    let mut stuffed = [0u8; 512];
    let stuffed_len = stuff_frame(&interrupted, &mut stuffed).unwrap();

    let prefixes: [Vec<u8>; 3] = [
        vec![0x7E; 32],
        // A frame cut off part-way, so a deframer is left mid-frame.
        {
            let mut p = vec![0x7E; 32];
            p.extend_from_slice(&stuffed[..stuffed_len * 2 / 3]);
            p
        },
        // Arbitrary non-flag junk.
        {
            let mut p = vec![0x7E; 32];
            p.extend((0u8..64).map(|i| i.wrapping_mul(37) | 1));
            p
        },
    ];

    let mut results = Vec::new();
    for prefix in &prefixes {
        let mut tx = prefix.clone();
        tx.extend_from_slice(&wrapped);
        tx.extend_from_slice(&[0x7E, 0x7E]);
        let frames = receive_fx25(&modulate(&tx));
        assert!(
            frames.contains(&carried),
            "the block's frame must decode whatever preceded it"
        );
        results.push(frames.iter().filter(|f| **f == carried).count());
    }
    assert!(
        results.iter().all(|&n| n == results[0]),
        "extraction depended on the preceding bit history: {results:?}"
    );
}

#[test]
fn backward_compat_plain_receiver_decodes_fx25_audio() {
    // The additive-wrapper guarantee: the embedded stuffed HDLC frame
    // (flags intact) comes through the plain non-FX.25 receiver.
    let body = frame_body(b"!4903.50N/07201.75W-legacy path");
    let (tx, _) = wrapped_bytes(&body);
    let audio = modulate(&with_idle(&tx));
    let got = receive_plain(&audio);
    assert_eq!(got, vec![body]);
}

#[test]
fn tag_hunter_tolerates_tag_bit_errors() {
    let body = frame_body(b">tag damage");
    let (tx, _) = wrapped_bytes(&body);
    // Flip TAG_TOLERANCE bits spread across the 8 tag bytes: the
    // hunter must still lock and the frame decode cleanly.
    let mut damaged = tx.clone();
    for e in 0..TAG_TOLERANCE as usize {
        damaged[e % TAG_BYTES] ^= 1 << ((5 * e) % 8);
    }
    let audio = modulate(&with_idle(&damaged));
    let got = receive_fx25(&audio);
    assert_eq!(got, vec![body]);
}

#[test]
fn tag_hunter_rejects_beyond_tolerance() {
    // 15 flipped tag bits: within half the pairwise distance of a
    // *different* tag but beyond the acceptance threshold — the hunter
    // must not lock (the embedded frame still decodes via the plain
    // path inside the receiver, so check no *wrong-tag* decode occurs
    // by requiring the plain-path result to equal the embedded frame).
    let body = frame_body(b">no lock");
    let (tx, _) = wrapped_bytes(&body);
    let mut damaged = tx.clone();
    for i in 0..15 {
        damaged[i / 8] ^= 1 << (i % 8);
    }
    let audio = modulate(&with_idle(&damaged));
    // With no tag lock, the FX.25-aware receiver falls back to its
    // plain HDLC path and still recovers the embedded frame.
    let got = receive_fx25(&audio);
    assert_eq!(got, vec![body]);
}

#[test]
fn plain_audio_decodes_through_fx25_receiver() {
    // Non-FX.25 traffic through the FX.25-aware path.
    let body = frame_body(b"!4903.50N/07201.75W-plain tx");
    let audio: Vec<i16> = warble::ax25::tx_i16(&body, modulator()).collect();
    let got = receive_fx25(&audio);
    assert_eq!(got, vec![body]);
}

#[test]
fn byte_bits_is_lsb_first() {
    let bits: Vec<Bit> = byte_bits(&[0x01, 0x80]).collect();
    assert_eq!(bits.len(), 16);
    assert_eq!(bits[0], Bit::One);
    assert!(bits[1..15].iter().all(|&b| b == Bit::Zero));
    assert_eq!(bits[15], Bit::One);
}

#[test]
fn byte_bits_type_is_nameable() {
    let owned = [0xF0u8];
    let it: ByteBits<'_> = byte_bits(&owned);
    assert_eq!(it.count(), 8);
}

#[test]
fn wrap_rejects_oversize_and_short_buffers() {
    let big = [0u8; 240];
    let mut out = [0u8; WRAP_MAX];
    assert!(wrap(&big, &mut out).is_err());
    let small = [0u8; 10];
    let mut tiny = [0u8; 8];
    assert!(wrap(&small, &mut tiny).is_err());
}

#[test]
fn every_tag_round_trips_at_byte_level() {
    // Force each published tag by sizing the stuffed input exactly to
    // its capacity, wrap, and decode through the bit-level receiver.
    for tag in CorrelationTag::ALL {
        let body = frame_body(b">per-tag");
        let mut stuffed = [0u8; 512];
        let stuffed_len = stuff_frame(&body, &mut stuffed).unwrap();
        assert!(
            stuffed_len <= tag.data_len(),
            "test frame outgrew tag {tag:?}"
        );
        let mut out = [0u8; WRAP_MAX];
        let wrapped = wrap_with(tag, &stuffed[..stuffed_len], &mut out).unwrap();
        assert_eq!(wrapped.tag(), tag);

        let mut rx = Fx25Receiver::<RX_CAP>::new();
        let mut got = Vec::new();
        for bit in byte_bits(&out[..wrapped.len()]) {
            if let Some(Ok(frame)) = rx.push(bit) {
                got.push(frame.to_vec());
            }
        }
        assert_eq!(got, vec![body], "tag {tag:?}");
    }
}

#[test]
fn rs_parity_matches_tag_family() {
    for tag in CorrelationTag::ALL {
        let expected = match tag.rs_data_len() {
            239 => RsParity::Sixteen,
            223 => RsParity::ThirtyTwo,
            191 => RsParity::SixtyFour,
            other => panic!("unexpected rs data length {other}"),
        };
        assert_eq!(tag.parity(), expected);
        assert_eq!(tag.block_len(), tag.data_len() + tag.parity().len());
    }
}

// --- Provenance: the tag family, rebuilt from the specification's own
// --- construction ---------------------------------------------------
//
// The FX.25 specification (Jim McGuire KB3MPL, "FX.25 FEC Extension to
// AX.25 Link Protocol for Amateur Packet Radio", Stensat Group LLC,
// document version 0.01.06 DRAFT, 2006) is copyright with no
// redistribution grant, so unlike the FT8 tables it is not vendored into
// this repository.
//
// It does not need to be. Section "Correlation Tag Details" publishes the
// CONSTRUCTION of the tag family, not just its values:
//
//   * Gold codes from two m-sequences over the polynomials
//       I(x) = x^6 + x^5              (first)
//       Q(x) = x^6 + x^5 + x^3 + x^2  (second)
//   * "By fixing the initial seed of the first polynomial to 0x3F and
//     varying the second polynomial seed from 0x01 through 0x3F, one
//     obtains 2^r - 1 = 63 distinct Gold Codes."
//   * "A leading zero is transmitted at the beginning of the Correlation
//     Tag to bring the total bit count up to 64-bits."
//   * "The above Correlation Tag values are represented in 64-bit
//     notation, with the MSB at the left and the LSB at the right.
//     Transmission order of the bytes for Tag_01 would be 0x3E 0x2F 0x53
//     0x8A 0xDF 0xB7 0x4D 0xB7."
//
// So the eleven embedded constants can be regenerated from a paper
// description alone. That is a stronger provenance than comparing against
// a copy of a file: it needs no copy.

/// Sixty-three output bits of a 6-stage Fibonacci LFSR.
///
/// `taps` is the feedback mask over stages 0..=5, with stage 5 the oldest.
/// A degree-6 polynomial term `x^n` taps stage `n - 1`, so
/// `I(x) = x^6 + x^5` is stages 5 and 4 (`0b110000`) and
/// `Q(x) = x^6 + x^5 + x^3 + x^2` is stages 5, 4, 2 and 1 (`0b110110`).
fn m_sequence(seed: u8, taps: u8) -> [u8; 63] {
    let mut state = seed & 0x3F;
    let mut out = [0u8; 63];
    for slot in &mut out {
        *slot = (state >> 5) & 1;
        let feedback = (state & taps).count_ones() as u8 & 1;
        state = ((state << 1) | feedback) & 0x3F;
    }
    out
}

/// One Gold code of the FX.25 family, as the spec's 64-bit notation.
///
/// The two m-sequences are XORed; the result is transmitted first-bit-first
/// after a leading zero, and the spec's hex notation is the on-air bit
/// order read LSB-first — so output bit `k` lands at bit `k + 1`, leaving
/// bit 0 as the leading zero.
fn gold_tag(second_seed: u8) -> u64 {
    const I_TAPS: u8 = 0b11_0000;
    const Q_TAPS: u8 = 0b11_0110;
    let i = m_sequence(0x3F, I_TAPS);
    let q = m_sequence(second_seed, Q_TAPS);
    let mut tag = 0u64;
    for (k, (a, b)) in i.iter().zip(q.iter()).enumerate() {
        tag |= u64::from(a ^ b) << (k + 1);
    }
    tag
}

/// Every embedded correlation tag must fall out of the specification's
/// published Gold-code construction, at the tag index the spec assigns it.
#[test]
fn tags_regenerate_from_the_published_gold_code() {
    // Table 1 assigns Tag_01..Tag_0B to the eleven FEC modes, in the order
    // `CorrelationTag::ALL` lists them.
    for (i, tag) in CorrelationTag::ALL.into_iter().enumerate() {
        let index = u8::try_from(i + 1).expect("tag index fits u8");
        assert_eq!(
            tag.tag_value(),
            gold_tag(index),
            "{tag:?} must equal Gold code Tag_{index:02X} from the published polynomials"
        );
    }
}

/// The spec prints the on-air byte order for Tag_01 explicitly, which pins
/// the endianness of the whole table: get this backwards and every frame is
/// unreadable while every self-round-trip still passes.
#[test]
fn tag_01_on_air_byte_order_matches_the_specification() {
    let value = CorrelationTag::Rs255_239.tag_value();
    let on_air: Vec<u8> = (0..8).map(|k| (value >> (8 * k)) as u8).collect();
    assert_eq!(
        on_air,
        vec![0x3E, 0x2F, 0x53, 0x8A, 0xDF, 0xB7, 0x4D, 0xB7],
        "spec: transmission order for Tag_01 is 3E 2F 53 8A DF B7 4D B7"
    );
}

/// The leading zero the spec prepends means every tag is even.
#[test]
fn every_tag_carries_the_specified_leading_zero() {
    for tag in CorrelationTag::ALL {
        assert_eq!(
            tag.tag_value() & 1,
            0,
            "{tag:?} must begin with the spec's leading zero bit"
        );
    }
}
