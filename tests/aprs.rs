//! Integration tests for the `aprs` feature: information-field
//! build/parse round trips with known spec vectors, typed rejection
//! vectors, the UI-frame glue, and (with the `mod` and `demod`
//! features) a full-stack trip through real AFSK modulation and
//! demodulation.
#![cfg(feature = "aprs")]

use yodel::aprs::{
    Addressee, AprsError, AprsPacket, CompressedCs, DataExtension, Latitude, Longitude, Message,
    MessageContent, Position, PositionTimestamped, Status, Symbol, Timestamp, build_ui_frame,
    packet_from_ui,
};
use yodel::ax25::{Address, UiFrame};
use yodel::geo::Ambiguity;

/// A coordinate magnitude in 1/100 arc-minutes, the unit the fixtures
/// in this file are written in. Storage is finer, so this rounds.
fn hundredths(units: i64) -> i64 {
    let step = yodel::geo::UNITS_PER_HUNDREDTH_MINUTE;
    let half = if units < 0 { -step / 2 } else { step / 2 };
    (units + half) / step
}

fn lat(v: i64) -> Latitude {
    Latitude::new(v * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn lon(v: i64) -> Longitude {
    Longitude::new(v * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn addr(call: &[u8], ssid: u8) -> Address {
    Address::new(call, ssid).unwrap()
}

#[test]
fn uncompressed_position_spec_vector_round_trip() {
    // APRS 1.01 example coordinate: 49 deg 03.50 min N, 072 deg 01.75 min W.
    let info = b"=4903.50N/07201.75W-Test 001234";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Position(p) => {
            assert_eq!(p.latitude, lat(49 * 6000 + 350));
            assert_eq!(p.longitude, lon(-(72 * 6000 + 175)));
            assert_eq!(p.symbol.to_wire(), (b'/', b'-'));
            assert!(p.messaging);
            assert!(!p.compressed);
            assert_eq!(p.comment, b"Test 001234");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], info);
}

#[test]
fn uncompressed_position_south_east_round_trip() {
    let packet = AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: lat(-(33 * 6000 + 5212)),
        longitude: lon(151 * 6000 + 1234),
        symbol: Symbol::from_wire(b'\\', b'k'),
        messaging: false,
        compressed: false,
        extension: None,
        comment: b"down under",
    });
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"!3352.12S\\15112.34Ekdown under");
    assert_eq!(AprsPacket::parse(&buf[..len]).unwrap(), packet);
}

#[test]
fn compressed_position_spec_vector() {
    // APRS 1.01 chapter 9: "/5L!!<*e7>" encodes 49.5 N, 72.75 W with
    // symbol '>' on the primary table; the trailing "{?!" is a
    // 20-mile radio range.
    let packet = AprsPacket::parse(b"=/5L!!<*e7>{?!").unwrap();
    match packet {
        AprsPacket::PositionCs(p) => {
            assert!(p.position.compressed);
            // The spec's prose rounds this to 49.5 N / 72.75 W. The
            // wire is finer: `<*e7` is base-91 20 427 156, so the
            // longitude is exactly -180 + 20427156/190463 degrees,
            // which is -72.75000393777269. Storing it to the nearest
            // 1/100 arc-minute, as this crate used to, moved the
            // station 0.44 m.
            let step_lat = yodel::geo::UNITS_PER_DEGREE / 380_926;
            let step_lon = yodel::geo::UNITS_PER_DEGREE / 190_463;
            assert_eq!(
                p.position.latitude,
                Latitude::new(90 * yodel::geo::UNITS_PER_DEGREE - 15_427_503 * step_lat).unwrap()
            );
            assert_eq!(
                p.position.longitude,
                Longitude::new(20_427_156 * step_lon - 180 * yodel::geo::UNITS_PER_DEGREE).unwrap()
            );
            assert_eq!(p.position.symbol.to_wire(), (b'/', b'>'));
            assert_eq!(p.cs, CompressedCs::RadioRange { miles: 20 });
            assert_eq!(p.position.comment, b"");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Building the same coordinates reproduces the spec's base-91 bytes.
    let rebuilt = AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: lat(49 * 6000 + 3000),
        longitude: lon(-(72 * 6000 + 4500)),
        symbol: Symbol::CAR,
        messaging: true,
        compressed: true,
        extension: None,
        comment: b"",
    });
    let mut buf = [0u8; 32];
    let len = rebuilt.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"=/5L!!<*e7> sT");
}

#[test]
fn compressed_position_round_trip_hemispheres() {
    for (la, lo, table) in [
        (49 * 6000 + 350, -(72 * 6000 + 175), b'/'),
        (-6001, 6001, b'\\'),
        (89 * 6000, 179 * 6000, b'A'),
    ] {
        let packet = AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(la),
            longitude: lon(lo),
            symbol: Symbol::from_wire(table, b'O'),
            messaging: false,
            compressed: true,
            extension: None,
            comment: b"asc",
        });
        let mut buf = [0u8; 32];
        let len = packet.build(&mut buf).unwrap();
        // A value on the 1/100 arc-minute grid is not generally on the
        // base-91 grid, so this quantises by up to one compressed step
        // (0.29 m on latitude). It used to look exact only because both
        // grids were rounded onto the coarser one, which is the defect.
        let back = AprsPacket::parse(&buf[..len]).unwrap();
        let (AprsPacket::Position(a), AprsPacket::Position(b)) = (&back, &packet) else {
            panic!("expected positions");
        };
        assert!(
            (a.latitude.units() - b.latitude.units()).abs()
                <= yodel::geo::UNITS_PER_DEGREE / 380_926
        );
        assert!(
            (a.longitude.units() - b.longitude.units()).abs()
                <= yodel::geo::UNITS_PER_DEGREE / 190_463
        );
        assert_eq!(a.symbol, b.symbol);
        assert_eq!(a.comment, b.comment);
    }
}

#[test]
fn status_round_trip() {
    let packet = AprsPacket::Status(Status {
        text: b"Balloon ascending through 5000m",
    });
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b">Balloon ascending through 5000m");
    assert_eq!(AprsPacket::parse(&buf[..len]).unwrap(), packet);
}

#[test]
fn message_with_id_round_trip() {
    let packet = AprsPacket::Message(Message {
        addressee: Addressee::new(b"N0CALL").unwrap(),
        content: MessageContent::Text {
            text: b"Testing",
            id: Some(b"003"),
        },
    });
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b":N0CALL   :Testing{003");
    assert_eq!(AprsPacket::parse(&buf[..len]).unwrap(), packet);
}

#[test]
fn ack_round_trip() {
    let packet = AprsPacket::Message(Message {
        addressee: Addressee::new(b"N1CALL-14").unwrap(),
        content: MessageContent::Ack { id: b"003" },
    });
    let mut buf = [0u8; 32];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b":N1CALL-14:ack003");
    assert_eq!(AprsPacket::parse(&buf[..len]).unwrap(), packet);
}

#[test]
fn timestamped_position_round_trip() {
    let packet = AprsPacket::parse(b"@092345z4903.50N/07201.75W>comment").unwrap();
    match packet {
        AprsPacket::PositionTimestamped(PositionTimestamped {
            timestamp,
            position,
            ..
        }) => {
            assert_eq!(
                timestamp,
                Timestamp::DhmZulu {
                    day: 9,
                    hour: 23,
                    minute: 45
                }
            );
            assert!(position.messaging);
            assert_eq!(position.comment, b"comment");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b"@092345z4903.50N/07201.75W>comment");
}

#[test]
fn rejection_vectors() {
    assert_eq!(
        AprsPacket::parse(b"!49x3.50N/07201.75W-"),
        Err(AprsError::BadDigit {
            got: b'x',
            position: 3
        })
    );
    assert_eq!(
        AprsPacket::parse(b"!9101.00N/07201.75W-"),
        Err(AprsError::BadLatitude {
            got: (91 * 6000 + 100) * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE
        })
    );
    assert_eq!(
        AprsPacket::parse(b"!4903.50N/18101.00W-"),
        Err(AprsError::BadLongitude {
            got: (-(181 * 6000 + 100)) * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE
        })
    );
    assert_eq!(
        AprsPacket::parse(b"!4903.50X/07201.75W-"),
        Err(AprsError::BadHemisphere { got: b'X' })
    );
    // A no-fix beacon sends '0' where the hemisphere belongs. It must
    // stay rejected: decoding it would place the station at 0,0 in the
    // Gulf of Guinea rather than reporting that it has no position.
    // MEASURED: 58 such frames in the corpus, all from one device.
    assert_eq!(
        AprsPacket::parse(b"!0000.000/00000.000>000/000"),
        Err(AprsError::BadHemisphere { got: b'0' })
    );
    assert_eq!(
        AprsPacket::parse(b"!4903.50N/07201.75W"),
        Err(AprsError::Truncated {
            expected: 20,
            got: 19
        })
    );
    // Lower-case hemispheres are accepted on receive. The spec specifies
    // upper case and we always transmit it, but a lower-case letter says
    // nothing about whether the position decoded -- and it is on the air.
    // MEASURED: recovers 2 corpus frames.
    let lower = AprsPacket::parse(b"!3301.22n/11653.34w_").expect("lower-case hemisphere");
    let upper = AprsPacket::parse(b"!3301.22N/11653.34W_").expect("upper-case hemisphere");
    assert_eq!(lower, upper, "case must not change the decoded position");
    let AprsPacket::Position(p) = lower else {
        panic!("expected a position")
    };
    // South/west equivalents, so the sign mapping is exercised too.
    let s = AprsPacket::parse(b"!3301.22s/11653.34e_").expect("lower-case S/E");
    let AprsPacket::Position(q) = s else {
        panic!("expected a position")
    };
    assert_eq!(
        hundredths(q.latitude.units()),
        -hundredths(p.latitude.units())
    );
    assert_eq!(
        hundredths(q.longitude.units()),
        -hundredths(p.longitude.units())
    );
    assert_eq!(
        AprsPacket::parse(b":TOOLONGADDR:hi"),
        Err(AprsError::ExpectedByte {
            expected: b':',
            got: b'D',
            position: 10
        })
    );
    // NOT a rejection: chapter 14 caps the identifier at five
    // characters, so `{123456` is not one and the brace belongs to the
    // text. See `message_text_may_open_with_a_brace` in
    // `tests/rebuild_fidelity.rs` for the traffic that forced this.
    assert!(matches!(
        AprsPacket::parse(b":N0CALL   :hello{123456"),
        Ok(AprsPacket::Message(_))
    ));
}

/// Pulls the course/speed extension out of a whole information field.
fn course_speed(info: &[u8]) -> (Option<u16>, Option<u16>) {
    let AprsPacket::Position(p) = AprsPacket::parse(info).expect("a position report") else {
        panic!("expected a plain position report from {info:?}")
    };
    let Some(DataExtension::CourseSpeed { course, speed }) = p.extension else {
        panic!("expected a course/speed extension in {info:?}")
    };
    (course.degrees(), speed.knots())
}

/// Pulls the wind extension out of a whole information field.
///
/// Goes through [`Position::parse`] rather than [`AprsPacket::parse`]
/// on purpose. `AprsPacket` routes every `_`-symbol position that the
/// weather decoder accepts to [`AprsPacket::PositionWeather`], so this
/// is the entry point that reaches the data-extension reading of the
/// same bytes -- which is exactly the reading under test.
fn wind(info: &[u8]) -> (Option<u16>, Option<u16>) {
    let p = Position::parse(info).expect("a position report");
    let Some(DataExtension::Wind { direction, speed }) = p.extension else {
        panic!("expected a wind extension in {info:?}")
    };
    (direction.degrees(), speed.knots())
}

/// A zero *speed* is a speed, not a missing reading.
///
/// Chapter 7 states the unknown sentinel for the **pair** — "if the
/// course and speed are unknown or not relevant, they can be set to
/// `000/000` or `.../...` or `   /   `" — and puts no lower bound on
/// the speed. Reading the two halves independently threw away the one
/// fact a stationary tracker is trying to report.
///
/// The independent reference agrees on the interesting case: it prints
/// `0 km/h (0 MPH), course 315` for `315/000`.
///
/// MEASURED: 18 corpus frames are a real course beside a zero speed;
/// only 2 are the `000/000` sentinel pair.
#[test]
fn zero_speed_beside_a_real_course_survives() {
    let head = b"!4903.50N/07201.75W>";
    let mut info = [0u8; 27];
    info[..head.len()].copy_from_slice(head);
    for (ext, expected) in [
        // The spec's own sentinel, spelled all three ways.
        (&b"000/000"[..], (None, None)),
        (b".../...", (None, None)),
        (b"   /   ", (None, None)),
        // A real course and a standing start: both are information.
        (b"315/000", (Some(315), Some(0))),
        (b"194/000", (Some(194), Some(0))),
        (b"035/000", (Some(35), Some(0))),
        // Course unknown, speed known: `000` is outside the stated
        // course domain of 001-360, and the pair rule does not fire.
        (b"000/048", (None, Some(48))),
        // Both ends of the course domain.
        (b"360/010", (Some(360), Some(10))),
        (b"001/010", (Some(1), Some(10))),
    ] {
        info[head.len()..].copy_from_slice(ext);
        assert_eq!(course_speed(&info), expected, "{:?}", &info[..]);
    }
}

/// The same seven bytes must mean the same thing on both of this
/// crate's paths through them.
///
/// `weather.rs` reads the `DDD/SSS` of a Complete Weather Report as a
/// plain number and has always called `240/000` calm. The `_`-symbol
/// data extension used to call the identical bytes *unknown*, so the
/// meaning of a frame flipped on whether a weather block happened to
/// follow. It no longer does.
#[test]
fn wind_extension_and_weather_report_agree_on_calm() {
    // No weather block: the seven bytes are a Wind data extension.
    assert_eq!(wind(b"!4903.50N/07201.75W_240/000"), (Some(240), Some(0)));

    // The same seven bytes at the head of a Complete Weather Report,
    // read by the weather decoder instead.
    let packet = AprsPacket::parse(b"!4903.50N/07201.75W_240/000g005t077r000p000P000h50b09900")
        .expect("a complete weather report");
    let AprsPacket::PositionWeather(w) = packet else {
        panic!("expected a weather report, got {packet:?}")
    };
    assert_eq!(w.weather.wind_direction, Some(240));
    assert_eq!(
        w.weather.wind_speed.map(|s| s.knots()),
        Some(0),
        "calm on the weather path too"
    );

    // And a real wind still reads as a real wind on both paths.
    assert_eq!(wind(b"!4903.50N/07201.75W_220/004"), (Some(220), Some(4)));
}

/// A law, not a table: reinterpreting `000` must not be able to move a
/// byte on the wire.
///
/// `Bearing` and `Speed` keep the received `[u8; 3]` and
/// `DataExtension::write` copies it verbatim, which is the whole reason
/// that field exists -- so the builder needed no change for the
/// zero-speed fix and this pins that. Swept over every `ddd/sss` a
/// station can send, for both the course/speed and the wind readings,
/// with a real position report around it and a real builder doing the
/// writing.
#[test]
fn every_ddd_sss_position_round_trips_byte_exactly() {
    let mut info = *b"!4903.50N/07201.75W>000/000";
    let symbol_at = 19;
    let ext_at = 20;
    let mut buf = [0u8; 64];
    for symbol in [b'>', b'_'] {
        info[symbol_at] = symbol;
        for d in 0u16..=360 {
            info[ext_at] = b'0' + (d / 100) as u8;
            info[ext_at + 1] = b'0' + (d / 10 % 10) as u8;
            info[ext_at + 2] = b'0' + (d % 10) as u8;
            for s in 0u16..=999 {
                info[ext_at + 4] = b'0' + (s / 100) as u8;
                info[ext_at + 5] = b'0' + (s / 10 % 10) as u8;
                info[ext_at + 6] = b'0' + (s % 10) as u8;
                let p = Position::parse(&info).expect("a position report");
                assert!(p.extension.is_some(), "no extension in {info:?}");
                let len = p.build(&mut buf).unwrap();
                assert_eq!(&buf[..len], &info[..], "round trip for {info:?}");
            }
        }
    }
    // Whole packets, including the timestamped and altitude-bearing
    // shapes, through the packet-level builder.
    for whole in [
        &b"!4903.50N/07201.75W>000/000"[..],
        b"!4903.50N/07201.75W>315/000",
        b"!4903.50N/07201.75W>000/048",
        b"!4903.50N/07201.75W>.../...",
        b"!4903.50N/07201.75W>   /   ",
        b"=4903.50N/07201.75W>035/000/A=001059",
        b"@092345z4903.50N/07201.75W>194/000 stationary",
    ] {
        let packet = AprsPacket::parse(whole).expect("a position report");
        let len = packet.build(&mut buf).unwrap();
        assert_eq!(&buf[..len], whole, "round trip for {whole:?}");
    }
}

#[test]
fn builder_overflow() {
    let packet = AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: lat(0),
        longitude: lon(0),
        symbol: Symbol::HOUSE,
        messaging: false,
        compressed: false,
        extension: None,
        comment: b"overflow",
    });
    let mut small = [0u8; 10];
    assert_eq!(
        packet.build(&mut small),
        Err(AprsError::BufferTooSmall {
            needed: 28,
            max: 10
        })
    );
}

#[test]
fn ui_frame_glue_round_trip() {
    let packet = AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: lat(49 * 6000 + 350),
        longitude: lon(-(72 * 6000 + 175)),
        symbol: Symbol::BALLOON,
        messaging: false,
        compressed: false,
        extension: None,
        comment: b"glue",
    });
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; 330];
    let len = build_ui_frame(
        &packet,
        addr(b"APZ001", 0),
        addr(b"N0CALL", 11),
        &[addr(b"WIDE1", 1)],
        &mut info_buf,
        &mut frame_buf,
    )
    .unwrap();
    let frame = UiFrame::parse(&frame_buf[..len]).unwrap();
    assert_eq!(frame.dest, addr(b"APZ001", 0));
    assert_eq!(frame.src, addr(b"N0CALL", 11));
    assert_eq!(packet_from_ui(&frame).unwrap(), packet);
}

#[cfg(all(feature = "mod", feature = "demod"))]
mod full_stack {
    use super::*;
    use yodel::SampleRate;
    use yodel::ax25::{FrameReceiver, tx_i16};
    use yodel::demodulator::DemodulatorConfig;
    use yodel::modulator::{Modulator, ModulatorConfig};

    #[test]
    fn packet_to_samples_and_back() {
        // AprsPacket -> UI frame -> HDLC/NRZI/AFSK samples -> demodulated
        // -> parsed back to an equal AprsPacket.
        let packet = AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(49 * 6000 + 350),
            longitude: lon(-(72 * 6000 + 175)),
            symbol: Symbol::BALLOON,
            messaging: true,
            compressed: false,
            extension: None,
            comment: b"Full stack 001",
        });
        let mut info_buf = [0u8; 64];
        let mut frame_buf = [0u8; 330];
        let len = build_ui_frame(
            &packet,
            addr(b"APRS", 0),
            addr(b"N0CALL", 11),
            &[addr(b"WIDE1", 1), addr(b"WIDE2", 1)],
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap();

        let sr = SampleRate::new(48_000).unwrap();
        let modulator = Modulator::new(ModulatorConfig::bell_202(sr).unwrap());
        let demod = yodel::AfskDemodulator::new(DemodulatorConfig::bell_202(sr).unwrap()).unwrap();
        let mut rx = FrameReceiver::<330>::new(demod);

        let mut recovered: Vec<Vec<u8>> = Vec::new();
        for sample in tx_i16(&frame_buf[..len], modulator) {
            if let Some(Ok(f)) = rx.push_sample_i16(sample) {
                recovered.push(f.to_vec());
            }
        }
        assert_eq!(recovered.len(), 1, "exactly one frame expected");
        let frame = UiFrame::parse(&recovered[0]).unwrap();
        assert_eq!(frame.src, addr(b"N0CALL", 11));
        assert_eq!(packet_from_ui(&frame).unwrap(), packet);
    }
}
