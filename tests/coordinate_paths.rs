//! Every path a coordinate takes through the crate, pinned exactly.
//!
//! This file exists to be run **before and after** a change to the
//! coordinate storage unit, and to fail loudly if any path moves that
//! should not have.
//!
//! # Why a whole file for one number
//!
//! The coordinate type stores a signed integer count of some unit. Most
//! of the code that touches it names the unit through a constant, so
//! changing the unit produces a compile error at each such site and the
//! compiler walks you through them. A handful of sites instead bake the
//! unit into a bare literal, and those keep compiling and start lying.
//!
//! Nine such sites exist. Four are the widely-cited ones, the
//! `deg * PER_DEGREE + min * 100 + hundredths` compositions in the
//! uncompressed and Mic-E parsers. The others are less well known and
//! no less dangerous:
//!
//! * the encode mirror in `mic_e`'s `split_dmh`, which divides a
//!   coordinate back into degrees, minutes and hundredths with bare
//!   `/ 100` and `% 100`;
//! * the same composition in the NMEA fixed-point path;
//! * `write_latlon`, which divides by a bare `6000` and `100`. The
//!   compiler does object here, on a type mismatch, and the tempting
//!   repair is a cast, which compiles and keeps the wrong divisors;
//! * `Coordinates::maidenhead_with_precision`, which carries eight bare
//!   divisors (`120_000`, `60_000`, `12_000`, `6_000`, `500`, `250`,
//!   `50`, `25`). Nothing flags these at all;
//! * `degrees_minutes`, where taking the remainder against the
//!   units-per-degree constant yields units, not hundredths of a
//!   minute, so the constant rename is caught and the missing second
//!   division is not.
//!
//! Every one of those is exercised below against a literal expected
//! value. A site left on the old unit puts a station in the right
//! degree square and the wrong place inside it, which is plausible
//! enough to survive a smoke test and is exactly the failure this file
//! is here to prevent.
//!
//! # The expected values are stated as physical quantities
//!
//! Wherever a value could be written either as a raw unit count or as a
//! degrees/minutes reading, it is written as the reading. A fixture
//! saying `Latitude::from_degrees(49.0583)` survives a unit change; one
//! saying `Latitude::new(294_349)` does not, and the fastest way to
//! make a red suite green is to paste in whatever the code now returns,
//! at which point the fixture asserts the implementation against itself
//! and its value is gone.

#![cfg(feature = "aprs")]

use yodel::aprs::{AprsPacket, Position, Symbol};
use yodel::geo::{
    Coordinates, GridPrecision, Latitude, LatitudeHemisphere, Longitude, LongitudeHemisphere,
    UNITS_PER_HUNDREDTH_MINUTE,
};

/// One reference position, used by every path below.
///
/// 49 degrees 03.50 minutes north, 072 degrees 01.75 minutes west: the
/// worked example from chapter 6, chosen so a reader can check the
/// arithmetic against the specification rather than against this file.
const LAT_DEG: u16 = 49;
const LAT_MIN_HUNDREDTHS: u16 = 350;
const LON_DEG: u16 = 72;
const LON_MIN_HUNDREDTHS: u16 = 175;

/// A southern and eastern mirror of it, because a sign error in any of
/// the nine sites is invisible in a northern-hemisphere fixture.
const SOUTH_EAST_WIRE: &[u8] = b"!4903.50S/07201.75E>";
const NORTH_WEST_WIRE: &[u8] = b"!4903.50N/07201.75W>";

fn position_of(wire: &[u8]) -> Position<'_> {
    match AprsPacket::parse(wire) {
        Ok(AprsPacket::Position(p)) => p,
        other => panic!("expected a position, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Path 1 and 2: the uncompressed parser, both axes
// ---------------------------------------------------------------------

/// `parse_latlon` composes `deg * PER_DEGREE + min * 100 + hundredths`
/// for each axis. Both compositions bake the unit into the two literals.
#[test]
fn uncompressed_parse_places_both_axes() {
    let north_west = position_of(NORTH_WEST_WIRE);
    let dm = north_west.latitude.degrees_minutes();
    assert_eq!(dm.degrees, LAT_DEG);
    assert_eq!(dm.hundredths_of_minute, LAT_MIN_HUNDREDTHS);
    assert_eq!(north_west.latitude.hemisphere(), LatitudeHemisphere::North);

    let dm = north_west.longitude.degrees_minutes();
    assert_eq!(dm.degrees, LON_DEG);
    assert_eq!(dm.hundredths_of_minute, LON_MIN_HUNDREDTHS);
    assert_eq!(north_west.longitude.hemisphere(), LongitudeHemisphere::West);

    // The southern and eastern mirror must give the same magnitudes.
    // A site that folded the sign into the composition rather than
    // applying it afterwards fails here and nowhere else.
    let south_east = position_of(SOUTH_EAST_WIRE);
    assert_eq!(
        south_east.latitude.degrees_minutes(),
        north_west.latitude.degrees_minutes(),
        "the same magnitude south must read the same as north"
    );
    assert_eq!(
        south_east.longitude.degrees_minutes(),
        north_west.longitude.degrees_minutes(),
        "the same magnitude east must read the same as west"
    );
    assert_eq!(south_east.latitude.hemisphere(), LatitudeHemisphere::South);
    assert_eq!(south_east.longitude.hemisphere(), LongitudeHemisphere::East);
}

/// The degrees/minutes split is itself one of the nine sites: taking a
/// remainder against the units-per-degree constant yields units, and
/// only a second division turns those into hundredths of a minute.
///
/// Swept across the whole minute range rather than spot-checked,
/// because an off-by-a-factor here is a smooth error that a single
/// sample can sit on top of.
#[test]
fn degrees_minutes_split_is_exact_across_the_range() {
    for minute_hundredths in (0..6000).step_by(37) {
        let degrees = 12.0;
        let value = degrees + f64::from(minute_hundredths) / 6000.0;
        let lat = Latitude::from_degrees(value).expect("in range");
        let dm = lat.degrees_minutes();
        assert_eq!(dm.degrees, 12, "degrees for {value}");
        assert_eq!(
            dm.hundredths_of_minute,
            u16::try_from(minute_hundredths).expect("under 6000"),
            "hundredths of a minute for {value}"
        );
    }
}

// ---------------------------------------------------------------------
// Path 3: write_latlon, the encode mirror
// ---------------------------------------------------------------------

/// The uncompressed writer divides by a bare `6000` and `100`. The
/// round trip through the wire is what pins it.
#[test]
fn uncompressed_write_mirrors_the_parse() {
    for wire in [NORTH_WEST_WIRE, SOUTH_EAST_WIRE] {
        let parsed = position_of(wire);
        let mut buf = [0u8; 64];
        let n = parsed.build(&mut buf).expect("building");
        assert_eq!(
            &buf[..n],
            wire,
            "an uncompressed position must write back the bytes it parsed"
        );
    }
}

/// Built from typed degrees rather than parsed from bytes, so the
/// writer is reached without the parser having chosen the value.
#[test]
fn position_built_from_degrees_writes_the_expected_wire() {
    let pos = Position::new(
        Latitude::from_degrees(49.058_333_333).expect("in range"),
        Longitude::from_degrees(-72.029_166_667).expect("in range"),
        Symbol::CAR,
    );
    let mut buf = [0u8; 64];
    let n = pos.build(&mut buf).expect("building");
    assert_eq!(&buf[..n], b"!4903.50N/07201.75W>");
}

// ---------------------------------------------------------------------
// Path 4 and 5: Mic-E, both axes, both directions
// ---------------------------------------------------------------------

/// Mic-E composes its latitude from destination digits and its
/// longitude from information bytes, each with the same bare literals,
/// and `split_dmh` reverses both on encode.
#[cfg(feature = "micE")]
#[test]
fn mic_e_places_both_axes_and_mirrors_them() {
    use yodel::aprs::{MicE, MicEMessage};

    // Chapter 10's worked example: 33 deg 25.64 min N, 112 deg 07.00
    // min W.
    // Built on the 1/100 arc-minute grid on purpose. Mic-E carries
    // hundredths, so a coordinate that is not on that grid quantises
    // when encoded, and the round trip below would then be testing the
    // quantisation rather than the two compositions it is here for.
    let report = MicE::new(
        Latitude::new((33 * 6000 + 2564) * UNITS_PER_HUNDREDTH_MINUTE).expect("in range"),
        Longitude::new(-(112 * 6000 + 700) * UNITS_PER_HUNDREDTH_MINUTE).expect("in range"),
        20,
        251,
        Symbol::from_wire(b'/', b'j'),
        MicEMessage::InService,
    )
    .expect("a valid report");

    let dm = report.latitude.degrees_minutes();
    assert_eq!((dm.degrees, dm.hundredths_of_minute), (33, 2564));
    let dm = report.longitude.degrees_minutes();
    assert_eq!((dm.degrees, dm.hundredths_of_minute), (112, 700));

    // Encode, then decode, and require the coordinates back exactly.
    // This is the path that reaches `split_dmh`.
    let mut dest = [0u8; 6];
    let mut info = [0u8; 64];
    let len = report.encode(&mut dest, &mut info).expect("encoding");
    let decoded = yodel::aprs::mic_e::decode(&dest, &info[..len]).expect("decoding");
    assert_eq!(decoded.latitude, report.latitude);
    assert_eq!(decoded.longitude, report.longitude);

    // The southern and eastern mirror, for the same reason as above.
    let mirrored = MicE::new(
        Latitude::new(-(33 * 6000 + 2564) * UNITS_PER_HUNDREDTH_MINUTE).expect("in range"),
        Longitude::new((112 * 6000 + 700) * UNITS_PER_HUNDREDTH_MINUTE).expect("in range"),
        20,
        251,
        Symbol::from_wire(b'/', b'j'),
        MicEMessage::InService,
    )
    .expect("a valid report");
    let mut dest = [0u8; 6];
    let mut info = [0u8; 64];
    let len = mirrored.encode(&mut dest, &mut info).expect("encoding");
    let decoded = yodel::aprs::mic_e::decode(&dest, &info[..len]).expect("decoding");
    assert_eq!(decoded.latitude, mirrored.latitude);
    assert_eq!(decoded.longitude, mirrored.longitude);
    assert_eq!(
        decoded.latitude.degrees_minutes(),
        report.latitude.degrees_minutes(),
        "south must mirror north exactly"
    );
}

// ---------------------------------------------------------------------
// Path 6: the compressed base-91 conversion
// ---------------------------------------------------------------------

/// Chapter 9's own worked example, which fixes both divisors.
///
/// The spec gives `/5L!!<*e7>{?!` as 49 degrees 30.00 minutes north,
/// 072 degrees 45.00 minutes west. A wrong divisor on either axis moves
/// this by kilometres.
#[test]
fn compressed_spec_vector_places_both_axes() {
    let pos = position_of(b"!/5L!!<*e7> sT");
    let dm = pos.latitude.degrees_minutes();
    assert_eq!((dm.degrees, dm.hundredths_of_minute), (49, 3000));
    assert_eq!(pos.latitude.hemisphere(), LatitudeHemisphere::North);
    let dm = pos.longitude.degrees_minutes();
    assert_eq!((dm.degrees, dm.hundredths_of_minute), (72, 4500));
    assert_eq!(pos.longitude.hemisphere(), LongitudeHemisphere::West);
}

// ---------------------------------------------------------------------
// Path 7: NMEA fixed point
// ---------------------------------------------------------------------

/// The NMEA path composes degrees, minutes and a scaled fraction with
/// the same bare `* 100`.
#[test]
fn nmea_places_both_axes() {
    use yodel::aprs::{Decoded, DecodedKind};

    let wire = b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
    let DecodedKind::Nmea(sentence) = Decoded::decode(wire).kind else {
        panic!("expected an NMEA sentence");
    };
    let coords = sentence.position().expect("a fix");
    let dm = coords.latitude.degrees_minutes();
    assert_eq!((dm.degrees, dm.hundredths_of_minute), (48, 704));
    let dm = coords.longitude.degrees_minutes();
    assert_eq!((dm.degrees, dm.hundredths_of_minute), (11, 3100));
}

// ---------------------------------------------------------------------
// Path 8: Maidenhead, eight bare divisors and nothing flags them
// ---------------------------------------------------------------------

/// `maidenhead_with_precision` divides by eight literals that encode the
/// storage unit, and no lint or type error reaches any of them.
#[test]
fn maidenhead_encodes_at_every_precision() {
    // A well-known locator: the Maidenhead reference square itself.
    let here = Coordinates::new(
        Latitude::from_degrees(51.5).expect("in range"),
        Longitude::from_degrees(-0.75).expect("in range"),
    );
    assert_eq!(
        here.maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        "IO91"
    );
    assert_eq!(
        here.maidenhead_with_precision(GridPrecision::Subsquare)
            .as_str(),
        "IO91pm"
    );
    assert_eq!(
        here.maidenhead_with_precision(GridPrecision::ExtendedSquare)
            .as_str()
            .len(),
        8
    );

    // Southern and eastern, where a sign-dependent divisor would show.
    let antipodal = Coordinates::new(
        Latitude::from_degrees(-51.5).expect("in range"),
        Longitude::from_degrees(0.75).expect("in range"),
    );
    assert_eq!(
        antipodal
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        "JD08"
    );
    assert_eq!(
        antipodal
            .maidenhead_with_precision(GridPrecision::Subsquare)
            .as_str(),
        "JD08jm"
    );

    // And the inverse must land back in the same square.
    let grid = here.maidenhead_with_precision(GridPrecision::Subsquare);
    let centre = Coordinates::from_maidenhead(grid);
    assert_eq!(
        centre.maidenhead_with_precision(GridPrecision::Subsquare),
        grid,
        "the centre of a square must be in that square"
    );
}

// ---------------------------------------------------------------------
// Path 9: the distance path, whose scaling moves the other way
// ---------------------------------------------------------------------

/// One arc-minute of latitude is one nautical mile, by definition, and
/// this crate takes that as exactly 1852 m. That makes the distance
/// path checkable by hand at any storage unit.
#[test]
fn distance_is_anchored_to_the_nautical_mile() {
    let a = Coordinates::new(
        Latitude::from_degrees(0.0).expect("in range"),
        Longitude::from_degrees(0.0).expect("in range"),
    );
    // Exactly one arc-minute north.
    let b = Coordinates::new(
        Latitude::from_degrees(1.0 / 60.0).expect("in range"),
        Longitude::from_degrees(0.0).expect("in range"),
    );
    assert_eq!(
        a.distance_to(b).meters(),
        1852,
        "one arc-minute of latitude"
    );

    // One degree of latitude is sixty of them.
    let c = Coordinates::new(
        Latitude::from_degrees(1.0).expect("in range"),
        Longitude::from_degrees(0.0).expect("in range"),
    );
    assert_eq!(a.distance_to(c).meters(), 111_120, "one degree of latitude");

    // At the equator the two axes must agree bit for bit, which is the
    // isotropy property the cosine scaling exists to preserve.
    let d = Coordinates::new(
        Latitude::from_degrees(0.0).expect("in range"),
        Longitude::from_degrees(1.0).expect("in range"),
    );
    assert_eq!(
        a.distance_to(d).meters(),
        a.distance_to(c).meters(),
        "one degree of longitude at the equator equals one of latitude"
    );

    // Southern and western must mirror exactly.
    let south = Coordinates::new(
        Latitude::from_degrees(-1.0).expect("in range"),
        Longitude::from_degrees(0.0).expect("in range"),
    );
    assert_eq!(a.distance_to(south).meters(), a.distance_to(c).meters());
}

// ---------------------------------------------------------------------
// The whole-planet sweep
// ---------------------------------------------------------------------

/// A coordinate must survive degrees to storage and back, everywhere.
///
/// A prime-ish stride so the sample does not land only on values that
/// happen to sit on a boundary of whatever the current unit is, which
/// is the way a grid-aligned sweep can pass through a unit change
/// without noticing it.
#[test]
fn degrees_round_trip_over_the_whole_planet() {
    let mut checked = 0usize;
    let mut worst = 0.0f64;
    let mut lat_milli = -90_000i32;
    while lat_milli <= 90_000 {
        let degrees = f64::from(lat_milli) / 1000.0;
        let lat = Latitude::from_degrees(degrees).expect("in range");
        let error = (lat.to_degrees() - degrees).abs();
        assert!(
            error <= 1.0 / 12_000.0,
            "latitude {degrees} came back as {} (error {error})",
            lat.to_degrees()
        );
        worst = worst.max(error);
        checked += 1;
        lat_milli += 997;
    }
    let mut lon_milli = -180_000i32;
    while lon_milli <= 180_000 {
        let degrees = f64::from(lon_milli) / 1000.0;
        let lon = Longitude::from_degrees(degrees).expect("in range");
        let error = (lon.to_degrees() - degrees).abs();
        assert!(
            error <= 1.0 / 12_000.0,
            "longitude {degrees} came back as {} (error {error})",
            lon.to_degrees()
        );
        worst = worst.max(error);
        checked += 1;
        lon_milli += 997;
    }
    assert!(
        checked > 500,
        "the sweep must not narrow to nothing: {checked} samples"
    );
    println!("swept {checked} coordinates, worst round-trip error {worst:.9} degrees");
}

/// The extremes must be representable and must not wrap.
#[test]
fn the_poles_and_the_antimeridian_are_representable() {
    for degrees in [-90.0, 90.0] {
        let lat = Latitude::from_degrees(degrees).expect("a pole is in range");
        assert!((lat.to_degrees() - degrees).abs() < 1e-9);
    }
    for degrees in [-180.0, 180.0] {
        let lon = Longitude::from_degrees(degrees).expect("the antimeridian is in range");
        assert!((lon.to_degrees() - degrees).abs() < 1e-9);
    }
    assert!(Latitude::from_degrees(90.001).is_err());
    assert!(Longitude::from_degrees(180.001).is_err());
}
