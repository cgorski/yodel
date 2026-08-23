//! Integration tests for the session-3 APRS extras: weather reports
//! (positionless and position-with-weather), telemetry, objects and
//! items — build/parse round trips, hand-derived spec vectors and typed
//! rejection vectors.
//!
//! Also the APRS 1.1 reply-ACK message-id accessors, which are a
//! read-only reinterpretation of a field the parser already stores; see
//! the section at the end of the file.
#![cfg(feature = "aprs")]

use warble::geo::Ambiguity;
use warble::units::{Humidity, Pressure, Rainfall, Speed, Temperature};

use warble::aprs::{
    Addressee, AprsError, AprsPacket, Item, Latitude, Longitude, Message, MessageContent, Object,
    PositionWeather, PositionlessWeather, Symbol, Telemetry, TelemetryValue, Timestamp,
    WeatherReport,
};

fn lat(v: i64) -> Latitude {
    Latitude::new(v * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn lon(v: i64) -> Longitude {
    Longitude::new(v * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

// ---------------------------------------------------------------- weather

#[test]
fn positionless_weather_spec_vector() {
    // Hand-derived from the APRS 1.01 chapter 12 field definitions:
    // Oct 9, 05:56, wind 220 deg at 4 mph gusting 5, 77 F, rain 0.00
    // last hour, 0.00 in 24 h, 0.00 since midnight, humidity 50%,
    // pressure 990.0 hPa.
    let info = b"_10090556c220s004g005t077r000p000P000h50b09900wRSW";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Weather(w) => {
            assert_eq!((w.month, w.day, w.hour, w.minute), (10, 9, 5, 56));
            assert_eq!(w.weather.wind_direction, Some(220));
            // The positionless `sNNN` field is miles per hour; the
            // same three digits in a Complete Weather Report's
            // `DDD/SSS` extension would be knots (chapter 7 vs 12).
            assert_eq!(w.weather.wind_speed, Some(Speed::from_mph(4)));
            assert_eq!(w.weather.wind_speed.map(Speed::kmh), Some(6));
            assert_eq!(w.weather.gust, Some(Speed::from_mph(5)));
            assert_eq!(
                w.weather.temperature,
                Some(Temperature::from_fahrenheit(77))
            );
            assert_eq!(w.weather.temperature.map(Temperature::celsius), Some(25));
            assert_eq!(w.weather.rain_1h.map(Rainfall::hundredths_inch), Some(0));
            assert_eq!(w.weather.rain_24h.map(Rainfall::hundredths_inch), Some(0));
            assert_eq!(
                w.weather.rain_midnight.map(Rainfall::hundredths_inch),
                Some(0)
            );
            assert_eq!(w.weather.humidity.map(Humidity::percent), Some(50));
            assert_eq!(
                w.weather.barometric_pressure.map(Pressure::tenths_hpa),
                Some(9900)
            );
            assert_eq!(w.rest, b"wRSW");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Rebuild round trip: the builder emits fields in canonical order,
    // which matches this vector exactly.
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);
}

#[test]
fn positionless_weather_all_missing_round_trip() {
    let report = PositionlessWeather {
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        weather: WeatherReport::default(),
        rest: b"",
    };
    let mut buf = [0u8; 64];
    let len = report.build(&mut buf).unwrap();
    // Absence is now spelled by omission rather than by a dotted
    // placeholder, so a report carrying no measurements is just its
    // timestamp. Chapter 12 allows both spellings ("may not even
    // exist"); omission is chosen because a placeholder run can only
    // lengthen a packet, and when a tag has been swallowed into `rest`
    // the run is written before it and the tag then appears twice.
    assert_eq!(&buf[..len], b"_01010000");
    let parsed = PositionlessWeather::parse(&buf[..len]).unwrap();
    assert_eq!(parsed, report);
    assert_eq!(parsed.weather, WeatherReport::default());
}

#[test]
fn weather_negative_temperature_and_humidity_00() {
    let report = PositionlessWeather {
        month: 12,
        day: 31,
        hour: 23,
        minute: 59,
        weather: WeatherReport {
            wind_direction: Some(360),
            wind_speed: Some(Speed::from_mph(0)),
            gust: Some(Speed::from_mph(999)),
            temperature: Some(Temperature::from_fahrenheit(-42)),
            rain_1h: Some(Rainfall::from_hundredths_inch(999)),
            rain_24h: Some(Rainfall::from_hundredths_inch(1)),
            rain_midnight: Some(Rainfall::from_hundredths_inch(0)),
            humidity: Some(Humidity::new(100).expect("in range")),
            barometric_pressure: Some(Pressure::from_tenths_hpa(10132)),
            // The standard block's `s` is the wind speed above. A snow
            // depth would be spelled as a *second* `s` after `b`; this
            // report has none, and must not grow one. See
            // `positionless_weather_snowfall_builds_and_round_trips`.
            luminosity: None,
            snowfall: None,
        },
        rest: b"",
    };
    let mut buf = [0u8; 64];
    let len = report.build(&mut buf).unwrap();
    assert_eq!(
        &buf[..len],
        b"_12312359c360s000g999t-42r999p001P000h00b10132"
    );
    let parsed = PositionlessWeather::parse(&buf[..len]).unwrap();
    assert_eq!(
        parsed.weather.temperature.map(Temperature::fahrenheit),
        Some(-42)
    );
    // "00" humidity decodes to 100%.
    assert_eq!(parsed.weather.humidity.map(Humidity::percent), Some(100));
    assert_eq!(parsed.weather.humidity.map(Humidity::wire_percent), Some(0));
    assert_eq!(parsed, report);
}

#[test]
fn weather_temperature_boundaries_round_trip() {
    for t in [-99i16, -1, 0, 1, 999] {
        let report = PositionlessWeather {
            month: 6,
            day: 15,
            hour: 12,
            minute: 30,
            weather: WeatherReport {
                temperature: Some(Temperature::from_fahrenheit(i32::from(t))),
                ..WeatherReport::default()
            },
            rest: b"",
        };
        let mut buf = [0u8; 64];
        let len = report.build(&mut buf).unwrap();
        assert_eq!(PositionlessWeather::parse(&buf[..len]), Ok(report), "{t}");
    }
}

#[test]
fn positionless_weather_rejections() {
    // Bad month (13).
    assert_eq!(
        PositionlessWeather::parse(b"_13090556c220s004"),
        Err(AprsError::BadTimestamp {
            field: b'M',
            got: 13
        })
    );
    // Non-digit in the timestamp.
    assert_eq!(
        PositionlessWeather::parse(b"_10x90556c220s004"),
        Err(AprsError::BadDigit {
            got: b'x',
            position: 3
        })
    );
    // Unknown weather field tag, at the head of the block where it
    // cannot be the trailer. After a real measurement the same shape
    // *is* the trailer -- chapter 12 ends a report with a
    // software-type letter and a free-form 2-4 character unit code --
    // and rejecting it there cost 38 corpus frames a complete weather
    // report over a two-byte manufacturer stamp.
    assert_eq!(
        PositionlessWeather::parse(b"_10090556q123c220s004"),
        Err(AprsError::UnknownWeatherField { got: b'q' })
    );
    let tolerated = PositionlessWeather::parse(b"_10090556c220s004q123").expect("trailer");
    assert_eq!(tolerated.weather.wind_direction, Some(220));
    assert_eq!(tolerated.rest, b"q123");
    // Truncated field.
    assert_eq!(
        PositionlessWeather::parse(b"_100905"),
        Err(AprsError::Truncated {
            expected: 9,
            got: 7
        })
    );
    // Non-digit inside a value.
    assert_eq!(
        PositionlessWeather::parse(b"_10090556c2x0s004"),
        Err(AprsError::BadDigit {
            got: b'x',
            position: 11
        })
    );
}

#[test]
fn positionless_weather_build_rejections() {
    let mut buf = [0u8; 64];
    let bad_month = PositionlessWeather {
        month: 0,
        day: 1,
        hour: 0,
        minute: 0,
        weather: WeatherReport::default(),
        rest: b"",
    };
    assert_eq!(
        bad_month.build(&mut buf),
        Err(AprsError::BadTimestamp {
            field: b'M',
            got: 0
        })
    );
    let bad_wind = PositionlessWeather {
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        weather: WeatherReport {
            wind_direction: Some(361),
            ..WeatherReport::default()
        },
        rest: b"",
    };
    assert_eq!(
        bad_wind.build(&mut buf),
        Err(AprsError::BadWeatherValue {
            field: b'c',
            got: 361
        })
    );
    let ok = PositionlessWeather {
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        weather: WeatherReport::default(),
        rest: b"",
    };
    let mut small = [0u8; 8];
    assert_eq!(
        ok.build(&mut small),
        Err(AprsError::BufferTooSmall { needed: 9, max: 8 })
    );
}

#[test]
fn position_with_weather_spec_vector() {
    // Complete weather report with uncompressed position (chapter 12
    // format): wind 220 deg at 4 mph, gust 5, 77 F.
    let info = b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900wRSW";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::PositionWeather(w) => {
            assert_eq!(w.latitude, lat(49 * 6000 + 350));
            assert_eq!(w.longitude, lon(-(72 * 6000 + 175)));
            assert_eq!(w.symbol.to_wire().0, b'/');
            assert!(!w.messaging);
            assert_eq!(w.weather.wind_direction, Some(220));
            // Chapter 12 says this 7-byte field *is* the Wind
            // Direction and Wind Speed Data Extension, and chapter 7
            // defines that in knots -- unlike the `sNNN` of a
            // positionless report, which is miles per hour. Reading it
            // as mph is a silent 15% error, and was one until the
            // field-level differential asked an independent decoder.
            assert_eq!(w.weather.wind_speed, Some(Speed::from_knots(4)));
            assert_eq!(w.weather.wind_speed.map(Speed::mph), Some(5));
            // The gust is miles per hour in both layouts.
            assert_eq!(w.weather.gust, Some(Speed::from_mph(5)));
            assert_eq!(
                w.weather.temperature,
                Some(Temperature::from_fahrenheit(77))
            );
            assert_eq!(w.weather.humidity.map(Humidity::percent), Some(50));
            assert_eq!(
                w.weather.barometric_pressure.map(Pressure::tenths_hpa),
                Some(9900)
            );
            assert_eq!(w.rest, b"wRSW");
            assert_eq!(w.position().symbol.to_wire().1, b'_');
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 80];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);
}

/// Chapter 12's *timestamped* Complete Weather Report, from the
/// specification's own worked example for that layout.
///
/// Four uncompressed spellings exist -- `!` and `=` without a
/// timestamp, `/` and `@` with -- and only the first two were
/// implemented. MEASURED: 92 corpus frames use this one, 54 directly
/// and 38 inside third-party wrappers, and every one came back as an
/// ordinary timestamped position whose entire weather block stayed
/// uninterpreted comment text. The field-level differential is what
/// noticed, by asking an independent decoder what it saw.
///
/// Note the negative temperature, which is the spec's own choice here
/// and exercises the `-07` spelling in the same vector.
#[test]
fn timestamped_position_with_weather_spec_vector() {
    let info = b"@092345z4903.50N/07201.75W_220/004g005t-07r000p000P000h50b09900wRSW";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::PositionWeather(w) => {
            assert_eq!(
                w.timestamp,
                Some(Timestamp::DhmZulu {
                    day: 9,
                    hour: 23,
                    minute: 45
                })
            );
            assert!(w.messaging, "`@` is the messaging-capable spelling");
            assert_eq!(w.latitude, lat(49 * 6000 + 350));
            assert_eq!(w.longitude, lon(-(72 * 6000 + 175)));
            assert_eq!(w.weather.wind_direction, Some(220));
            assert_eq!(w.weather.wind_speed, Some(Speed::from_knots(4)));
            assert_eq!(w.weather.gust, Some(Speed::from_mph(5)));
            assert_eq!(
                w.weather.temperature,
                Some(Temperature::from_fahrenheit(-7))
            );
            assert_eq!(w.weather.humidity.map(Humidity::percent), Some(50));
            assert_eq!(
                w.weather.barometric_pressure.map(Pressure::tenths_hpa),
                Some(9900)
            );
            assert_eq!(w.rest, b"wRSW");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Byte-exact rebuild, which is what an igate re-transmitting this
    // depends on.
    let mut buf = [0u8; 96];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);

    // The non-messaging spelling of the same layout.
    let info = b"/092345z4903.50N/07201.75W_220/004g005t-07";
    match AprsPacket::parse(info).unwrap() {
        AprsPacket::PositionWeather(w) => assert!(!w.messaging),
        other => panic!("wrong variant: {other:?}"),
    }

    // A timestamped position whose symbol is `_` but whose body is not
    // a wind block must still parse -- as a position, not as a loss.
    let info = b"@092345z4903.50N/07201.75W_hello there";
    match AprsPacket::parse(info).unwrap() {
        AprsPacket::PositionTimestamped(_) => {}
        other => panic!("expected a fall back to position, got {other:?}"),
    }
}

#[test]
fn position_with_weather_missing_wind_round_trip() {
    let report = PositionWeather {
        ambiguity: Ambiguity::EXACT,
        timestamp: None,
        latitude: lat(-(33 * 6000 + 5212)),
        longitude: lon(151 * 6000 + 1234),
        symbol: Symbol::WEATHER_STATION,
        messaging: true,
        weather: WeatherReport {
            temperature: Some(Temperature::from_fahrenheit(-5)),
            ..WeatherReport::default()
        },
        rest: b"",
    };
    let mut buf = [0u8; 80];
    let len = report.build(&mut buf).unwrap();
    assert_eq!(
        &buf[..len],
        // Absent fields are omitted rather than dotted; see the note
        // on `positionless_weather_all_missing_round_trip`.
        b"=3352.12S/15112.34E_.../...t-05"
    );
    let parsed = match AprsPacket::parse(&buf[..len]) {
        Ok(AprsPacket::PositionWeather(w)) => w,
        other => panic!("wrong variant: {other:?}"),
    };
    assert_eq!(parsed, report);
}

/// The tagged `s` of a Complete Weather Report is **snowfall**, and
/// reading it as wind speed destroyed a field that was already right.
///
/// Chapter 12 says of this layout that "the 7-byte Wind Direction and
/// Wind Speed Data Extension **replace the cccc and ssss fields** of a
/// Positionless Weather Report", and lists among the extra parameters
/// `s` = "snowfall (in inches) in the last 24 hours". So the wind here
/// is the positional `220/004` — 4 **knots** — and the `s050` after the
/// barometer is 50 inches of snow.
///
/// VERIFIED before the fix: `wind_speed` came back as
/// `Speed::from_mph(50)`, silently overwriting the 4 knots the
/// positional field had already decoded correctly, while an independent
/// decoder read the same bytes as "4.6 mph, 50.0 snow in 24 hours".
/// There are **0 such frames in the corpus**, so no ratchet could ever
/// have seen this; it needs a hand-written vector, which is this one.
#[test]
fn complete_weather_tagged_s_is_snowfall_not_wind_speed() {
    let info = b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900s050wRSW";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::PositionWeather(w) => {
            // The whole point: the positional knots survive the `s` tag.
            assert_eq!(
                w.weather.wind_speed,
                Some(Speed::from_knots(4)),
                "the tagged `s` clobbered the positional wind speed"
            );
            assert_eq!(w.weather.wind_speed.map(Speed::mph), Some(5));
            assert_eq!(w.weather.wind_direction, Some(220));
            // ...and the snow is decoded rather than discarded: three
            // digits of whole inches, not the hundredths `r`/`p`/`P` use.
            assert_eq!(
                w.weather.snowfall,
                Some(Rainfall::from_hundredths_inch(5_000))
            );
            assert_eq!(w.weather.snowfall.map(Rainfall::millimeters), Some(1270));
            // The neighbours are untouched.
            assert_eq!(w.weather.gust, Some(Speed::from_mph(5)));
            assert_eq!(
                w.weather.temperature,
                Some(Temperature::from_fahrenheit(77))
            );
            assert_eq!(
                w.weather.barometric_pressure.map(Pressure::tenths_hpa),
                Some(9900)
            );
            assert_eq!(w.rest, b"wRSW");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Byte-exact rebuild, which is what an igate relaying this needs:
    // the snow field goes back where chapter 12's extra parameters live,
    // after the nine standard fields.
    let mut buf = [0u8; 96];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);
}

/// The regression a careless fix would cause: a **positionless** report
/// has no positional wind field, so its `sNNN` really is the sustained
/// one-minute wind speed, in miles per hour.
#[test]
fn positionless_weather_tagged_s_is_still_wind_speed_in_mph() {
    let info = b"_10090556c220s050g005t077";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Weather(w) => {
            assert_eq!(w.weather.wind_speed, Some(Speed::from_mph(50)));
            assert_eq!(w.weather.wind_speed.map(Speed::mph), Some(50));
            // Not knots: 50 mph is 43 knots, and the difference is the
            // whole reason the layout has to be named.
            assert_ne!(w.weather.wind_speed, Some(Speed::from_knots(50)));
            assert_eq!(w.weather.snowfall, None, "no snow field in this layout");
            assert_eq!(w.weather.wind_direction, Some(220));
            assert_eq!(w.weather.gust, Some(Speed::from_mph(5)));
        }
        other => panic!("wrong variant: {other:?}"),
    }

    // And the same three digits in the *other* layout are 50 inches of
    // snow, leaving that layout's wind alone. One wire spelling, two
    // measurements, decided by the layout and nothing else.
    let complete = PositionWeather::parse(b"!4903.50N/07201.75W_220/004s050").unwrap();
    assert_eq!(complete.weather.wind_speed, Some(Speed::from_knots(4)));
    assert_eq!(
        complete.weather.snowfall,
        Some(Rainfall::from_hundredths_inch(5_000))
    );
}

/// The half of the `s` defect that gating on the **layout** left live:
/// chapter 12's `ssss` slot is consumed *once*, so a positionless
/// report's **second** `s` is snowfall exactly as a Complete report's
/// first one is.
///
/// MEASURED against the code before this fix, on the wire bytes below:
/// `wind_speed` came back as **12 mph** (overwriting the 4 mph the
/// standard block had already read correctly), `snowfall` as `None`,
/// and the rebuild was
/// `_10090556c220s012g005t077r000p000P000h50b09900wRSW` — a frame that
/// lies about the wind and has dropped the snow. The independent
/// decoder reads the same bytes as "wind 4.0 mph …
/// 12.0 snow in 24 hours". There are **0 such frames in the corpus**,
/// so no ratchet could ever have seen it.
#[test]
fn positionless_weather_second_tagged_s_is_snowfall() {
    let info = b"_10090556c220s004g005t077r000p000P000h50b09900s012wRSW";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Weather(w) => {
            // The whole point: the standard block's mph survives the
            // second `s` tag.
            assert_eq!(
                w.weather.wind_speed,
                Some(Speed::from_mph(4)),
                "the second `s` clobbered the standard block's wind speed"
            );
            assert_eq!(w.weather.wind_direction, Some(220));
            // ...and the snow is decoded rather than discarded: three
            // digits of whole inches, not the hundredths `r`/`p`/`P` use.
            assert_eq!(
                w.weather.snowfall,
                Some(Rainfall::from_hundredths_inch(1_200))
            );
            // 12 inches is 304.8 mm, which rounds half away from zero.
            assert_eq!(w.weather.snowfall.map(Rainfall::millimeters), Some(305));
            // The neighbours are untouched.
            assert_eq!(w.weather.gust, Some(Speed::from_mph(5)));
            assert_eq!(
                w.weather.temperature,
                Some(Temperature::from_fahrenheit(77))
            );
            assert_eq!(w.weather.humidity.map(Humidity::percent), Some(50));
            assert_eq!(
                w.weather.barometric_pressure.map(Pressure::tenths_hpa),
                Some(9900)
            );
            assert_eq!(w.rest, b"wRSW");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Byte-exact rebuild, which is what an igate relaying this needs.
    let mut buf = [0u8; 96];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);

    // The Complete-layout sibling, unchanged: there the slot is spent
    // positionally, so the *first* `s` is already the snow.
    let complete =
        PositionWeather::parse(b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900s012wRSW")
            .unwrap();
    assert_eq!(complete.weather.wind_speed, Some(Speed::from_knots(4)));
    assert_eq!(
        complete.weather.snowfall,
        Some(Rainfall::from_hundredths_inch(1_200))
    );

    // An explicitly absent wind (`s...`) spends the slot just the same,
    // which is why the flag cannot be `wind_speed.is_some()`.
    let dotted = PositionlessWeather::parse(b"_10090556c...s...g...t...s012").unwrap();
    assert_eq!(dotted.weather.wind_speed, None);
    assert_eq!(
        dotted.weather.snowfall,
        Some(Rainfall::from_hundredths_inch(1_200))
    );
}

/// There **is** a byte-exact spelling for snowfall in the positionless
/// layout — a second `sNNN` after the standard block, exactly where the
/// reference reads it — so `build` emits it instead of refusing.
///
/// It used to return `BadWeatherValue { field: b's', got: 50 }` on the
/// grounds that this layout had nowhere to put the field. That was the
/// same mistake as defect 1, one layer up: the slot is spent by the
/// standard block's own `s`, and everything after it is free.
#[test]
fn positionless_weather_snowfall_builds_and_round_trips() {
    let report = PositionlessWeather {
        month: 10,
        day: 9,
        hour: 5,
        minute: 56,
        weather: WeatherReport {
            wind_speed: Some(Speed::from_mph(4)),
            snowfall: Some(Rainfall::from_hundredths_inch(5_000)),
            ..WeatherReport::default()
        },
        rest: b"wRSW",
    };
    let mut buf = [0u8; 96];
    let len = report.build(&mut buf).unwrap();
    assert_eq!(
        &buf[..len],
        // Absent fields are omitted rather than dotted; see the note
        // on `positionless_weather_all_missing_round_trip`.
        b"_10090556s004s050wRSW"
    );
    assert_eq!(len, report.encoded_len());
    assert_eq!(PositionlessWeather::parse(&buf[..len]), Ok(report));

    // The extra field costs nothing when it is absent: a snow-free
    // report does not grow a dotted `s...`, which is what keeps every
    // report the crate has ever emitted byte-exact.
    let bare = PositionlessWeather {
        weather: WeatherReport {
            snowfall: None,
            ..report.weather
        },
        ..report
    };
    let bare_len = bare.build(&mut buf).unwrap();
    assert_eq!(
        &buf[..bare_len],
        // Absent fields are omitted rather than dotted; see the note
        // on `positionless_weather_all_missing_round_trip`.
        b"_10090556s004wRSW"
    );

    // Boundaries, both layouts, byte for byte.
    for inches in [0u32, 1, 999] {
        let weather = WeatherReport {
            snowfall: Some(Rainfall::from_hundredths_inch(
                i32::try_from(inches).unwrap() * 100,
            )),
            ..WeatherReport::default()
        };
        let positionless = PositionlessWeather::new(1, 1, 0, 0, weather).unwrap();
        let len = positionless.build(&mut buf).unwrap();
        assert_eq!(PositionlessWeather::parse(&buf[..len]), Ok(positionless));
        let complete = PositionWeather::new(lat(0), lon(0), weather);
        let len = complete.build(&mut buf).unwrap();
        assert_eq!(PositionWeather::parse(&buf[..len]), Ok(complete));
    }

    // Out of range is still refused, in both layouts and with the same
    // error: a *field* is three digits even though a depth is not.
    let blizzard = WeatherReport {
        snowfall: Some(Rainfall::from_hundredths_inch(100_000)),
        ..WeatherReport::default()
    };
    let want = Err(AprsError::BadWeatherValue {
        field: b's',
        got: 1000,
    });
    assert_eq!(
        PositionlessWeather::new(1, 1, 0, 0, blizzard)
            .unwrap()
            .build(&mut buf),
        want
    );
    assert_eq!(
        PositionWeather::new(lat(0), lon(0), blizzard).build(&mut buf),
        want
    );
}

/// A tagged `c` in a Complete Weather Report is **not** a wind
/// direction: the same chapter 12 sentence that retires `ssss` retires
/// `cccc`, and unlike `s` the spec gives `c` no second meaning — the
/// "other parameters" list has exactly four entries (`L`, `l`, `s`,
/// `#`). So the scan ends and the bytes reach the caller verbatim.
///
/// MEASURED before the fix, and **visible on the wire** contrary to what
/// the conformance notes claimed: `build` skips the `c` *tag* but then
/// writes `wind_direction` into the positional `DDD` field, so
/// `!4903.50N/07201.75W_220/004c123g005t077` rebuilt as
/// `!4903.50N/07201.75W_123/004g005t077r...` — 220 became 123. The
/// independent decoder reads the input as "wind 4.6 mph, direction 220"
/// with `c123g005t077` left as comment text, which is exactly this.
#[test]
fn complete_weather_tagged_c_leaves_the_positional_wind_direction() {
    let info = b"!4903.50N/07201.75W_220/004c123g005t077";
    let packet = AprsPacket::parse(info).unwrap();
    let report = match packet {
        // Not an error to the caller: still a typed weather report.
        AprsPacket::PositionWeather(w) => w,
        other => panic!("wrong variant: {other:?}"),
    };
    assert_eq!(
        report.weather.wind_direction,
        Some(220),
        "the tagged `c` clobbered the positional wind direction"
    );
    assert_eq!(report.weather.wind_speed, Some(Speed::from_knots(4)));
    // Everything from the `c` on is uninterpreted, including the `g`
    // and `t` behind it: the scan ended, it did not skip one tag.
    assert_eq!(report.rest, b"c123g005t077");
    assert_eq!(report.weather.gust, None);
    assert_eq!(report.weather.temperature, None);

    // The wire half of the defect: the rebuilt frame must still say 220.
    let mut buf = [0u8; 96];
    let len = report.build(&mut buf).unwrap();
    assert!(
        buf[..len].starts_with(b"!4903.50N/07201.75W_220/004"),
        "{}",
        core::str::from_utf8(&buf[..len]).unwrap()
    );
    // The rebuild adds the dotted standard block the scan never reached
    // (the module's known build-is-not-wire-faithful limitation), but it
    // is a fixpoint: parsing it back gives the same report, and building
    // that gives the same bytes.
    let again = PositionWeather::parse(&buf[..len]).unwrap();
    assert_eq!(again.weather.wind_direction, Some(220));
    assert_eq!(again.rest, b"c123g005t077");
    let mut second = [0u8; 96];
    let second_len = again.build(&mut second).unwrap();
    assert_eq!(&second[..second_len], &buf[..len]);

    // The positionless layout still reads `c`: there `cccc` is the only
    // wind direction there is.
    let positionless = PositionlessWeather::parse(b"_10090556c123s004").unwrap();
    assert_eq!(positionless.weather.wind_direction, Some(123));
}

/// A Complete Weather Report whose first post-wind byte is an unknown
/// letter+digit must keep its typed weather report.
///
/// The trailer tolerance in `parse_tagged` only breaks out of the scan
/// once something has been parsed; otherwise it returns the error, and
/// `AprsPacket::parse` then degrades the whole frame to a plain
/// `Position`. In this layout the positional `DDD/SSS` block **is** a
/// successfully read field, so that guard was seeded wrong: MEASURED,
/// `!4903.50N/07201.75W_220/004X123` came back as
/// `Err(UnknownWeatherField { got: b'X' })` and the frame lost its
/// entire weather report over a manufacturer stamp — the same defect
/// that cost 38 corpus frames in the positionless layout, one layout
/// over.
#[test]
fn complete_weather_tolerates_a_trailer_before_any_tag() {
    let info = b"!4903.50N/07201.75W_220/004X123";
    let report = match AprsPacket::parse(info).unwrap() {
        AprsPacket::PositionWeather(w) => w,
        other => panic!("wrong variant: {other:?}"),
    };
    assert_eq!(report.weather.wind_direction, Some(220));
    assert_eq!(report.weather.wind_speed, Some(Speed::from_knots(4)));
    assert_eq!(report.rest, b"X123");

    // A *malformed* known tag in the same position is the same story.
    let report = PositionWeather::parse(b"!4903.50N/07201.75W_220/004t9x").unwrap();
    assert_eq!(report.weather.wind_direction, Some(220));
    assert_eq!(report.rest, b"t9x");

    // The positionless layout is unchanged: with nothing parsed yet, a
    // block that *starts* with a field we do not know is still broken.
    assert_eq!(
        PositionlessWeather::parse(b"_10090556q123c220s004"),
        Err(AprsError::UnknownWeatherField { got: b'q' })
    );
}

/// Luminosity was not just undecoded: because the tag scan stops at
/// the first byte it does not know, and chapter 12 puts `L` in the
/// middle of the block, one `L050` cost **four** downstream fields.
///
/// MEASURED before the fix, on the first vector below: `r` came back as
/// 0.00 in and `p`, `P`, `h` and `b` were **all** in `rest`
/// (`"L050p000P000h50b09900"`). The independent decoder reads the same
/// bytes as all three rain fields *plus* "50 watts/m^2" — nothing is
/// displaced, so the spec's parenthetical "(L is inserted in place of
/// one of the rain values)" is guidance about the fixed-width diagram
/// and means nothing to a tag scanner.
#[test]
fn weather_luminosity_recovers_the_rest_of_the_block() {
    let info = b"!4903.50N/07201.75W_220/004g005t077r000L050p000P000h50b09900";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::PositionWeather(w) => {
            assert_eq!(w.weather.luminosity, Some(50));
            // The four fields the scan used to abandon, plus the `r` it
            // read just before giving up.
            assert_eq!(w.weather.rain_1h.map(Rainfall::hundredths_inch), Some(0));
            assert_eq!(w.weather.rain_24h.map(Rainfall::hundredths_inch), Some(0));
            assert_eq!(
                w.weather.rain_midnight.map(Rainfall::hundredths_inch),
                Some(0)
            );
            assert_eq!(w.weather.humidity.map(Humidity::percent), Some(50));
            assert_eq!(
                w.weather.barometric_pressure.map(Pressure::tenths_hpa),
                Some(9900)
            );
            assert_eq!(w.rest, b"", "the block was abandoned at the `L`");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    // Byte-exact: chapter 12 puts the field among the rain values and
    // the reference's own reading keeps it directly after `r`, so that
    // is where the rebuild puts it back.
    let mut buf = [0u8; 96];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);

    // `l` is the same measurement 1000 higher, and which letter a value
    // spells is a total function of the value -- so nothing has to
    // remember which one arrived.
    let info = b"!4903.50N/07201.75W_220/004g005t077r000l050p000P000h50b09900";
    let high = PositionWeather::parse(info).unwrap();
    assert_eq!(high.weather.luminosity, Some(1050));
    let len = high.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);

    // Both layouts, both tags, the boundary values, byte for byte. The
    // `l` form's digits are 1000 less than the value, which is the only
    // place in this module where the wire integer is not the quantity.
    for (watts, spelling) in [
        (0u16, &b"L000"[..]),
        (1, &b"L001"[..]),
        (999, &b"L999"[..]),
        (1000, &b"l000"[..]),
        (1999, &b"l999"[..]),
    ] {
        let weather = WeatherReport {
            luminosity: Some(watts),
            ..WeatherReport::default()
        };
        let positionless = PositionlessWeather::new(10, 9, 5, 56, weather).unwrap();
        let len = positionless.build(&mut buf).unwrap();
        // Every other field is absent and therefore omitted, so the
        // luminosity is all that follows the timestamp. It still sits
        // where the `r` slot would have been, which is what this loop
        // is checking.
        let want = [&b"_10090556"[..], spelling].concat();
        assert_eq!(&buf[..len], want.as_slice(), "{watts}");
        assert_eq!(len, positionless.encoded_len());
        assert_eq!(
            PositionlessWeather::parse(&buf[..len]),
            Ok(positionless),
            "{watts}"
        );
        let complete = PositionWeather::new(lat(0), lon(0), weather);
        let len = complete.build(&mut buf).unwrap();
        // Absent fields are omitted, so the luminosity is all that
        // follows the positional wind block. It still lands where the
        // `r` slot would have been, which is the point of this loop.
        let want = [&b"!0000.00N/00000.00E_.../..."[..], spelling].concat();
        assert_eq!(&buf[..len], want.as_slice(), "{watts}");
        assert_eq!(len, complete.encoded_len());
        assert_eq!(PositionWeather::parse(&buf[..len]), Ok(complete), "{watts}");
    }

    // The positionless spelling, in full, with every optional field at
    // once: the standard nine, `L` among the rain values, and the extra
    // `s` after the barometer.
    let info = b"_10090556c220s004g005t077r000L050p000P000h50b09900s012wRSW";
    let both = PositionlessWeather::parse(info).unwrap();
    assert_eq!(both.weather.wind_speed, Some(Speed::from_mph(4)));
    assert_eq!(both.weather.luminosity, Some(50));
    assert_eq!(
        both.weather.snowfall,
        Some(Rainfall::from_hundredths_inch(1_200))
    );
    assert_eq!(both.rest, b"wRSW");
    let len = both.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);

    // A quantity is unbounded; a field is three digits plus the choice
    // of tag, so `l999` is the ceiling.
    let too_bright = WeatherReport {
        luminosity: Some(2000),
        ..WeatherReport::default()
    };
    assert_eq!(
        PositionWeather::new(lat(0), lon(0), too_bright).build(&mut buf),
        Err(AprsError::BadWeatherValue {
            field: b'L',
            got: 2000
        })
    );

    // The raw rain counter `#` is not implemented -- the spec gives it
    // no width, unit or scaling, and the reference does not decode it
    // either -- so it stays byte-exact in `rest`.
    let info = b"!4903.50N/07201.75W_220/004g005t077#123";
    let raw = PositionWeather::parse(info).unwrap();
    assert_eq!(raw.rest, b"#123");
}

/// Byte-exact `parse` → `build` for a Complete Weather Report carrying a
/// tagged `s`, in both the plain and the timestamped spelling.
///
/// A third case used to live here, `=4903.50N/07201.75W_.../...g...t...
/// r...p...P...h..b.....s999`, spelling every absent field with dots. It
/// moved to `dotted_absences_normalise_to_omission` below, because
/// absence is now written by leaving the field out. Both spellings are
/// legal (chapter 12: the parameters "may not even exist"), so this is
/// a choice between two valid forms and not a correctness question; the
/// reason for the choice is on `write_fields`.
#[test]
fn complete_weather_snowfall_round_trips_byte_for_byte() {
    for info in [
        &b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900s050"[..],
        &b"@092345z4903.50N/07201.75W_220/004g005t-07r000p000P000h50b09900s000wRSW"[..],
    ] {
        let parsed = PositionWeather::parse(info).unwrap();
        assert!(
            parsed.weather.snowfall.is_some(),
            "{}",
            core::str::from_utf8(info).unwrap()
        );
        let mut buf = [0u8; 96];
        let len = parsed.build(&mut buf).unwrap();
        assert_eq!(&buf[..len], info, "{}", core::str::from_utf8(info).unwrap());
        assert_eq!(len, parsed.encoded_len());
    }

    // Chapter 12 permits a decimal point ("A decimal point is allowed
    // for non-integer values"), which this crate does not parse: three
    // digits is the only spelling it writes. The scanner's trailer rule
    // then makes such a field *tolerated* rather than mis-read -- it
    // lands in `rest` whole, the neighbouring wind speed is untouched,
    // and the rebuild is still byte-exact.
    let info = b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900s0.5wRSW";
    let parsed = PositionWeather::parse(info).unwrap();
    assert_eq!(parsed.weather.wind_speed, Some(Speed::from_knots(4)));
    assert_eq!(parsed.weather.snowfall, None);
    assert_eq!(parsed.rest, b"s0.5wRSW");
    let mut buf = [0u8; 96];
    let len = parsed.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);

    // An explicitly *absent* snow field (`s...`) is the one spelling
    // that normalizes away: it decodes to `None`, and a report with no
    // snow does not grow a dotted field on rebuild -- which is what
    // keeps every snow-free Complete Weather Report byte-exact. The
    // wind, which is what the old code destroyed here, survives.
    let parsed =
        PositionWeather::parse(b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900s...")
            .unwrap();
    assert_eq!(parsed.weather.wind_speed, Some(Speed::from_knots(4)));
    assert_eq!(parsed.weather.snowfall, None);
    let len = parsed.build(&mut buf).unwrap();
    assert_eq!(
        &buf[..len],
        b"!4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900"
    );
}

#[test]
fn position_with_weather_rejections() {
    // Missing '/' between wind direction and speed.
    assert_eq!(
        PositionWeather::parse(b"!4903.50N/07201.75W_220x004g005"),
        Err(AprsError::ExpectedByte {
            expected: b'/',
            got: b'x',
            position: 23
        })
    );
    // Symbol code is not '_'.
    assert!(PositionWeather::parse(b"!4903.50N/07201.75W-220/004").is_err());
    // A '-' symbol code parses as a plain position, not weather.
    match AprsPacket::parse(b"!4903.50N/07201.75W-220/004") {
        Ok(AprsPacket::Position(_)) => {}
        other => panic!("wrong variant: {other:?}"),
    }
}

// -------------------------------------------------------------- telemetry

#[test]
fn telemetry_spec_vector_round_trip() {
    // Chapter 13 example values.
    let info = b"T#005,199,000,255,073,123,01101001";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Telemetry(t) => {
            assert_eq!(t.seq, 5);
            assert_eq!(
                t.analog,
                Telemetry::integer_channels([199, 0, 255, 73, 123])
            );
            assert_eq!(
                t.digital,
                Some([false, true, true, false, true, false, false, true])
            );
            assert_eq!(t.rest, b"");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 64];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);
}

#[test]
fn telemetry_boundary_sequences_round_trip() {
    // Chapter 13's three-digit range, and the wider forms real
    // trackers emit: 88 captured reports use four digits and 16 use
    // five, so a fixed three-digit build would report 1812 as 812.
    for seq in [0u32, 1, 999, 1000, 1812, 46_144, 99_999] {
        let t = Telemetry {
            seq,
            analog: Telemetry::integer_channels([0, 255, 128, 1, 254]),
            digital: Some([true; 8]),
            rest: b"comment",
        };
        let mut buf = [0u8; 64];
        let len = t.build(&mut buf).unwrap();
        assert_eq!(Telemetry::parse(&buf[..len]), Ok(t), "{seq}");
    }
}

#[test]
fn telemetry_rejections() {
    // Non-numeric sequence (the MIC form is unsupported).
    assert_eq!(
        Telemetry::parse(b"T#MIC,199,000,255,073,123,01101001"),
        Err(AprsError::BadTelemetrySequence { got: b'M' })
    );
    // Non-digit analog byte.
    assert_eq!(
        Telemetry::parse(b"T#005,1x9,000,255,073,123,01101001"),
        Err(AprsError::BadDigit {
            got: b'x',
            position: 7
        })
    );
    // NOT an error any more: chapter 13 caps an analog channel at 255,
    // but 1 724 captured reports carry an ordinary value above it, and
    // the value type now holds them.
    assert_eq!(
        Telemetry::parse(b"T#005,256,000,255,073,123,01101001").map(|t| t.analog[0]),
        Ok(Some(TelemetryValue::integer(256)))
    );
    // Still an error: a mantissa past i64 is a number this crate
    // cannot hold, and clamping it would publish a reading the sender
    // never made.
    assert_eq!(
        Telemetry::parse(b"T#005,99999999999999999999,000"),
        Err(AprsError::BadAnalogValue { position: 6 })
    );
    // A missing comma glues the last analog channel to the digital
    // field, and `x` is then a non-digit inside an analog value. The
    // fixed-width parser reported a missing comma; a comma-splitting
    // one has nothing that says where the field should have ended.
    assert_eq!(
        Telemetry::parse(b"T#005,199,000,255,073,12301101001x"),
        Err(AprsError::BadDigit {
            got: b'x',
            position: 33
        })
    );
    // Bad digital bit.
    assert_eq!(
        Telemetry::parse(b"T#005,199,000,255,073,123,01101002"),
        Err(AprsError::BadDigitalBit {
            got: b'2',
            position: 33
        })
    );
    // NOT truncated any more: one analog channel and no digital field
    // is a shape chapter 13 permits and 142 captured reports use.
    assert!(Telemetry::parse(b"T#005,199").is_ok());
    // Truncated: more analog fields than there are slots.
    assert_eq!(
        Telemetry::parse(b"T#005,1,2,3,4,5,6,7"),
        Err(AprsError::Truncated {
            expected: 5,
            got: 7
        })
    );
    // Build: sequence out of range. Five digits is the widest field
    // seen on the air, so 1000 now builds and a millionth does not.
    let t = Telemetry {
        seq: 1_000_000,
        analog: Telemetry::integer_channels([0; 5]),
        digital: Some([false; 8]),
        rest: b"",
    };
    let mut buf = [0u8; 64];
    assert_eq!(
        t.build(&mut buf),
        Err(AprsError::TelemetrySequenceOutOfRange { got: 1_000_000 })
    );
}

// ----------------------------------------------------------------- object

#[test]
fn object_spec_vector_round_trip() {
    // Chapter 11-style live object with DHM zulu timestamp.
    let info = b";LEADER   *092345z4903.50N/07201.75W>088/036";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Object(o) => {
            assert_eq!(o.name, b"LEADER");
            assert!(o.live);
            assert_eq!(
                o.timestamp,
                Timestamp::DhmZulu {
                    day: 9,
                    hour: 23,
                    minute: 45
                }
            );
            assert_eq!(o.latitude, lat(49 * 6000 + 350));
            assert_eq!(o.longitude, lon(-(72 * 6000 + 175)));
            assert_eq!(o.symbol.to_wire(), (b'/', b'>'));
            assert_eq!(o.comment, b"088/036");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 80];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);
}

#[test]
fn object_killed_and_timestamp_formats_round_trip() {
    let stamps = [
        Timestamp::DhmZulu {
            day: 1,
            hour: 0,
            minute: 0,
        },
        Timestamp::DhmLocal {
            day: 31,
            hour: 23,
            minute: 59,
        },
        Timestamp::Hms {
            hour: 23,
            minute: 59,
            second: 59,
        },
    ];
    for (live, timestamp) in [(true, stamps[0]), (false, stamps[1]), (true, stamps[2])] {
        let o = Object {
            ambiguity: Ambiguity::EXACT,
            name: b"WX GATE 9",
            live,
            timestamp,
            latitude: lat(0),
            longitude: lon(0),
            symbol: Symbol::from_wire(b'\\', b'-'),
            comment: b"note",
        };
        let mut buf = [0u8; 80];
        let len = o.build(&mut buf).unwrap();
        assert_eq!(Object::parse(&buf[..len]), Ok(o), "{timestamp:?}");
    }
}

#[test]
fn object_single_char_name_pads_to_nine() {
    let o = Object {
        ambiguity: Ambiguity::EXACT,
        name: b"X",
        live: false,
        timestamp: Timestamp::Hms {
            hour: 1,
            minute: 2,
            second: 3,
        },
        latitude: lat(6000),
        longitude: lon(-6000),
        symbol: Symbol::from_wire(b'/', b'c'),
        comment: b"",
    };
    let mut buf = [0u8; 80];
    let len = o.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b";X        _010203h0100.00N/00100.00Wc");
    assert_eq!(Object::parse(&buf[..len]), Ok(o));
}

#[test]
fn object_rejections() {
    // Bad live/killed byte.
    assert_eq!(
        Object::parse(b";LEADER   ?092345z4903.50N/07201.75W>"),
        Err(AprsError::BadLiveKilled { got: b'?' })
    );
    // Unknown timestamp format letter.
    assert_eq!(
        Object::parse(b";LEADER   *092345x4903.50N/07201.75W>"),
        Err(AprsError::BadTimestamp {
            field: b'?',
            got: i32::from(b'x')
        })
    );
    // Out-of-range timestamp day.
    assert_eq!(
        Object::parse(b";LEADER   *322345z4903.50N/07201.75W>"),
        Err(AprsError::BadTimestamp {
            field: b'D',
            got: 32
        })
    );
    // All-space name.
    assert_eq!(
        Object::parse(b";         *092345z4903.50N/07201.75W>"),
        Err(AprsError::NameLengthInvalid {
            len: 0,
            min: 1,
            max: 9
        })
    );
    // Non-printable name byte.
    assert_eq!(
        Object::parse(b";LEAD\x07R   *092345z4903.50N/07201.75W>"),
        Err(AprsError::BadNameChar {
            got: 0x07,
            position: 5
        })
    );
    // Truncated.
    assert_eq!(
        Object::parse(b";LEADER   *0923"),
        Err(AprsError::Truncated {
            expected: 37,
            got: 15
        })
    );
    // Build: name too long.
    let o = Object {
        ambiguity: Ambiguity::EXACT,
        name: b"TOOLONGNAME",
        live: true,
        timestamp: Timestamp::DhmZulu {
            day: 1,
            hour: 0,
            minute: 0,
        },
        latitude: lat(0),
        longitude: lon(0),
        symbol: Symbol::HOUSE,
        comment: b"",
    };
    let mut buf = [0u8; 80];
    assert_eq!(
        o.build(&mut buf),
        Err(AprsError::NameLengthInvalid {
            len: 11,
            min: 1,
            max: 9
        })
    );
}

// ------------------------------------------------------------------- item

#[test]
fn item_spec_vector_round_trip() {
    // Chapter 11-style live item.
    let info = b")AID #2!4903.50N/07201.75WA";
    let packet = AprsPacket::parse(info).unwrap();
    match packet {
        AprsPacket::Item(i) => {
            assert_eq!(i.name, b"AID #2");
            assert!(i.live);
            assert_eq!(i.latitude, lat(49 * 6000 + 350));
            assert_eq!(i.longitude, lon(-(72 * 6000 + 175)));
            assert_eq!(i.symbol.to_wire(), (b'/', b'A'));
            assert_eq!(i.comment, b"");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let mut buf = [0u8; 80];
    let len = packet.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], &info[..]);
}

#[test]
fn item_name_length_boundaries_round_trip() {
    for (name, live) in [
        (&b"AAA"[..], true),
        (&b"NINECHARS"[..], false),
        (&b"MID-5"[..], true),
    ] {
        let item = Item {
            ambiguity: Ambiguity::EXACT,
            name,
            live,
            latitude: lat(-(89 * 6000 + 5999)),
            longitude: lon(179 * 6000 + 5999),
            symbol: Symbol::from_wire(b'/', b'r'),
            comment: b"comment text",
        };
        let mut buf = [0u8; 80];
        let len = item.build(&mut buf).unwrap();
        assert_eq!(Item::parse(&buf[..len]), Ok(item), "{name:?}");
    }
}

#[test]
fn item_killed_round_trip() {
    let item = Item {
        ambiguity: Ambiguity::EXACT,
        name: b"W7 NWS",
        live: false,
        latitude: lat(45 * 6000),
        longitude: lon(-(122 * 6000)),
        symbol: Symbol::from_wire(b'\\', b'W'),
        comment: b"",
    };
    let mut buf = [0u8; 80];
    let len = item.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b")W7 NWS_4500.00N\\12200.00WW");
    assert_eq!(Item::parse(&buf[..len]), Ok(item));
}

#[test]
fn item_rejections() {
    // Name too short (2 chars).
    assert_eq!(
        Item::parse(b")AB!4903.50N/07201.75WA"),
        Err(AprsError::NameLengthInvalid {
            len: 2,
            min: 3,
            max: 9
        })
    );
    // Name too long (no terminator within 9 chars).
    assert_eq!(
        Item::parse(b")ABCDEFGHIJ!4903.50N/07201.75WA"),
        Err(AprsError::NameLengthInvalid {
            len: 10,
            min: 3,
            max: 9
        })
    );
    // Non-printable name byte.
    assert_eq!(
        Item::parse(b")AB\x01!4903.50N/07201.75WA"),
        Err(AprsError::BadNameChar {
            got: 0x01,
            position: 3
        })
    );
    // Truncated position.
    assert_eq!(
        Item::parse(b")AID #2!4903.50N"),
        Err(AprsError::Truncated {
            expected: 27,
            got: 16
        })
    );
    // Build: name with a terminator byte.
    let item = Item {
        ambiguity: Ambiguity::EXACT,
        name: b"AB_",
        live: true,
        latitude: lat(0),
        longitude: lon(0),
        symbol: Symbol::HOUSE,
        comment: b"",
    };
    let mut buf = [0u8; 80];
    assert_eq!(
        item.build(&mut buf),
        Err(AprsError::BadNameChar {
            got: b'_',
            position: 2
        })
    );
    // Build: name too short.
    let short = Item {
        name: b"AB",
        ..item
    };
    assert_eq!(
        short.build(&mut buf),
        Err(AprsError::NameLengthInvalid {
            len: 2,
            min: 3,
            max: 9
        })
    );
}

// ------------------------------------------------- 1.1 reply-ACK ids
//
// Chapter 14, "New Message Number Format" (December 1999), gives the
// message id an internal structure: "the format for the line number for
// outgoing message numbers is `{MM}AA` where MM is the outgoing message
// number and AA is the 'free ACK' if needed. If no ACK is pending, then
// the message # is `{MM}`."
//
// The parser has always *tolerated* this — `MM}AA` is five ordinary id
// bytes and fits the existing length rule — so nothing about the wire
// handling changes here and every vector below asserts a byte-exact
// rebuild to prove it. What is new is reading the two halves back out.

/// Parses `info` as a message, checks both accessors, and asserts that
/// rebuilding it reproduces `info` byte for byte — through
/// [`Message`] directly and through [`AprsPacket`], which is the path a
/// receiver takes.
#[track_caller]
fn reply_ack_vector(info: &[u8], reply_ack: Option<(&[u8], &[u8])>, acked: Option<&[u8]>) {
    let msg = Message::parse(info).unwrap();
    assert_eq!(msg.content.reply_ack(), reply_ack, "reply_ack of {info:?}");
    assert_eq!(
        msg.content.acked_number(),
        acked,
        "acked_number of {info:?}"
    );

    let mut buf = [0u8; 128];
    let len = msg.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], info, "Message rebuild is not byte-exact");

    let packet = match AprsPacket::parse(info).unwrap() {
        AprsPacket::Message(m) => m,
        other => panic!("wrong variant: {other:?}"),
    };
    assert_eq!(packet, msg);
    let len = AprsPacket::Message(packet).build(&mut buf).unwrap();
    assert_eq!(&buf[..len], info, "AprsPacket rebuild is not byte-exact");
}

#[test]
fn reply_ack_splits_the_chapter_14_forms() {
    // "{MM}AA where MM is the outgoing message number and AA is the
    // 'free ACK' if needed."
    reply_ack_vector(
        b":WA6LDQ   :Okay{Re}1j",
        Some((b"Re", b"1j")),
        // Text acknowledges nothing on its own; the free ACK is the
        // second half of `reply_ack`, not an `ack` frame.
        None,
    );

    // "If no ACK is pending, then the message # is {MM}" — and "even if
    // there is no ACK, the presence of the trailing } tells the other
    // end that the sender is REPLY-ACK capable". So the trailing `}`
    // must survive as an empty second half, not vanish.
    reply_ack_vector(b":WA6LDQ   :Okay{Re}", Some((b"Re", b"")), None);

    // A plain 1.01 id has no internal structure at all.
    reply_ack_vector(b":WA6LDQ   :Okay{003", None, None);

    // No id: nothing to report either way.
    reply_ack_vector(b":WA6LDQ   :Okay", None, None);
}

#[test]
fn acked_number_pulls_the_mm_out_of_ack_and_rej() {
    // "When you receive a message line XXX.., send the normal existing
    // ackXXX.. This algorithm is unchanged. Even if XXX.. is MM}AA then
    // the ack is just the exact copy as before ackMM}AA." — so the id
    // stays whole on the wire...
    reply_ack_vector(
        b":WA6UVQ   :ackRe}1j",
        // ...and still reads as a reply-ACK, being a copy of one.
        Some((b"Re", b"1j")),
        // "...you must pull out the MM here and use IT to match with the
        // outstanding {MM} in your outgoing message queue."
        Some(b"Re"),
    );

    // The 1.01 spelling: the whole id is the number being acked.
    reply_ack_vector(b":WA6UVQ   :ack003", None, Some(b"003"));

    // Rejections carry the identical id field and get identical
    // treatment; the chapter draws no distinction.
    reply_ack_vector(b":WA6UVQ   :rejRe}1j", Some((b"Re", b"1j")), Some(b"Re"));
    reply_ack_vector(b":WA6UVQ   :rej003", None, Some(b"003"));
    reply_ack_vector(b":WA6UVQ   :rej9", None, Some(b"9"));

    // The bare capability marker, acked verbatim: MM with an empty AA.
    reply_ack_vector(b":WA6UVQ   :ackRe}", Some((b"Re", b"")), Some(b"Re"));
}

#[test]
fn reply_ack_decodes_the_off_air_qso() {
    // Both halves of one real QSO from the off-air corpus (four
    // repetitions of each frame). This is the only reply-ACK traffic in
    // the corpus, and it happens to exercise both spec forms.
    let outgoing = b":WA6LDQ   :Okay will do soon, uboth hav a happy Thanksgiving{Re}1j";
    reply_ack_vector(outgoing, Some((b"Re", b"1j")), None);

    let reply = b":WA6UVQ   :ackRe}1j";
    reply_ack_vector(reply, Some((b"Re", b"1j")), Some(b"Re"));

    // Spelled out: WA6UVQ sent its message number "Re" while
    // acknowledging WA6LDQ's "1j"; WA6LDQ's reply acknowledges "Re".
    // The independent reference decoder reads these frames the same way.
    let sent = Message::parse(outgoing).unwrap();
    assert_eq!(sent.addressee.as_bytes(), b"WA6LDQ");
    let (number, free_ack) = sent.content.reply_ack().unwrap();
    assert_eq!((number, free_ack), (&b"Re"[..], &b"1j"[..]));
    assert_eq!(
        sent.content,
        MessageContent::Text {
            text: b"Okay will do soon, uboth hav a happy Thanksgiving",
            id: Some(b"Re}1j"),
        }
    );

    let answer = Message::parse(reply).unwrap();
    assert_eq!(answer.addressee.as_bytes(), b"WA6UVQ");
    assert_eq!(answer.content.acked_number(), Some(number));
}

#[test]
fn reply_ack_degenerate_ids_round_trip_and_never_panic() {
    // `check_id` admits any 1..=5 bytes that contain no `{`, so all of
    // these reach the accessors from the wire. None may panic and all
    // must rebuild byte for byte.

    // An id of exactly `}`: both halves empty.
    reply_ack_vector(b":WA6LDQ   :x{}", Some((b"", b"")), None);
    // ...and acked, where the empty number matches nothing outstanding.
    reply_ack_vector(b":WA6LDQ   :ack}", Some((b"", b"")), Some(b""));

    // `}` first.
    reply_ack_vector(b":WA6LDQ   :x{}1j", Some((b"", b"1j")), None);
    // `}` last, at the full five-byte id length.
    reply_ack_vector(b":WA6LDQ   :x{abcd}", Some((b"abcd", b"")), None);

    // Two `}`: the split is at the first, the rest is returned verbatim.
    reply_ack_vector(b":WA6LDQ   :x{a}b}c", Some((b"a", b"b}c")), None);
    reply_ack_vector(b":WA6LDQ   :acka}b}c", Some((b"a", b"b}c")), Some(b"a"));

    // Non-ASCII: the scan is over bytes, so a multi-byte character
    // neither hides nor fabricates a `}`.
    reply_ack_vector(
        b":WA6LDQ   :x{\xc3\xa9}\xc3\xa9",
        Some((b"\xc3\xa9", b"\xc3\xa9")),
        None,
    );
    reply_ack_vector(b":WA6LDQ   :x{\xff\xfe", None, None);
    reply_ack_vector(b":WA6LDQ   :ack\xff\xfe", None, Some(b"\xff\xfe"));

    // An empty id never reaches the accessors from the wire: a
    // trailing brace with nothing behind it is not an identifier, so
    // the brace stays in the text and the id is absent.
    assert_eq!(
        Message::parse(b":WA6LDQ   :x{").map(|m| m.content),
        Ok(MessageContent::Text {
            text: b"x{",
            id: None
        })
    );
    // ...but the enum is public, so hand-built content must still
    // answer rather than panic.
    let empty_text = MessageContent::Text {
        text: b"x",
        id: Some(b""),
    };
    assert_eq!(empty_text.reply_ack(), None);
    assert_eq!(empty_text.acked_number(), None);
    let empty_ack = MessageContent::Ack { id: b"" };
    assert_eq!(empty_ack.reply_ack(), None);
    assert_eq!(empty_ack.acked_number(), Some(&b""[..]));
}

#[test]
fn reply_ack_needs_no_build_support() {
    // The pin behind the whole design: a reply-ACK id is emitted by the
    // unchanged builder, because it was never taken apart. Constructing
    // the id as the opaque slice it is produces the chapter-14 bytes.
    let msg = Message {
        addressee: Addressee::new(b"WA6LDQ").unwrap(),
        content: MessageContent::Text {
            text: b"Okay",
            id: Some(b"Re}1j"),
        },
    };
    let mut buf = [0u8; 64];
    let len = msg.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b":WA6LDQ   :Okay{Re}1j");
    assert_eq!(msg.encoded_len(), len);

    // The bare capability marker is one byte longer than its number,
    // and the builder has no say in whether the `}` is there.
    let capable = Message {
        content: MessageContent::Text {
            text: b"Okay",
            id: Some(b"Re}"),
        },
        ..msg
    };
    let len = capable.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b":WA6LDQ   :Okay{Re}");

    // And the ack is "just the exact copy as before": no `{`, no
    // reassembly, five id bytes.
    let ack = Message {
        content: MessageContent::Ack { id: b"Re}1j" },
        ..msg
    };
    let len = ack.build(&mut buf).unwrap();
    assert_eq!(&buf[..len], b":WA6LDQ   :ackRe}1j");

    // The reply-ACK form uses the same five-byte budget as a plain id,
    // so the length rule is untouched in both directions.
    let over = Message {
        content: MessageContent::Ack { id: b"Ree}1j" },
        ..msg
    };
    assert_eq!(
        over.build(&mut buf),
        Err(AprsError::MessageIdLengthInvalid { len: 6 })
    );
    // On the parse side the same over-long id is not an error but a
    // non-identifier: six characters cannot be one, so the brace is
    // text. The build side above still rejects it, because there the
    // caller asked for an id that cannot be spelled.
    assert_eq!(
        Message::parse(b":WA6LDQ   :Okay{Ree}1j").map(|m| m.content),
        Ok(MessageContent::Text {
            text: b"Okay{Ree}1j",
            id: None
        })
    );
}

/// A sender who spells an absent field with dots gets it back omitted.
///
/// The wire has **three** states per weather field (a value, a dotted
/// "no data", and absent entirely) and `Option<T>` has two, so parsing
/// cannot tell the last two apart and building has to pick one. Both
/// are legal: chapter 12 says the parameters "may not even exist".
///
/// Omission is the one chosen, and this test is the cost of that
/// choice, recorded rather than hidden. What is *not* acceptable is
/// losing the values, so that is what is asserted: the report survives
/// the round trip, and the rebuild parses back to exactly the same
/// value even though the bytes are shorter.
///
/// The benefit bought is on the other side of the ledger and is not
/// cosmetic. A placeholder run is written *before* `rest`, so on any
/// report whose tag scan stopped early the rebuild inserts synthetic
/// bytes in the middle of content the sender did send. MEASURED over
/// 64 918 live packets: 1 308 weather reports have both a non-empty
/// `rest` and at least one absent standard field, and on one of them a
/// four-character temperature turned 53 bytes into 74 with five tags
/// appearing twice. Omission cannot lengthen a packet, so it cannot do
/// that.
#[test]
fn dotted_absences_normalise_to_omission() {
    let info = &b"=4903.50N/07201.75W_.../...g...t...r...p...P...h..b.....s999"[..];
    let parsed = PositionWeather::parse(info).expect("a valid report");
    assert_eq!(
        parsed.weather.snowfall.map(Rainfall::hundredths_inch),
        Some(99_900),
        "the snowfall is the only measurement this report carries"
    );

    let mut buf = [0u8; 96];
    let len = parsed.build(&mut buf).expect("building");
    assert_eq!(
        &buf[..len],
        &b"=4903.50N/07201.75W_.../...s999"[..],
        "absent fields are omitted, not dotted"
    );
    assert_eq!(
        len,
        parsed.encoded_len(),
        "encoded_len must track the build"
    );

    // The bytes are shorter and the meaning is identical: semantic
    // idempotence holds even where byte fidelity does not, and that is
    // the property that matters.
    assert_eq!(
        PositionWeather::parse(&buf[..len]),
        Ok(parsed),
        "the rebuild must parse back to the same value"
    );
    assert!(
        len < info.len(),
        "omission can only shorten: {len} vs {}",
        info.len()
    );
}
