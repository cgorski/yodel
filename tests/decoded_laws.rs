//! Algebraic laws of the total APRS decode entry points.
//!
//! [`Decoded::decode`] makes three promises that its signature alone
//! cannot enforce, and that no other test checks:
//!
//! 1. **Totality.** It returns no `Result` and must therefore never
//!    panic, for *any* byte sequence. A panic in a receive path is a
//!    remote denial of service: the bytes come off the air from a
//!    stranger.
//! 2. **Byte preservation.** `decode(x).info` is `x`, always. This is
//!    the property that makes "the bytes are never lost" true of the
//!    type rather than just promised in prose.
//! 3. **Agreement with the strict parser.** The two entry points must
//!    never disagree about the same input: whenever
//!    [`AprsPacket::parse`] succeeds, `decode` must report exactly that
//!    packet. Otherwise a caller's choice of entry point silently
//!    changes the meaning of a packet.
//!
//! Law 3 is the interesting one, because `decode` *intercepts* several
//! data type identifiers before consulting `AprsPacket::parse`
//! (Ultimeter records share `!`, `*` and `#` with other formats). Those
//! interceptions must only ever catch inputs the strict parser rejects
//! — which is asserted here rather than assumed.
//!
//! # The frame-level entry point
//!
//! [`Decoded::decode_frame`] additionally takes the AX.25 destination
//! address, because Mic-E splits one report across the destination
//! callsign and the information field. It carries four laws, mirroring
//! the three above:
//!
//! 1. **Totality**, now over `(dest, info)` pairs: the destination also
//!    arrives off the air from a stranger.
//! 2. **Byte preservation**: `decode_frame(d, x).info` is `x`.
//! 3. **Destination independence.** For every information field whose
//!    first byte is not `` ` `` or `'`, `decode_frame(d, info).kind`
//!    equals `decode(info).kind` for **every** `d`. This is the law
//!    that matters: it is what makes two constructors safe side by
//!    side. If a destination could change the meaning of an ordinary
//!    position report, a caller's choice between them would silently
//!    move stations around the map.
//! 4. **Only `decode_frame` yields Mic-E, and only for `` ` `` / `'`.**
//!    `decode` never can — it has not been given the latitude digits —
//!    and it says so with [`DecodedKind::NeedsDestination`] rather than
//!    the untrue [`DecodedKind::Unsupported`].
//!
//! Law 3 rests on a measured fact rather than an argument, so
//! [`mic_e_decode_never_overlaps_the_information_field_decoder`]
//! re-measures it: over the same generated cross product, `mic_e::decode`
//! never succeeds on a non-Mic-E identifier and never succeeds on an
//! input `Decoded::decode` already typed. The two decoders partition the
//! identifier space; they do not compete for it.
//!
//! Inputs come from a fixed-seed generator plus a hand-written corpus of
//! shapes known to be adversarial, so failures reproduce exactly.
#![cfg(feature = "aprs")]

use warble::aprs::{AprsError, AprsPacket, Decoded, DecodedKind};
use warble::ax25::Address;

/// Deterministic xorshift64*, so any failure reproduces from the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }

    /// A random but *valid* AX.25 destination address.
    ///
    /// Valid, not arbitrary bytes, because that is the only thing
    /// [`Decoded::decode_frame`] can be handed: [`Address`] validates on
    /// construction, so a hostile destination reaches the decoder as a
    /// legal callsign carrying illegal Mic-E content, never as raw
    /// garbage.
    fn address(&mut self) -> Address {
        let len = 1 + (self.next() % 6) as usize;
        let mut call = [b'A'; 6];
        for slot in call.iter_mut().take(len) {
            *slot = CALL_CHARS[(self.next() as usize) % CALL_CHARS.len()];
        }
        let ssid = (self.next() % 16) as u8;
        Address::new(&call[..len], ssid).expect("a generated callsign is valid by construction")
    }
}

/// The AX.25 callsign alphabet, which is also the outer bound of the
/// Mic-E destination alphabet.
const CALL_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Destinations spanning the structure the Mic-E decoder cares about:
/// the spec's own example, all-ambiguity, both hemisphere halves
/// (`0`-`9`/`L` vs `P`-`Z`), the custom message set (`A`-`K`), short
/// callsigns that pad with spaces, and ordinary tocalls.
fn destinations() -> Vec<Address> {
    let calls: &[&[u8]] = &[
        b"S32UVT", // APRS 1.01 chapter 10's worked example
        b"T2SUVT", // custom message set in column 0
        b"S3LLLL", // four trailing ambiguity digits
        b"LLLLLL", // all six blank: rejected as over-ambiguous
        b"PPPPPP", // north, offset, west
        b"000000", // south, no offset, east
        b"ZZZZZZ", // top of the P-Z half in every column
        b"AAAAAA", // custom set in 0-2, illegal in 3-5
        b"999999", // top of the digit half in every column
        b"APRS",   // the generic tocall: pads to "APRS  "
        b"APZ123", // a software tocall
        b"N0CALL", // an ordinary station callsign in the dest slot
        b"WIDE2",  // a path alias, which real frames do carry here
        b"K",      // one character, five pad spaces
    ];
    let mut out = Vec::new();
    for call in calls {
        for ssid in [0u8, 7, 15] {
            out.push(Address::new(call, ssid).expect("fixed destination is valid"));
        }
    }
    out
}

/// Every data type identifier the specification assigns or reserves,
/// plus the ones it marks "do not use" — a receiver sees all of them.
const ALL_DTIS: &[u8] = b"!\"#$%&'()*+,-./0123456789:;<=>?@ABCTUWZ[\\]^_`abcz{|}~";

/// Byte shapes that have historically broken APRS parsers: structural
/// punctuation, digits, hex, hemisphere letters and field separators.
const INTERESTING: &[u8] = b"0123456789ABCDEFabcdefNSEW/\\*,:>=!{}|~ .-_\x00\x0d\x0a\xff";

fn assert_laws(info: &[u8]) {
    let decoded = Decoded::decode(info);

    // Law 2: the input is always recoverable, byte for byte.
    assert_eq!(
        decoded.info, info,
        "decode must preserve its input verbatim"
    );

    // Law 3: never disagree with the strict parser.
    match AprsPacket::parse(info) {
        Ok(strict) => match decoded.kind {
            DecodedKind::Packet(ref total) => assert_eq!(
                *total, strict,
                "strict and total parsers disagree on {info:?}"
            ),
            ref other => panic!(
                "AprsPacket::parse accepted {info:?} but decode reported {other:?}; \
                 a caller's choice of entry point must not change the meaning"
            ),
        },
        Err(AprsError::InvalidDataType { .. }) => {
            // The strict parser does not implement this identifier.
            // `decode` may still type it (NMEA, Ultimeter, third-party)
            // or label it, but must never claim it is an `AprsPacket`.
            assert!(
                !matches!(decoded.kind, DecodedKind::Packet(_)),
                "decode produced a Packet for an identifier the strict \
                 parser rejects as unimplemented: {info:?}"
            );
        }
        Err(_) => {
            // A recognized identifier whose body did not parse. `decode`
            // may recover it through an intercepting format, but must
            // not report it as a successfully parsed `AprsPacket`.
            assert!(
                !matches!(decoded.kind, DecodedKind::Packet(_)),
                "decode produced a Packet where the strict parser found a \
                 malformed body: {info:?}"
            );
        }
    }

    // Law 4: the convenience accessors agree with the variant.
    assert_accessors(&decoded, info);

    // `decode` has no destination, so it can never produce a Mic-E
    // report; it must label the two identifiers instead of typing them.
    #[cfg(feature = "micE")]
    {
        assert!(
            decoded.mic_e().is_none(),
            "decode() produced a Mic-E report without a destination: {info:?}"
        );
        assert_eq!(
            matches!(decoded.kind, DecodedKind::NeedsDestination { .. }),
            matches!(info.first(), Some(b'`' | b'\'')),
            "NeedsDestination must mean exactly `the Mic-E identifiers`: {info:?}"
        );
    }
}

/// The accessors are views on `kind`, so they must never disagree with
/// it. Shared by both entry points.
fn assert_accessors(decoded: &Decoded<'_>, info: &[u8]) {
    assert_eq!(
        decoded.is_typed(),
        !matches!(
            decoded.kind,
            DecodedKind::NeedsDestination { .. }
                | DecodedKind::Unsupported { .. }
                | DecodedKind::Malformed { .. }
        ),
        "is_typed disagrees with the variant for {info:?}"
    );
    assert_eq!(
        decoded.packet().is_some(),
        matches!(decoded.kind, DecodedKind::Packet(_)),
        "packet() disagrees with the variant for {info:?}"
    );
    #[cfg(feature = "micE")]
    assert_eq!(
        decoded.mic_e().is_some(),
        matches!(decoded.kind, DecodedKind::MicE(_)),
        "mic_e() disagrees with the variant for {info:?}"
    );
}

/// Renders a destination for an assertion message.
fn show(dest: Address) -> String {
    format!(
        "{}-{}",
        String::from_utf8_lossy(&dest.callsign.as_padded()),
        dest.ssid.value()
    )
}

/// The four frame-level laws, for one `(dest, info)` pair.
///
/// Law 1 (totality) is discharged by this function returning at all:
/// `decode_frame` has no `Result`, so the only way it can fail is a
/// panic, and the callers below drive it with generated destinations
/// and generated bytes.
fn assert_frame_laws(dest: Address, info: &[u8]) {
    let framed = Decoded::decode_frame(dest, info);

    // Law 2: the input is always recoverable, byte for byte.
    assert_eq!(
        framed.info, info,
        "decode_frame must preserve its input verbatim"
    );

    let plain = Decoded::decode(info);
    let mic_e_dti = matches!(info.first(), Some(b'`' | b'\''));

    // Law 3: outside Mic-E, the destination changes nothing. This is
    // the whole justification for having two constructors.
    if !mic_e_dti {
        assert_eq!(
            framed.kind,
            plain.kind,
            "destination {} changed the meaning of a non-Mic-E information \
             field {info:?}: decode_frame said {:?}, decode said {:?}",
            show(dest),
            framed.kind,
            plain.kind
        );
    }

    // Law 4: Mic-E comes only from `decode_frame`, and only for the two
    // Mic-E identifiers. (`decode`'s half is asserted in `assert_laws`.)
    #[cfg(feature = "micE")]
    {
        assert!(
            mic_e_dti || framed.mic_e().is_none(),
            "decode_frame produced a Mic-E report for a non-Mic-E \
             identifier: dest {}, info {info:?}",
            show(dest)
        );
        assert!(
            !matches!(framed.kind, DecodedKind::NeedsDestination { .. }),
            "decode_frame was given a destination and still asked for one: \
             dest {}, info {info:?}",
            show(dest)
        );
    }

    assert_accessors(&framed, info);
}

/// The empty field and every single byte, including non-ASCII.
#[test]
fn laws_hold_for_every_one_byte_field() {
    assert_laws(b"");
    for b in 0..=u8::MAX {
        assert_laws(&[b]);
    }
}

/// Every data type identifier against a spread of bodies.
#[test]
fn laws_hold_for_every_identifier_with_adversarial_bodies() {
    let bodies: &[&[u8]] = &[
        b"",
        b"0",
        b"!",
        b"!!",
        b"0000.00N/00000.00W-",
        b"ULTW0000000001FF000427C70002CCD30001026E003A050F00040000",
        b"GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
        b"N0CALL>APRS,TCPIP*:>x",
        b"\x00\x00\x00",
        b"\xff\xff\xff\xff",
        b"                                        ",
    ];
    let mut buf = Vec::new();
    for &dti in ALL_DTIS {
        for body in bodies {
            buf.clear();
            buf.push(dti);
            buf.extend_from_slice(body);
            assert_laws(&buf);
        }
    }
}

/// Random bytes drawn from the alphabet that breaks parsers.
#[test]
fn laws_hold_for_structured_random_input() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut buf = Vec::with_capacity(300);
    for _ in 0..60_000 {
        buf.clear();
        let len = (rng.next() % 80) as usize;
        // Bias the first byte towards a real identifier so the deeper
        // parsers are reached rather than bouncing off the dispatcher.
        if len > 0 {
            buf.push(ALL_DTIS[(rng.next() as usize) % ALL_DTIS.len()]);
        }
        for _ in 1..len {
            let b = if rng.next().is_multiple_of(3) {
                rng.byte()
            } else {
                INTERESTING[(rng.next() as usize) % INTERESTING.len()]
            };
            buf.push(b);
        }
        assert_laws(&buf);
    }
}

/// Uniformly random bytes, to reach shapes the biased generator misses.
#[test]
fn laws_hold_for_uniform_random_input() {
    let mut rng = Rng(0x0BAD_C0DE_0BAD_C0DE);
    let mut buf = Vec::with_capacity(300);
    for _ in 0..30_000 {
        buf.clear();
        let len = (rng.next() % 256) as usize;
        for _ in 0..len {
            buf.push(rng.byte());
        }
        assert_laws(&buf);
    }
}

/// Truncation is a classic parser killer: every prefix of a valid
/// packet must decode without panicking and still return its bytes.
#[test]
fn laws_hold_for_every_prefix_of_valid_packets() {
    let valid: &[&[u8]] = &[
        b"!4903.50N/07201.75W-Test /A=001234",
        b"=/5L!!<*e7>7P[Compressed",
        b"@092345z4903.50N/07201.75W>Timestamped",
        b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900",
        b"_10090556c220s004g005t077r000p000P000h50b09900",
        b"T#005,199,000,255,073,123,01101001",
        b";LEADER   *092345z4903.50N/07201.75W>Object",
        b")AID!4903.50N/07201.75WA",
        b">Status text",
        b":WB2OSZ   :Hello{001",
        b"<IGATE,MSG_CNT=13,LOC_CNT=54",
        b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
        b"$ULTW0000000001FF000427C70002CCD30001026E003A050F00040000",
        b"!!006B005803500000----03E9--------002105140000005D",
        b"}N0CALL>APRS,TCPIP,IGATE*:!4903.50N/07201.75W-",
        b"*7007600000000",
        b"#50B7500820082",
    ];
    for packet in valid {
        // The array says "valid", so hold it to that first. Without this
        // guard a bad entry still satisfies every assertion below --
        // `assert_laws` only requires no panic and byte preservation, both
        // of which a REJECTED packet also satisfies -- so the entry
        // silently contributes nothing. One did: a digit-transposed
        // Ultimeter II record sat here decoding as `Unsupported`, and the
        // `#` (km/h) path it was meant to cover went untested.
        //
        // `Unsupported`/`Malformed` are the two ways the total parser says
        // "I did not understand this", so a valid fixture must be neither.
        let kind = Decoded::decode(packet).kind;
        assert!(
            !matches!(
                kind,
                DecodedKind::Malformed { .. } | DecodedKind::Unsupported { .. }
            ),
            "fixture is listed as valid but decodes as {:?}: {}",
            kind,
            String::from_utf8_lossy(packet)
        );
        for end in 0..=packet.len() {
            assert_laws(&packet[..end]);
        }
        // ...and every suffix, which strips the identifier and so
        // exercises the dispatcher's fallback paths.
        for start in 0..packet.len() {
            assert_laws(&packet[start..]);
        }
    }
}

/// A byte flipped anywhere in an otherwise valid packet.
#[test]
fn laws_hold_under_single_byte_corruption() {
    let valid: &[&[u8]] = &[
        b"!4903.50N/07201.75W-Test",
        b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
        b"$ULTW0000000001FF000427C70002CCD30001026E003A050F00040000",
        b"}N0CALL>APRS,TCPIP,IGATE*:!4903.50N/07201.75W-",
        b"_10090556c220s004g005t077r000p000P000h50b09900",
    ];
    let mut buf = Vec::new();
    for packet in valid {
        for i in 0..packet.len() {
            for flip in [0x01u8, 0x20, 0x80, 0xff] {
                buf.clear();
                buf.extend_from_slice(packet);
                buf[i] ^= flip;
                assert_laws(&buf);
            }
        }
    }
}

/// The receive-only formats reach the decoder through the *whole* radio
/// stack, not just as byte slices handed to a parser.
///
/// Every other test of `nmea`, `ultimeter` and `thirdparty` feeds them
/// an information field directly. That leaves an untested seam: an
/// information field has to survive AX.25 framing, bit stuffing, NRZI,
/// modulation, demodulation and deframing before a decoder ever sees
/// it, and these formats contain byte values the earlier layers treat
/// specially — long runs of ones that trigger bit stuffing, and the
/// `0x7e` flag pattern. Because they are receive-only they cannot be
/// built through `AprsPacket`, so this goes through the raw frame
/// builder.
#[cfg(all(feature = "tnc", feature = "alloc"))]
#[test]
fn receive_only_formats_survive_the_full_radio_stack() {
    use warble::SampleRate;
    use warble::ax25::Address;
    use warble::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};

    let rate = SampleRate::new(44_100).expect("rate");
    let config = TncConfig::bell_202(rate).expect("config");
    let tx = TncTransmitter::new(config);
    let dest = Address::new(b"APRS", 0).expect("dest");
    let src = Address::new(b"N0CALL", 1).expect("src");

    let cases: &[(&[u8], &str)] = &[
        (
            b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
            "nmea",
        ),
        (
            b"$ULTW0000000001FF000427C70002CCD30001026E003A050F00040000",
            "ultimeter",
        ),
        (
            b"}N0CALL>APRS,TCPIP,IGATE*:!4903.50N/07201.75W-",
            "thirdparty",
        ),
        (b"<IGATE,MSG_CNT=13,LOC_CNT=54", "capabilities"),
    ];

    for (info, label) in cases {
        let mut frame_buf = [0u8; 330];
        let len = tx
            .build_frame_raw(dest, src, &[], info, &mut frame_buf)
            .unwrap_or_else(|e| panic!("{label}: build frame: {e}"));

        let mut rx: DefaultTncReceiver = DefaultTncReceiver::new(config).expect("receiver");
        let mut recovered = 0;
        for sample in tx.frame_samples_i16(&frame_buf[..len]) {
            let Some(frame) = rx.push_i16(sample) else {
                continue;
            };
            recovered += 1;

            // The information field must survive the round trip exactly.
            assert_eq!(
                frame.info(),
                *info,
                "{label}: information field corrupted by the radio stack"
            );

            let decoded = Decoded::decode(frame.info());
            assert_eq!(decoded.info, *info, "{label}: info not preserved");
            let typed = matches!(
                (&decoded.kind, *label),
                (DecodedKind::Nmea(_), "nmea")
                    | (DecodedKind::Ultimeter(_), "ultimeter")
                    | (DecodedKind::ThirdParty(_), "thirdparty")
                    | (
                        DecodedKind::Packet(AprsPacket::Capabilities(_)),
                        "capabilities"
                    )
            );
            assert!(
                typed,
                "{label}: decoded to the wrong kind after the radio stack: {:?}",
                decoded.kind
            );

            // The NMEA case additionally proves the coordinates survive.
            if let DecodedKind::Nmea(sentence) = decoded.kind {
                let at = sentence.position().expect("RMC carries a position");
                assert!((at.latitude.to_degrees() + 37.860_833).abs() < 1e-4);
                assert!((at.longitude.to_degrees() - 145.122_667).abs() < 1e-4);
            }
        }
        assert_eq!(recovered, 1, "{label}: expected exactly one frame");
    }
}

/// The frame-level laws over every identifier, every adversarial body
/// and every fixed destination — the full cross product.
#[test]
fn frame_laws_hold_over_the_identifier_destination_cross_product() {
    let bodies: &[&[u8]] = &[
        b"",
        b"0",
        b"!",
        b"(_fn\"Oj/",                       // the spec's Mic-E body
        b"(_fn\"Oj/\x1c]\"4T}Mic-E status", // … with prefix + altitude
        b"0000.00N/00000.00W-",
        b"ULTW0000000001FF000427C70002CCD30001026E003A050F00040000",
        b"GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
        b"N0CALL>APRS,TCPIP*:>x",
        b"\x00\x00\x00",
        b"\xff\xff\xff\xff",
        b"                                        ",
    ];
    let dests = destinations();
    let mut buf = Vec::new();
    for &dti in ALL_DTIS {
        for body in bodies {
            buf.clear();
            buf.push(dti);
            buf.extend_from_slice(body);
            for &dest in &dests {
                assert_frame_laws(dest, &buf);
            }
        }
    }
    // The empty field has no identifier at all, and is the one input
    // that reaches the classifier's `info.first()` fallback.
    for &dest in &dests {
        assert_frame_laws(dest, b"");
        for b in 0..=u8::MAX {
            assert_frame_laws(dest, &[b]);
        }
    }
}

/// Random bytes *and* a random destination: on the air both halves of a
/// frame come from a stranger, so both are generated here.
#[test]
fn frame_laws_hold_for_structured_random_input() {
    let mut rng = Rng(0x5EED_1234_ABCD_0002);
    let mut buf = Vec::with_capacity(300);
    for _ in 0..60_000 {
        buf.clear();
        let len = (rng.next() % 80) as usize;
        if len > 0 {
            // Bias towards the Mic-E identifiers as well as the rest,
            // so the one destination-sensitive arm is reached.
            buf.push(if rng.next().is_multiple_of(4) {
                if rng.next().is_multiple_of(2) {
                    b'`'
                } else {
                    b'\''
                }
            } else {
                ALL_DTIS[(rng.next() as usize) % ALL_DTIS.len()]
            });
        }
        for _ in 1..len {
            let b = if rng.next().is_multiple_of(3) {
                rng.byte()
            } else {
                INTERESTING[(rng.next() as usize) % INTERESTING.len()]
            };
            buf.push(b);
        }
        let dest = rng.address();
        assert_frame_laws(dest, &buf);
    }
}

/// Uniformly random bytes with a uniformly random destination.
#[test]
fn frame_laws_hold_for_uniform_random_input() {
    let mut rng = Rng(0x0BAD_C0DE_0BAD_C0DF);
    let mut buf = Vec::with_capacity(300);
    for _ in 0..30_000 {
        buf.clear();
        let len = (rng.next() % 256) as usize;
        for _ in 0..len {
            buf.push(rng.byte());
        }
        let dest = rng.address();
        assert_frame_laws(dest, &buf);
    }
}

/// Every prefix and suffix of a valid packet, against every fixed
/// destination: truncation is a classic parser killer, and law 3 must
/// survive it too.
#[test]
fn frame_laws_hold_for_every_prefix_of_valid_packets() {
    let valid: &[&[u8]] = &[
        b"!4903.50N/07201.75W-Test /A=001234",
        b"@092345z4903.50N/07201.75W>Timestamped",
        b"`(_fn\"Oj/\x1c]\"4T}Mic-E with altitude",
        b"'(_fn\"Oj/]\"4T}old Mic-E",
        b">Status text",
        b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
        b"}N0CALL>APRS,TCPIP,IGATE*:!4903.50N/07201.75W-",
    ];
    let dests = destinations();
    for packet in valid {
        for end in 0..=packet.len() {
            for &dest in &dests {
                assert_frame_laws(dest, &packet[..end]);
            }
        }
        for start in 0..packet.len() {
            for &dest in &dests {
                assert_frame_laws(dest, &packet[start..]);
            }
        }
    }
}

/// Re-measures the fact law 3 rests on: the two decoders do not compete
/// for any input.
///
/// Destination independence is only *safe* because `mic_e::decode` and
/// `Decoded::decode` partition the identifier space rather than overlap
/// on it. If some information field both decoded as Mic-E and typed as
/// an `AprsPacket`, then whether you passed a destination really would
/// change what a packet meant, and no amount of careful dispatch would
/// rescue that. So this asserts the partition directly:
///
/// * `mic_e::decode` never succeeds on an identifier other than
///   `` ` `` / `'` — measured **0** of 60420 successes;
/// * `mic_e::decode` never succeeds on an input `Decoded::decode`
///   already gave a typed value — measured **0** overlaps.
///
/// Both are asserted against a running count, and the count of Mic-E
/// successes is asserted non-zero so the test cannot pass vacuously.
#[cfg(feature = "micE")]
#[test]
fn mic_e_decode_never_overlaps_the_information_field_decoder() {
    use warble::aprs::mic_e;

    let dests = destinations();
    let mut rng = Rng(0x5EED_0000_C0FF_EE01);
    let (mut successes, mut wrong_dti, mut overlaps) = (0usize, 0usize, 0usize);

    let mut buf = Vec::with_capacity(64);
    for _ in 0..40_000 {
        buf.clear();
        let len = 1 + (rng.next() % 40) as usize;
        buf.push(if rng.next().is_multiple_of(2) {
            // Half the draws are Mic-E identifiers, so the success
            // count is not vacuous …
            if rng.next().is_multiple_of(2) {
                b'`'
            } else {
                b'\''
            }
        } else {
            // … and half are everything else a receiver sees.
            ALL_DTIS[(rng.next() as usize) % ALL_DTIS.len()]
        });
        for _ in 1..len {
            buf.push(if rng.next().is_multiple_of(3) {
                rng.byte()
            } else {
                INTERESTING[(rng.next() as usize) % INTERESTING.len()]
            });
        }
        for &dest in &dests {
            if mic_e::decode_address(dest, &buf).is_err() {
                continue;
            }
            successes += 1;
            if !matches!(buf.first(), Some(b'`' | b'\'')) {
                wrong_dti += 1;
            }
            if Decoded::decode(&buf).is_typed() {
                overlaps += 1;
            }
        }
    }

    assert!(
        successes > 0,
        "no Mic-E report decoded at all: the generator stopped producing \
         valid input, so this test proves nothing"
    );
    assert_eq!(
        wrong_dti, 0,
        "mic_e::decode succeeded on a non-Mic-E identifier {wrong_dti} \
         times out of {successes}: the identifier space is no longer \
         partitioned, so destination independence is not safe"
    );
    assert_eq!(
        overlaps, 0,
        "{overlaps} of {successes} Mic-E successes were inputs \
         Decoded::decode already typed: passing a destination would \
         change what those packets mean"
    );
}

/// Third-party packets nest by construction; decoding the inner payload
/// is the caller's explicit choice, so a deeply nested packet must not
/// cost anything until the caller asks. Descending by hand must also
/// terminate.
#[test]
fn nesting_is_bounded_by_the_caller() {
    // Twenty levels of encapsulation.
    let mut info: Vec<u8> = b">innermost".to_vec();
    for _ in 0..20 {
        let mut wrapped = b"}N0CALL>APRS,TCPIP*:".to_vec();
        wrapped.extend_from_slice(&info);
        info = wrapped;
    }

    // One decode reaches exactly one level: no recursion happened.
    let decoded = Decoded::decode(&info);
    let DecodedKind::ThirdParty(outer) = decoded.kind else {
        panic!("expected third-party, got {:?}", decoded.kind);
    };
    assert!(outer.payload.starts_with(b"}"), "one level only");

    // Descending explicitly terminates at the innermost payload.
    let mut payload = outer.payload;
    let mut depth = 0;
    while let DecodedKind::ThirdParty(tp) = Decoded::decode(payload).kind {
        payload = tp.payload;
        depth += 1;
        assert!(depth < 100, "descent failed to terminate");
    }
    assert_eq!(depth, 19, "19 further levels below the outermost");
    assert_eq!(payload, b">innermost");
}
