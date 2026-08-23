//! Integration tests for compressed-position `csT` trailer support and
//! timestamped position reports (APRS 1.01 chapters 8 and 9).
#![cfg(feature = "aprs")]

use warble::aprs::{
    AprsError, AprsPacket, CompressedCs, CompressionOrigin, CompressionType, Latitude, Longitude,
    NmeaSource, Position, PositionCs, PositionTimestamped, Symbol, Timestamp,
};
use warble::geo::Ambiguity;

fn lat(v: i64) -> Latitude {
    Latitude::new(v * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn lon(v: i64) -> Longitude {
    Longitude::new(v * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

/// The chapter 9 example coordinates: 49.5 N, 72.75 W, symbol `/>`.
/// Storage units per step of the compressed grid, both axes.
const COMP_LAT_STEP: i64 = warble::geo::UNITS_PER_DEGREE / 380_926;
const COMP_LON_STEP: i64 = warble::geo::UNITS_PER_DEGREE / 190_463;

/// The coordinates chapter 9's `/5L!!<*e7` vector carries.
///
/// The spec's prose rounds them to 49.5 N / 72.75 W and the wire is
/// finer: `5L!!` is base-91 15 427 503 and `<*e7` is 20 427 156, so the
/// longitude is exactly -180 + 20427156/190463 = -72.75000393777269
/// degrees. Rounding that to the nearest 1/100 arc-minute, which is
/// what this crate used to store, moved the station 0.44 m.
fn spec_latlon() -> (Latitude, Longitude) {
    let deg = warble::geo::UNITS_PER_DEGREE;
    (
        Latitude::new(90 * deg - 15_427_503 * COMP_LAT_STEP).expect("in range"),
        Longitude::new(20_427_156 * COMP_LON_STEP - 180 * deg).expect("in range"),
    )
}

fn spec_position(messaging: bool, comment: &[u8]) -> Position<'_> {
    let (latitude, longitude) = spec_latlon();
    Position {
        ambiguity: Ambiguity::EXACT,
        latitude,
        longitude,
        symbol: Symbol::CAR,
        messaging,
        compressed: true,
        extension: None,
        comment,
    }
}

/// The chapter 9 example T byte: current fix, RMC, software-compressed.
fn t_rmc() -> CompressionType {
    CompressionType {
        current_fix: true,
        nmea_source: NmeaSource::Rmc,
        origin: CompressionOrigin::Software,
    }
}

#[test]
fn spec_course_speed_vector() {
    // Chapter 9: cs = "7P" with a non-GGA T byte is course 88 degrees,
    // speed 1.08^47 - 1 = 36.2 knots (36 to the nearest knot).
    let packet = AprsPacket::parse(b"=/5L!!<*e7>7P[").unwrap();
    match packet {
        AprsPacket::PositionCs(p) => {
            assert_eq!(p.position, spec_position(true, b""));
            assert_eq!(
                p.cs,
                CompressedCs::CourseSpeed {
                    course: 88,
                    speed: 36
                }
            );
            assert_eq!(p.compression_type, t_rmc());
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Encoding the decoded values reproduces the wire bytes.
    let mut buf = [0u8; 32];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"=/5L!!<*e7>7P[");
}

#[test]
fn spec_radio_range_vector() {
    // Chapter 9: c = '{', s = '?' is a range of 2 * 1.08^30 = 20 miles.
    let packet = AprsPacket::parse(b"!/5L!!<*e7>{?[").unwrap();
    match packet {
        AprsPacket::PositionCs(p) => {
            assert_eq!(p.cs, CompressedCs::RadioRange { miles: 20 });
            assert_eq!(p.compression_type, t_rmc());
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 32];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"!/5L!!<*e7>{?[");
}

#[test]
fn spec_altitude_vector() {
    // Chapter 9: cs = "S]" with a GGA T byte is an altitude of
    // 1.002^(50*91 + 60) = 10004 feet.
    // T = 'S': current fix, GGA, software-compressed.
    let packet = AprsPacket::parse(b"!/5L!!<*e7>S]S").unwrap();
    match packet {
        AprsPacket::PositionCs(p) => {
            assert_eq!(p.cs, CompressedCs::Altitude { feet: 10004 });
            assert_eq!(p.compression_type.nmea_source, NmeaSource::Gga);
            assert!(p.compression_type.current_fix);
            assert_eq!(p.compression_type.origin, CompressionOrigin::Software);
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 32];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"!/5L!!<*e7>S]S");
}

#[test]
fn no_data_trailer_stays_plain_position() {
    // c = ' ' parses to the plain Position variant with the " sT"
    // literal reproduced on build.
    let packet = AprsPacket::parse(b"!/5L!!<*e7> sT").unwrap();
    match packet {
        AprsPacket::Position(p) => assert_eq!(p, spec_position(false, b"")),
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 32];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"!/5L!!<*e7> sT");
}

#[test]
fn cs_round_trips_all_variants_and_quadrants() {
    let coords = [
        (49 * 6000 + 3000, -(72 * 6000 + 4500)),
        (-(89 * 6000 + 5999), 179 * 6000 + 5999),
        (0, 0),
        (540_000, -1_080_000),
    ];
    let variants = [
        CompressedCs::NoData,
        CompressedCs::CourseSpeed {
            course: 0,
            speed: 0,
        },
        CompressedCs::CourseSpeed {
            course: 356,
            speed: 36,
        },
        CompressedCs::RadioRange { miles: 20 },
        CompressedCs::Altitude { feet: 10004 },
    ];
    for (la, lo) in coords {
        for cs in variants {
            let packet = AprsPacket::PositionCs(PositionCs {
                position: Position {
                    ambiguity: Ambiguity::EXACT,
                    latitude: lat(la),
                    longitude: lon(lo),
                    symbol: Symbol::BALLOON,
                    messaging: false,
                    compressed: true,
                    extension: None,
                    comment: b"rt",
                },
                cs,
                compression_type: t_rmc(),
            });
            let mut buf = [0u8; 32];
            let len = packet.build(&mut buf).unwrap();
            let parsed = AprsPacket::parse(&buf[..len]).unwrap();
            match (parsed, cs) {
                (AprsPacket::Position(p), CompressedCs::NoData) => {
                    // Quantised onto the base-91 grid, which does not
                    // contain the 1/100 arc-minute grid these inputs
                    // are written on.
                    assert!((p.latitude.units() - lat(la).units()).abs() <= COMP_LAT_STEP);
                    assert!((p.longitude.units() - lon(lo).units()).abs() <= COMP_LON_STEP);
                }
                (AprsPacket::PositionCs(p), _) => {
                    assert_eq!(p.cs, cs, "({la}, {lo}) {cs:?}");
                    assert!(
                        (p.position.latitude.units() - lat(la).units()).abs() <= COMP_LAT_STEP,
                        "({la}, {lo}) {cs:?}"
                    );
                    assert!(
                        (p.position.longitude.units() - lon(lo).units()).abs() <= COMP_LON_STEP,
                        "({la}, {lo}) {cs:?}"
                    );
                }
                (other, _) => panic!("wrong variant: {other:?}"),
            }
        }
    }
}

#[test]
fn cs_boundary_values_round_trip() {
    // Course 0 and 356 (the extreme 4-degree steps), speed 0, the
    // largest one-digit speed/range exponents, and a large altitude.
    let cases = [
        CompressedCs::CourseSpeed {
            course: 0,
            speed: 0,
        },
        CompressedCs::CourseSpeed {
            course: 356,
            speed: 1018,
        },
        CompressedCs::RadioRange { miles: 2 },
        CompressedCs::RadioRange { miles: 2038 },
        CompressedCs::Altitude { feet: 1 },
        // 363 feet is code 2951, whose nearest 1.002-power code is
        // 2950, which reads back as 362. Building through the power
        // lost that foot on every altitude sitting where the two
        // roundings disagree, which is 999 of the 8281 codes. Here as a
        // public-API regression case for `every_cs_code_is_value_
        // stable_through_a_rebuild`, which sweeps the whole domain one
        // layer down.
        CompressedCs::Altitude { feet: 363 },
    ];
    for cs in cases {
        let packet = AprsPacket::PositionCs(PositionCs {
            position: spec_position(false, b""),
            cs,
            compression_type: CompressionType::default(),
        });
        let mut buf = [0u8; 32];
        let len = packet.build(&mut buf).unwrap();
        match AprsPacket::parse(&buf[..len]).unwrap() {
            AprsPacket::PositionCs(p) => assert_eq!(p.cs, cs, "{cs:?}"),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}

#[test]
fn large_altitude_is_wire_stable() {
    // A large altitude is quantized to the nearest exponent of 1.002;
    // the decoded value is within the wire resolution (0.2%) and a
    // decode/re-encode reproduces the same bytes.
    let requested: u32 = 15_000_000;
    let packet = AprsPacket::PositionCs(PositionCs {
        position: spec_position(false, b""),
        cs: CompressedCs::Altitude { feet: requested },
        compression_type: CompressionType::default(),
    });
    let mut buf = [0u8; 32];
    let len = packet.build(&mut buf).unwrap();
    let decoded = match AprsPacket::parse(&buf[..len]).unwrap() {
        AprsPacket::PositionCs(p) => match p.cs {
            CompressedCs::Altitude { feet } => feet,
            other => panic!("wrong cs: {other:?}"),
        },
        other => panic!("wrong variant: {other:?}"),
    };
    let diff = decoded.abs_diff(requested);
    assert!(diff < requested / 400, "decoded {decoded}");
    let rebuilt = AprsPacket::PositionCs(PositionCs {
        position: spec_position(false, b""),
        cs: CompressedCs::Altitude { feet: decoded },
        compression_type: CompressionType::default(),
    });
    let mut buf2 = [0u8; 32];
    let len2 = rebuilt.build(&mut buf2).unwrap();
    assert_eq!(&buf2[..len2], &buf[..len]);
}

#[test]
fn course_rounds_to_nearest_step() {
    // 358 degrees rounds up to 360 == 0; 87 rounds to 88.
    for (course, expect) in [(358u16, 0u16), (87, 88), (2, 4), (1, 0)] {
        let packet = AprsPacket::PositionCs(PositionCs {
            position: spec_position(false, b""),
            cs: CompressedCs::CourseSpeed { course, speed: 10 },
            compression_type: CompressionType::default(),
        });
        let mut buf = [0u8; 32];
        let len = packet.build(&mut buf).unwrap();
        match AprsPacket::parse(&buf[..len]).unwrap() {
            AprsPacket::PositionCs(p) => match p.cs {
                CompressedCs::CourseSpeed { course: got, .. } => {
                    assert_eq!(got, expect, "course {course}");
                }
                other => panic!("wrong cs: {other:?}"),
            },
            other => panic!("wrong variant: {other:?}"),
        }
    }
}

#[test]
fn timestamped_positions_round_trip_both_dtis() {
    // '/' (no messaging) and '@' (messaging), uncompressed and
    // compressed bodies, all three timestamp forms.
    let stamps = [
        Timestamp::DhmZulu {
            day: 9,
            hour: 23,
            minute: 45,
        },
        Timestamp::DhmLocal {
            day: 31,
            hour: 0,
            minute: 0,
        },
        Timestamp::Hms {
            hour: 23,
            minute: 59,
            second: 59,
        },
    ];
    for messaging in [false, true] {
        for compressed in [false, true] {
            for timestamp in stamps {
                let packet = AprsPacket::PositionTimestamped(PositionTimestamped {
                    timestamp,
                    position: Position {
                        ambiguity: Ambiguity::EXACT,
                        latitude: lat(49 * 6000 + 350),
                        longitude: lon(-(72 * 6000 + 175)),
                        symbol: Symbol::CAR,
                        messaging,
                        compressed,
                        extension: None,
                        comment: b"ts",
                    },
                    cs: if compressed {
                        CompressedCs::CourseSpeed {
                            course: 88,
                            speed: 36,
                        }
                    } else {
                        CompressedCs::NoData
                    },
                    compression_type: if compressed {
                        t_rmc()
                    } else {
                        CompressionType::default()
                    },
                });
                let mut buf = [0u8; 64];
                let len = packet.build(&mut buf).unwrap();
                assert_eq!(buf[0], if messaging { b'@' } else { b'/' });
                let back = AprsPacket::parse(&buf[..len]).unwrap();
                if compressed {
                    // Compressed quantises onto the base-91 grid; the
                    // uncompressed form is exact.
                    let (AprsPacket::PositionTimestamped(a), AprsPacket::PositionTimestamped(b)) =
                        (&back, &packet)
                    else {
                        panic!("expected timestamped positions");
                    };
                    assert_eq!(a.timestamp, b.timestamp);
                    assert_eq!(a.cs, b.cs);
                    assert!(
                        (a.position.latitude.units() - b.position.latitude.units()).abs()
                            <= COMP_LAT_STEP
                    );
                    assert!(
                        (a.position.longitude.units() - b.position.longitude.units()).abs()
                            <= COMP_LON_STEP
                    );
                } else {
                    assert_eq!(back, packet);
                }
            }
        }
    }
}

#[test]
fn timestamped_uncompressed_wire_bytes() {
    let packet = AprsPacket::parse(b"@092345z4903.50N/07201.75W>Test1234").unwrap();
    match packet {
        AprsPacket::PositionTimestamped(p) => {
            assert_eq!(
                p.timestamp,
                Timestamp::DhmZulu {
                    day: 9,
                    hour: 23,
                    minute: 45
                }
            );
            assert_eq!(p.position.latitude, lat(49 * 6000 + 350));
            assert_eq!(p.position.longitude, lon(-(72 * 6000 + 175)));
            assert!(p.position.messaging);
            assert!(!p.position.compressed);
            assert_eq!(p.cs, CompressedCs::NoData);
            assert_eq!(p.position.comment, b"Test1234");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"@092345z4903.50N/07201.75W>Test1234");
}

#[test]
fn timestamped_compressed_wire_bytes() {
    let packet = AprsPacket::parse(b"/234517h/5L!!<*e7>{?[net").unwrap();
    match packet {
        AprsPacket::PositionTimestamped(p) => {
            assert_eq!(
                p.timestamp,
                Timestamp::Hms {
                    hour: 23,
                    minute: 45,
                    second: 17
                }
            );
            assert!(!p.position.messaging);
            assert!(p.position.compressed);
            assert_eq!(p.cs, CompressedCs::RadioRange { miles: 20 });
            assert_eq!(p.position.comment, b"net");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"/234517h/5L!!<*e7>{?[net");
}

#[test]
fn rejects_bad_base91_in_trailer() {
    // s byte below '!' in a course/speed trailer.
    assert_eq!(
        AprsPacket::parse(b"!/5L!!<*e7>7\x20["),
        Err(AprsError::BadBase91 {
            got: b' ',
            position: 12
        })
    );
    // T byte above '{'.
    assert_eq!(
        AprsPacket::parse(b"!/5L!!<*e7>7P~"),
        Err(AprsError::BadBase91 {
            got: b'~',
            position: 13
        })
    );
}

#[test]
fn rejects_truncated_trailer() {
    // 12 of the 13 compressed body bytes present.
    assert_eq!(
        AprsPacket::parse(b"!/5L!!<*e7>7P"),
        Err(AprsError::Truncated {
            expected: 14,
            got: 13
        })
    );
}

#[test]
fn rejects_bad_timestamp() {
    assert_eq!(
        AprsPacket::parse(b"@09x345z4903.50N/07201.75W>"),
        Err(AprsError::BadDigit {
            got: b'x',
            position: 3
        })
    );
    assert_eq!(
        AprsPacket::parse(b"@322345z4903.50N/07201.75W>"),
        Err(AprsError::BadTimestamp {
            field: b'D',
            got: 32
        })
    );
    // Unknown format suffix letter.
    assert_eq!(
        AprsPacket::parse(b"/092345q4903.50N/07201.75W>"),
        Err(AprsError::BadTimestamp {
            field: b'?',
            got: i32::from(b'q')
        })
    );
}

#[test]
fn rejects_out_of_range_builds() {
    let base = PositionCs {
        position: spec_position(false, b""),
        cs: CompressedCs::NoData,
        compression_type: CompressionType::default(),
    };
    let mut buf = [0u8; 32];
    let with = |cs, t| PositionCs {
        cs,
        compression_type: t,
        ..base
    };
    assert_eq!(
        with(
            CompressedCs::CourseSpeed {
                course: 360,
                speed: 0
            },
            CompressionType::default()
        )
        .build(&mut buf),
        Err(AprsError::BadCourse { got: 360 })
    );
    assert_eq!(
        with(
            CompressedCs::CourseSpeed {
                course: 0,
                speed: 60_000
            },
            CompressionType::default()
        )
        .build(&mut buf),
        Err(AprsError::BadSpeed { got: 60_000 })
    );
    assert_eq!(
        with(
            CompressedCs::RadioRange { miles: 60_000 },
            CompressionType::default()
        )
        .build(&mut buf),
        Err(AprsError::BadRadioRange { got: 60_000 })
    );
    assert_eq!(
        with(
            CompressedCs::Altitude { feet: u32::MAX },
            CompressionType::default()
        )
        .build(&mut buf),
        Err(AprsError::BadAltitude { got: u32::MAX })
    );
    // Course/speed and radio range cannot claim the GGA source: that
    // bit pattern selects the altitude form on the wire.
    let gga = CompressionType {
        nmea_source: NmeaSource::Gga,
        ..CompressionType::default()
    };
    assert_eq!(
        with(
            CompressedCs::CourseSpeed {
                course: 88,
                speed: 36
            },
            gga
        )
        .build(&mut buf),
        Err(AprsError::NmeaSourceConflict)
    );
    assert_eq!(
        with(CompressedCs::RadioRange { miles: 20 }, gga).build(&mut buf),
        Err(AprsError::NmeaSourceConflict)
    );
}

#[test]
fn compression_type_byte_round_trips() {
    // Every field combination survives a to_byte/from_byte trip and
    // stays within the base-91 alphabet.
    let sources = [
        NmeaSource::Other,
        NmeaSource::Gll,
        NmeaSource::Gga,
        NmeaSource::Rmc,
    ];
    let origins = [
        CompressionOrigin::Compressed,
        CompressionOrigin::TncBtext,
        CompressionOrigin::Software,
        CompressionOrigin::Tbd,
        CompressionOrigin::Kpc3,
        CompressionOrigin::Pico,
        CompressionOrigin::OtherTracker,
        CompressionOrigin::Digipeater,
    ];
    for current_fix in [false, true] {
        for nmea_source in sources {
            for origin in origins {
                let t = CompressionType {
                    current_fix,
                    nmea_source,
                    origin,
                };
                let byte = t.to_byte();
                assert!((0x21..=0x7b).contains(&byte));
                assert_eq!(CompressionType::from_byte(byte, 0), Ok(t));
            }
        }
    }
    assert_eq!(
        CompressionType::from_byte(b'~', 5),
        Err(AprsError::BadBase91 {
            got: b'~',
            position: 5
        })
    );
}

/// KNOWN-GAP PIN, not an endorsement: the compression-type byte is
/// **not** round-trip exact, and this test asserts the current *lossy*
/// behaviour on purpose so that any change to it fails loudly.
///
/// `CompressionType::from_byte` reads bit 5 (fix age), bits 4-3 (NMEA
/// source) and bits 2-0 (origin). Bit 6 is read by nothing and
/// `to_byte` never sets it, yet a base-91 digit runs to 90 -- so every
/// wire value `64..=90` has bit 6 set and re-encodes as a *different*
/// byte, aliasing onto the value 64 lower.
///
/// Why it is pinned rather than fixed: the APRS 1.01 chapter 9 table
/// leaves bits 7-6 undefined, so conforming traffic has bit 6 clear and
/// no position is decoded wrongly by this. But the crate's stated
/// invariant is byte-exact wire fidelity, and this silently breaks it --
/// any parse-then-forward path (digipeater, igate, KISS bridge) rewrites
/// the byte. The real fix is a raw carrier that keeps the received byte
/// alongside the typed fields, which is a design change and out of
/// scope here.
///
/// Recorded in `docs/APRS_CONFORMANCE.md` section 4 ("The type-design
/// tension", point 3, wire-fidelity losses), which named this loss and
/// noted that nothing pinned it. This test is that pin; if it ever
/// fails, the behaviour moved and that record needs updating with it.
///
/// `compression_type_byte_round_trips` above cannot catch this and is
/// not weakened by it: every typed field combination encodes to a value
/// below 64, so bit 6 is never set on the bytes it exercises.
#[test]
fn compression_type_byte_drops_bit_6_known_gap() {
    // 'a' is base-91 value 64: bit 6 alone. Every typed field reads as
    // its zero variant, so the byte re-encodes as '!' (value 0).
    let lone_bit6 = CompressionType::from_byte(b'a', 0).expect("'a' is inside '!'..='{'");
    assert_eq!(
        lone_bit6,
        CompressionType {
            current_fix: false,
            nmea_source: NmeaSource::Other,
            origin: CompressionOrigin::Compressed,
        }
    );
    assert_eq!(
        lone_bit6.to_byte(),
        b'!',
        "today bit 6 of 'a' is dropped, not preserved"
    );
    // '{' is 90, the top of the alphabet: bits 6, 4, 3 and 1 are set.
    // Bits 4-3 (Rmc) and 1 (Software) survive; bit 6 does not, so the
    // byte comes back as ';' (value 26).
    let top = CompressionType::from_byte(b'{', 0).expect("'{' is inside '!'..='{'");
    assert_eq!(
        top,
        CompressionType {
            current_fix: false,
            nmea_source: NmeaSource::Rmc,
            origin: CompressionOrigin::Software,
        }
    );
    assert_eq!(
        top.to_byte(),
        b';',
        "today bit 6 of the top base-91 digit is dropped, not preserved"
    );
    // The loss is exactly bit 6 across the whole affected range: each
    // value 64..=90 decodes identically to the value 64 below it, and
    // only that lower spelling survives a re-encode.
    for value in 64u8..=90 {
        let with_bit6 = 0x21 + value;
        let without = 0x21 + (value & !(1u8 << 6));
        assert_eq!(
            CompressionType::from_byte(with_bit6, 0),
            CompressionType::from_byte(without, 0),
            "wire byte {with_bit6:#04x} aliases onto {without:#04x} today"
        );
        assert_eq!(
            CompressionType::from_byte(with_bit6, 0).map(CompressionType::to_byte),
            Ok(without),
            "wire byte {with_bit6:#04x} re-encodes as {without:#04x}: bit 6 is lost"
        );
    }
}
