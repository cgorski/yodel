//! Geographic coordinates, grid squares, and integer-only geometry.
//!
//! Coordinates are stored fixed-point as signed **1/100 arc-minutes** —
//! the native resolution of the APRS uncompressed position format, so
//! nothing is lost passing through this module — with north and east
//! positive.
//!
//! # The Maidenhead Locator System
//!
//! Devised by John Morris, G4ANB, and adopted at the IARU Region 1 VHF
//! Working Group meeting held in Maidenhead, England, in 1980; in
//! general amateur use from 1 January 1986. No original publication by
//! its author appears to survive online, so the citable description is
//! the IARU's:
//!
//! > International Amateur Radio Union Region 1, "IARU-R1 VHF Handbook",
//! > Version 9.00, November 2020, "The Locator System".
//! > <https://www.rsgbcc.org/vhf/VHF_Handbook_V9.00.pdf>
//!
//! That is the source for the field/square/subsquare structure and its
//! 18 / 10 / 24 alphabet progression as implemented here.
//!
//! This module lives at the crate root rather than under `aprs` because
//! it is not an APRS concept: WSPR and FT8 exchange Maidenhead grid
//! squares, and [`crate::units`] would have to point into `aprs` to
//! express a distance. Like `units` it is integer-only, allocation-free
//! and not feature-gated.
//!
//! # What is absent
//!
//! An exact haversine. It needs transcendental functions, which in
//! `no_std` means a `libm` dependency this crate does not have. The
//! equirectangular approximation below states its error bound and is
//! tested against an `f64` haversine reference *in the test only*;
//! refusing to answer at all would simply push every user into writing
//! the same approximation in float, less carefully.

use crate::units::{Bearing, Distance};

/// Storage units per degree.
///
/// Coordinates are a signed integer count of this unit. The number is
/// chosen so that **every position format APRS carries is stored
/// exactly**, with no rounding at any point, which is what makes a
/// decoded position re-encodable to the bytes it arrived as.
///
/// Each format pins a denominator that must divide this constant:
///
/// | format | units per degree it needs |
/// |---|---:|
/// | uncompressed `DDMM.hh`, Mic-E | 6 000 |
/// | `!DAO!` decimal | 60 000 |
/// | `!DAO!` base-91 | 546 000 |
/// | NMEA `ddmm.mmmm` | 600 000 |
/// | compressed base-91 latitude | 380 926 |
/// | compressed base-91 longitude | 190 463 |
///
/// The least common multiple of those six is 114 277 800 000. This
/// constant is **3000 times** that, for two reasons the six-format list
/// does not show:
///
/// * **NMEA does not fix its decimal count.** The standard defines the
///   field as a variable number of digits of decimal minutes, and
///   current GNSS receivers emit five by default and up to seven. Five
///   places needs 6 000 000 per degree, which is `2^7 * 3 * 5^6`, and
///   the six-format LCM carries only `2^6` and `5^5`, so it does not
///   divide. Multiplying by 1000 covers up to seven places.
/// * **A factor of three** additionally makes whole arc-seconds and
///   1/16 arc-second exact, closing a family of grids APRS does not use
///   today for the price of one factor.
///
/// 180 degrees is 6.17e16, so `i64` keeps 149x of headroom, and one
/// unit is 0.32 nanometres. The resolution is not the point and is a
/// side effect; exactness is the point.
pub const UNITS_PER_DEGREE: i64 = 342_833_400_000_000;

/// Storage units in one arc-minute.
pub const UNITS_PER_MINUTE: i64 = UNITS_PER_DEGREE / 60;

/// Storage units in one 1/100 arc-minute, the resolution of the
/// uncompressed APRS position format and of Mic-E.
pub const UNITS_PER_HUNDREDTH_MINUTE: i64 = UNITS_PER_MINUTE / 100;

/// Maximum latitude magnitude in storage units (90 degrees).
pub(crate) const LAT_MAX: i64 = 90 * UNITS_PER_DEGREE;
/// Maximum longitude magnitude in storage units (180 degrees).
pub(crate) const LON_MAX: i64 = 180 * UNITS_PER_DEGREE;

/// Micrometres per storage unit, as an exact rational `NUM / DEN`.
///
/// A single integer will not do, because one unit is 0.32 nanometres
/// and micrometres per unit is not a whole number. The rational is
/// exact: the nautical mile is *defined* as one arc-minute of latitude
/// (1852 m), so one degree is 111 120 000 000 µm, and reducing that
/// against [`UNITS_PER_DEGREE`] by their common factor of 120 000 000
/// gives these two small numbers. One arc-minute comes back as exactly
/// 1 852 000 000 µm, which is checkable by hand and is asserted in
/// `tests/coordinate_paths.rs`.
const UM_NUM: i128 = 926;
/// Denominator of [`UM_NUM`].
const UM_DEN: i128 = 2_856_945;

/// Q15 unity **as the shared sine table stores it**.
///
/// `crate::types::SINE_I16` holds `round(sin · 32767)`, so `cos_q15`
/// returns 32767 for a zero angle and not `1 << 15`. Both axes of a
/// displacement must be scaled by *this* value, and it must be the same
/// value that is divided back out at the end. Mixing it with `1 << 15`
/// puts the two axes in different units: the east-west component then
/// comes back short by exactly one part in 32768 (a relative 3.05e-5,
/// which is 305 m across a quarter of the equator) while its
/// north-south twin — which the same geometry gives *identically* at
/// the equator — is exact.
///
/// Two alternatives were considered and rejected. Please do not
/// reintroduce either:
///
/// * **Scale `cos_q15` up to 32768 instead.** An `i16` table physically
///   cannot hold 32768, so this means a multiply-and-shift inside
///   `cos_q15`, re-quantising the cosine on every call to buy back a
///   power of two that nothing needs.
/// * **Keep the shift because a shift is cheaper than a divide.** True,
///   but this divide runs once per user-facing query, not once per
///   sample; the modem's hot paths are elsewhere. Correctness wins.
const COS_Q15_ONE: i64 = 32_767;

/// Failure of a validated geographic constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GeoError {
    /// A latitude beyond 90 degrees from the equator.
    BadLatitude {
        /// The rejected value, in [`UNITS_PER_DEGREE`] units.
        got: i64,
    },
    /// A longitude beyond 180 degrees from Greenwich.
    BadLongitude {
        /// The rejected value, in [`UNITS_PER_DEGREE`] units.
        got: i64,
    },
    /// A position ambiguity outside `0..=4` masked digits.
    BadAmbiguity {
        /// The rejected digit count.
        got: u8,
    },
    /// A Maidenhead locator whose length is not 4, 6 or 8.
    BadGridLength {
        /// The rejected length, in characters.
        got: usize,
    },
    /// A Maidenhead locator character outside its position's alphabet.
    BadGridChar {
        /// The rejected byte.
        got: u8,
        /// Its zero-based offset in the locator.
        position: usize,
    },
}

impl core::fmt::Display for GeoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadLatitude { got } => {
                write!(f, "latitude {got} (storage units) exceeds 90 degrees")
            }
            Self::BadLongitude { got } => {
                write!(f, "longitude {got} (storage units) exceeds 180 degrees")
            }
            Self::BadAmbiguity { got } => {
                write!(f, "position ambiguity {got} is outside 0..=4 digits")
            }
            Self::BadGridLength { got } => {
                write!(f, "Maidenhead locator length {got} is not 4, 6 or 8")
            }
            Self::BadGridChar { got, position } => write!(
                f,
                "byte {got:#04x} at offset {position} is not valid in a Maidenhead locator"
            ),
        }
    }
}

impl core::error::Error for GeoError {}

/// Which side of the equator a latitude is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatitudeHemisphere {
    /// North of the equator; the wire letter is `N`.
    North,
    /// South of the equator; the wire letter is `S`.
    South,
}

impl LatitudeHemisphere {
    /// The APRS wire letter, `b'N'` or `b'S'`.
    #[must_use]
    pub const fn letter(self) -> u8 {
        match self {
            Self::North => b'N',
            Self::South => b'S',
        }
    }
}

/// Which side of the prime meridian a longitude is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LongitudeHemisphere {
    /// East of Greenwich; the wire letter is `E`.
    East,
    /// West of Greenwich; the wire letter is `W`.
    West,
}

impl LongitudeHemisphere {
    /// The APRS wire letter, `b'E'` or `b'W'`.
    #[must_use]
    pub const fn letter(self) -> u8 {
        match self {
            Self::East => b'E',
            Self::West => b'W',
        }
    }
}

/// A coordinate magnitude as degrees and decimal minutes.
///
/// The form a radio's screen and every APRS client show, and the form
/// the APRS wire uses: `4903.50N` is 49 degrees and 3.50 minutes north.
/// Unsigned, because the sign belongs to the hemisphere -- writing
/// `-72.0292` as `72 degrees 1.75 minutes WEST` is exactly the step
/// that goes wrong when each caller reimplements it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DegreesMinutes {
    /// Whole degrees, `0..=90` for a latitude and `0..=180` for a
    /// longitude.
    pub degrees: u16,
    /// Arc-minutes in hundredths, `0..=5999`: 3.50 minutes is `350`.
    pub hundredths_of_minute: u16,
}

/// A geographic coordinate pair, with the precision it was reported to.
///
/// Returned wherever this crate hands back a location, in preference to
/// a `(Latitude, Longitude)` tuple. Transposing the two is one of the
/// most common defect classes in geospatial code, and a tuple offers a
/// reader nothing but positional convention — `pos.0` and `pos.1` are
/// equally plausible either way round at the call site.
///
/// Note that [`Latitude`] and [`Longitude`] are already distinct types,
/// so a transposed *constructor* call has always been a compile error.
/// What this struct adds is readable access and a name to pass around:
///
/// ```
/// use yodel::geo::{Coordinates, Latitude, Longitude};
///
/// let here = Coordinates::new(
///     Latitude::from_degrees(49.0583)?,
///     Longitude::from_degrees(-72.0292)?,
/// );
/// assert!((here.latitude.to_degrees() - 49.0583).abs() < 1e-4);
/// assert!((here.longitude.to_degrees() - -72.0292).abs() < 1e-4);
/// # Ok::<(), yodel::geo::GeoError>(())
/// ```
///
/// [`Self::ambiguity`] qualifies the *precision of the measurement*
/// rather than the format it arrived in, which is why it lives here and
/// not on the report types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coordinates {
    /// Degrees north of the equator.
    pub latitude: Latitude,
    /// Degrees east of the prime meridian.
    pub longitude: Longitude,
    /// How many low-order digits the sender masked out.
    pub ambiguity: Ambiguity,
}

impl Coordinates {
    /// Pairs a latitude with a longitude, reported exactly.
    #[must_use]
    pub const fn new(latitude: Latitude, longitude: Longitude) -> Self {
        Self {
            latitude,
            longitude,
            ambiguity: Ambiguity::EXACT,
        }
    }

    /// Returns the pair with the given reporting ambiguity.
    #[must_use]
    pub const fn with_ambiguity(self, ambiguity: Ambiguity) -> Self {
        Self { ambiguity, ..self }
    }

    /// The six-character Maidenhead locator containing this position.
    #[must_use]
    pub const fn maidenhead(self) -> MaidenheadGrid {
        self.maidenhead_with_precision(GridPrecision::Subsquare)
    }

    /// The Maidenhead locator containing this position, at the given
    /// precision.
    #[must_use]
    pub const fn maidenhead_with_precision(self, precision: GridPrecision) -> MaidenheadGrid {
        let len = precision.characters() as u8;
        // Shift both axes to be non-negative so every division floors
        // the way the grid is numbered, from the south-west corner.
        let mut lat = self.latitude.0 + LAT_MAX;
        let mut lon = self.longitude.0 + LON_MAX;
        // The two axes need opposite treatment at their top edge, and
        // both would otherwise index a nineteenth field ('S', which does
        // not exist).
        //
        // Longitude wraps: +180 and -180 name the same meridian, so it
        // belongs in field 'A' alongside -180.
        if lon >= 2 * LON_MAX {
            lon -= 2 * LON_MAX;
        }
        // Latitude cannot wrap -- there is nothing north of the pole --
        // so the top edge is clamped into the last square instead.
        if lat >= 2 * LAT_MAX {
            lat = 2 * LAT_MAX - 1;
        }

        // Every cell size is named in storage units rather than
        // written as a literal, because a bare divisor here is a unit
        // baked into a number that no compiler or lint can check.
        let lon_field = 20 * UNITS_PER_DEGREE;
        let lat_field = 10 * UNITS_PER_DEGREE;
        let lon_square = 2 * UNITS_PER_DEGREE;
        let lat_square = UNITS_PER_DEGREE;
        let lon_sub = lon_square / 24;
        let lat_sub = lat_square / 24;

        let mut chars = [0u8; 8];
        chars[0] = b'A' + (lon / lon_field) as u8;
        chars[1] = b'A' + (lat / lat_field) as u8;
        chars[2] = b'0' + (lon % lon_field / lon_square) as u8;
        chars[3] = b'0' + (lat % lat_field / lat_square) as u8;
        if len >= 6 {
            chars[4] = b'a' + (lon % lon_square / lon_sub) as u8;
            chars[5] = b'a' + (lat % lat_square / lat_sub) as u8;
        }
        if len >= 8 {
            chars[6] = b'0' + (lon % lon_sub * 10 / lon_sub) as u8;
            chars[7] = b'0' + (lat % lat_sub * 10 / lat_sub) as u8;
        }
        MaidenheadGrid { chars, len }
    }

    /// The coordinates at the centre of a Maidenhead locator's square.
    ///
    /// The centre, not the corner: a locator names an area, and its
    /// centre is the point that minimises the worst-case error of
    /// treating it as a position.
    #[must_use]
    pub const fn from_maidenhead(grid: MaidenheadGrid) -> Self {
        let chars = grid.chars;
        let lon_square = 2 * UNITS_PER_DEGREE;
        let lat_square = UNITS_PER_DEGREE;
        let lon_sub = lon_square / 24;
        let lat_sub = lat_square / 24;
        let mut lon = (chars[0] - b'A') as i64 * 20 * UNITS_PER_DEGREE
            + (chars[2] - b'0') as i64 * lon_square;
        let mut lat = (chars[1] - b'A') as i64 * 10 * UNITS_PER_DEGREE
            + (chars[3] - b'0') as i64 * lat_square;
        // Half of the smallest addressed cell, added last, is what makes
        // this the centre rather than the south-west corner.
        let (lon_cell, lat_cell) = if grid.len >= 6 {
            lon += (chars[4] - b'a') as i64 * lon_sub;
            lat += (chars[5] - b'a') as i64 * lat_sub;
            if grid.len == 8 {
                lon += (chars[6] - b'0') as i64 * (lon_sub / 10);
                lat += (chars[7] - b'0') as i64 * (lat_sub / 10);
                (lon_sub / 10, lat_sub / 10)
            } else {
                (lon_sub, lat_sub)
            }
        } else {
            (lon_square, lat_square)
        };
        lon += lon_cell / 2;
        lat += lat_cell / 2;
        Self {
            latitude: Latitude(lat - LAT_MAX),
            longitude: Longitude(lon - LON_MAX),
            ambiguity: Ambiguity::EXACT,
        }
    }

    /// The great-circle-ish distance to another position.
    ///
    /// # Accuracy
    ///
    /// An **equirectangular** approximation: the longitude difference is
    /// scaled by the cosine of the mean latitude and the result taken as
    /// a plane triangle. There are two error terms, and which one
    /// dominates depends on where you are, not just how far:
    ///
    /// * **The projection.** About `(d/R)²/24` along a meridian or the
    ///   equator — 0.009% at 300 km. That textbook formula omits the
    ///   term that dominates elsewhere: `cos φ` varies along the path,
    ///   so the error also grows with the latitude *spread* and with
    ///   `tan φ`, and runs away towards the poles.
    /// * **The integer cosine.** `cos_q15` answers in Q15 against a
    ///   table whose unity is 32767, and its absolute error is about
    ///   1 LSB, or 3.1e-5. That is a `3.1e-5 / cos φ` error in the
    ///   east-west component — a floor that does **not** shrink with
    ///   separation, and the dominant term at short range and at high
    ///   latitude. It is 3.1e-5 at the equator, 6e-5 at 60 degrees and
    ///   3.5e-4 at 85.
    ///
    ///   This term used to be 1.5 LSB and one-sided (always low),
    ///   because the interpolation floored. It is now centred, which
    ///   shortened the short-range column below and lengthened the
    ///   300 km one: the old bias had been cancelling part of the
    ///   projection error, which over-estimates east-west distance.
    ///   The cancellation was luck, not design, and it was worth least
    ///   exactly where this crate operates.
    ///
    /// Worst-case relative error against an `f64` haversine on the same
    /// sphere, swept over azimuth, latitude and separation. The bands
    /// are on the *magnitude* of the latitude, so they are symmetric
    /// about the equator, and each column is cumulative — "to 300 km"
    /// includes the short paths too. `tests/geo.rs` asserts exactly
    /// these numbers, and the sweep's own worst cases sit a few percent
    /// under them.
    ///
    /// | latitude | to 100 km | to 300 km |
    /// |---|---|---|
    /// | 0–45° | 0.005% | 0.016% |
    /// | 0–60° | 0.008% | 0.036% |
    /// | 0–75° | 0.022% | 0.15% |
    /// | 0–85° | 0.16% | 1.6% |
    ///
    /// So: better than **0.05% over any path a VHF APRS station can
    /// hear, up to 60 degrees** of latitude, which is where the crate is
    /// used — and explicitly not a polar tool. By 80 degrees a 300 km
    /// path is out by a third of a percent, by 85 degrees by one and a
    /// half, and past 85 the integer cosine alone costs up to 0.05% at
    /// *any* separation.
    ///
    /// A flat "better than 0.01% below 300 km" appears elsewhere in this
    /// repository's design notes (and in earlier revisions of this doc).
    /// It is the `(d/R)²/24` term evaluated at 300 km on the equator,
    /// quoted as though it were universal; the table above is what the
    /// implementation delivers.
    ///
    /// The two axes are **isotropic**: at the equator, where `cos φ` is
    /// unity, one arc-minute of longitude returns bit-for-bit the same
    /// distance as one arc-minute of latitude. The `COS_Q15_ONE`
    /// constant in this module records why that needs saying, and what
    /// it cost when it was not true.
    ///
    /// # Which Earth
    ///
    /// The result is on a sphere of radius **6366.707 km**, because one
    /// hundredth of an arc-minute of latitude is taken as exactly
    /// 18.52 m — the nautical mile is *defined* as one arc-minute of
    /// latitude (1852 m), which fixes the radius at
    /// `1852 · 60 · 180 / π`. A calculator using the more common mean
    /// radius of 6371 km will therefore read **0.067% larger**. That is
    /// a choice, not an error: it makes one arc-minute of latitude come
    /// back as exactly one nautical mile, which is checkable by hand and
    /// is the convention every marine and aviation chart uses.
    ///
    /// Integer throughout: the geometry runs in hundredths of an
    /// arc-minute and converts to a [`Distance`] exactly once at the
    /// end.
    ///
    /// ```
    /// use yodel::geo::{Coordinates, Latitude, Longitude};
    ///
    /// let here = Coordinates::new(Latitude::from_degrees(49.0)?, Longitude::from_degrees(-72.0)?);
    /// let north = Coordinates::new(Latitude::from_degrees(49.1)?, Longitude::from_degrees(-72.0)?);
    /// // A tenth of a degree of latitude is 6 arc-minutes: 11.112 km.
    /// assert_eq!(here.distance_to(north).meters(), 11_112);
    /// # Ok::<(), yodel::geo::GeoError>(())
    /// ```
    #[must_use]
    pub fn distance_to(self, other: Self) -> Distance {
        let (east, north) = self.displacement(other);
        // Both components are storage units, bounded by 180 degrees =
        // 6.17e16, so squaring one needs more than i64 and the sum of
        // squares is taken in i128. It peaks at 7.6e33 against i128's
        // 1.7e38, leaving 22 339x of headroom.
        let (east, north) = (i128::from(east), i128::from(north));
        #[allow(clippy::cast_sign_loss)] // squares are non-negative
        let sum_of_squares = (east * east + north * north) as u128;
        #[allow(clippy::cast_possible_truncation)] // bounded by 8.7e16
        let magnitude = sum_of_squares.isqrt() as i128;
        // Micrometres per unit is not a whole number, so the conversion
        // is an exact rational. The multiply reaches 8.1e19 and must
        // therefore happen in i128 before the divide; doing it in i64
        // would wrap silently. Rounding to nearest, and the magnitude is
        // an isqrt result so it is never negative.
        #[allow(clippy::cast_possible_truncation)] // bounded by 2.9e13
        let micrometers = ((magnitude * UM_NUM + UM_DEN / 2) / UM_DEN) as i64;
        Distance::from_micrometers(micrometers)
    }

    /// The initial compass bearing towards another position.
    ///
    /// # Accuracy
    ///
    /// Exact to the nearest whole degree *for the equirectangular
    /// model*, because the result is found by searching the 360 whole
    /// degrees rather than by an `atan2` approximation — with one
    /// caveat: the east-west component carries `cos_q15`'s
    /// `3.1e-5 / cos φ` error (see [`Coordinates::distance_to`]), which
    /// tilts the direction by about 0.02 degrees even at 85 degrees of
    /// latitude. That is far inside the one-degree quantum, but it can
    /// pick the other neighbour for a direction sitting within 0.02
    /// degrees of a half-degree boundary.
    ///
    /// "Exact" is a recent claim. The candidate vectors came from the
    /// private `sine_at` helper, which truncates the phase to a table
    /// index, so candidate `d` sat up to 0.088 degrees below its
    /// nominal angle. Sin and cos share that index, so each candidate
    /// stayed a coherent unit vector — simply at the wrong angle — and
    /// every half-degree decision boundary moved with it. Measured over
    /// 3240 directions, 28 came back as the neighbouring degree, and
    /// unlike the `cos_q15` tilt above it happened at the equator too.
    /// The search now interpolates;
    /// `geo::tests::bearing_to_returns_the_nearest_whole_degree` pins it.
    ///
    /// The model itself, however, yields the **mean** course over the
    /// path rather than the **initial** great-circle bearing, and the
    /// two differ by roughly `Δλ · sin φ / 2` — the convergence of the
    /// meridians. At VHF APRS separations (under ~100 km, so `Δλ` under
    /// about 1.5 degrees at mid latitudes) that is below 0.5 degrees; at
    /// 570 km it is nearer 2 degrees. `tests/geo.rs` asserts that
    /// predicted difference rather than a hand-tuned tolerance.
    ///
    /// Two identical positions have no bearing between them; the result
    /// is then due north.
    #[must_use]
    pub fn bearing_to(self, other: Self) -> Bearing {
        let (east, north) = self.displacement(other);
        if east == 0 && north == 0 {
            return Bearing::NORTH;
        }
        // Displacements reach 6.17e16 storage units and the sine table
        // runs to 32 767, so the dot products below reach 2.0e21 and do
        // need widening: in i64 they would wrap.
        let (east, north) = (i128::from(east), i128::from(north));
        // The bearing is the angle whose unit vector (sin, cos) is most
        // nearly parallel to the displacement, i.e. the one maximising
        // the dot product. Searching all 360 candidates is O(1) in the
        // only sense that matters here (this is a user-called
        // convenience, not a per-sample path) and removes the need to
        // bound an atan2 approximation's error.
        let mut best_degrees = 0u16;
        let mut best_dot = i128::MIN;
        for degrees in 0..360u16 {
            let phase = (u64::from(degrees) << 32) / 360;
            #[allow(clippy::cast_possible_truncation)] // masked to 32 bits
            let phase = phase as u32;
            // Interpolated, not the bare table lookup: truncating the
            // phase to a table index would place each candidate up to
            // 0.088 degrees below its nominal angle, which shifts every
            // half-degree decision boundary and hands back the
            // neighbouring degree for roughly one direction in a
            // hundred. `bearing_to_returns_the_nearest_whole_degree`
            // pins it.
            let sin = i128::from(crate::types::sine_at_interpolated(phase));
            // cos θ = sin(θ + 90°), and 90 degrees is a quarter of the
            // phase accumulator's full turn.
            let cos = i128::from(crate::types::sine_at_interpolated(
                phase.wrapping_add(1 << 30),
            ));
            let dot = east * sin + north * cos;
            if dot > best_dot {
                best_dot = dot;
                best_degrees = degrees;
            }
        }
        Bearing::new(best_degrees).unwrap_or(Bearing::NORTH)
    }

    /// The east and north displacement to `other` in storage units,
    /// with the eastward component scaled by the cosine of the mean
    /// latitude so the two are directly comparable.
    ///
    /// "Directly comparable" is the whole point: both callers,
    /// [`Coordinates::distance_to`] taking a magnitude and
    /// [`Coordinates::bearing_to`] taking a ratio, are wrong if the two
    /// axes are in different units.
    ///
    /// # Why the cosine is divided straight back out
    ///
    /// This function used to return both components still scaled by
    /// [`COS_Q15_ONE`], so that dividing back down could not quantise
    /// the result to a whole 1/100 arc-minute, which at 7 km would have
    /// been a 0.13% error and worse than the projection error the
    /// accuracy claim is about.
    ///
    /// That reasoning was correct for a coordinate unit of 18.55 m and
    /// is obsolete at 0.32 nanometres. Dividing back to units now costs
    /// at most one unit, and keeping Q15 would cost correctness: the
    /// components would reach 2.0e21, the sum of their squares 8.2e42,
    /// and `i128` stops at 1.7e38.
    fn displacement(self, other: Self) -> (i64, i64) {
        let north = other.latitude.0 - self.latitude.0;
        let mut east = other.longitude.0 - self.longitude.0;
        // Take the short way round: +179 to -179 is two degrees east,
        // not 358 degrees west.
        let full_turn = 2 * LON_MAX;
        if east > LON_MAX {
            east -= full_turn;
        } else if east < -LON_MAX {
            east += full_turn;
        }
        let mean_latitude = (self.latitude.0 + other.latitude.0) / 2;
        // 6.17e16 x 32 767 is 2.0e21, so the scaling happens in i128 and
        // the result is back inside i64 by construction, the cosine
        // never exceeding unity.
        #[allow(clippy::cast_possible_truncation)]
        let east = ((i128::from(east) * i128::from(cos_q15(mean_latitude)))
            / i128::from(COS_Q15_ONE)) as i64;
        (east, north)
    }
}

// NOT provided on `Coordinates`:
//
// * `to_degrees(self) -> (f64, f64)` — a tuple-returning convenience
//   reintroduces exactly the ambiguity this type exists to remove. Two
//   `f64`s in a tuple are mutually assignable, so a transposed
//   destructuring compiles silently and yields a position in the wrong
//   hemisphere. Write `c.latitude.to_degrees()` and
//   `c.longitude.to_degrees()` — longer, but wrong code then fails to
//   compile instead of flying a station into the sea.
// * `From<(Latitude, Longitude)>` / `Into` — same reasoning. Every
//   construction goes through `Coordinates::new`, whose two parameters
//   have distinct types, so the compiler rejects a swap.

/// A latitude, stored as signed [`UNITS_PER_DEGREE`] units north of the
/// equator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Latitude(pub(crate) i64);

/// A longitude, stored as signed [`UNITS_PER_DEGREE`] units east of
/// Greenwich.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Longitude(pub(crate) i64);

impl Latitude {
    /// Creates a latitude from signed storage units (north positive).
    ///
    /// See [`UNITS_PER_DEGREE`] for what a unit is, and
    /// [`Self::from_degrees`] for the convenient way in.
    ///
    /// # Errors
    ///
    /// [`GeoError::BadLatitude`] when the magnitude exceeds 90 degrees.
    pub const fn new(units: i64) -> Result<Self, GeoError> {
        if units < -LAT_MAX || units > LAT_MAX {
            Err(GeoError::BadLatitude { got: units })
        } else {
            Ok(Self(units))
        }
    }

    /// The value in signed storage units (north positive).
    #[must_use]
    pub const fn units(self) -> i64 {
        self.0
    }

    /// The value in degrees, as a convenience `f64` conversion.
    ///
    /// The only floating point in this module, and additive: the
    /// integer path ([`Self::new`], [`Self::units`],
    /// [`Self::from_degrees_minutes`]) is complete, so nothing forces a
    /// no-FPU target through soft float.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // 149x under f64's exact range
    pub fn to_degrees(self) -> f64 {
        self.0 as f64 / UNITS_PER_DEGREE as f64
    }

    /// Creates a latitude from degrees, rounding to the storage unit.
    ///
    /// # Errors
    ///
    /// [`GeoError::BadLatitude`] when out of `-90.0..=90.0` (or not
    /// finite).
    pub fn from_degrees(degrees: f64) -> Result<Self, GeoError> {
        Self::new(round_scaled(degrees).ok_or(GeoError::BadLatitude { got: i64::MAX })?)
    }

    /// Creates a latitude from the form the APRS wire uses: whole
    /// degrees, hundredths of an arc-minute, and a hemisphere.
    ///
    /// The exact inverse of [`Self::degrees_minutes`] paired with
    /// [`Self::hemisphere`], and the constructor to reach for when the
    /// value came off the air or out of a `DDMM.hh` field.
    ///
    /// # Why this exists beside [`Self::from_degrees`]
    ///
    /// Storage is finer than 1/100 arc-minute, and `from_degrees` goes
    /// through `f64`, so a decimal literal lands a unit or two off the
    /// hundredths grid. That is invisible for most purposes and is not
    /// invisible when the value is then written to a format that
    /// carries hundredths: Mic-E and the uncompressed position both
    /// round on the way out, so `decode(encode(x))` returns a
    /// neighbouring grid point rather than `x`. Building on the grid in
    /// the first place makes that round trip an identity.
    ///
    /// The hemisphere is a separate argument rather than a sign,
    /// because that is how the wire carries it and because an unsigned
    /// magnitude cannot be given the wrong sign by accident.
    ///
    /// ```
    /// use yodel::geo::{Latitude, LatitudeHemisphere};
    ///
    /// // 33 degrees 25.64 minutes north.
    /// let lat = Latitude::from_degrees_minutes(33, 2564, LatitudeHemisphere::North)?;
    /// let dm = lat.degrees_minutes();
    /// assert_eq!((dm.degrees, dm.hundredths_of_minute), (33, 2564));
    /// # Ok::<(), yodel::geo::GeoError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`GeoError::BadLatitude`] beyond 90 degrees, which includes any
    /// `hundredths_of_minute` of 6000 or more.
    pub const fn from_degrees_minutes(
        degrees: u16,
        hundredths_of_minute: u16,
        hemisphere: LatitudeHemisphere,
    ) -> Result<Self, GeoError> {
        let magnitude = degrees as i64 * UNITS_PER_DEGREE
            + hundredths_of_minute as i64 * UNITS_PER_HUNDREDTH_MINUTE;
        match hemisphere {
            LatitudeHemisphere::North => Self::new(magnitude),
            LatitudeHemisphere::South => Self::new(-magnitude),
        }
    }

    /// Which side of the equator this latitude is on.
    ///
    /// The equator itself reports [`LatitudeHemisphere::North`], which
    /// is the convention the wire format follows.
    #[must_use]
    pub const fn hemisphere(self) -> LatitudeHemisphere {
        if self.0 < 0 {
            LatitudeHemisphere::South
        } else {
            LatitudeHemisphere::North
        }
    }

    /// The magnitude as degrees and decimal minutes, for display.
    ///
    /// Pair it with [`Self::hemisphere`]: together they are exactly what
    /// a radio shows and what the APRS wire carries.
    ///
    /// ```
    /// use yodel::geo::{Latitude, LatitudeHemisphere};
    ///
    /// let lat = Latitude::from_degrees(49.0583)?;
    /// let dm = lat.degrees_minutes();
    /// assert_eq!(dm.degrees, 49);
    /// assert_eq!(dm.hundredths_of_minute, 350); // 3.50 arc-minutes
    /// assert_eq!(lat.hemisphere(), LatitudeHemisphere::North);
    /// # Ok::<(), yodel::geo::GeoError>(())
    /// ```
    #[must_use]
    pub const fn degrees_minutes(self) -> DegreesMinutes {
        degrees_minutes(self.0)
    }
}

impl Longitude {
    /// Creates a longitude from signed storage units (east positive).
    ///
    /// See [`UNITS_PER_DEGREE`] for what a unit is, and
    /// [`Self::from_degrees`] for the convenient way in.
    ///
    /// # Errors
    ///
    /// [`GeoError::BadLongitude`] when the magnitude exceeds 180
    /// degrees.
    pub const fn new(units: i64) -> Result<Self, GeoError> {
        if units < -LON_MAX || units > LON_MAX {
            Err(GeoError::BadLongitude { got: units })
        } else {
            Ok(Self(units))
        }
    }

    /// The value in signed storage units (east positive).
    #[must_use]
    pub const fn units(self) -> i64 {
        self.0
    }

    /// The value in degrees, as a convenience `f64` conversion.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // 149x under f64's exact range
    pub fn to_degrees(self) -> f64 {
        self.0 as f64 / UNITS_PER_DEGREE as f64
    }

    /// Creates a longitude from degrees, rounding to the storage unit.
    ///
    /// # Errors
    ///
    /// [`GeoError::BadLongitude`] when out of `-180.0..=180.0` (or not
    /// finite).
    pub fn from_degrees(degrees: f64) -> Result<Self, GeoError> {
        Self::new(round_scaled(degrees).ok_or(GeoError::BadLongitude { got: i64::MAX })?)
    }

    /// Creates a longitude from whole degrees, hundredths of an
    /// arc-minute, and a hemisphere.
    ///
    /// The exact inverse of [`Self::degrees_minutes`] paired with
    /// [`Self::hemisphere`]. See
    /// [`Latitude::from_degrees_minutes`] for why this exists beside
    /// [`Self::from_degrees`].
    ///
    /// ```
    /// use yodel::geo::{Longitude, LongitudeHemisphere};
    ///
    /// // 112 degrees 07.00 minutes west.
    /// let lon = Longitude::from_degrees_minutes(112, 700, LongitudeHemisphere::West)?;
    /// let dm = lon.degrees_minutes();
    /// assert_eq!((dm.degrees, dm.hundredths_of_minute), (112, 700));
    /// # Ok::<(), yodel::geo::GeoError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`GeoError::BadLongitude`] beyond 180 degrees.
    pub const fn from_degrees_minutes(
        degrees: u16,
        hundredths_of_minute: u16,
        hemisphere: LongitudeHemisphere,
    ) -> Result<Self, GeoError> {
        let magnitude = degrees as i64 * UNITS_PER_DEGREE
            + hundredths_of_minute as i64 * UNITS_PER_HUNDREDTH_MINUTE;
        match hemisphere {
            LongitudeHemisphere::East => Self::new(magnitude),
            LongitudeHemisphere::West => Self::new(-magnitude),
        }
    }

    /// Which side of the prime meridian this longitude is on.
    ///
    /// Greenwich itself reports [`LongitudeHemisphere::East`], which is
    /// the convention the wire format follows.
    #[must_use]
    pub const fn hemisphere(self) -> LongitudeHemisphere {
        if self.0 < 0 {
            LongitudeHemisphere::West
        } else {
            LongitudeHemisphere::East
        }
    }

    /// The magnitude as degrees and decimal minutes, for display.
    ///
    /// Pair it with [`Self::hemisphere`].
    ///
    /// ```
    /// use yodel::geo::{Longitude, LongitudeHemisphere};
    ///
    /// let lon = Longitude::from_degrees(-72.0292)?;
    /// let dm = lon.degrees_minutes();
    /// assert_eq!(dm.degrees, 72);
    /// assert_eq!(dm.hundredths_of_minute, 175); // 1.75 arc-minutes
    /// assert_eq!(lon.hemisphere(), LongitudeHemisphere::West);
    /// # Ok::<(), yodel::geo::GeoError>(())
    /// ```
    #[must_use]
    pub const fn degrees_minutes(self) -> DegreesMinutes {
        degrees_minutes(self.0)
    }
}

/// Splits a signed 1/100 arc-minute value into degrees and minutes.
///
/// The sign is dropped: it belongs to the hemisphere.
/// Splits a signed storage-unit value into degrees and minutes.
///
/// The sign is dropped: it belongs to the hemisphere.
///
/// The storage unit is finer than the 1/100 arc-minute this reports, so
/// the conversion rounds to nearest rather than truncating. Truncation
/// would throw away up to a whole hundredth (18.55 m) where rounding
/// throws away at most half of one. Rounding the **total** first, then
/// splitting, is what keeps a value that rounds up to a full 60 minutes
/// from reporting `59.100` minutes: the carry lands in the degrees on
/// its own.
const fn degrees_minutes(units: i64) -> DegreesMinutes {
    let magnitude = units.unsigned_abs();
    let half = UNITS_PER_HUNDREDTH_MINUTE.unsigned_abs() / 2;
    let total = (magnitude + half) / UNITS_PER_HUNDREDTH_MINUTE.unsigned_abs();
    #[allow(clippy::cast_possible_truncation)] // bounded by 180 and 5999
    DegreesMinutes {
        degrees: (total / 6000) as u16,
        hundredths_of_minute: (total % 6000) as u16,
    }
}

/// The cosine of a latitude, in Q15, by interpolating the shared sine
/// table.
///
/// Unity is [`COS_Q15_ONE`] — 32767, not 32768 — because that is what
/// the table holds. Callers must scale their other axis to match.
///
/// # Two error terms, and only one of them is small
///
/// **Angle quantisation, dealt with.** The bare table has 4096 entries
/// over a full turn, so a nearest-entry lookup quantises the angle to
/// 0.088 degrees. At mid latitudes that is a **0.09% error in the
/// cosine** (`tan φ · Δφ`), which would swamp everything else. Linear
/// interpolation between neighbouring entries drops that residual to the
/// table's curvature term, around 3e-7, which really is negligible.
///
/// **Output quantisation, irreducible.** What interpolation cannot fix is
/// that the answer is an integer. The table entries are themselves
/// `round(sin · 32767)`, so ±0.5 LSB, and the rounded interpolation in
/// [`crate::types::sine_at_interpolated`] adds at most another half.
/// About **1 LSB, or 3.1e-5 absolute** — still far above the
/// interpolation residual, and therefore the term that matters. Because
/// it is absolute, the *relative* error is `3.1e-5 / cos φ`: negligible
/// at the equator, 6e-5 at 60 degrees, 3.5e-4 at 85. It is what sets the
/// high-latitude floor in [`Coordinates::distance_to`]'s accuracy table,
/// and no amount of care in the interpolation removes it — only a wider
/// output would.
///
/// The interpolation used to arithmetic-shift the product, which floors.
/// The delta keeps one sign across a quarter turn, so that cost was
/// one-sided: a measured mean of -0.49 LSB, which read as a short
/// east-west distance every time rather than as noise. `cos_q15_is_not_biased`
/// pins the centred version.
fn cos_q15(latitude_units: i64) -> i64 {
    // cos is even, so the sign of the latitude does not matter.
    let turn = 360 * UNITS_PER_DEGREE.unsigned_abs();
    // The shift would overflow u64 at this unit (6.2e16 << 32 is 2.6e26),
    // so the phase reduction happens in u128.
    #[allow(clippy::cast_possible_truncation)] // reduced modulo one turn
    let phase =
        ((u128::from(latitude_units.unsigned_abs() % turn) << 32) / u128::from(turn)) as u64;
    // A quarter turn of phase past the angle is its cosine.
    let phase = (phase as u32).wrapping_add(1 << 30);
    i64::from(crate::types::sine_at_interpolated(phase))
}

/// Rounds `degrees * 6000` to the nearest integer without `std`.
///
/// Returns `None` for non-finite or wildly out-of-range input; the
/// callers' range checks reject anything a `Some` sneaks past.
#[allow(clippy::cast_precision_loss)] // 149x under f64's exact range
fn round_scaled(degrees: f64) -> Option<i64> {
    let scaled = degrees * UNITS_PER_DEGREE as f64;
    let limit = 2.0 * LON_MAX as f64;
    if !(-limit..=limit).contains(&scaled) {
        return None;
    }
    let rounded = if scaled >= 0.0 {
        scaled + 0.5
    } else {
        scaled - 0.5
    };
    // In range and finite by the check above; truncation is the rounding.
    #[allow(clippy::cast_possible_truncation)]
    Some(rounded as i64)
}

/// How many low-order digits of a position the sender masked out.
///
/// APRS lets a station blur its position on purpose, by replacing
/// trailing coordinate digits with spaces — one digit is a tenth of an
/// arc-minute of vagueness, four is a whole degree. It is a property of
/// the *measurement*, not of the report format, which is why it sits on
/// [`Coordinates`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Ambiguity(u8);

impl Ambiguity {
    /// No masking: the position is reported to full resolution.
    pub const EXACT: Self = Self(0);

    /// Creates an ambiguity from a masked-digit count.
    ///
    /// # Errors
    ///
    /// [`GeoError::BadAmbiguity`] above 4: the wire format has only four
    /// maskable digits.
    pub const fn new(digits: u8) -> Result<Self, GeoError> {
        if digits > 4 {
            Err(GeoError::BadAmbiguity { got: digits })
        } else {
            Ok(Self(digits))
        }
    }

    /// The number of masked digits, `0..=4`.
    #[must_use]
    pub const fn digits(self) -> u8 {
        self.0
    }

    /// Whether the position was reported to full resolution.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        self.0 == 0
    }

    /// The coordinate step this level reports to, in storage units:
    /// the place value of the lowest digit still transmitted.
    ///
    /// Chapter 6 blanks the `DDMM.hh` digits from the right, so one
    /// masked digit leaves tenths of an arc-minute, two leaves whole
    /// minutes, three leaves tens of minutes and four leaves whole
    /// degrees. [`Ambiguity::EXACT`] reports a step of one unit, which
    /// makes [`Ambiguity::mask`] the identity rather than a special
    /// case.
    #[must_use]
    pub const fn step(self) -> i64 {
        match self.0 {
            1 => UNITS_PER_HUNDREDTH_MINUTE * 10,
            2 => UNITS_PER_MINUTE,
            3 => UNITS_PER_MINUTE * 10,
            // Four masked digits is a whole degree. Values above four
            // cannot be constructed: `new` rejects them.
            4 => UNITS_PER_DEGREE,
            _ => 1,
        }
    }

    /// Masks a coordinate to this level, the way chapter 6 defines it.
    ///
    /// # Why this is on `Ambiguity` and not on the parsers
    ///
    /// The rule is one rule and the wire has two ways of spelling it.
    /// An uncompressed report blanks latitude digits with spaces, and
    /// chapter 6 says outright that "the level of ambiguity specified
    /// in the latitude will automatically apply to the longitude as
    /// well — it is permissible but not necessary to include any
    /// space characters in the longitude". Mic-E cannot send a space
    /// at all, so it spells the same thing in the destination address
    /// and transmits the longitude at full precision regardless;
    /// chapter 10 says the receiver ignores the matching low-order
    /// digits. Both end up here.
    ///
    /// # It truncates the magnitude, not the value
    ///
    /// Blanking a digit is a text operation on `DDMM.hh`, which spells
    /// a magnitude beside a hemisphere letter. So 49 degrees 3.57
    /// minutes **south** blanked to the whole minute is 49 degrees 3
    /// minutes south, not 49 degrees 4 minutes south: the magnitude
    /// falls and the position moves *north*. Rounding the signed value
    /// toward zero is what does that, and it is the reason this is one
    /// function rather than a subtraction written out at each call
    /// site, where a southern hemisphere would eventually get it
    /// backwards.
    #[must_use]
    pub const fn mask(self, units: i64) -> i64 {
        let step = self.step();
        // `units / step` truncates toward zero in Rust, which is the
        // magnitude truncation chapter 6 describes, in both
        // hemispheres, with no sign handling of its own.
        (units / step) * step
    }
}

/// How finely a Maidenhead locator names a place.
///
/// An enum rather than a character count because only three lengths
/// exist: an integer parameter would have to either reject values or
/// silently pick one, and both are worse than not being able to ask the
/// question. This mirrors the crate's existing preference for named
/// variants over blind primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum GridPrecision {
    /// Four characters, e.g. `FN42` — a 1 by 2 degree square.
    Square,
    /// Six characters, e.g. `IO91wm` — a 2.5 by 5 arc-minute subsquare.
    /// The usual precision in conversation and the default.
    #[default]
    Subsquare,
    /// Eight characters, e.g. `IO91wm55` — a 15 by 30 arc-second cell.
    ExtendedSquare,
}

impl GridPrecision {
    /// The locator length in characters: 4, 6 or 8.
    #[must_use]
    pub const fn characters(self) -> usize {
        match self {
            Self::Square => 4,
            Self::Subsquare => 6,
            Self::ExtendedSquare => 8,
        }
    }
}

/// A Maidenhead locator (grid square) of 4, 6 or 8 characters.
///
/// The amateur radio convention for naming an area: two field letters
/// (`A`–`R`), two square digits, optionally two subsquare letters
/// (`a`–`x`), optionally two extended-square digits. `FN42` is a
/// 1° × 2° box; `IO91wm` narrows it to 2.5′ × 5′.
///
/// Stored canonically — fields upper case, subsquares lower — so two
/// grids naming the same square compare equal however they were typed.
///
/// ```
/// use yodel::geo::{Coordinates, GridPrecision, Latitude, Longitude, MaidenheadGrid};
///
/// let boston = Coordinates::new(
///     Latitude::from_degrees(42.5)?,
///     Longitude::from_degrees(-71.0)?,
/// );
/// assert_eq!(
///     boston.maidenhead_with_precision(GridPrecision::Square).as_str(),
///     "FN42"
/// );
/// assert_eq!(boston.maidenhead().to_string(), "FN42mm");
///
/// // Parsing is case-insensitive; storage is canonical.
/// assert_eq!(MaidenheadGrid::new("io91WM")?.as_str(), "IO91wm");
/// # Ok::<(), yodel::geo::GeoError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaidenheadGrid {
    /// The canonical characters; only the first `len` are meaningful.
    chars: [u8; 8],
    /// The locator length: 4, 6 or 8.
    len: u8,
}

impl MaidenheadGrid {
    /// Parses a locator from text, accepting either case.
    ///
    /// # Errors
    ///
    /// [`GeoError::BadGridLength`] for a length other than 4, 6 or 8;
    /// [`GeoError::BadGridChar`] for a character outside its position's
    /// alphabet.
    pub const fn new(text: &str) -> Result<Self, GeoError> {
        Self::from_bytes(text.as_bytes())
    }

    /// Parses a locator from raw bytes, accepting either case.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub const fn from_bytes(bytes: &[u8]) -> Result<Self, GeoError> {
        let len = bytes.len();
        if len != 4 && len != 6 && len != 8 {
            return Err(GeoError::BadGridLength { got: len });
        }
        let mut chars = [0u8; 8];
        let mut i = 0;
        while i < len {
            let byte = bytes[i];
            // Position determines the alphabet: field letters, square
            // digits, subsquare letters, extended-square digits.
            let canonical = match i {
                0 | 1 => match byte {
                    b'A'..=b'R' => byte,
                    b'a'..=b'r' => byte - 32,
                    _ => {
                        return Err(GeoError::BadGridChar {
                            got: byte,
                            position: i,
                        });
                    }
                },
                2 | 3 | 6 | 7 => match byte {
                    b'0'..=b'9' => byte,
                    _ => {
                        return Err(GeoError::BadGridChar {
                            got: byte,
                            position: i,
                        });
                    }
                },
                _ => match byte {
                    b'a'..=b'x' => byte,
                    b'A'..=b'X' => byte + 32,
                    _ => {
                        return Err(GeoError::BadGridChar {
                            got: byte,
                            position: i,
                        });
                    }
                },
            };
            chars[i] = canonical;
            i += 1;
        }
        #[allow(clippy::cast_possible_truncation)] // len is 4, 6 or 8
        Ok(Self {
            chars,
            len: len as u8,
        })
    }

    /// The canonical locator text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every stored byte came from an ASCII alphabet above.
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    /// The canonical locator bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.chars.get(..self.len as usize).unwrap_or(&[])
    }

    /// How finely this locator names a place.
    #[must_use]
    pub const fn precision(&self) -> GridPrecision {
        match self.len {
            4 => GridPrecision::Square,
            8 => GridPrecision::ExtendedSquare,
            _ => GridPrecision::Subsquare,
        }
    }

    /// The coordinates at the centre of this locator's square.
    #[must_use]
    pub const fn center(self) -> Coordinates {
        Coordinates::from_maidenhead(self)
    }

    /// This locator at a coarser (or equal) precision.
    ///
    /// Locators are hierarchical -- `IO91wm` lies inside `IO91` -- so
    /// dropping characters is exact and never changes which place is
    /// named, only how precisely. Asking for a *finer* precision than
    /// the locator has returns it unchanged, because the missing
    /// characters are information nobody has.
    ///
    /// This exists because several protocols carry only a
    /// [`GridPrecision::Square`]: WSPR's type-1 message and FT8's
    /// `g15` field are both 15 bits, which is exactly two field letters
    /// and two square digits. Both reject a finer locator rather than
    /// truncating it silently, so this method is how a station with a
    /// six-character locator opts in to sending only the square.
    ///
    /// ```
    /// use yodel::geo::{GridPrecision, MaidenheadGrid};
    ///
    /// let precise = MaidenheadGrid::new("IO91wm")?;
    /// assert_eq!(
    ///     precise.to_precision(GridPrecision::Square).as_str(),
    ///     "IO91"
    /// );
    /// // Asking for more than is known changes nothing.
    /// assert_eq!(
    ///     precise.to_precision(GridPrecision::ExtendedSquare).as_str(),
    ///     "IO91wm"
    /// );
    /// # Ok::<(), yodel::geo::GeoError>(())
    /// ```
    #[must_use]
    pub const fn to_precision(self, precision: GridPrecision) -> Self {
        let wanted = precision.characters() as u8;
        if wanted >= self.len {
            return self;
        }
        let mut chars = self.chars;
        let mut i = wanted as usize;
        while i < chars.len() {
            chars[i] = 0;
            i += 1;
        }
        Self { chars, len: wanted }
    }
}

impl core::fmt::Display for MaidenheadGrid {
    /// Writes the canonical locator text.
    ///
    /// `Display` is offered here but not on the quantity types in
    /// [`crate::units`]: a locator is unambiguous text, whereas a bare
    /// number without its unit is the hazard those types exist to
    /// remove.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cos_q15` must not lean one way.
    ///
    /// The interpolation term `((b - a) * fraction) >> bits`
    /// arithmetic-shifts a NEGATIVE delta: cos over 0..90 degrees maps
    /// onto sin over 90..180, which decreases. `>>` floors, so the term
    /// rounded away from zero every time, and the chord of a concave
    /// arc already sits below the curve. Both pushed the same way, so
    /// the result was systematically low and every east-west distance
    /// read short.
    ///
    /// Rounding the interpolation leaves the table's own half-LSB and
    /// the curvature residual, neither of which shares a sign.
    ///
    /// Needs `std` for the reference cosine; the function under test is
    /// integer-only either way.
    #[cfg(feature = "std")]
    #[test]
    fn cos_q15_is_not_biased() {
        let mut total_lsb = 0.0f64;
        let mut worst_lsb = 0.0f64;
        let mut samples = 0u32;
        // Tenths of a degree over the whole quarter turn. 0 and 90 are
        // exact by construction and would only dilute the mean.
        for tenth in 1..900u32 {
            let degrees = f64::from(tenth) / 10.0;
            #[allow(clippy::cast_possible_truncation)]
            let units = (degrees * UNITS_PER_DEGREE as f64) as i64;
            let got = cos_q15(units) as f64;
            let want = (degrees * core::f64::consts::PI / 180.0).cos() * 32_767.0;
            let error_lsb = got - want;
            total_lsb += error_lsb;
            worst_lsb = worst_lsb.max(error_lsb.abs());
            samples += 1;
        }
        let mean_lsb = total_lsb / f64::from(samples);
        assert!(
            mean_lsb.abs() < 0.2,
            "cos_q15 is biased: mean error {mean_lsb:.3} LSB over {samples} samples \
             (worst {worst_lsb:.3} LSB); a rounding interpolation should centre this"
        );
        assert!(
            worst_lsb < 1.2,
            "cos_q15 worst-case error {worst_lsb:.3} LSB exceeds the table's own \
             half-LSB plus one rounding step"
        );
    }

    /// `bearing_to` must actually return the nearest whole degree.
    ///
    /// The 360-candidate search used [`crate::types::sine_at`], which
    /// truncates the phase to a table index. Candidate `d` therefore
    /// sat at an angle up to 0.088 degrees BELOW its nominal one, and
    /// since sin and cos share that index the candidate stayed a
    /// coherent unit vector -- just at the wrong angle. That shifted
    /// every half-degree decision boundary, so a few percent of
    /// directions came back as the neighbouring degree. Unlike the
    /// `cos_q15` tilt the docs describe, this was present at the
    /// equator.
    ///
    /// The reference here is `atan2` of the SAME displacement the
    /// implementation uses, so this measures only the candidate search,
    /// not the equirectangular projection around it.
    #[cfg(feature = "std")]
    #[test]
    fn bearing_to_returns_the_nearest_whole_degree() {
        let origin = Coordinates::new(Latitude::new(0).unwrap(), Longitude::new(0).unwrap());
        // A tenth of a degree: big enough that the storage-unit
        // rounding is nothing, small enough that the mean-latitude
        // cosine stays within a hair of unity.
        let radius = (UNITS_PER_DEGREE / 10) as f64;

        let mut mismatches = 0u32;
        let mut checked = 0u32;
        for tenth in 0..3600u32 {
            let angle = f64::from(tenth) / 10.0;
            let radians = angle * core::f64::consts::PI / 180.0;
            #[allow(clippy::cast_possible_truncation)]
            let north = (radius * radians.cos()) as i64;
            #[allow(clippy::cast_possible_truncation)]
            let east = (radius * radians.sin()) as i64;
            let target =
                Coordinates::new(Latitude::new(north).unwrap(), Longitude::new(east).unwrap());

            // Reference angle from the displacement the implementation
            // itself derives, so the projection cancels out.
            let (e, n) = origin.displacement(target);
            let want = (e as f64).atan2(n as f64).to_degrees().rem_euclid(360.0);
            // A direction sitting on a half-degree boundary may round
            // either way; those are ties, not errors.
            let fraction = want - want.floor();
            if (fraction - 0.5).abs() < 0.02 {
                continue;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let nearest = (want.round() as i64).rem_euclid(360) as u16;
            checked += 1;
            if origin.bearing_to(target).degrees() != nearest {
                mismatches += 1;
            }
        }
        assert_eq!(
            mismatches, 0,
            "{mismatches} of {checked} directions returned the wrong whole degree"
        );
    }

    #[test]
    fn coordinate_range_checks() {
        // Derived from the constant rather than written as a literal,
        // so the boundary follows the unit instead of having to be
        // recomputed by hand whenever the unit moves.
        assert!(Latitude::new(LAT_MAX).is_ok());
        assert_eq!(
            Latitude::new(LAT_MAX + 1),
            Err(GeoError::BadLatitude { got: LAT_MAX + 1 })
        );
        assert_eq!(
            Latitude::new(-LAT_MAX - 1),
            Err(GeoError::BadLatitude { got: -LAT_MAX - 1 })
        );
        assert!(Longitude::new(-LON_MAX).is_ok());
        assert_eq!(
            Longitude::new(LON_MAX + 1),
            Err(GeoError::BadLongitude { got: LON_MAX + 1 })
        );
    }

    #[test]
    fn degree_conversions() {
        // 49 deg 03.50 min = 49.058333... deg
        let l = match Latitude::from_degrees(49.0 + 3.5 / 60.0) {
            Ok(l) => l,
            Err(e) => panic!("{e}"),
        };
        // `from_degrees` goes through f64, whose 53-bit mantissa is
        // narrower than the storage range (1.7e16 here), so the last
        // couple of units are not recoverable. Two units is 0.6
        // nanometres; the tolerance says so rather than pretending the
        // conversion is exact.
        let expected = (49 * 6000 + 350) * UNITS_PER_HUNDREDTH_MINUTE;
        assert!(
            (l.units() - expected).abs() <= 4,
            "{} vs {expected}",
            l.units()
        );
        assert!((l.to_degrees() - (49.0 + 3.5 / 60.0)).abs() < 1e-9);
        assert!(Latitude::from_degrees(90.001).is_err());
        assert!(Latitude::from_degrees(f64::NAN).is_err());
        assert!(Longitude::from_degrees(-180.001).is_err());
        let g = match Longitude::from_degrees(-72.75) {
            Ok(g) => g,
            Err(e) => panic!("{e}"),
        };
        let expected = -(72 * 6000 + 45 * 100) * UNITS_PER_HUNDREDTH_MINUTE;
        assert!(
            (g.units() - expected).abs() <= 4,
            "{} vs {expected}",
            g.units()
        );
    }

    #[test]
    fn ambiguity_range() {
        assert_eq!(Ambiguity::new(0).map(Ambiguity::digits), Ok(0));
        assert_eq!(Ambiguity::new(4).map(Ambiguity::digits), Ok(4));
        assert_eq!(Ambiguity::new(5), Err(GeoError::BadAmbiguity { got: 5 }));
        assert!(Ambiguity::EXACT.is_exact());
    }

    /// The four masking levels, against chapter 6's own words.
    ///
    /// The spec blanks `4903.5N` one digit at a time and says in prose
    /// what each level means: nearest tenth of a minute, nearest
    /// minute, nearest ten minutes, nearest degree. Those words are the
    /// oracle, written out here as independent arithmetic rather than
    /// by calling `step`, so the test would survive `step` being wrong.
    #[test]
    fn ambiguity_masks_to_the_chapter_6_levels() {
        let minute = UNITS_PER_DEGREE / 60;
        let hundredth = minute / 100;
        // 49 deg 03.57 min, a hundredth finer than the spec's example
        // so that every level has something to remove.
        let value = 49 * UNITS_PER_DEGREE + 3 * minute + 57 * hundredth;
        let cases = [
            // (masked digits, expected remainder above 49 degrees)
            (0u8, 3 * minute + 57 * hundredth),
            (1, 3 * minute + 50 * hundredth),
            (2, 3 * minute),
            (3, 0),
            (4, 0),
        ];
        for (digits, offset) in cases {
            let a = Ambiguity::new(digits).expect("0..=4 is in range");
            assert_eq!(
                a.mask(value),
                49 * UNITS_PER_DEGREE + offset,
                "north, {digits} masked digits"
            );
            // Chapter 6 blanks digits in a magnitude written beside a
            // hemisphere letter, so the southern mirror must move the
            // same distance toward the equator, never away from it.
            assert_eq!(
                a.mask(-value),
                -(49 * UNITS_PER_DEGREE + offset),
                "south, {digits} masked digits"
            );
            assert!(
                a.mask(-value).abs() <= value,
                "masking must never increase a magnitude"
            );
        }
    }

    /// Masking is idempotent, and coarser levels absorb finer ones.
    ///
    /// Both properties are what let the same call sit on a parser and
    /// on an accessor without anyone tracking whether it has already
    /// been applied.
    #[test]
    fn ambiguity_masking_is_idempotent_and_ordered() {
        let step = UNITS_PER_DEGREE / 6000;
        for units in [
            0i64,
            step,
            12_345 * step,
            -12_345 * step,
            180 * UNITS_PER_DEGREE,
        ] {
            let mut previous = units;
            for digits in 0u8..=4 {
                let a = Ambiguity::new(digits).expect("0..=4 is in range");
                let once = a.mask(units);
                assert_eq!(a.mask(once), once, "idempotence at {digits} for {units}");
                assert!(
                    once.abs() <= previous.abs(),
                    "level {digits} reported a larger magnitude than level {}",
                    digits.saturating_sub(1)
                );
                assert_eq!(
                    once % a.step(),
                    0,
                    "a masked value must be a whole number of steps"
                );
                previous = once;
            }
        }
    }
}
