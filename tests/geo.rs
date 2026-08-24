//! Maidenhead locators and the integer-only geometry in `warble::geo`.
//!
//! # How the geometry is judged
//!
//! `distance_to` is an approximation, so the right way to pin it is to
//! test the **error bound** rather than the value: the reference
//! haversine below is computed in `f64` *in this test only*, and the
//! integer result must land inside the tolerance the method documents.
//!
//! One subtlety decides which reference is correct. The implementation
//! converts through "one hundredth of an arc-minute of latitude is
//! exactly 18.52 m", which is the definition of the nautical mile and
//! therefore fixes the sphere at `1852 · 60 · 180 / π` = 6366.707 km.
//! A haversine using the more familiar mean radius of 6371 km reads
//! 0.067% larger — seven times the accuracy `distance_to` claims — so
//! comparing against it would fail an implementation that is doing
//! exactly what it says. The reference here is derived from the same
//! definition the code uses, so the only thing left in the comparison is
//! the equirectangular projection error, which is what is being tested.
//!
//! (An earlier design note tabulated reference distances against
//! 6371 km while separately specifying the 18 520 000 µm constant the
//! code uses. The two are mutually inconsistent; the constant won,
//! because it is the one the shipped arithmetic is built on and the one
//! this test can hold it to.)

use std::f64::consts::PI;

use warble::geo::{
    Ambiguity, Coordinates, GeoError, GridPrecision, Latitude, LatitudeHemisphere, Longitude,
    LongitudeHemisphere, MaidenheadGrid,
};

/// A coordinate magnitude in 1/100 arc-minutes, the unit every fixture
/// in this file is written in. The storage unit is finer, so this
/// rounds; anything asserting the finer value says so explicitly.
fn hundredths(units: i64) -> i64 {
    let step = warble::geo::UNITS_PER_HUNDREDTH_MINUTE;
    let half = if units < 0 { -step / 2 } else { step / 2 };
    (units + half) / step
}

/// The sphere the implementation's exact conversion constant implies.
const EARTH_RADIUS_M: f64 = 1852.0 * 60.0 * 180.0 / PI;

/// Builds coordinates from decimal degrees, panicking on invalid input.
fn at(latitude: f64, longitude: f64) -> Coordinates {
    Coordinates::new(
        Latitude::from_degrees(latitude).expect("valid latitude"),
        Longitude::from_degrees(longitude).expect("valid longitude"),
    )
}

/// Builds coordinates from exact 1/100 arc-minutes, with no `f64` in the
/// way — for the assertions that are meant to be exact.
fn at_hundredths(latitude: i64, longitude: i64) -> Coordinates {
    let step = warble::geo::UNITS_PER_HUNDREDTH_MINUTE;
    Coordinates::new(
        Latitude::new(latitude * step).expect("valid latitude"),
        Longitude::new(longitude * step).expect("valid longitude"),
    )
}

/// The reference great-circle distance, in metres.
///
/// `f64` and transcendental — legitimate here and nowhere in the crate,
/// which is `no_std` and has no `libm`.
fn haversine_m(a: Coordinates, b: Coordinates) -> f64 {
    let (lat1, lat2) = (a.latitude.to_degrees(), b.latitude.to_degrees());
    let (lon1, lon2) = (a.longitude.to_degrees(), b.longitude.to_degrees());
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// The reference *initial* great-circle bearing, in degrees.
fn initial_bearing_deg(a: Coordinates, b: Coordinates) -> f64 {
    let (p1, p2) = (
        a.latitude.to_degrees().to_radians(),
        b.latitude.to_degrees().to_radians(),
    );
    let dl = (b.longitude.to_degrees() - a.longitude.to_degrees()).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

// ------------------------------------------------------------- Maidenhead

#[test]
fn maidenhead_known_grids() {
    // Grids from the amateur radio literature, with the coordinates they
    // contain. These are the known-answer half: a transposed divisor or
    // a wrong offset cannot survive them.
    // Note the four `mm` subsquares: each of those coordinates is the
    // exact CENTRE of its four-character square, so the middle
    // subsquare is the right answer and any other would mean the
    // subsquare divisor is wrong. IO91wm is the one off-centre case.
    let cases: &[(f64, f64, &str, &str)] = &[
        (42.5, -71.0, "FN42", "FN42mm"),     // Boston area
        (55.5, 13.0, "JO65", "JO65mm"),      // Malmo / Copenhagen
        (51.5208, -0.125, "IO91", "IO91wm"), // central London
        (-89.5, -179.0, "AA00", "AA00mm"),   // south-west corner
        (89.5, 179.0, "RR99", "RR99mm"),     // north-east corner
    ];
    for &(latitude, longitude, four, six) in cases {
        let here = at(latitude, longitude);
        assert_eq!(
            here.maidenhead_with_precision(GridPrecision::Square)
                .as_str(),
            four,
            "{latitude},{longitude} 4-char"
        );
        assert_eq!(
            here.maidenhead().as_str(),
            six,
            "{latitude},{longitude} 6-char"
        );
    }
}

#[test]
fn maidenhead_corners_are_the_extremes_of_the_grid() {
    // AA00 and RR99 are the two corner squares; anything outside them
    // would mean an off-by-one in the field offset.
    assert_eq!(
        at(-90.0, -180.0)
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        "AA00"
    );
    assert_eq!(
        at(89.999, 179.999)
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        "RR99"
    );
    // The two axes behave differently at their top edge, and both used
    // to index a nineteenth field that does not exist.
    //
    // +180 and -180 are the same meridian, so they must produce the
    // same locator...
    assert_eq!(
        at(0.0, 180.0)
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        at(0.0, -180.0)
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str()
    );
    // ...while there is nothing north of the pole to wrap to, so the
    // north pole belongs to the last square.
    // "JR09", not "JR99": the characters interleave the axes as
    // lon-field, lat-field, lon-square, lat-square, and longitude 0 is
    // the west edge of field J.
    assert_eq!(
        at(90.0, 0.0)
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        "JR09"
    );
    assert_eq!(
        at(-90.0, 0.0)
            .maidenhead_with_precision(GridPrecision::Square)
            .as_str(),
        "JA00"
    );
}

#[test]
fn maidenhead_square_centers_round_trip() {
    // grid -> centre -> grid must be the identity at every precision,
    // for every square. That is 324 four-character squares sampled
    // across the whole globe, not a handful of examples.
    for lat_field in 0..18u8 {
        for lon_field in 0..18u8 {
            for square in [0u8, 5, 9] {
                let text = [
                    b'A' + lon_field,
                    b'A' + lat_field,
                    b'0' + square,
                    b'0' + square,
                ];
                let grid = MaidenheadGrid::from_bytes(&text).expect("constructed in range");
                let center = grid.center();
                assert_eq!(
                    center.maidenhead_with_precision(GridPrecision::Square),
                    grid,
                    "{}",
                    grid.as_str()
                );
            }
        }
    }
    // ...and the finer precisions on a specific square.
    for grid in ["IO91wm", "FN42ma", "JO65xx", "IO91wm55"] {
        let parsed = MaidenheadGrid::new(grid).expect("valid");
        assert_eq!(
            parsed
                .center()
                .maidenhead_with_precision(parsed.precision()),
            parsed,
            "{grid}"
        );
    }
}

#[test]
fn maidenhead_center_is_the_middle_not_the_corner() {
    // FN42 spans 42..43 N and -72..-70 E; its centre is 42.5, -71.
    let center = MaidenheadGrid::new("FN42").expect("valid").center();
    assert_eq!(hundredths(center.latitude.units()), 255_000);
    assert_eq!(hundredths(center.longitude.units()), -426_000);
    // IO91wm is a 2.5' x 5' subsquare; its centre is the plan's vector.
    let center = MaidenheadGrid::new("IO91wm").expect("valid").center();
    assert_eq!(hundredths(center.latitude.units()), 309_125);
    assert_eq!(hundredths(center.longitude.units()), -750);
}

#[test]
fn maidenhead_coarsening_is_exact_and_never_moves_the_place() {
    // Locators are hierarchical, so dropping characters is exact: the
    // coarsened locator must be the same one the position itself
    // encodes at that precision. Anything else would mean coarsening
    // and encoding disagree about which square a point is in.
    let precise = MaidenheadGrid::new("IO91wm").expect("valid");
    assert_eq!(precise.to_precision(GridPrecision::Square).as_str(), "IO91");

    // Asking for finer than is known is a no-op: the characters simply
    // do not exist.
    assert_eq!(precise.to_precision(GridPrecision::ExtendedSquare), precise);
    assert_eq!(precise.to_precision(GridPrecision::Subsquare), precise);

    // Coarsening is idempotent, and agrees with encoding the position
    // directly at that precision -- across the whole globe, not one
    // example.
    let mut latitude = -89.0f64;
    while latitude <= 89.0 {
        let mut longitude = -179.0f64;
        while longitude <= 179.0 {
            let here = at(latitude, longitude);
            let fine = here.maidenhead_with_precision(GridPrecision::ExtendedSquare);
            for precision in [
                GridPrecision::Square,
                GridPrecision::Subsquare,
                GridPrecision::ExtendedSquare,
            ] {
                let coarse = fine.to_precision(precision);
                assert_eq!(
                    coarse,
                    here.maidenhead_with_precision(precision),
                    "{latitude},{longitude} at {precision:?}"
                );
                assert_eq!(coarse.precision(), precision);
                assert_eq!(coarse.as_str().len(), precision.characters());
                assert_eq!(coarse.to_precision(precision), coarse, "idempotent");
                // The coarsened locator must still contain the point.
                assert_eq!(coarse.center().maidenhead_with_precision(precision), coarse);
            }
            longitude += 7.0;
        }
        latitude += 7.0;
    }
}

#[test]
fn maidenhead_parsing_is_case_insensitive_and_storage_is_canonical() {
    for text in ["IO91wm", "io91wm", "IO91WM", "iO91Wm"] {
        assert_eq!(
            MaidenheadGrid::new(text).expect("valid").as_str(),
            "IO91wm",
            "{text}"
        );
    }
}

#[test]
fn maidenhead_rejects_what_is_not_a_locator() {
    assert_eq!(
        MaidenheadGrid::new("FN4"),
        Err(GeoError::BadGridLength { got: 3 })
    );
    assert_eq!(
        MaidenheadGrid::new("FN425"),
        Err(GeoError::BadGridLength { got: 5 })
    );
    assert_eq!(
        MaidenheadGrid::new(""),
        Err(GeoError::BadGridLength { got: 0 })
    );
    // 'S' is past the last field letter 'R'.
    assert_eq!(
        MaidenheadGrid::new("SN42"),
        Err(GeoError::BadGridChar {
            got: b'S',
            position: 0
        })
    );
    // A letter where a square digit belongs.
    assert_eq!(
        MaidenheadGrid::new("FNx2"),
        Err(GeoError::BadGridChar {
            got: b'x',
            position: 2
        })
    );
    // 'y' is past the last subsquare letter 'x'.
    assert_eq!(
        MaidenheadGrid::new("IO91ym"),
        Err(GeoError::BadGridChar {
            got: b'y',
            position: 4
        })
    );
}

#[test]
fn every_position_on_earth_has_a_parseable_locator() {
    // Totality: the encoder must never emit something the decoder
    // rejects, anywhere, including at the poles and the antimeridian.
    let mut checked = 0;
    let mut latitude = -90.0f64;
    while latitude <= 90.0 {
        let mut longitude = -180.0f64;
        while longitude <= 180.0 {
            let here = at(latitude, longitude);
            for precision in [
                GridPrecision::Square,
                GridPrecision::Subsquare,
                GridPrecision::ExtendedSquare,
            ] {
                let grid = here.maidenhead_with_precision(precision);
                let reparsed = MaidenheadGrid::new(grid.as_str())
                    .unwrap_or_else(|e| panic!("{latitude},{longitude} -> {grid:?}: {e}"));
                assert_eq!(reparsed, grid);
                checked += 1;
            }
            longitude += 3.0;
        }
        latitude += 3.0;
    }
    assert!(checked > 10_000, "only {checked} positions checked");
}

// --------------------------------------------------------------- distance

#[test]
fn distance_matches_haversine_within_the_documented_bound() {
    // (from, to, tolerance as a fraction, why this case is here)
    let cases: &[(Coordinates, Coordinates, f64, &str)] = &[
        (
            at(49.0583, -72.0292),
            at(49.1583, -72.0292),
            0.0001,
            "0.1 degree due north, the pure-latitude case",
        ),
        (
            at(49.0583, -72.0292),
            at(49.0583, -71.9292),
            0.00001,
            "0.1 degree due east, where cos(latitude) does the work",
        ),
        (
            at(51.5, -0.1),
            at(52.5, 1.0),
            0.0001,
            "about 140 km diagonal, a typical VHF path",
        ),
        (
            at(33.4484, -112.0740),
            at(34.0522, -118.2437),
            0.001,
            "Phoenix to Los Angeles, ~574 km",
        ),
        (
            at(0.0, 0.0),
            at(0.0, 90.0),
            0.0001,
            "a quarter of the equator: equirectangular is exact there",
        ),
    ];
    for &(from, to, tolerance, why) in cases {
        // Micrometres, not metres. `meters()` rounds to the whole metre,
        // which at the 7 km east-west case below is 1.4e-4 of the answer
        // -- an order coarser than the tightest tolerance here, so a
        // metre-resolution comparison would be measuring its own
        // rounding rather than the geometry. `Distance` keeps the exact
        // micrometre value, so use it.
        #[allow(clippy::cast_precision_loss)] // under 2^53 um for every case
        let measured = from.distance_to(to).micrometers() as f64 / 1e6;
        let reference = haversine_m(from, to);
        let error = (measured - reference).abs() / reference;
        assert!(
            error <= tolerance,
            "{why}: measured {measured:.6} m, haversine {reference:.6} m, \
             relative error {:.5}% exceeds {:.5}%",
            error * 100.0,
            tolerance * 100.0
        );
    }
}

#[test]
fn one_arc_minute_of_latitude_is_one_nautical_mile() {
    // The property that fixes the conversion constant, and the reason
    // 6366.707 km rather than 6371 km is the right sphere here. Checkable
    // against any marine chart.
    let from = Coordinates::new(
        Latitude::new(0).expect("valid"),
        Longitude::new(0).expect("valid"),
    );
    let to = Coordinates::new(
        // One arc-minute, expressed through the constant rather than
        // as a literal so it follows the unit.
        Latitude::new(warble::geo::UNITS_PER_MINUTE).expect("valid"),
        Longitude::new(0).expect("valid"),
    );
    assert_eq!(from.distance_to(to).meters(), 1852);
    assert_eq!(from.distance_to(to).nautical_miles(), 1);
    // ...and sixty of them are one degree.
    let degree = Coordinates::new(
        Latitude::new(warble::geo::UNITS_PER_DEGREE).expect("valid"),
        Longitude::new(0).expect("valid"),
    );
    assert_eq!(from.distance_to(degree).meters(), 111_120);
}

#[test]
fn distance_is_symmetric_and_zero_to_itself() {
    let a = at(49.0583, -72.0292);
    let b = at(33.4484, -112.0740);
    assert_eq!(a.distance_to(b), b.distance_to(a));
    assert_eq!(a.distance_to(a).meters(), 0);
}

#[test]
fn distance_takes_the_short_way_across_the_antimeridian() {
    // +179.5 to -179.5 is one degree apart, not 359. Getting this wrong
    // would put two neighbouring stations most of a planet apart.
    let west = at(0.0, 179.5);
    let east = at(0.0, -179.5);
    let near = at(0.0, 178.5);
    assert_eq!(west.distance_to(east), west.distance_to(near));
    assert!(west.distance_to(east).kilometers() < 120);
    assert_eq!(west.bearing_to(east).degrees(), 90);
    assert_eq!(east.bearing_to(west).degrees(), 270);
}

#[test]
fn distance_is_isotropic_at_the_equator() {
    // THE law, and until now nothing asserted it. On the equator
    // cos(latitude) is exactly one, so the same angular separation taken
    // east and taken north is the same distance — not nearly the same,
    // bit for bit the same — and both are exactly 18.52 m per hundredth
    // of an arc-minute, the constant that defines the nautical mile.
    //
    // Eastward used to be short by exactly one part in 32768, because
    // the east axis was scaled by the sine table's real unity (32767,
    // since the table holds `round(sin * 32767)`) while the north axis
    // was shifted left by 15 (32768). Two axes, two units. One
    // arc-minute east came back 1 851 943 481 µm against north's exact
    // 1 852 000 000, and a quarter of the equator was 305 m short —
    // uniformly 3.0518e-5, which is 1/32768 and not a coincidence.
    let origin = at_hundredths(0, 0);
    for hundredths in [1i64, 2, 7, 100, 600, 6_000, 100_000, 540_000] {
        let north = origin.distance_to(at_hundredths(hundredths, 0));
        let south = origin.distance_to(at_hundredths(-hundredths, 0));
        let east = origin.distance_to(at_hundredths(0, hundredths));
        let west = origin.distance_to(at_hundredths(0, -hundredths));
        assert_eq!(
            north,
            east,
            "{hundredths} hundredths of an arc-minute at the equator: north is \
             {} µm but east is {} µm, {} µm apart — the two axes are not in the \
             same unit",
            north.micrometers(),
            east.micrometers(),
            north.micrometers() - east.micrometers()
        );
        // Sign must not matter on either axis.
        assert_eq!(north, south, "{hundredths}: north and south differ");
        assert_eq!(east, west, "{hundredths}: east and west differ");
        // And the shared answer is the exact latitude constant, so the
        // whole path — cosine, isqrt, micrometre conversion — rounded
        // nothing away.
        let exact = hundredths * 18_520_000;
        assert_eq!(north.micrometers(), exact, "{hundredths} north is inexact");
        assert_eq!(east.micrometers(), exact, "{hundredths} east is inexact");
    }
}

#[test]
fn the_two_axes_share_one_unit_at_every_latitude() {
    // Away from the equator the east axis *is* shorter than the north
    // one, by cos(latitude), so the law there is that the ratio of the
    // two equals that cosine — to within the quantisation of the Q15
    // integer the library holds the cosine in, and nothing more.
    //
    // That integer comes off a table of `round(sin * 32767)` (±0.5 LSB)
    // and its interpolation ROUNDS (a further ±0.5 LSB), so the window
    // is ±1 LSB and centred.
    //
    // It was not always. The interpolation used to arithmetic-shift the
    // product, which floors, and the delta keeps one sign across a
    // quarter turn — so the term cost 0 to 1 LSB one-sidedly DOWN and
    // the window was [-1.5, +0.5]. Scaling the north axis by `1 << 15`
    // while the east axis gets the table's 32767 dragged that down by a
    // further cos(latitude) LSB, to a swept [-2.314, +0.280] that
    // breached its lower bound first at 27.25 degrees. Rounding the
    // interpolation (see `types::sine_at_interpolated`) narrows the
    // sweep to [-0.838, +0.877]: a third tighter, and no longer leaning
    // one way. `geo::tests::cos_q15_is_not_biased` pins the centring at
    // the source; this pins what it is worth downstream.
    const LSB: f64 = 1.0 / 32_767.0;
    let mut lowest = f64::MAX;
    let mut highest = f64::MIN;
    for step in 0..=340i32 {
        // 0.25-degree steps to 85 degrees. 0.25 deg is exactly 1500
        // hundredths of an arc-minute, so the sweep needs no rounding
        // and is bit-reproducible.
        let latitude_hundredths = i64::from(step) * 1_500;
        #[allow(clippy::cast_precision_loss)]
        let latitude = latitude_hundredths as f64 / 6_000.0;
        let origin = at_hundredths(latitude_hundredths, 0);
        for separation in [100i64, 600, 6_000, 30_000] {
            // The pure-latitude leg is exact by construction, so it is a
            // clean denominator: 18.52 m per hundredth, no cosine in it.
            let north = origin
                .distance_to(at_hundredths(latitude_hundredths + separation, 0))
                .micrometers();
            let east = origin
                .distance_to(at_hundredths(latitude_hundredths, separation))
                .micrometers();
            assert_eq!(north, separation * 18_520_000);
            #[allow(clippy::cast_precision_loss)] // both well under 2^53
            let ratio = east as f64 / north as f64;
            let residual = (ratio - latitude.to_radians().cos()) / LSB;
            lowest = lowest.min(residual);
            highest = highest.max(residual);
            assert!(
                (-1.0..=1.0).contains(&residual),
                "at {latitude} degrees over {separation} hundredths of an \
                 arc-minute: east/north is {ratio:.9}, cos is {:.9}, a residual \
                 of {residual:.4} Q15 LSB — outside the ±1.5 LSB the cosine \
                 quantisation can account for, so the two axes are being \
                 scaled by different unities",
                latitude.to_radians().cos()
            );
        }
    }
    // The window is the documented accuracy term, so pin it as one
    // rather than only as 1364 separate spot checks.
    assert!(
        lowest >= -1.0 && highest <= 1.0,
        "east/north residual swept [{lowest:.4}, {highest:.4}] Q15 LSB"
    );
}

#[test]
fn distance_error_stays_inside_the_documented_latitude_bands() {
    // `distance_to`'s accuracy table, asserted. A single figure cannot
    // describe this function: the projection error grows with the
    // latitude *spread* of the path and the Q15 cosine's fixed 4.6e-5
    // absolute error becomes 4.6e-5/cos(phi) relative, so both terms run
    // away towards the poles. Hence bands.
    //
    // (band in whole degrees, bound to 100 km, bound to 300 km); the
    // sweep's measured worst cases are 0.00385 / 0.01496, 0.00728 /
    // 0.03496, 0.02129 / 0.14835 and 0.15064 / 1.47891 percent
    // respectively.
    //
    // These moved when `cos_q15`'s interpolation started rounding
    // instead of flooring. The old one-sided bias made the cosine read
    // LOW, which shortened the east component -- and the
    // equirectangular model OVER-estimates east-west distance, because
    // a great circle bows poleward. The two errors were cancelling by
    // luck, not by design: the previous table's own comment called the
    // flooring a defect, not a compensation.
    //
    // Removing it improved the near field at the latitudes VHF APRS
    // actually works in (0.00554 -> 0.00385 percent to 100 km below 45
    // degrees, 0.00787 -> 0.00728 below 60) and cost up to 0.002
    // percentage points on the 300 km paths, where the projection error
    // dominates and the cancellation had been worth most. The cosine
    // itself is strictly better: see the swept residual window in
    // `the_two_axes_share_one_unit_at_every_latitude`.
    const BANDS: &[(f64, f64, f64)] = &[
        (45.0, 0.000_05, 0.000_16),
        (60.0, 0.000_08, 0.000_36),
        (75.0, 0.000_22, 0.001_5),
        (85.0, 0.001_6, 0.016),
    ];
    const NEAR_KM: &[i32] = &[1, 2, 5, 10, 25, 50, 100];
    const FAR_KM: &[i32] = &[150, 200, 250, 300];

    // Per band: (worst to 100 km, worst to 300 km). The second is
    // cumulative — it includes the near separations too.
    let mut worst = [(0.0f64, 0.0f64); 4];
    for step in 0..=340i32 {
        let latitude_hundredths = i64::from(step) * 1_500; // 0.25-degree steps to 85
        #[allow(clippy::cast_precision_loss)]
        let latitude = latitude_hundredths as f64 / 6_000.0;
        let cos_latitude = latitude.to_radians().cos();
        let from = at_hundredths(latitude_hundredths, 0);
        for (near, kilometres) in [(true, NEAR_KM), (false, FAR_KM)] {
            for &km in kilometres {
                for sector in 0..24i32 {
                    // Every 15 degrees of azimuth: the error depends on
                    // how the separation splits between the two axes,
                    // so a cardinal-only sweep would miss the worst
                    // case (which lands near 45-60 degrees).
                    let azimuth = f64::from(sector) * 15.0;
                    let radians = azimuth.to_radians();
                    let metres = f64::from(km) * 1000.0;
                    // 18.52 m per hundredth of an arc-minute of
                    // latitude, and that times cos(phi) of longitude.
                    let d_latitude = metres * radians.cos() / 18.52 / 6_000.0;
                    let d_longitude = metres * radians.sin() / 18.52 / 6_000.0 / cos_latitude;
                    let (target_lat, target_lon) = (latitude + d_latitude, d_longitude);
                    if !(-90.0..=90.0).contains(&target_lat)
                        || !(-180.0..=180.0).contains(&target_lon)
                    {
                        continue;
                    }
                    let to = at(target_lat, target_lon);
                    let reference = haversine_m(from, to);
                    if reference < 0.5 {
                        continue;
                    }
                    #[allow(clippy::cast_precision_loss)] // under 2^53 µm
                    let measured = from.distance_to(to).micrometers() as f64 / 1e6;
                    let error = (measured - reference).abs() / reference;
                    for (slot, &(band, _, _)) in worst.iter_mut().zip(BANDS) {
                        if latitude <= band {
                            if near {
                                slot.0 = slot.0.max(error);
                            }
                            slot.1 = slot.1.max(error);
                        }
                    }
                }
            }
        }
    }
    for (&(band, near_bound, far_bound), &(near, far)) in BANDS.iter().zip(&worst) {
        assert!(
            near <= near_bound,
            "|latitude| <= {band} deg, paths to 100 km: worst relative error \
             {:.5}% exceeds the documented {:.5}%",
            near * 100.0,
            near_bound * 100.0
        );
        assert!(
            far <= far_bound,
            "|latitude| <= {band} deg, paths to 300 km: worst relative error \
             {:.5}% exceeds the documented {:.5}%",
            far * 100.0,
            far_bound * 100.0
        );
    }
}

// ---------------------------------------------------------------- bearing

#[test]
fn bearing_cardinal_directions_are_exact() {
    let here = at(45.0, 0.0);
    assert_eq!(here.bearing_to(at(46.0, 0.0)).degrees(), 0);
    assert_eq!(here.bearing_to(at(45.0, 1.0)).degrees(), 90);
    assert_eq!(here.bearing_to(at(44.0, 0.0)).degrees(), 180);
    assert_eq!(here.bearing_to(at(45.0, -1.0)).degrees(), 270);
    // A position has no bearing to itself; north is the documented
    // answer rather than a panic or a garbage angle.
    assert_eq!(here.bearing_to(here).degrees(), 0);
}

#[test]
fn bearing_differs_from_the_initial_great_circle_bearing_as_predicted() {
    // The equirectangular model yields the MEAN course over the path,
    // not the INITIAL great-circle bearing. The two differ by the
    // convergence of the meridians, about d_lon * sin(lat) / 2.
    //
    // Asserting that predicted difference is a much stronger statement
    // than a hand-tuned tolerance would be: it says the deviation has
    // the mechanism the documentation claims, not just that it is
    // small.
    let cases = [
        (at(49.0583, -72.0292), at(49.1583, -71.9292)),
        (at(51.5, -0.1), at(52.5, 1.0)),
        (at(33.4484, -112.0740), at(34.0522, -118.2437)),
        (at(-33.9, 18.4), at(-26.2, 28.0)),
    ];
    for (from, to) in cases {
        let measured = f64::from(from.bearing_to(to).degrees());
        let reference = initial_bearing_deg(from, to);
        let mut difference = measured - reference;
        while difference > 180.0 {
            difference -= 360.0;
        }
        while difference < -180.0 {
            difference += 360.0;
        }
        let d_lon = to.longitude.to_degrees() - from.longitude.to_degrees();
        let mean_lat = ((to.latitude.to_degrees() + from.latitude.to_degrees()) / 2.0).to_radians();
        let predicted = d_lon * mean_lat.sin() / 2.0;
        assert!(
            (difference - predicted).abs() < 1.0,
            "bearing deviation {difference:.3} deg does not match the predicted \
             meridian convergence {predicted:.3} deg (measured {measured}, \
             great-circle {reference:.3})"
        );
    }
}

#[test]
fn bearing_sweeps_through_every_direction_monotonically() {
    // Walk a circle of points around a centre and check the reported
    // bearing tracks the angle walked. Catches a quadrant sign error,
    // which cardinal-direction spot checks can miss.
    let center = at(45.0, 0.0);
    for step in 0..36 {
        let angle = f64::from(step) * 10.0;
        let radians = angle.to_radians();
        // One degree of latitude north, scaled into longitude by the
        // cosine so the walk really is a circle on the ground.
        let north = radians.cos();
        let east = radians.sin() / 45.0f64.to_radians().cos();
        let target = at(45.0 + north, east);
        let measured = f64::from(center.bearing_to(target).degrees());
        let mut difference = measured - angle;
        while difference > 180.0 {
            difference -= 360.0;
        }
        while difference < -180.0 {
            difference += 360.0;
        }
        assert!(
            difference.abs() <= 1.5,
            "walking {angle} degrees reported a bearing of {measured}"
        );
    }
}

// ------------------------------------------------- degrees/minutes display

#[test]
fn degrees_minutes_matches_what_a_radio_shows() {
    // The APRS wire form and every radio screen: 4903.50N/07201.75W.
    // Note the sign moves to the hemisphere -- writing -72.0292 as
    // "72 degrees 1.75 minutes WEST" is the step that goes wrong when
    // each caller reimplements it, which is why it is a method.
    let lat = Latitude::from_degrees(49.0583).expect("valid");
    assert_eq!(lat.degrees_minutes().degrees, 49);
    assert_eq!(lat.degrees_minutes().hundredths_of_minute, 350);
    assert_eq!(lat.hemisphere(), LatitudeHemisphere::North);
    assert_eq!(lat.hemisphere().letter(), b'N');

    let lon = Longitude::from_degrees(-72.0292).expect("valid");
    assert_eq!(lon.degrees_minutes().degrees, 72);
    assert_eq!(lon.degrees_minutes().hundredths_of_minute, 175);
    assert_eq!(lon.hemisphere(), LongitudeHemisphere::West);
    assert_eq!(lon.hemisphere().letter(), b'W');

    // Southern and eastern hemispheres, and the sign-symmetry property:
    // the magnitude must not depend on which side of zero it is.
    let south = Latitude::from_degrees(-49.0583).expect("valid");
    assert_eq!(south.degrees_minutes(), lat.degrees_minutes());
    assert_eq!(south.hemisphere(), LatitudeHemisphere::South);
    let east = Longitude::from_degrees(72.0292).expect("valid");
    assert_eq!(east.degrees_minutes(), lon.degrees_minutes());
    assert_eq!(east.hemisphere(), LongitudeHemisphere::East);

    // Zero belongs to the positive hemisphere, matching the wire.
    assert_eq!(
        Latitude::new(0).expect("valid").hemisphere(),
        LatitudeHemisphere::North
    );
    assert_eq!(
        Longitude::new(0).expect("valid").hemisphere(),
        LongitudeHemisphere::East
    );
}

#[test]
fn degrees_minutes_reconstructs_the_original_value() {
    // Totality and exactness: the split must lose nothing, at every
    // representable coordinate.
    for hundredths in (-540_000..=540_000).step_by(719) {
        let lat =
            Latitude::new(hundredths * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).expect("in range");
        let dm = lat.degrees_minutes();
        let magnitude = i64::from(dm.degrees) * 6000 + i64::from(dm.hundredths_of_minute);
        let signed = match lat.hemisphere() {
            LatitudeHemisphere::North => magnitude,
            LatitudeHemisphere::South => -magnitude,
        };
        assert_eq!(signed, hundredths);
        assert!(dm.hundredths_of_minute < 6000);
    }
    for hundredths in (-1_080_000..=1_080_000).step_by(1439) {
        let lon =
            Longitude::new(hundredths * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).expect("in range");
        let dm = lon.degrees_minutes();
        let magnitude = i64::from(dm.degrees) * 6000 + i64::from(dm.hundredths_of_minute);
        let signed = match lon.hemisphere() {
            LongitudeHemisphere::East => magnitude,
            LongitudeHemisphere::West => -magnitude,
        };
        assert_eq!(signed, hundredths);
        assert!(dm.hundredths_of_minute < 6000);
    }
}

// -------------------------------------------------------------- ambiguity

#[test]
fn ambiguity_is_part_of_the_position_not_the_report_format() {
    let exact = at(49.0583, -72.0292);
    assert!(exact.ambiguity.is_exact());
    let blurred = exact.with_ambiguity(Ambiguity::new(2).expect("valid"));
    assert_eq!(blurred.ambiguity.digits(), 2);
    // The coordinates themselves are untouched: ambiguity records how
    // precisely the sender reported, not a different position.
    assert_eq!(blurred.latitude, exact.latitude);
    assert_eq!(blurred.longitude, exact.longitude);
    // ...but it does take part in equality, because two reports of
    // different precision are not the same report.
    assert_ne!(blurred, exact);
}
