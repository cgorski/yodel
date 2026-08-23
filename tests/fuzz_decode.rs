//! Fuzz-style robustness suite: every public parser/decoder is fed
//! thousands of deterministic pseudo-random, truncated, and corrupted
//! inputs. The only assertion about *content* is that each call returns
//! `Ok` or a typed error — no call may panic, index out of bounds,
//! overflow-abort, or loop forever.
//!
//! Determinism: every byte comes from a fixed-seed 64-bit LCG
//! (Knuth MMIX multiplier 6364136223846793005, increment
//! 1442695040888963407); failures reproduce exactly from the literal
//! seeds below. No wall clock, no external corpus.

#![cfg(feature = "tnc")]

use warble::SampleRate;
use warble::aprs::{
    Addressee, AprsError, AprsPacket, Capabilities, CompressedCs, CompressionType, DataExtension,
    Item, Latitude, Longitude, Message, MessageContent, Object, Position, PositionCs,
    PositionTimestamped, PositionWeather, PositionlessWeather, Status, Symbol, Telemetry,
    Timestamp, WeatherReport,
};
use warble::ax25::{Address, UiFrame};
use warble::geo::Ambiguity;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};
use warble::units::{Humidity, Pressure, Rainfall, Speed, Temperature};

/// 64-bit LCG (MMIX constants). Deterministic, allocation-free.
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

    /// One pseudo-random byte (upper bits: LCG low bits are weak).
    fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    /// Uniform-ish in `0..bound` (bound > 0).
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() >> 33) as usize % bound
    }

    /// Fills a fresh buffer of pseudo-random length `0..max_len`.
    fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.below(max_len + 1);
        (0..len).map(|_| self.next_u8()).collect()
    }
}

/// How many data-type identifiers `AprsPacket::parse` dispatches on:
/// `!`, `=`, `/`, `@`, `_`, `T`, `;`, `)`, `>`, `:`, `<`.
///
/// This number is not documentation. The test below derives the
/// dispatch set *from the parser*, by sweeping all 256 possible first
/// bytes, and fails if the derived set is a different size or holds a
/// byte [`DTIS`] is missing. That is the guard: the sweep once claimed
/// to cover "every DTI branch" while silently missing `<`, because
/// nothing compared the claim to the code.
const DISPATCH_DTI_COUNT: usize = 11;

/// How many Mic-E data-type identifiers ride along in [`DTIS`].
///
/// `AprsPacket::parse` rejects both (Mic-E lives on `Decoded` and
/// `mic_e::decode`), but the rejecting branch must still be total, so
/// they are fuzzed alongside the dispatched identifiers.
const MIC_E_DTI_COUNT: usize = 2;

/// The Mic-E identifiers themselves: current-fix and old-fix.
const MIC_E_DTIS: [u8; MIC_E_DTI_COUNT] = [b'`', b'\''];

/// Every data-type identifier `AprsPacket::parse` dispatches on, plus
/// the two Mic-E DTIs.
const DTIS: [u8; DISPATCH_DTI_COUNT + MIC_E_DTI_COUNT] = [
    b'!', b'=', b'/', b'@', b':', b'>', b'_', b'T', b';', b')', b'<', b'`', b'\'',
];

/// The number of `AprsPacket` variants this file knows about; see
/// [`variant_index`].
const VARIANT_COUNT: usize = 11;

/// One index per [`AprsPacket`] variant, `None` for a variant this file
/// has not been taught about.
///
/// `AprsPacket` is `#[non_exhaustive]`, so from a test crate the
/// wildcard arm is mandatory and a twelfth variant compiles silently.
/// Mapping the wildcard to `None` — and asserting that no corpus packet
/// ever lands there — is what makes "the corpus reaches every variant"
/// a property the suite can fail on rather than a claim in prose.
fn variant_index(packet: &AprsPacket<'_>) -> Option<usize> {
    match *packet {
        AprsPacket::Position(_) => Some(0),
        AprsPacket::PositionCs(_) => Some(1),
        AprsPacket::PositionTimestamped(_) => Some(2),
        AprsPacket::PositionWeather(_) => Some(3),
        AprsPacket::Weather(_) => Some(4),
        AprsPacket::Telemetry(_) => Some(5),
        AprsPacket::Object(_) => Some(6),
        AprsPacket::Item(_) => Some(7),
        AprsPacket::Status(_) => Some(8),
        AprsPacket::Message(_) => Some(9),
        AprsPacket::Capabilities(_) => Some(10),
        _ => None,
    }
}

/// The DTI table and the valid corpus, checked against the parser
/// itself rather than against a comment.
///
/// Two things rot silently and have: a DTI sweep that stops enumerating
/// every branch of `AprsPacket::parse`, and a "one encoding per packet
/// kind" corpus that stops covering every kind. Both claims are
/// re-derived here — the dispatch set from a 256-byte probe of the
/// parser, the variant set from parsing the corpus — so adding a DTI or
/// a variant to `src/aprs.rs` fails this test until the fuzz inputs
/// follow.
#[test]
fn dti_table_and_corpus_cover_every_aprs_packet_variant() {
    // A byte the parser dispatches on reaches a sub-parser, which
    // answers `Ok` or a body-level error; an undispatched byte comes
    // straight back as `InvalidDataType`. That difference is the
    // machine-readable form of "the DTI dispatch table".
    let dispatched: Vec<u8> = (0..=u8::MAX)
        .filter(|&byte| {
            !matches!(
                AprsPacket::parse(&[byte]),
                Err(AprsError::InvalidDataType { .. })
            )
        })
        .collect();
    let as_chars: Vec<char> = dispatched.iter().map(|&b| b as char).collect();
    assert_eq!(
        dispatched.len(),
        DISPATCH_DTI_COUNT,
        "AprsPacket::parse dispatches on {as_chars:?}, which is not {DISPATCH_DTI_COUNT} \
         identifiers: update DISPATCH_DTI_COUNT and DTIS together"
    );
    for &dti in &dispatched {
        assert!(
            DTIS.contains(&dti),
            "DTIS is missing the dispatched identifier {:?}, so the fuzz sweep never reaches \
             its branch",
            dti as char
        );
    }
    // The Mic-E pair is extra: fuzzed, but not dispatched.
    for &dti in &MIC_E_DTIS {
        assert!(DTIS.contains(&dti), "DTIS lost a Mic-E identifier");
        assert!(
            !dispatched.contains(&dti),
            "{:?} is now a dispatched identifier, not a rejected Mic-E one",
            dti as char
        );
    }
    assert_eq!(DTIS.len(), DISPATCH_DTI_COUNT + MIC_E_DTI_COUNT);
    for (i, &a) in DTIS.iter().enumerate() {
        assert!(
            !DTIS[i + 1..].contains(&a),
            "DTIS lists {:?} twice, inflating the case count without adding a branch",
            a as char
        );
    }

    // The corpus: a floor so the loops below cannot pass over nothing,
    // then per-variant reachability.
    let corpus = corpus();
    assert!(
        corpus.len() >= MIN_CORPUS_CASES,
        "corpus shrank to {}, below the {MIN_CORPUS_CASES}-encoding floor",
        corpus.len()
    );
    let mut seen = [false; VARIANT_COUNT];
    for encoded in &corpus {
        let parsed = AprsPacket::parse(encoded).expect("every corpus encoding must parse");
        let index = variant_index(&parsed).expect(
            "the corpus produced an AprsPacket variant this file does not know: extend \
             variant_index, VARIANT_COUNT and the corpus together",
        );
        seen[index] = true;
    }
    for (index, hit) in seen.iter().enumerate() {
        assert!(
            *hit,
            "no corpus encoding parses as AprsPacket variant #{index}, so truncation and \
             corruption never reach it"
        );
    }
}

/// Random-bytes fuzz of `AprsPacket::parse`: fully random inputs and
/// inputs pinned to each data-type-ID branch. ~16,650 cases.
#[test]
fn fuzz_aprs_packet_parse_random() {
    let mut rng = Lcg::new(0xA905_2024_0001);
    // Fully random first bytes.
    for _ in 0..3000 {
        let input = rng.bytes(100);
        let _ = AprsPacket::parse(&input);
    }
    // Each DTI branch with a random tail.
    for &dti in &DTIS {
        for _ in 0..1050 {
            let mut input = vec![dti];
            input.extend_from_slice(&rng.bytes(99));
            let _ = AprsPacket::parse(&input);
        }
    }
}

/// Random-bytes fuzz of every individually reachable APRS sub-parser
/// (position with/without csT and timestamp, both weather forms,
/// telemetry, object, item, status, message, capabilities, data
/// extension, object timestamp).
#[test]
fn fuzz_aprs_subparsers_random() {
    // `DataExtension::parse` reads the same seven bytes two ways: with
    // the weather symbol `DDD/SSS` is wind, with any other it is
    // course/speed. Both symbol classes are fuzzed so neither branch is
    // reached only by accident.
    const EXT_SYMBOLS: [Symbol; 2] = [Symbol::CAR, Symbol::from_wire(b'/', b'_')];

    let mut rng = Lcg::new(0xA905_2024_0002);
    for _ in 0..2000 {
        let input = rng.bytes(100);
        let _ = Position::parse(&input);
        let _ = PositionCs::parse(&input);
        let _ = PositionTimestamped::parse(&input);
        let _ = PositionWeather::parse(&input);
        let _ = PositionlessWeather::parse(&input);
        let _ = Telemetry::parse(&input);
        let _ = Object::parse(&input);
        let _ = Item::parse(&input);
        let _ = Status::parse(&input);
        let _ = Message::parse(&input);
        let _ = Capabilities::parse(&input);
        for symbol in EXT_SYMBOLS {
            let _ = DataExtension::parse(&input, symbol);
        }
        if !input.is_empty() {
            let _ = Timestamp::parse(&input, rng.below(input.len()));
        }
        let _ = Timestamp::parse(&input, 0);
    }
    // Sub-parsers also see inputs whose first byte is a plausible DTI.
    for &dti in &DTIS {
        for _ in 0..200 {
            let mut input = vec![dti];
            input.extend_from_slice(&rng.bytes(60));
            let _ = Position::parse(&input);
            let _ = PositionCs::parse(&input);
            let _ = PositionTimestamped::parse(&input);
            let _ = PositionWeather::parse(&input);
            let _ = PositionlessWeather::parse(&input);
            let _ = Telemetry::parse(&input);
            let _ = Object::parse(&input);
            let _ = Item::parse(&input);
            let _ = Status::parse(&input);
            let _ = Message::parse(&input);
            let _ = Capabilities::parse(&input);
            for symbol in EXT_SYMBOLS {
                let _ = DataExtension::parse(&input, symbol);
            }
        }
    }
}

/// Random dest + info fuzz of the Mic-E decoder, including
/// correct-length destinations and Mic-E DTI-prefixed info fields.
#[cfg(feature = "micE")]
#[test]
fn fuzz_mic_e_decode_random() {
    use warble::aprs::mic_e;
    let mut rng = Lcg::new(0xA905_2024_0003);
    for _ in 0..3000 {
        let dest = rng.bytes(10);
        let info = rng.bytes(60);
        let _ = mic_e::decode(&dest, &info);
    }
    // Force the length-6 destination path and each Mic-E DTI so the
    // deep decode branches are reached, with printable-biased bytes.
    for _ in 0..3000 {
        let dest: Vec<u8> = (0..6).map(|_| 0x20 + rng.next_u8() % 0x5F).collect();
        let mut info = vec![if rng.next_u8() & 1 == 0 { b'`' } else { b'\'' }];
        let len = rng.below(40);
        info.extend((0..len).map(|_| {
            if rng.next_u8() & 3 == 0 {
                rng.next_u8()
            } else {
                0x20 + rng.next_u8() % 0x5F
            }
        }));
        let _ = mic_e::decode(&dest, &info);
    }
}

/// Random byte streams through the KISS deframer (including seeded
/// bursts of FEND/FESC patterns) plus the exhaustive command-byte parse.
#[cfg(feature = "kiss")]
#[test]
fn fuzz_kiss_deframer_and_command() {
    use warble::kiss::{FEND, FESC, KissCommand, KissDeframer, TFEND, TFESC};

    // Command-byte parse is total over all 256 bytes.
    for byte in 0..=255u8 {
        let _ = KissCommand::from_byte(byte);
    }

    let mut rng = Lcg::new(0xA905_2024_0004);
    let mut deframer = KissDeframer::<64>::new();
    // Long fully-random stream through one persistent deframer.
    for _ in 0..20_000 {
        let _ = deframer.push(rng.next_u8());
    }
    // Streams biased toward framing/escape bytes to hit every
    // deframer state transition (FESC followed by every byte, nested
    // FENDs, empty frames).
    let specials = [FEND, FESC, TFEND, TFESC, 0x00, 0xFF];
    for _ in 0..200 {
        let mut d = KissDeframer::<32>::new();
        for _ in 0..rng.below(200) {
            let byte = if rng.next_u8() & 1 == 0 {
                specials[rng.below(specials.len())]
            } else {
                rng.next_u8()
            };
            let _ = d.push(byte);
        }
    }
}

/// Random-bytes fuzz of the AX.25 UI-frame parser and address decode.
#[test]
fn fuzz_ax25_frame_parse_random() {
    let mut rng = Lcg::new(0xA905_2024_0005);
    for _ in 0..4000 {
        let input = rng.bytes(100);
        let _ = UiFrame::parse(&input);
        if input.len() >= 7 {
            let field: &[u8; 7] = input[..7].try_into().unwrap();
            let _ = Address::decode(field);
        }
    }
    // Frames that begin with plausible shifted-ASCII address bytes so
    // parsing proceeds past the address field before hitting garbage.
    for _ in 0..2000 {
        let mut input: Vec<u8> = (0..14)
            .map(|_| (0x41u8 << 1) | (rng.next_u8() & 1))
            .collect();
        input.extend_from_slice(&rng.bytes(60));
        let _ = UiFrame::parse(&input);
    }
}

/// Random bit streams through the HDLC deframer, including runs of
/// flag bytes to open/close frames around garbage.
#[test]
fn fuzz_hdlc_deframer_random_bits() {
    use warble::Bit;
    use warble::ax25::HdlcDeframer;

    let mut rng = Lcg::new(0xA905_2024_0006);
    let mut deframer = HdlcDeframer::<64>::new();
    for _ in 0..50_000 {
        let bit = if rng.next_u8() & 1 == 1 {
            Bit::One
        } else {
            Bit::Zero
        };
        let _ = deframer.push(bit);
    }
    // Interleave flag octets so frames open and close.
    for _ in 0..500 {
        for i in 0..8 {
            let bit = if (0x7Eu8 >> i) & 1 == 1 {
                Bit::One
            } else {
                Bit::Zero
            };
            let _ = deframer.push(bit);
        }
        for _ in 0..rng.below(64) {
            let bit = if rng.next_u8() & 1 == 1 {
                Bit::One
            } else {
                Bit::Zero
            };
            let _ = deframer.push(bit);
        }
    }
}

fn lat(v: i64) -> Latitude {
    Latitude::new(v * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn lon(v: i64) -> Longitude {
    Longitude::new(v * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

/// Floor on [`corpus`]: at least one encoding per `AprsPacket` variant,
/// plus the extra compressed-`Position` spelling and the `csT` altitude
/// trailer. Asserted in
/// `dti_table_and_corpus_cover_every_aprs_packet_variant`, so the
/// truncation and corruption loops below cannot pass having compared
/// nothing, and cannot pass having skipped a variant.
const MIN_CORPUS_CASES: usize = VARIANT_COUNT + 2;

/// A valid corpus: one encoded information field per packet kind.
fn corpus() -> Vec<Vec<u8>> {
    let packets: [AprsPacket<'static>; MIN_CORPUS_CASES] = [
        AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(49 * 6000 + 350),
            longitude: lon(-(72 * 6000 + 175)),
            symbol: Symbol::HOUSE,
            messaging: false,
            compressed: false,
            extension: None,
            comment: b"fuzz corpus",
        }),
        AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(-6001),
            longitude: lon(6001),
            symbol: Symbol::from_wire(b'\\', b'O'),
            messaging: true,
            compressed: true,
            extension: None,
            comment: b"cmp",
        }),
        AprsPacket::Status(Status {
            text: b"fuzz status",
        }),
        AprsPacket::Message(Message {
            addressee: Addressee::new(b"N1CALL").unwrap(),
            content: MessageContent::Text {
                text: b"hello fuzz",
                id: Some(b"7"),
            },
        }),
        AprsPacket::Weather(PositionlessWeather {
            month: 6,
            day: 15,
            hour: 12,
            minute: 30,
            weather: WeatherReport {
                wind_direction: Some(220),
                // A positionless report spells wind speed in mph.
                wind_speed: Some(Speed::from_mph(4)),
                gust: Some(Speed::from_mph(5)),
                temperature: Some(Temperature::from_fahrenheit(77)),
                rain_1h: Some(Rainfall::from_hundredths_inch(0)),
                rain_24h: Some(Rainfall::from_hundredths_inch(0)),
                rain_midnight: Some(Rainfall::from_hundredths_inch(0)),
                humidity: Some(Humidity::new(50).expect("in range")),
                barometric_pressure: Some(Pressure::from_tenths_hpa(9900)),
                // Chapter 12's optional "other parameters" are written
                // only when present; this report has neither.
                luminosity: None,
                snowfall: None,
            },
            rest: b"",
        }),
        AprsPacket::Telemetry(Telemetry {
            seq: 5,
            analog: Telemetry::integer_channels([199, 0, 255, 73, 123]),
            digital: Some([false, true, true, false, true, false, false, true]),
            rest: b"",
        }),
        AprsPacket::Object(Object {
            ambiguity: Ambiguity::EXACT,
            name: b"LEADER",
            live: true,
            timestamp: Timestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45,
            },
            latitude: lat(49 * 6000 + 350),
            longitude: lon(-(72 * 6000 + 175)),
            symbol: Symbol::CAR,
            comment: b"088/036",
        }),
        AprsPacket::Item(Item {
            ambiguity: Ambiguity::EXACT,
            name: b"AID#2",
            live: true,
            latitude: lat(6000),
            longitude: lon(-6000),
            symbol: Symbol::from_wire(b'/', b'8'),
            comment: b"first aid",
        }),
        // A compressed position whose csT trailer carries data, so the
        // dispatch returns `PositionCs` rather than `Position`.
        AprsPacket::PositionCs(PositionCs {
            position: Position {
                ambiguity: Ambiguity::EXACT,
                latitude: lat(49 * 6000 + 350),
                longitude: lon(-(72 * 6000 + 175)),
                symbol: Symbol::from_wire(b'/', b'>'),
                messaging: true,
                compressed: true,
                extension: None,
                comment: b"rng",
            },
            cs: CompressedCs::RadioRange { miles: 20 },
            compression_type: CompressionType::default(),
        }),
        // The altitude trailer, which this corpus could not carry while
        // the corruption family exempted it from the fixed-point check.
        // 363 feet is the value that exposed the defect: the code that
        // decodes to it is one above the code nearest 1.002^n = 363.
        AprsPacket::PositionCs(PositionCs {
            position: Position {
                ambiguity: Ambiguity::EXACT,
                latitude: lat(49 * 6000 + 350),
                longitude: lon(-(72 * 6000 + 175)),
                symbol: Symbol::from_wire(b'/', b'>'),
                messaging: false,
                compressed: true,
                extension: None,
                comment: b"alt",
            },
            cs: CompressedCs::Altitude { feet: 363 },
            compression_type: CompressionType::default(),
        }),
        AprsPacket::PositionTimestamped(PositionTimestamped {
            timestamp: Timestamp::Hms {
                hour: 23,
                minute: 45,
                second: 17,
            },
            position: Position {
                ambiguity: Ambiguity::EXACT,
                latitude: lat(49 * 6000 + 350),
                longitude: lon(-(72 * 6000 + 175)),
                symbol: Symbol::CAR,
                messaging: true,
                compressed: false,
                extension: None,
                comment: b"stamped",
            },
            cs: CompressedCs::NoData,
            compression_type: CompressionType::default(),
        }),
        // A Complete Weather Report: the `_` symbol code plus the
        // positional `DDD/SSS` wind block. Free of a tagged `s` field,
        // whose meaning differs between the two weather layouts and is
        // not this suite's to pin.
        AprsPacket::PositionWeather(PositionWeather {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(49 * 6000 + 350),
            longitude: lon(-(72 * 6000 + 175)),
            symbol: Symbol::from_wire(b'/', b'_'),
            messaging: false,
            timestamp: None,
            weather: WeatherReport {
                wind_direction: Some(220),
                // The positional half of a Complete Weather Report
                // spells wind speed in knots.
                wind_speed: Some(Speed::from_knots(4)),
                temperature: Some(Temperature::from_fahrenheit(77)),
                humidity: Some(Humidity::new(50).expect("in range")),
                barometric_pressure: Some(Pressure::from_tenths_hpa(9900)),
                ..WeatherReport::default()
            },
            rest: b"",
        }),
        AprsPacket::Capabilities(Capabilities {
            body: b"IGATE,MSG_CNT=13,LOC_CNT=54",
        }),
    ];
    packets
        .iter()
        .map(|p| {
            let mut buf = [0u8; 256];
            let len = p.build(&mut buf).unwrap();
            buf[..len].to_vec()
        })
        .collect()
}

/// Truncation fuzz: every prefix of every valid encoding parses
/// without panicking.
#[test]
fn fuzz_truncated_valid_encodings() {
    for encoded in corpus() {
        for cut in 0..encoded.len() {
            let _ = AprsPacket::parse(&encoded[..cut]);
        }
        // The full form must still parse.
        assert!(AprsPacket::parse(&encoded).is_ok());
    }
}

/// Corruption fuzz: seeded random byte replacements and single-bit
/// flips over the valid corpus. Where a corrupted form still parses,
/// its re-encoding must parse back to the same typed packet (stable).
#[test]
fn fuzz_corrupted_valid_encodings() {
    let mut rng = Lcg::new(0xA905_2024_0007);
    let corpus = corpus();
    for _ in 0..3000 {
        let mut bytes = corpus[rng.below(corpus.len())].clone();
        // 1..4 mutations: random byte replacement or single-bit flip.
        for _ in 0..(1 + rng.below(4)) {
            let at = rng.below(bytes.len());
            if rng.next_u8() & 1 == 0 {
                bytes[at] = rng.next_u8();
            } else {
                bytes[at] ^= 1 << rng.below(8);
            }
        }
        if let Ok(parsed) = AprsPacket::parse(&bytes) {
            let mut buf = [0u8; 300];
            let len = parsed
                .build(&mut buf)
                .expect("a parsed packet must re-encode");
            let reparsed = AprsPacket::parse(&buf[..len]).expect("re-encoding must re-parse");
            // The csT altitude used to be exempt here: decode truncates
            // to whole feet (per chapter 9's worked example) while
            // encode rounded to the nearest 1.002-exponent, so the
            // cycle wandered by a foot instead of settling. `build` now
            // inverts the parser rather than the power and reaches the
            // fixed point like everything else, so the exemption is
            // gone and this family covers the altitude scale again.
            //
            // Every packet canonicalizes on the first re-encode; a
            // second encode must then be a byte-for-byte fixed point.
            let mut buf2 = [0u8; 300];
            let len2 = reparsed
                .build(&mut buf2)
                .expect("a re-parsed packet must re-encode");
            assert_eq!(
                &buf2[..len2],
                &buf[..len],
                "re-encode/re-parse must reach a fixed point"
            );
        }
    }
}

/// Truncation + corruption fuzz of the AX.25 layer: whole UI frames
/// (with FCS appended) cut at every length and randomly mutated.
#[test]
fn fuzz_ax25_truncation_and_corruption() {
    use warble::ax25::crc16_x25;

    let sr = SampleRate::new(11_025).unwrap();
    let cfg = TncConfig::bell_202(sr).unwrap();
    let tx = TncTransmitter::new(cfg);
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 7).unwrap(),
            &[Address::new(b"WIDE1", 1).unwrap()],
            b"!4903.50N/07201.75W-fuzz",
            &mut frame_buf,
        )
        .unwrap();
    let mut with_fcs = frame_buf[..len].to_vec();
    with_fcs.extend_from_slice(&crc16_x25(&frame_buf[..len]).to_le_bytes());

    for cut in 0..=with_fcs.len() {
        let _ = UiFrame::parse(&with_fcs[..cut]);
    }
    let mut rng = Lcg::new(0xA905_2024_0008);
    for _ in 0..3000 {
        let mut bytes = with_fcs.clone();
        for _ in 0..(1 + rng.below(4)) {
            let at = rng.below(bytes.len());
            bytes[at] ^= 1 << rng.below(8);
        }
        let _ = UiFrame::parse(&bytes);
    }
}

/// Seeded random and pathological PCM into the TNC receiver: no panic,
/// and the error counters only ever grow.
#[test]
fn fuzz_tnc_receiver_pcm() {
    let sr = SampleRate::new(11_025).unwrap();
    let cfg = TncConfig::bell_202(sr).unwrap();

    // Fully random i16 samples (white noise at full scale).
    let mut rng = Lcg::new(0xA905_2024_0009);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut prev = rx.stats();
    for _ in 0..60_000 {
        let sample = rng.next_u64() as i16;
        let _ = rx.push_i16(sample);
        let now = rx.stats();
        assert!(now.frames_ok >= prev.frames_ok, "counter went backwards");
        assert!(now.fcs_errors >= prev.fcs_errors, "counter went backwards");
        assert!(now.oversize >= prev.oversize, "counter went backwards");
        assert!(now.malformed >= prev.malformed, "counter went backwards");
        prev = now;
    }

    // Pathological extremes: constant rails and alternating extremes.
    for pattern in 0..4u8 {
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        for i in 0..20_000u32 {
            let sample = match pattern {
                0 => i16::MAX,
                1 => i16::MIN,
                2 => {
                    if i & 1 == 0 {
                        i16::MAX
                    } else {
                        i16::MIN
                    }
                }
                _ => 0,
            };
            let _ = rx.push_i16(sample);
        }
    }

    // Random f32 samples including out-of-range and non-finite values.
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    for i in 0..20_000u32 {
        let sample = match i % 7 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => f32::NEG_INFINITY,
            3 => 1.0e9,
            _ => (rng.next_u64() >> 40) as f32 / 8_388_608.0 - 1.0,
        };
        let _ = rx.push_f32(sample);
    }
}
