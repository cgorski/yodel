//! Unit-conversion laws for `yodel::units`.
//!
//! # Why the known-answer tests come first
//!
//! A round-trip test **cannot** catch a wrong conversion factor.
//! `Distance::from_feet(n).feet() == n` passes for any internally
//! consistent factor, including 305 000 µm per foot. That is not a
//! hypothetical failure mode: this crate shipped an IL2P implementation
//! that could not exchange a single frame with any other station while
//! every one of its round-trip tests passed, because an encoder and a
//! decoder that are mutual inverses stay mutual inverses when a shared
//! constant is wrong (see `docs/APRS_CONFORMANCE.md` §6.1).
//!
//! So every quantity is tested twice, and in this order:
//!
//! 1. **Known answers** against the published unit definitions, with the
//!    canonical stored value written out so a changed factor fails
//!    loudly rather than cancelling itself out.
//! 2. **Round trips and properties**, which catch a different class of
//!    bug — asymmetric rounding, saturation, sign handling.
//!
//! Several vectors are ones any reader can check without this crate:
//! 100 °C is 212 °F, 32 °F is 0 °C, one standard atmosphere is
//! 1013.2 hPa and 29.92 inHg.

use yodel::units::{
    Bearing, CompassPoint, Distance, Humidity, Power, Pressure, Rainfall, Speed, Temperature,
    UnitError,
};

// ---------------------------------------------------------------- Distance

#[test]
fn distance_known_answers() {
    // (constructor result, canonical micrometers, feet, meters)
    let cases: &[(Distance, i64, i32, i32)] = &[
        (Distance::from_feet(0), 0, 0, 0),
        (Distance::from_feet(1), 304_800, 1, 0),
        (Distance::from_feet(1234), 376_123_200, 1234, 376),
        (
            Distance::from_feet(999_999),
            304_799_695_200,
            999_999,
            304_800,
        ),
        (Distance::from_meters(376), 376_000_000, 1234, 376),
        (
            Distance::from_meters(743_570),
            743_570_000_000,
            2_439_534,
            743_570,
        ),
    ];
    for &(distance, micrometers, feet, meters) in cases {
        assert_eq!(
            distance.micrometers(),
            micrometers,
            "{distance:?} canonical"
        );
        assert_eq!(distance.feet(), feet, "{distance:?} feet");
        assert_eq!(distance.meters(), meters, "{distance:?} meters");
    }
}

#[test]
fn distance_factors_match_the_published_definitions() {
    // A foot is defined as exactly 0.3048 m; a nautical mile as 1852 m;
    // a statute mile as 5280 ft; an inch as 25.4 mm.
    assert_eq!(Distance::from_feet(1).micrometers(), 304_800);
    assert_eq!(Distance::from_meters(1).micrometers(), 1_000_000);
    assert_eq!(Distance::from_kilometers(1).micrometers(), 1_000_000_000);
    assert_eq!(
        Distance::from_nautical_miles(1).micrometers(),
        1_852_000_000
    );
    assert_eq!(Distance::from_statute_miles(1).micrometers(), 1_609_344_000);
    assert_eq!(Distance::from_inches(1).micrometers(), 25_400);

    // Cross-checks a reader can verify independently.
    assert_eq!(Distance::from_statute_miles(1).feet(), 5280);
    assert_eq!(Distance::from_nautical_miles(1).meters(), 1852);
    assert_eq!(Distance::from_inches(12).feet(), 1);
    assert_eq!(Distance::from_kilometers(1).meters(), 1000);
}

#[test]
fn distance_rounds_half_away_from_zero_not_towards_it() {
    // The single most likely silent defect in this module: 376 m is
    // 1233.6 ft, so truncation gives 1233 and rounding gives 1234.
    assert_eq!(Distance::from_meters(376).feet(), 1234);
    assert_eq!(Distance::from_meters(-376).feet(), -1234);

    // An exact tie goes away from zero in both directions. Half a foot
    // is 152 400 µm.
    assert_eq!(Distance::from_micrometers(152_400).feet(), 1);
    assert_eq!(Distance::from_micrometers(-152_400).feet(), -1);
    // Just under a tie stays put.
    assert_eq!(Distance::from_micrometers(152_399).feet(), 0);
    assert_eq!(Distance::from_micrometers(-152_399).feet(), 0);
}

// ------------------------------------------------------------------- Speed

#[test]
fn speed_known_answers() {
    // (speed, canonical mm/h, knots, mph, km/h)
    let cases: &[(Speed, i64, i32, i32, i32)] = &[
        (Speed::from_knots(1), 1_852_000, 1, 1, 2),
        (Speed::from_knots(36), 66_672_000, 36, 41, 67),
        (Speed::from_knots(999), 1_850_148_000, 999, 1150, 1850),
        (Speed::from_mph(60), 96_560_640, 52, 60, 97),
    ];
    for &(speed, canonical, knots, mph, kmh) in cases {
        assert_eq!(
            speed.millimeters_per_hour(),
            canonical,
            "{speed:?} canonical"
        );
        assert_eq!(speed.knots(), knots, "{speed:?} knots");
        assert_eq!(speed.mph(), mph, "{speed:?} mph");
        assert_eq!(speed.kmh(), kmh, "{speed:?} km/h");
    }
}

#[test]
fn speed_from_the_units_a_sensor_reports_in() {
    // An anemometer reads metres per second; the APRS wire field is
    // miles per hour. This conversion is the whole reason a weather
    // station needs this type, and it used to be done by hand.
    assert_eq!(
        Speed::from_meters_per_second(1).millimeters_per_hour(),
        3_600_000
    );
    assert_eq!(
        Speed::from_millimeters_per_second(1).millimeters_per_hour(),
        3_600
    );
    // 10 m/s is a stiff breeze: 22 mph, 36 km/h, 19 knots.
    let breeze = Speed::from_meters_per_second(10);
    assert_eq!(breeze.mph(), 22);
    assert_eq!(breeze.kmh(), 36);
    assert_eq!(breeze.knots(), 19);
    // Whole m/s is coarse for light winds, which is why the millimetre
    // constructor exists: 2.5 m/s is 5.6 mph, not 4.5 (2 m/s) or 6.7
    // (3 m/s).
    assert_eq!(Speed::from_millimeters_per_second(2500).mph(), 6);
    assert_eq!(
        Speed::from_millimeters_per_second(2500).millimeters_per_second(),
        2500
    );
    // A hurricane-force 33 m/s, the threshold the definition uses.
    assert_eq!(Speed::from_meters_per_second(33).knots(), 64);
}

#[test]
fn speed_factors_match_the_published_definitions() {
    // A knot is one nautical mile per hour; mph one statute mile per
    // hour. The conversion 1 kn = 1.15078 mph is the published figure.
    assert_eq!(Speed::from_knots(1).millimeters_per_hour(), 1_852_000);
    assert_eq!(Speed::from_mph(1).millimeters_per_hour(), 1_609_344);
    assert_eq!(Speed::from_kmh(1).millimeters_per_hour(), 1_000_000);
    // 100 kn = 115.078 mph, so 115 after rounding.
    assert_eq!(Speed::from_knots(100).mph(), 115);
    // 100 kn = 185.2 km/h.
    assert_eq!(Speed::from_knots(100).kmh(), 185);
    // A knot and its distance unit agree.
    assert_eq!(
        Speed::from_knots(1).millimeters_per_hour() * 1000,
        Distance::from_nautical_miles(1).micrometers()
    );
}

// -------------------------------------------------------------- Temperature

#[test]
fn temperature_known_answers() {
    // Temperature is the one quantity whose canonical value is not
    // asserted here, because it is not observable: the 1/45 000 °C unit
    // is finer than every accessor, so no public method can recover it
    // and exposing one would violate the module's no-raw-accessor rule.
    // Its canonical vectors are pinned by the unit test inside
    // `src/units.rs`, where the field is visible; what makes the choice
    // of unit *externally* verifiable is the exact round trip in all
    // three input units below, which a coarser unit (millidegrees
    // Celsius, the obvious choice) fails.
    //
    // (temperature, fahrenheit, celsius)
    let cases: &[(Temperature, i32, i32)] = &[
        (Temperature::from_fahrenheit(-99), -99, -73),
        (Temperature::from_fahrenheit(32), 32, 0),
        (Temperature::from_fahrenheit(72), 72, 22),
        (Temperature::from_fahrenheit(999), 999, 537),
        (Temperature::from_celsius(21), 70, 21),
        (Temperature::from_celsius(100), 212, 100),
    ];
    for &(temperature, fahrenheit, celsius) in cases {
        assert_eq!(temperature.fahrenheit(), fahrenheit, "{temperature:?} F");
        assert_eq!(temperature.celsius(), celsius, "{temperature:?} C");
    }
}

#[test]
fn temperature_anchors_everybody_knows() {
    // If these two pass, the affine transform is right; if the scale
    // factor or the offset were wrong, at least one would fail.
    assert_eq!(Temperature::from_celsius(100).fahrenheit(), 212);
    assert_eq!(Temperature::from_celsius(0).fahrenheit(), 32);
    assert_eq!(Temperature::from_fahrenheit(212).celsius(), 100);
    assert_eq!(Temperature::from_fahrenheit(32).celsius(), 0);
    // -40 is the scales' fixed point.
    assert_eq!(Temperature::from_celsius(-40).fahrenheit(), -40);
    assert_eq!(Temperature::from_fahrenheit(-40).celsius(), -40);
}

#[test]
fn temperature_round_trips_exactly_in_all_three_input_units() {
    // This is the property that 1/45 000 °C was chosen to give, and the
    // reason millidegrees Celsius (the obvious choice) was rejected: it
    // cannot represent a whole degree Fahrenheit.
    for fahrenheit in -999..=999 {
        assert_eq!(
            Temperature::from_fahrenheit(fahrenheit).fahrenheit(),
            fahrenheit,
            "{fahrenheit} F"
        );
    }
    for celsius in -999..=999 {
        assert_eq!(
            Temperature::from_celsius(celsius).celsius(),
            celsius,
            "{celsius} C"
        );
    }
    for millidegrees in (-999_000..=999_000).step_by(37) {
        assert_eq!(
            Temperature::from_millidegrees_celsius(millidegrees).millidegrees_celsius(),
            millidegrees,
            "{millidegrees} mC"
        );
    }
}

#[test]
fn temperature_cross_scale_rounding_is_documented_not_accidental() {
    // 21 C is 69.8 F. Rounding gives 70; truncation would give 69.
    assert_eq!(Temperature::from_celsius(21).fahrenheit(), 70);
    // 72 F is 22.22 C, which rounds down.
    assert_eq!(Temperature::from_fahrenheit(72).celsius(), 22);
    // 73 F is 22.78 C, which rounds up.
    assert_eq!(Temperature::from_fahrenheit(73).celsius(), 23);
    // Below zero it still moves away from zero: -99 F is -72.8 C.
    assert_eq!(Temperature::from_fahrenheit(-99).celsius(), -73);
}

/// A tie in Fahrenheit must round away from **zero Fahrenheit**, not
/// away from the freezing point.
///
/// The distinction is invisible to a round trip —
/// `from_fahrenheit(n).fahrenheit() == n` holds under either rule,
/// because a whole number of degrees is never a tie — and it was wrong
/// here until a weather station reporting tenths of a degree walked
/// into it. `fahrenheit()` divided first and added the 32-degree
/// offset afterwards, so it rounded the *distance from freezing*: half
/// a degree Fahrenheit came back as zero, and 31.5 F as 31.
#[test]
fn temperature_ties_round_away_from_zero_on_the_scale_asked_for() {
    let cases: &[(i32, i32)] = &[
        (5, 1),      // 0.5 F -> 1, not 0
        (-5, -1),    // -0.5 F -> -1
        (4, 0),      // 0.4 F -> 0
        (-4, 0),     // -0.4 F -> 0
        (315, 32),   // 31.5 F -> 32, not 31
        (325, 33),   // 32.5 F -> 33
        (-315, -32), // -31.5 F -> -32
        (725, 73),   // 72.5 F -> 73
        (715, 72),   // 71.5 F -> 72
    ];
    for &(tenths, want) in cases {
        assert_eq!(
            Temperature::from_tenths_fahrenheit(tenths).fahrenheit(),
            want,
            "{}.{} F",
            tenths / 10,
            (tenths % 10).abs()
        );
    }

    // The tenths accessor is the inverse, exactly, over the whole
    // range a weather station can report.
    for tenths in -9990..=9990 {
        assert_eq!(
            Temperature::from_tenths_fahrenheit(tenths).tenths_fahrenheit(),
            tenths,
            "{tenths} tenths F"
        );
    }

    // And the anchors are still exact in tenths.
    assert_eq!(
        Temperature::from_tenths_fahrenheit(320),
        Temperature::FREEZING
    );
    assert_eq!(Temperature::from_celsius(100).tenths_fahrenheit(), 2120);
    assert_eq!(Temperature::from_tenths_fahrenheit(2120).celsius(), 100);
}

// ---------------------------------------------------------------- Pressure

#[test]
fn pressure_known_answers() {
    let slp = Pressure::from_tenths_hpa(10132);
    assert_eq!(slp.millipascals(), 101_320_000);
    assert_eq!(slp.tenths_hpa(), 10132);
    assert_eq!(slp.hpa(), 1013);
    assert_eq!(slp.pascals(), 101_320);

    let field_max = Pressure::from_tenths_hpa(99999);
    assert_eq!(field_max.millipascals(), 999_990_000);
    assert_eq!(field_max.hpa(), 10_000);
    assert_eq!(field_max.pascals(), 999_990);
}

#[test]
fn pressure_standard_atmosphere_reads_29_92_inches_of_mercury() {
    // The cross-check every US weather station displays: 1013.2 hPa is
    // 29.92 inHg. Uses the conventional 1 inHg = 3386.389 Pa.
    assert_eq!(Pressure::from_tenths_hpa(10132).hundredths_inhg(), 2992);
    assert_eq!(Pressure::from_hundredths_inhg(2992).tenths_hpa(), 10132);
    assert_eq!(
        Pressure::from_hundredths_inhg(100).millipascals(),
        3_386_389
    );
    assert_eq!(Pressure::from_hpa(1).millipascals(), 100_000);
    assert_eq!(Pressure::from_pascals(1).millipascals(), 1_000);
}

// ---------------------------------------------------------------- Rainfall

#[test]
fn rainfall_known_answers() {
    // (rainfall, canonical micrometers, hundredths of an inch, mm)
    let cases: &[(Rainfall, i64, i32, i32)] = &[
        (Rainfall::from_hundredths_inch(1), 254, 1, 0),
        (Rainfall::from_hundredths_inch(254), 64_516, 254, 65),
        (
            Rainfall::from_hundredths_inch(65535),
            16_645_890,
            65_535,
            16_646,
        ),
    ];
    for &(rain, canonical, hundredths, millimeters) in cases {
        assert_eq!(rain.micrometers(), canonical, "{rain:?} canonical");
        assert_eq!(rain.hundredths_inch(), hundredths, "{rain:?} 0.01in");
        assert_eq!(rain.millimeters(), millimeters, "{rain:?} mm");
    }
    // One inch of rain, the unit a US gauge is marked in.
    assert_eq!(Rainfall::from_hundredths_inch(100).millimeters(), 25);
    assert_eq!(Rainfall::from_millimeters(1).micrometers(), 1_000);
}

// ------------------------------------------------------------------- Power

#[test]
fn power_dbm_anchors_every_ham_knows() {
    // The decibel-milliwatt figures a licence exam expects. If the
    // decade split or the mantissa table were wrong these would move.
    let cases: &[(i32, i32, &str)] = &[
        (0, 1, "0 dBm is 1 mW"),
        (10, 10, "10 dBm is 10 mW"),
        (20, 100, "20 dBm is 100 mW"),
        (30, 1_000, "30 dBm is 1 W"),
        (33, 1_995, "33 dBm is about 2 W"),
        (37, 5_012, "37 dBm is about 5 W, the common WSPR level"),
        (40, 10_000, "40 dBm is 10 W"),
        (50, 100_000, "50 dBm is 100 W"),
        (60, 1_000_000, "60 dBm is 1 kW"),
    ];
    for &(dbm, milliwatts, why) in cases {
        assert_eq!(Power::from_dbm(dbm).milliwatts(), milliwatts, "{why}");
    }
    // Doubling the power is +3 dB, halving is -3 dB: the property that
    // makes the scale useful, and one a wrong mantissa table breaks.
    assert_eq!(Power::from_dbm(37).watts(), 5);
    assert_eq!(Power::from_dbm(34).watts(), 3); // 2.5 W rounds to 3
    assert_eq!(Power::from_dbm(43).watts(), 20);
}

#[test]
fn power_dbm_round_trips_and_is_honest_about_zero() {
    // Every whole dBm from the documented floor upwards comes back
    // unchanged, which is what makes the logarithm's inexactness
    // invisible to a caller who works in dBm.
    for dbm in -24..=160 {
        assert_eq!(Power::from_dbm(dbm).dbm(), Some(dbm), "{dbm} dBm");
    }
    // ...and the floor is exactly there: -25 is the first figure that
    // collides with its neighbour. Pinning the boundary keeps the
    // documented claim and the code from drifting apart.
    assert_ne!(Power::from_dbm(-25).dbm(), Some(-25));
    // Below the floor, neighbouring dBm figures share a microwatt and
    // become indistinguishable. That is a real limit of the storage, so
    // it is asserted rather than left to be discovered: -30 and -29 dBm
    // are both 1 microwatt. No transmitter operates there -- 0 dBm is a
    // milliwatt -- which is why the floor is acceptable.
    assert_eq!(Power::from_dbm(-30).microwatts(), 1);
    assert_eq!(Power::from_dbm(-29).microwatts(), 1);
    assert_eq!(Power::from_dbm(-29).dbm(), Some(-30));
    assert_eq!(Power::from_dbm(-26).microwatts(), 3);
    assert_eq!(Power::from_dbm(-25).microwatts(), 3);
    // Zero power is negative infinity on a log scale. Saying so with
    // None beats inventing a sentinel.
    assert_eq!(Power::ZERO.dbm(), None);
    assert_eq!(Power::from_watts(-1).dbm(), None);
    // The WSPR power levels, which are constrained to end in 0, 3 or 7.
    for dbm in [
        0, 3, 7, 10, 13, 17, 20, 23, 27, 30, 33, 37, 40, 43, 47, 50, 53, 57, 60,
    ] {
        assert_eq!(Power::from_dbm(dbm).dbm(), Some(dbm), "WSPR {dbm} dBm");
    }
}

#[test]
fn power_known_answers() {
    assert_eq!(Power::from_watts(1).microwatts(), 1_000_000);
    assert_eq!(Power::from_milliwatts(1).microwatts(), 1_000);
    // The nine APRS PHG power codes are the squares of 0..=9 watts.
    for code in 0..=9i32 {
        let watts = code * code;
        assert_eq!(Power::from_watts(watts).watts(), watts, "PHG code {code}");
    }
    assert_eq!(Power::from_watts(1500).microwatts(), 1_500_000_000);
    assert_eq!(Power::from_milliwatts(2500).watts(), 3);
}

// ----------------------------------------------------------------- Bearing

#[test]
fn bearing_validates_its_enumerable_range() {
    assert_eq!(Bearing::new(0).map(Bearing::degrees), Ok(0));
    assert_eq!(Bearing::new(359).map(Bearing::degrees), Ok(359));
    // 360 is due north on the wire, not an error: APRS spells north as
    // `360` because a `000` course means "unknown".
    assert_eq!(Bearing::new(360).map(Bearing::degrees), Ok(0));
    assert_eq!(Bearing::new(361), Err(UnitError::BadBearing { got: 361 }));
    assert_eq!(
        Bearing::new(65535),
        Err(UnitError::BadBearing { got: 65535 })
    );
}

#[test]
fn bearing_reciprocal_is_an_involution() {
    for degrees in 0..360u16 {
        let bearing = Bearing::new(degrees).expect("in range");
        assert_eq!(bearing.reciprocal().reciprocal(), bearing, "{degrees}");
        let opposed = bearing.reciprocal().degrees();
        assert_eq!((opposed + 360 - degrees) % 360, 180, "{degrees}");
    }
}

#[test]
fn compass_points_cover_every_degree_exactly_once_per_sector() {
    // Each of the sixteen points must own exactly 360/16 = 22.5 degrees,
    // i.e. 22 or 23 whole degrees, and the sixteen sectors must tile the
    // circle with no gap and no overlap.
    let mut counts = [0usize; 16];
    for degrees in 0..360u16 {
        let point = Bearing::new(degrees).expect("in range").compass_point();
        let index = CompassPoint::ALL
            .iter()
            .position(|&p| p == point)
            .expect("a named point");
        counts[index] += 1;
    }
    assert_eq!(counts.iter().sum::<usize>(), 360);
    for (index, &count) in counts.iter().enumerate() {
        assert!(
            count == 22 || count == 23,
            "{:?} owns {count} degrees, expected 22 or 23",
            CompassPoint::ALL[index]
        );
    }
    // The cardinal and half-cardinal directions, spot-checked.
    let cases = [
        (0u16, "N"),
        (45, "NE"),
        (90, "E"),
        (135, "SE"),
        (180, "S"),
        (225, "SW"),
        (270, "W"),
        (315, "NW"),
        (22, "NNE"),
        (88, "E"),
    ];
    for (degrees, abbreviation) in cases {
        assert_eq!(
            Bearing::new(degrees)
                .expect("in range")
                .compass_point()
                .abbreviation(),
            abbreviation,
            "{degrees} degrees"
        );
    }
}

// ---------------------------------------------------------------- Humidity

#[test]
fn humidity_absorbs_the_wire_quirk_once() {
    // `h00` on the wire is 100%, not 0%. Read as 0% it would report the
    // driest possible air where the wettest was measured, which is why
    // this lives in the type rather than at each call site.
    assert_eq!(
        Humidity::from_wire_percent(0).map(Humidity::percent),
        Ok(100)
    );
    assert_eq!(Humidity::new(100).map(Humidity::wire_percent), Ok(0));
    for percent in 1..=99u8 {
        let humidity = Humidity::new(percent).expect("in range");
        assert_eq!(humidity.percent(), percent);
        assert_eq!(humidity.wire_percent(), percent);
        assert_eq!(
            Humidity::from_wire_percent(percent).map(Humidity::percent),
            Ok(percent)
        );
    }
    // Zero is rejected by the plain constructor, on purpose: it is not a
    // humidity, it is the wire's spelling of 100.
    assert_eq!(Humidity::new(0), Err(UnitError::BadHumidity { got: 0 }));
    assert_eq!(Humidity::new(101), Err(UnitError::BadHumidity { got: 101 }));
    assert_eq!(
        Humidity::from_wire_percent(101),
        Err(UnitError::BadHumidity { got: 101 })
    );
}

// ------------------------------------------------- same-unit exactness laws

#[test]
fn same_unit_access_is_exact_across_a_wide_range() {
    // The complement of the known-answer tests: they pin the factors,
    // this pins that nothing is lost on the way out and back.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        {
            (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32
        }
    };
    for _ in 0..20_000 {
        // Constrained to the ranges each unit can represent without the
        // canonical value saturating; the extremes are covered by the
        // no-panic test below.
        let small = next() % 1_000_000;
        assert_eq!(Distance::from_feet(small).feet(), small);
        assert_eq!(Distance::from_meters(small).meters(), small);
        assert_eq!(Distance::from_inches(small).inches(), small);
        assert_eq!(Speed::from_knots(small).knots(), small);
        assert_eq!(Speed::from_mph(small).mph(), small);
        assert_eq!(Speed::from_kmh(small).kmh(), small);
        assert_eq!(
            Rainfall::from_hundredths_inch(small).hundredths_inch(),
            small
        );
        assert_eq!(Rainfall::from_millimeters(small).millimeters(), small);
        assert_eq!(Temperature::from_celsius(small).celsius(), small);
        assert_eq!(Temperature::from_fahrenheit(small).fahrenheit(), small);
        assert_eq!(Pressure::from_pascals(small).pascals(), small);
        assert_eq!(Pressure::from_tenths_hpa(small).tenths_hpa(), small);
        // The one inexact constructor: hundredths of an inch of mercury
        // are 33863.89 mPa, so storage rounds. It must still round trip.
        assert_eq!(
            Pressure::from_hundredths_inhg(small).hundredths_inhg(),
            small
        );
        assert_eq!(Distance::from_millimeters(small).millimeters(), small);
        assert_eq!(
            Speed::from_meters_per_second(small).meters_per_second(),
            small
        );
        assert_eq!(Power::from_watts(small).watts(), small);
        assert_eq!(Power::from_milliwatts(small).milliwatts(), small);

        let tiny = next() % 2_000;
        assert_eq!(Distance::from_kilometers(tiny).kilometers(), tiny);
        assert_eq!(Distance::from_nautical_miles(tiny).nautical_miles(), tiny);
        assert_eq!(Distance::from_statute_miles(tiny).statute_miles(), tiny);
    }
}

// ------------------------------------------------------------ no-panic law

/// Every extreme an `i32`-taking constructor or an `i64`-taking one can
/// be handed.
const EXTREMES_32: &[i32] = &[i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
const EXTREMES_64: &[i64] = &[i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX];

#[test]
fn no_constructor_accessor_or_operator_can_panic() {
    // Debug builds panic on overflow, so this test is only meaningful in
    // the default (debug) profile — which is where `cargo test` runs it.
    // It is the reason every operation in `units` saturates.
    for &value in EXTREMES_32 {
        let distances = [
            Distance::from_feet(value),
            Distance::from_meters(value),
            Distance::from_millimeters(value),
            Distance::from_kilometers(value),
            Distance::from_nautical_miles(value),
            Distance::from_statute_miles(value),
            Distance::from_inches(value),
        ];
        for distance in distances {
            exercise_distance(distance);
        }
        exercise_speed(Speed::from_knots(value));
        exercise_speed(Speed::from_mph(value));
        exercise_speed(Speed::from_kmh(value));
        exercise_speed(Speed::from_meters_per_second(value));
        exercise_power(Power::from_dbm(value));
        exercise_rainfall(Rainfall::from_hundredths_inch(value));
        exercise_rainfall(Rainfall::from_millimeters(value));
        exercise_temperature(Temperature::from_celsius(value));
        exercise_temperature(Temperature::from_fahrenheit(value));
        exercise_temperature(Temperature::from_millidegrees_celsius(value));
        exercise_pressure(Pressure::from_pascals(value));
        exercise_pressure(Pressure::from_hpa(value));
        exercise_pressure(Pressure::from_tenths_hpa(value));
        exercise_pressure(Pressure::from_hundredths_inhg(value));
        exercise_power(Power::from_watts(value));
        exercise_power(Power::from_milliwatts(value));
    }
    for &value in EXTREMES_64 {
        exercise_distance(Distance::from_micrometers(value));
        exercise_speed(Speed::from_millimeters_per_hour(value));
        exercise_speed(Speed::from_millimeters_per_second(value));
        exercise_rainfall(Rainfall::from_micrometers(value));
        exercise_pressure(Pressure::from_millipascals(value));
        exercise_power(Power::from_microwatts(value));
    }
    // The two validated types cannot be constructed out of range at all,
    // so the whole input domain is the test.
    for degrees in 0..=u16::MAX {
        if let Ok(bearing) = Bearing::new(degrees) {
            let _ = bearing.degrees();
            let _ = bearing.compass_point();
            let _ = bearing.reciprocal();
        }
    }
    for percent in 0..=u8::MAX {
        if let Ok(humidity) = Humidity::new(percent) {
            let _ = humidity.percent();
            let _ = humidity.wire_percent();
        }
        let _ = Humidity::from_wire_percent(percent);
    }
}

fn exercise_distance(distance: Distance) {
    let _ = distance.feet();
    let _ = distance.meters();
    let _ = distance.millimeters();
    let _ = distance.kilometers();
    let _ = distance.nautical_miles();
    let _ = distance.statute_miles();
    let _ = distance.inches();
    let _ = distance.micrometers();
    for other in [Distance::ZERO, distance, -distance] {
        let _ = distance + other;
        let _ = distance - other;
    }
    let _ = -distance;
}

fn exercise_speed(speed: Speed) {
    let _ = speed.knots();
    let _ = speed.mph();
    let _ = speed.kmh();
    let _ = speed.meters_per_second();
    let _ = speed.millimeters_per_second();
    let _ = speed.millimeters_per_hour();
    for other in [Speed::ZERO, speed, -speed] {
        let _ = speed + other;
        let _ = speed - other;
    }
    let _ = -speed;
}

fn exercise_rainfall(rain: Rainfall) {
    let _ = rain.hundredths_inch();
    let _ = rain.millimeters();
    let _ = rain.micrometers();
    for other in [Rainfall::ZERO, rain, -rain] {
        let _ = rain + other;
        let _ = rain - other;
    }
    let _ = -rain;
}

fn exercise_temperature(temperature: Temperature) {
    let _ = temperature.celsius();
    let _ = temperature.fahrenheit();
    let _ = temperature.millidegrees_celsius();
    // No arithmetic: Temperature is an interval scale and implements
    // none. Comparison is all there is to exercise.
    let _ = temperature.cmp(&Temperature::FREEZING);
}

fn exercise_pressure(pressure: Pressure) {
    let _ = pressure.pascals();
    let _ = pressure.hpa();
    let _ = pressure.tenths_hpa();
    let _ = pressure.hundredths_inhg();
    let _ = pressure.millipascals();
    for other in [Pressure::ZERO, pressure, -pressure] {
        let _ = pressure + other;
        let _ = pressure - other;
    }
    let _ = -pressure;
}

fn exercise_power(power: Power) {
    let _ = power.watts();
    let _ = power.milliwatts();
    let _ = power.microwatts();
    let _ = power.dbm();
    for other in [Power::ZERO, power, -power] {
        let _ = power + other;
        let _ = power - other;
    }
    let _ = -power;
}

// ----------------------------------------------------- quantities are wide

#[test]
fn quantities_accept_values_no_aprs_field_could_hold() {
    // The other half of the design: a quantity is a physical value, and
    // the range check belongs on the wire setter. Both of these are
    // legitimate measurements and neither fits its APRS field, so a
    // quantity that clamped here would corrupt them silently.

    // A Mic-E altitude at its maximum, converted to feet, overflows the
    // six-digit `/A=` field.
    assert_eq!(Distance::from_meters(743_570).feet(), 2_439_534);
    assert!(Distance::from_meters(743_570).feet() > 999_999);

    // A legal APRS knot value exceeds the three-digit mph wire field.
    assert_eq!(Speed::from_knots(999).mph(), 1150);
    assert!(Speed::from_knots(999).mph() > 999);

    // A perfectly ordinary Antarctic temperature is outside the `t`
    // field's -99..=999 degrees Fahrenheit.
    assert_eq!(Temperature::from_celsius(-100).fahrenheit(), -148);
    assert!(Temperature::from_celsius(-100).fahrenheit() < -99);
}

#[test]
fn ordering_follows_the_physical_value_not_the_unit() {
    // Derived `Ord` on the canonical field, which is only correct
    // because there is exactly one canonical unit per quantity.
    assert!(Distance::from_feet(1) < Distance::from_meters(1));
    assert!(Distance::from_kilometers(1) < Distance::from_statute_miles(1));
    assert!(Speed::from_mph(1) < Speed::from_knots(1));
    assert!(Temperature::from_celsius(0) < Temperature::from_fahrenheit(33));
    assert!(Temperature::from_fahrenheit(-40) == Temperature::from_celsius(-40));
}

#[test]
fn arithmetic_stays_within_the_type_and_saturates() {
    assert_eq!(
        Distance::from_meters(1) + Distance::from_meters(2),
        Distance::from_meters(3)
    );
    assert_eq!(
        Distance::from_feet(10) - Distance::from_feet(4),
        Distance::from_feet(6)
    );
    assert_eq!(-Distance::from_feet(7), Distance::from_feet(-7));
    // Saturation rather than overflow at the limit.
    let huge = Distance::from_micrometers(i64::MAX);
    assert_eq!((huge + huge).micrometers(), i64::MAX);
    let tiny = Distance::from_micrometers(i64::MIN);
    assert_eq!((tiny - huge).micrometers(), i64::MIN);
}
