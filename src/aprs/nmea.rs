//! Raw NMEA 0183 sentences carried under the APRS `$` data-type
//! identifier (APRS 1.01 chapter 8, "Raw NMEA Reports"; the sentences
//! APRS carries are listed in chapter 6, "NMEA Data").
//!
//! NMEA 0183 itself is a paid standard from the National Marine
//! Electronics Association and is not quoted here. The APRS chapters
//! above are the normative source for *which* sentences appear and how
//! they are framed, which is all this module needs; for the field
//! layouts, Eric S. Raymond's "NMEA Revealed"
//! (<https://gpsd.gitlab.io/gpsd/NMEA.html>) is the usual freely
//! readable description.
//!
//! A tracker that has no APRS position encoder of its own simply relays
//! the receiver's ASCII sentence, so the AX.25 information field *is*
//! the sentence: `$GPRMC,013641.06,A,3348.1607,N,11807.4631,W,...`. The
//! `$` is simultaneously the APRS data-type identifier and the NMEA
//! start delimiter, so [`parse`] takes the **whole information field**,
//! leading `$` included.
//!
//! Five sentence formatters carry something an APRS consumer wants:
//! [`Rmc`], [`Gga`], [`Gll`], [`Vtg`] and [`Wpl`]. Everything else
//! (`GSA`, `GSV`, `ZDA`, proprietary `$P...`) is rejected with the typed
//! [`NmeaError::UnsupportedFormatter`] carrying the three formatter
//! bytes.
//!
//! Everything here is `no_std`, allocation-free and integer-only:
//! [`parse`] borrows the waypoint name from the input and every numeric
//! field decodes into a scaled integer (see the unit suffix on each
//! field name). Coordinates become the crate's [`Latitude`] /
//! [`Longitude`] newtypes, which store signed **1/100 arc-minutes**; a
//! sentence with four or five fractional-minute digits is therefore
//! rounded to that grid (about 18 m), the same resolution an
//! uncompressed APRS position report would have carried anyway.
//!
//! # Design decisions
//!
//! Each of these is a place where a naive decoder silently loses real
//! traffic:
//!
//! * **The talker varies.** Dispatch is on the 3-character *formatter*
//!   (`RMC`), never the 5-character tag (`GPRMC`). Multi-constellation
//!   receivers emit `GN`, `GL`, `GA`, `BD`/`GB`, `GQ` and `GI` as
//!   readily as `GP`; the talker is kept as metadata
//!   ([`NmeaSentence::talker`], [`Talker::constellation`]).
//! * **Field counts grew with the standard.** Each formatter has a
//!   documented *minimum* count (the fields its defining payload needs);
//!   trailing fields are optional and unknown trailing extras are
//!   ignored. RMC has been seen with 12 (NMEA 2.0), 13 (2.3, adds the
//!   FAA mode) and 14 (4.1, adds the navigational status) fields; GLL
//!   with 5, 7 and 8. No exact count is ever asserted.
//! * **Any field may be empty**, and empty means *no data*, never zero.
//!   Every decoded value is an `Option`.
//! * **The checksum is advisory.** `*hh` is the XOR of every byte
//!   strictly between `$` and `*`, searched for from the end of the
//!   sentence, upper or lower case. It is reported as a tri-state
//!   ([`ChecksumStatus`]) and **never** fails the parse: many trackers
//!   strip it entirely, and by the time an information field reaches
//!   this module the AX.25 FCS has already vouched for the bytes. A
//!   consumer that wants stricter behavior checks
//!   [`ChecksumStatus::is_valid`] itself.
//! * **A `V` status is not a rejection.** RMC and GLL mark a fix `A`
//!   (valid) or `V` (navigation-receiver warning), but a `V` fix
//!   routinely still carries a usable position (high DOP, low elevation
//!   mask, dead reckoning). It decodes to [`FixQuality::Degraded`];
//!   only a *missing* or all-zero coordinate pair becomes
//!   [`FixQuality::Invalid`].
//! * **GGA quality is a range, not a pair.** Values 0-8 are all
//!   defined and 3/4/5 (PPS, RTK fixed, RTK float) are *better* than 1,
//!   so the fix test is `!= 0` ([`GgaQuality::has_fix`]), never
//!   `== 1 || == 2`.
//! * **Course 360 is due north** and is preserved verbatim; only `0`
//!   conventionally means "unknown", and this module does not fold one
//!   into the other.
//! * **VTG has two incompatible historical forms**, told apart by field
//!   2 being the literal `T`. It carries **no position**.
//! * **WPL is a waypoint**, not the transmitting station. It is a
//!   distinct variant and [`NmeaSentence::position`] returns `None` for
//!   it, so a consumer cannot mistake it for a posit.
//!
//! # Example
//!
//! ```
//! use warble::aprs::nmea::{self, ChecksumStatus, FixQuality, NmeaData, NmeaError};
//!
//! let sentence =
//!     nmea::parse(b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62")?;
//! assert_eq!(sentence.talker.as_bytes(), *b"GP");
//! assert_eq!(sentence.checksum, ChecksumStatus::Valid);
//!
//! let here = sentence.position().expect("RMC carries a position");
//! assert!((here.latitude.to_degrees() + 37.860_833).abs() < 1e-6);
//! assert!((here.longitude.to_degrees() - 145.122_667).abs() < 1e-6);
//!
//! match sentence.data {
//!     NmeaData::Rmc(rmc) => {
//!         assert_eq!(rmc.fix, FixQuality::Valid);
//!         assert_eq!(rmc.speed_milliknots, Some(0));
//!         // 360 means due north: preserved, never folded to 0.
//!         assert_eq!(rmc.course_centidegrees, Some(36_000));
//!     }
//!     _ => unreachable!("the formatter is RMC"),
//! }
//! # Ok::<(), NmeaError>(())
//! ```

use core::fmt;

use crate::geo::{Coordinates, Latitude, Longitude};
use crate::units::{Bearing, Distance, Speed};

/// Arc-minute hundredths per degree, widened to the unsigned type this
/// module's fixed-point arithmetic runs in.
///
/// Derived from the single definition in [`crate::geo`] rather than
/// written out again. Three independent copies of a coordinate unit is
/// how changing it puts stations in the wrong hemisphere: two of the
/// three would be updated, the third would keep compiling, and nothing
/// would fail until someone read a map.
const UNITS_PER_DEGREE: u64 = crate::geo::UNITS_PER_DEGREE.unsigned_abs();
/// Storage units in one arc-minute, unsigned.
const UNITS_PER_MINUTE: u64 = crate::geo::UNITS_PER_MINUTE.unsigned_abs();

/// Decimal places of arc-minutes kept exactly from an NMEA sentence.
///
/// Seven is the most the storage unit can hold without rounding, and it
/// is also the most any receiver documented emits (u-blox calls it high
/// precision mode). A sentence with more places is rounded to nearest.
const FRACTION_DIGITS: usize = 7;

/// Storage units in the last place kept by [`FRACTION_DIGITS`].
///
/// Exact: a minute is 5 713 890 000 000 units and 10^7 divides it.
const UNITS_PER_FRACTION: u64 = UNITS_PER_MINUTE / 10_u64.pow(FRACTION_DIGITS as u32);

/// Powers of ten for the fixed-point scales this module uses.
const POW10: [u64; 4] = [1, 10, 100, 1000];

/// Minimum RMC field count (tag through the date): the "recommended
/// minimum" payload. NMEA 2.0 emits 12, 2.3 emits 13, 4.1 emits 14.
const RMC_MIN_FIELDS: usize = 10;
/// Minimum GGA field count (tag through the quality indicator);
/// altitude, satellite count, HDOP and geoid separation follow it and
/// are optional here. A complete GGA has 15 fields.
const GGA_MIN_FIELDS: usize = 7;
/// Minimum GLL field count (tag through the E/W hemisphere): the
/// NMEA 1.5 form. Later versions add time + status (7) and the FAA mode
/// (8).
const GLL_MIN_FIELDS: usize = 5;
/// Minimum field count of the legacy indicator-less VTG form
/// (tag, true course, magnetic course, knots, km/h).
const VTG_LEGACY_FIELDS: usize = 5;
/// Minimum field count of the modern `T`/`M`/`N`/`K` VTG form; the FAA
/// mode makes 10.
const VTG_MODERN_FIELDS: usize = 9;
/// Minimum WPL field count (tag, latitude, N/S, longitude, E/W, name).
const WPL_MIN_FIELDS: usize = 6;

/// The two-character talker identifier at the head of a sentence tag
/// (the `GP` of `$GPRMC`), stored as ASCII, upper-cased.
///
/// The talker is metadata only: dispatch is on the formatter, so a
/// `GN`, `GL` or `BD` sentence decodes exactly like its `GP`
/// equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Talker([u8; 2]);

impl Talker {
    /// The two ASCII bytes of the talker identifier.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 2] {
        self.0
    }

    /// The satellite constellation the talker identifies.
    #[must_use]
    pub const fn constellation(self) -> Constellation {
        match (self.0[0], self.0[1]) {
            (b'G', b'P') => Constellation::Gps,
            (b'G', b'L') => Constellation::Glonass,
            (b'G', b'A') => Constellation::Galileo,
            (b'B', b'D') | (b'G', b'B') => Constellation::BeiDou,
            (b'G', b'Q') => Constellation::Qzss,
            (b'G', b'I') => Constellation::NavIc,
            (b'G', b'N') => Constellation::Combined,
            _ => Constellation::Other,
        }
    }
}

/// The satellite constellation a [`Talker`] identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constellation {
    /// `GP` — GPS (NAVSTAR).
    Gps,
    /// `GL` — GLONASS.
    Glonass,
    /// `GA` — Galileo.
    Galileo,
    /// `BD` (legacy) or `GB` — BeiDou.
    BeiDou,
    /// `GQ` — QZSS.
    Qzss,
    /// `GI` — NavIC (IRNSS).
    NavIc,
    /// `GN` — a combined multi-constellation solution.
    Combined,
    /// Any other talker: integrated instrumentation (`II`, `IN`),
    /// electronic charts (`EC`), a proprietary tag, and so on.
    Other,
}

/// The three-character sentence formatter this module decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formatter {
    /// `RMC` — recommended minimum: position, speed, course, date.
    Rmc,
    /// `GGA` — fix data: position, altitude, satellites, HDOP, quality.
    Gga,
    /// `GLL` — geographic position: latitude/longitude, time, status.
    Gll,
    /// `VTG` — course and speed over ground. Carries no position.
    Vtg,
    /// `WPL` — waypoint location. Not the transmitting station.
    Wpl,
}

impl Formatter {
    /// The three ASCII bytes of the formatter.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 3] {
        match self {
            Formatter::Rmc => *b"RMC",
            Formatter::Gga => *b"GGA",
            Formatter::Gll => *b"GLL",
            Formatter::Vtg => *b"VTG",
            Formatter::Wpl => *b"WPL",
        }
    }

    /// Recognizes an upper-cased formatter, or `None` when this module
    /// does not decode it.
    const fn from_bytes(bytes: [u8; 3]) -> Option<Self> {
        match (bytes[0], bytes[1], bytes[2]) {
            (b'R', b'M', b'C') => Some(Formatter::Rmc),
            (b'G', b'G', b'A') => Some(Formatter::Gga),
            (b'G', b'L', b'L') => Some(Formatter::Gll),
            (b'V', b'T', b'G') => Some(Formatter::Vtg),
            (b'W', b'P', b'L') => Some(Formatter::Wpl),
            _ => None,
        }
    }
}

/// The state of a sentence's trailing `*hh` checksum.
///
/// A sentence is decoded regardless of this value; see the module
/// documentation for why an invalid checksum is reported rather than
/// rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumStatus {
    /// A `*hh` trailer was present and matched the XOR of the body.
    Valid,
    /// A `*hh` trailer was present and did not match.
    Invalid {
        /// The XOR of every byte strictly between `$` and `*`.
        computed: u8,
        /// The value the two hex digits after `*` carried.
        received: u8,
    },
    /// No `*hh` trailer was present (many trackers strip it).
    Absent,
}

impl ChecksumStatus {
    /// Whether a checksum was present and matched.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        matches!(self, ChecksumStatus::Valid)
    }

    /// Whether a `*hh` trailer was present at all, valid or not.
    #[must_use]
    pub const fn is_present(self) -> bool {
        !matches!(self, ChecksumStatus::Absent)
    }
}

/// How much a sentence's position may be trusted.
///
/// This is a three-way classification precisely so that a `V`-status
/// RMC is not thrown away: `Degraded` still carries coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixQuality {
    /// A position is present and the sentence reports a good fix.
    Valid,
    /// A position is present but the sentence flags it: an RMC/GLL
    /// status other than `A`, a missing status, or a GGA quality of
    /// dead reckoning, manual input or simulator.
    Degraded,
    /// No usable position: the coordinate fields were empty, or both
    /// were exactly zero, or the GGA quality indicator was 0.
    Invalid,
}

impl FixQuality {
    /// Whether a position is present at all (`Valid` or `Degraded`).
    #[must_use]
    pub const fn has_position(self) -> bool {
        !matches!(self, FixQuality::Invalid)
    }
}

/// The GGA quality indicator (field 6).
///
/// All of 0-8 are defined and in live use; 3, 4 and 5 are *more*
/// accurate than a plain autonomous fix, so a decoder must never test
/// for `1` or `2` alone. Use [`GgaQuality::has_fix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgaQuality {
    /// 0 — fix not available or invalid.
    Invalid,
    /// 1 — autonomous GNSS fix.
    Gps,
    /// 2 — differential GNSS fix (DGPS/SBAS).
    Differential,
    /// 3 — PPS (precise positioning service) fix.
    Pps,
    /// 4 — RTK with integer ambiguities fixed.
    RtkFixed,
    /// 5 — RTK float.
    RtkFloat,
    /// 6 — dead reckoning (estimated).
    DeadReckoning,
    /// 7 — manual input mode.
    ManualInput,
    /// 8 — simulator mode.
    Simulator,
    /// 9 or above: vendor extension, held verbatim.
    Other(
        /// The raw indicator value.
        u8,
    ),
}

impl GgaQuality {
    /// Classifies a raw indicator digit.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => GgaQuality::Invalid,
            1 => GgaQuality::Gps,
            2 => GgaQuality::Differential,
            3 => GgaQuality::Pps,
            4 => GgaQuality::RtkFixed,
            5 => GgaQuality::RtkFloat,
            6 => GgaQuality::DeadReckoning,
            7 => GgaQuality::ManualInput,
            8 => GgaQuality::Simulator,
            other => GgaQuality::Other(other),
        }
    }

    /// The raw indicator digit.
    #[must_use]
    pub const fn to_raw(self) -> u8 {
        match self {
            GgaQuality::Invalid => 0,
            GgaQuality::Gps => 1,
            GgaQuality::Differential => 2,
            GgaQuality::Pps => 3,
            GgaQuality::RtkFixed => 4,
            GgaQuality::RtkFloat => 5,
            GgaQuality::DeadReckoning => 6,
            GgaQuality::ManualInput => 7,
            GgaQuality::Simulator => 8,
            GgaQuality::Other(raw) => raw,
        }
    }

    /// Whether the receiver reports *any* fix, i.e. the indicator is
    /// not 0. Every non-zero value is a real positioning mode.
    #[must_use]
    pub const fn has_fix(self) -> bool {
        !matches!(self, GgaQuality::Invalid)
    }
}

/// The NMEA 2.3 FAA mode indicator, appended to RMC, GLL and VTG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaaMode {
    /// `A` — autonomous.
    Autonomous,
    /// `D` — differential.
    Differential,
    /// `E` — estimated (dead reckoning).
    Estimated,
    /// `F` — RTK float.
    RtkFloat,
    /// `M` — manual input.
    Manual,
    /// `N` — data not valid.
    NotValid,
    /// `P` — precise (no degradation, e.g. no SA).
    Precise,
    /// `R` — RTK integer.
    RtkInteger,
    /// `S` — simulator.
    Simulator,
    /// Any other byte, held verbatim.
    Other(
        /// The raw mode byte.
        u8,
    ),
}

impl FaaMode {
    /// Classifies a raw mode byte (upper-cased first).
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        match byte.to_ascii_uppercase() {
            b'A' => FaaMode::Autonomous,
            b'D' => FaaMode::Differential,
            b'E' => FaaMode::Estimated,
            b'F' => FaaMode::RtkFloat,
            b'M' => FaaMode::Manual,
            b'N' => FaaMode::NotValid,
            b'P' => FaaMode::Precise,
            b'R' => FaaMode::RtkInteger,
            b'S' => FaaMode::Simulator,
            _ => FaaMode::Other(byte),
        }
    }

    /// Whether the mode is anything other than the explicit `N`
    /// ("data not valid").
    #[must_use]
    pub const fn is_valid(self) -> bool {
        !matches!(self, FaaMode::NotValid)
    }
}

/// A UTC time of day from an `hhmmss.sss` field.
///
/// The fractional part is **truncated** to milliseconds, so a
/// `.9999` field can never manufacture an extra second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NmeaTime {
    /// Hour, `0..=23`.
    pub hour: u8,
    /// Minute, `0..=59`.
    pub minute: u8,
    /// Second, `0..=60` (60 admits a leap second).
    pub second: u8,
    /// Milliseconds within the second, `0..=999`.
    pub millisecond: u16,
}

/// A UTC date from a `ddmmyy` field.
///
/// The year is the raw two-digit value: NMEA specifies no epoch, so
/// the consumer applies its own pivot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NmeaDate {
    /// Day of the month, `1..=31`.
    pub day: u8,
    /// Month, `1..=12`.
    pub month: u8,
    /// Two-digit year, `0..=99`.
    pub year: u8,
}

/// `RMC` — recommended minimum specific GNSS data.
///
/// Carries position, ground speed, course, date and (from NMEA 2.3) an
/// FAA mode. It carries no altitude and no satellite count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rmc {
    /// UTC time of the fix.
    pub time: Option<NmeaTime>,
    /// The raw status byte: `A` valid, `V` navigation-receiver warning.
    /// A `V` is *not* a rejection — see [`Rmc::fix`].
    pub status: Option<u8>,
    /// Latitude, rounded to 1/100 arc-minute.
    pub latitude: Option<Latitude>,
    /// Longitude, rounded to 1/100 arc-minute.
    pub longitude: Option<Longitude>,
    /// Speed over ground in thousandths of a knot.
    pub speed_milliknots: Option<u32>,
    /// Course over ground (true) in hundredths of a degree,
    /// `0..=36000`. `0` conventionally means unknown and `36000` means
    /// due north; both are preserved verbatim.
    pub course_centidegrees: Option<u32>,
    /// UTC date of the fix.
    pub date: Option<NmeaDate>,
    /// Magnetic variation in hundredths of a degree, **east positive**
    /// (the `W` hemisphere byte negates it).
    pub magnetic_variation_centidegrees: Option<i32>,
    /// The NMEA 2.3 FAA mode indicator, when present.
    pub mode: Option<FaaMode>,
    /// The raw NMEA 4.1 navigational-status byte (`S`, `C`, `U`, `V`),
    /// when present.
    pub navigation_status: Option<u8>,
    /// How much the position may be trusted, derived from `status` and
    /// the coordinate fields.
    pub fix: FixQuality,
}

/// `GGA` — global positioning system fix data.
///
/// Carries position, altitude, satellite count, HDOP and the quality
/// indicator. It carries **no speed and no course**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gga {
    /// UTC time of the fix.
    pub time: Option<NmeaTime>,
    /// Latitude, rounded to 1/100 arc-minute.
    pub latitude: Option<Latitude>,
    /// Longitude, rounded to 1/100 arc-minute.
    pub longitude: Option<Longitude>,
    /// The quality indicator; `None` when the field was empty (which
    /// is *not* the same as the value 0).
    pub quality: Option<GgaQuality>,
    /// Satellites used in the solution.
    pub satellites: Option<u8>,
    /// Horizontal dilution of precision in hundredths.
    pub hdop_hundredths: Option<u32>,
    /// Altitude above mean sea level in centimeters. The `M` unit
    /// field is not inspected; GGA altitude is always meters in
    /// practice.
    pub altitude_centimeters: Option<i32>,
    /// Geoid separation (height of the geoid above the WGS-84
    /// ellipsoid) in centimeters. Add it to `altitude_centimeters` for
    /// an ellipsoidal height.
    pub geoid_separation_centimeters: Option<i32>,
    /// How much the position may be trusted, derived from `quality`
    /// and the coordinate fields.
    pub fix: FixQuality,
}

/// `GLL` — geographic position, latitude/longitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gll {
    /// Latitude, rounded to 1/100 arc-minute.
    pub latitude: Option<Latitude>,
    /// Longitude, rounded to 1/100 arc-minute.
    pub longitude: Option<Longitude>,
    /// UTC time of the fix (absent in the NMEA 1.5 five-field form).
    pub time: Option<NmeaTime>,
    /// The raw status byte: `A` valid, `V` warning. As with RMC, a `V`
    /// is not a rejection — see [`Gll::fix`].
    pub status: Option<u8>,
    /// The NMEA 2.3 FAA mode indicator, when present.
    pub mode: Option<FaaMode>,
    /// How much the position may be trusted, derived from `status` and
    /// the coordinate fields.
    pub fix: FixQuality,
}

/// Which of the two historical `VTG` layouts a sentence used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtgForm {
    /// The modern layout with `T` / `M` / `N` / `K` unit indicators
    /// (9 fields, 10 with the FAA mode).
    Modern,
    /// The legacy indicator-less layout: true course, magnetic course,
    /// knots, km/h (5 fields).
    Legacy,
}

/// `VTG` — course over ground and ground speed.
///
/// **Carries no position.** [`NmeaSentence::position`] returns `None`
/// for a VTG sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vtg {
    /// Which historical layout the sentence used.
    pub form: VtgForm,
    /// Course over ground referenced to true north, in hundredths of a
    /// degree. `36000` (due north) is preserved verbatim.
    pub course_true_centidegrees: Option<u32>,
    /// Course over ground referenced to magnetic north, in hundredths
    /// of a degree.
    pub course_magnetic_centidegrees: Option<u32>,
    /// Ground speed in thousandths of a knot (the `N` field).
    pub speed_milliknots: Option<u32>,
    /// Ground speed in meters per hour, i.e. thousandths of a
    /// kilometre per hour (the `K` field).
    pub speed_meters_per_hour: Option<u32>,
    /// The NMEA 2.3 FAA mode indicator, when present (modern form
    /// only).
    pub mode: Option<FaaMode>,
}

/// `WPL` — waypoint location.
///
/// This is a **waypoint**, not the transmitting station's position: it
/// is a point of interest the receiver holds in its route database.
/// [`NmeaSentence::position`] returns `None` for it so a consumer
/// cannot store it as a posit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wpl<'a> {
    /// Waypoint latitude, rounded to 1/100 arc-minute.
    pub latitude: Option<Latitude>,
    /// Waypoint longitude, rounded to 1/100 arc-minute.
    pub longitude: Option<Longitude>,
    /// The waypoint name, borrowed from the information field.
    pub name: &'a [u8],
}

/// The decoded payload of a sentence, one variant per formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NmeaData<'a> {
    /// An `RMC` sentence.
    Rmc(Rmc),
    /// A `GGA` sentence.
    Gga(Gga),
    /// A `GLL` sentence.
    Gll(Gll),
    /// A `VTG` sentence (no position).
    Vtg(Vtg),
    /// A `WPL` sentence (a waypoint, not a posit).
    Wpl(Wpl<'a>),
}

impl NmeaData<'_> {
    /// The formatter this payload came from.
    #[must_use]
    pub const fn formatter(&self) -> Formatter {
        match self {
            NmeaData::Rmc(_) => Formatter::Rmc,
            NmeaData::Gga(_) => Formatter::Gga,
            NmeaData::Gll(_) => Formatter::Gll,
            NmeaData::Vtg(_) => Formatter::Vtg,
            NmeaData::Wpl(_) => Formatter::Wpl,
        }
    }
}

/// A decoded NMEA 0183 sentence.
///
/// Produced by [`parse`]; borrows the waypoint name (WPL only) from the
/// information field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NmeaSentence<'a> {
    /// The two-character talker identifier, upper-cased.
    pub talker: Talker,
    /// The state of the trailing `*hh` checksum. Never fails a parse.
    pub checksum: ChecksumStatus,
    /// The decoded payload.
    pub data: NmeaData<'a>,
}

impl NmeaSentence<'_> {
    /// The three-character sentence formatter.
    #[must_use]
    pub const fn formatter(&self) -> Formatter {
        self.data.formatter()
    }

    /// The **transmitting station's** position, when the sentence
    /// carries one.
    ///
    /// Returns `None` for `VTG` (which has no position at all) and for
    /// `WPL` (whose coordinates describe a waypoint, not the station),
    /// and `None` whenever either coordinate field was empty.
    ///
    /// The coordinates are returned as parsed; consult
    /// [`NmeaSentence::fix`] before trusting them, since a receiver
    /// with no fix commonly transmits `0000.0000,N,00000.0000,E`.
    #[must_use]
    pub const fn position(&self) -> Option<Coordinates> {
        let (latitude, longitude) = match &self.data {
            NmeaData::Rmc(rmc) => (rmc.latitude, rmc.longitude),
            NmeaData::Gga(gga) => (gga.latitude, gga.longitude),
            NmeaData::Gll(gll) => (gll.latitude, gll.longitude),
            NmeaData::Vtg(_) | NmeaData::Wpl(_) => return None,
        };
        match (latitude, longitude) {
            (Some(latitude), Some(longitude)) => Some(Coordinates::new(latitude, longitude)),
            _ => None,
        }
    }

    /// How much the position may be trusted, for the three formatters
    /// that carry one; `None` for `VTG` and `WPL`.
    #[must_use]
    pub const fn fix(&self) -> Option<FixQuality> {
        match &self.data {
            NmeaData::Rmc(rmc) => Some(rmc.fix),
            NmeaData::Gga(gga) => Some(gga.fix),
            NmeaData::Gll(gll) => Some(gll.fix),
            NmeaData::Vtg(_) | NmeaData::Wpl(_) => None,
        }
    }

    /// The UTC time of day, for the formatters that carry one.
    #[must_use]
    pub const fn time(&self) -> Option<NmeaTime> {
        match &self.data {
            NmeaData::Rmc(rmc) => rmc.time,
            NmeaData::Gga(gga) => gga.time,
            NmeaData::Gll(gll) => gll.time,
            NmeaData::Vtg(_) | NmeaData::Wpl(_) => None,
        }
    }

    /// Course over ground referenced to **true** north, from `RMC` or
    /// `VTG`; `None` for the formatters that carry none.
    ///
    /// The wire field is hundredths of a degree and [`Bearing`] is
    /// whole degrees, so this rounds half away from zero and folds
    /// 360 back to 0 — `35 999` and `36 000` are both due north.
    ///
    /// A course of exactly `0` is returned as a bearing of zero, not
    /// as `None`. NMEA spells "unknown" as an **empty field**, which
    /// is already `None`; reading a transmitted zero as unknown would
    /// discard the course of every vehicle heading north.
    /// The convention that `0` means unknown belongs to the APRS
    /// `ddd/sss` extension (chapter 7), not to NMEA.
    #[must_use]
    pub const fn course(&self) -> Option<Bearing> {
        let centidegrees = match &self.data {
            NmeaData::Rmc(rmc) => rmc.course_centidegrees,
            NmeaData::Vtg(vtg) => vtg.course_true_centidegrees,
            NmeaData::Gga(_) | NmeaData::Gll(_) | NmeaData::Wpl(_) => None,
        };
        let Some(centidegrees) = centidegrees else {
            return None;
        };
        // Below 360 after the fold, so the narrowing cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        let degrees = (centidegrees.wrapping_add(50) / 100 % 360) as u16;
        match Bearing::new(degrees) {
            Ok(bearing) => Some(bearing),
            Err(_) => None,
        }
    }

    /// Speed over ground, from `RMC` or `VTG`.
    ///
    /// The conversion is exact: the wire field is thousandths of a
    /// knot and one knot is 1 852 000 mm/h by definition, so one
    /// milliknot is exactly 1852 mm/h. A `VTG` sentence that carries
    /// only the metric `K` field is read from that instead, also
    /// exactly (one metre per hour is 1000 mm/h).
    #[must_use]
    pub const fn speed(&self) -> Option<Speed> {
        let milliknots = match &self.data {
            NmeaData::Rmc(rmc) => rmc.speed_milliknots,
            NmeaData::Vtg(vtg) => match vtg.speed_milliknots {
                Some(milliknots) => Some(milliknots),
                None => match vtg.speed_meters_per_hour {
                    Some(meters_per_hour) => {
                        return Some(Speed::from_millimeters_per_hour(
                            meters_per_hour as i64 * 1000,
                        ));
                    }
                    None => None,
                },
            },
            NmeaData::Gga(_) | NmeaData::Gll(_) | NmeaData::Wpl(_) => None,
        };
        match milliknots {
            Some(milliknots) => Some(Speed::from_millimeters_per_hour(milliknots as i64 * 1852)),
            None => None,
        }
    }

    /// Altitude above mean sea level, from `GGA`; `None` for every
    /// other formatter, none of which carries one.
    ///
    /// Exact: the wire field is centimetres and one centimetre is
    /// 10 000 µm. To obtain an ellipsoidal height instead, add
    /// [`Gga::geoid_separation_centimeters`].
    #[must_use]
    pub const fn altitude(&self) -> Option<Distance> {
        match &self.data {
            NmeaData::Gga(gga) => match gga.altitude_centimeters {
                Some(centimeters) => Some(Distance::from_micrometers(centimeters as i64 * 10_000)),
                None => None,
            },
            NmeaData::Rmc(_) | NmeaData::Gll(_) | NmeaData::Vtg(_) | NmeaData::Wpl(_) => None,
        }
    }
}

/// A malformed raw-NMEA information field.
///
/// Every variant carries the offending byte or value together with the
/// rule it violated. A wrong or missing checksum is *not* an error:
/// see [`ChecksumStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NmeaError {
    /// The information field was empty.
    Empty,
    /// The first byte was not the `$` start delimiter (which is also
    /// the APRS raw-NMEA data-type identifier).
    MissingStartDelimiter {
        /// The rejected byte.
        got: u8,
    },
    /// A byte above `0x7E` appeared inside the sentence; NMEA 0183 is
    /// printable ASCII, so this is corruption.
    NonPrintable {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information
        /// field.
        position: usize,
    },
    /// The address field was not a five-character talker + formatter
    /// tag such as `GPRMC`.
    BadAddressLength {
        /// The rejected length in bytes.
        got: usize,
    },
    /// The three-character formatter is not one this module decodes.
    UnsupportedFormatter {
        /// The rejected formatter bytes, upper-cased.
        got: [u8; 3],
    },
    /// The sentence carried fewer comma-separated fields than the
    /// formatter's mandatory payload needs.
    TooFewFields {
        /// The field count present (the tag included).
        got: usize,
        /// The minimum this formatter requires.
        min: usize,
    },
    /// A byte that must be an ASCII digit was something else.
    BadDigit {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information
        /// field.
        position: usize,
    },
    /// A fixed literal byte (such as the `.` of `hhmmss.sss`) was
    /// wrong.
    ExpectedByte {
        /// The byte the format requires at this position.
        expected: u8,
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information
        /// field.
        position: usize,
    },
    /// A numeric field held no digits at all (a bare `.`, `-` or `+`).
    EmptyNumber {
        /// Byte offset of the field within the information field.
        position: usize,
    },
    /// A numeric field overflowed its decoded type, or was negative
    /// where the format is unsigned.
    NumberOutOfRange {
        /// Byte offset of the field within the information field.
        position: usize,
    },
    /// A coordinate magnitude had no `.` separating whole minutes from
    /// their fraction.
    MissingDecimalPoint {
        /// Byte offset of the field within the information field.
        position: usize,
    },
    /// A coordinate magnitude had fewer than three characters before
    /// its `.`: two are the whole minutes, so at least one degree
    /// digit must remain.
    CoordinateTooShort {
        /// The rejected field length in bytes.
        got: usize,
        /// Byte offset of the field within the information field.
        position: usize,
    },
    /// A coordinate's whole-minutes field was 60 or more.
    MinutesOutOfRange {
        /// The rejected whole-minutes value.
        got: u8,
        /// Byte offset of the minutes digits within the information
        /// field.
        position: usize,
    },
    /// A hemisphere field was not a single `N`/`S` (latitude) or
    /// `E`/`W` (longitude), in either case.
    BadHemisphere {
        /// The rejected byte.
        got: u8,
    },
    /// A latitude was outside `-90..=90` degrees.
    BadLatitude {
        /// The rejected value in signed 1/100 arc-minutes.
        got: i64,
    },
    /// A longitude was outside `-180..=180` degrees.
    BadLongitude {
        /// The rejected value in signed 1/100 arc-minutes.
        got: i64,
    },
    /// A time or date component was out of range.
    BadTimestamp {
        /// The component: `h` hour, `m` minute, `s` second, `D` day or
        /// `M` month.
        field: u8,
        /// The rejected value.
        got: u8,
    },
    /// A time field was shorter than the mandatory `hhmmss`.
    BadTimeLength {
        /// The rejected length in bytes.
        got: usize,
    },
    /// A date field was not exactly the six digits of `ddmmyy`.
    BadDateLength {
        /// The rejected length in bytes.
        got: usize,
    },
}

impl fmt::Display for NmeaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            NmeaError::Empty => write!(
                f,
                "information field is empty: a raw NMEA report starts with '$'"
            ),
            NmeaError::MissingStartDelimiter { got } => write!(
                f,
                "first byte 0x{got:02X} is not the NMEA start delimiter: 0x24 '$' is required"
            ),
            NmeaError::NonPrintable { got, position } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is above 0x7E: NMEA 0183 is printable ASCII"
            ),
            NmeaError::BadAddressLength { got } => write!(
                f,
                "address field of {got} bytes is invalid: a talker + formatter tag is 5 characters"
            ),
            NmeaError::UnsupportedFormatter { got } => write!(
                f,
                "sentence formatter '{}{}{}' is not decoded: RMC, GGA, GLL, VTG or WPL is required",
                got[0] as char, got[1] as char, got[2] as char
            ),
            NmeaError::TooFewFields { got, min } => write!(
                f,
                "sentence of {got} fields is truncated: at least {min} are required"
            ),
            NmeaError::BadDigit { got, position } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is not an ASCII digit"
            ),
            NmeaError::ExpectedByte {
                expected,
                got,
                position,
            } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is wrong: 0x{expected:02X} is required here"
            ),
            NmeaError::EmptyNumber { position } => {
                write!(f, "numeric field at offset {position} holds no digits")
            }
            NmeaError::NumberOutOfRange { position } => write!(
                f,
                "numeric field at offset {position} is out of range for its decoded type"
            ),
            NmeaError::MissingDecimalPoint { position } => write!(
                f,
                "coordinate at offset {position} has no '.': the format is ddmm.mmmm"
            ),
            NmeaError::CoordinateTooShort { got, position } => write!(
                f,
                "coordinate of {got} bytes at offset {position} is too short: at least one degree digit must precede the two whole-minute digits"
            ),
            NmeaError::MinutesOutOfRange { got, position } => write!(
                f,
                "whole minutes of {got} at offset {position} are out of range: must be below 60"
            ),
            NmeaError::BadHemisphere { got } => write!(
                f,
                "hemisphere byte 0x{got:02X} is invalid: 'N'/'S' (latitude) or 'E'/'W' (longitude) is required"
            ),
            NmeaError::BadLatitude { got } => write!(
                f,
                "latitude of {got} 1/100 arc-minutes is out of range: must be within \u{b1}90\u{b0}"
            ),
            NmeaError::BadLongitude { got } => write!(
                f,
                "longitude of {got} 1/100 arc-minutes is out of range: must be within \u{b1}180\u{b0}"
            ),
            NmeaError::BadTimestamp { field, got } => write!(
                f,
                "timestamp component '{}' of {got} is out of range",
                field as char
            ),
            NmeaError::BadTimeLength { got } => write!(
                f,
                "time field of {got} bytes is too short: hhmmss (6 digits) is the minimum"
            ),
            NmeaError::BadDateLength { got } => write!(
                f,
                "date field of {got} bytes is invalid: ddmmyy (exactly 6 digits) is required"
            ),
        }
    }
}

impl core::error::Error for NmeaError {}

/// Decodes a raw NMEA information field, `$` included.
///
/// A trailing `CR`, `LF` or blank run is trimmed first, so both the
/// bare sentence and the line-terminated form a serial receiver emits
/// are accepted. The `*hh` checksum is verified but never enforced;
/// its state is reported in [`NmeaSentence::checksum`].
///
/// # Errors
///
/// [`NmeaError::Empty`] and [`NmeaError::MissingStartDelimiter`] for a
/// field that is not a raw NMEA report at all,
/// [`NmeaError::UnsupportedFormatter`] for a sentence this module does
/// not decode (`GSA`, `GSV`, `ZDA`, proprietary `$P...`), and the
/// field-level variants — [`NmeaError::TooFewFields`],
/// [`NmeaError::BadDigit`], [`NmeaError::MinutesOutOfRange`],
/// [`NmeaError::BadHemisphere`], [`NmeaError::BadTimestamp`] and
/// friends — each carrying the offending byte or value and its offset.
///
/// # Examples
///
/// A non-`GP` talker and a five-decimal coordinate, with the course
/// field empty:
///
/// ```
/// use warble::aprs::nmea::{self, Constellation, NmeaData, NmeaError};
///
/// let sentence = nmea::parse(b"$GNRMC,001031.00,A,4404.13993,N,12118.86023,W,0.146,,100117,,,A*7B")?;
/// assert_eq!(sentence.talker.constellation(), Constellation::Combined);
/// match sentence.data {
///     NmeaData::Rmc(rmc) => {
///         assert_eq!(rmc.speed_milliknots, Some(146));
///         assert_eq!(rmc.course_centidegrees, None); // empty is "no data"
///     }
///     _ => unreachable!("the formatter is RMC"),
/// }
/// # Ok::<(), NmeaError>(())
/// ```
///
/// A waypoint is never mistaken for the station's position:
///
/// ```
/// use warble::aprs::nmea::{self, NmeaError};
///
/// let sentence = nmea::parse(b"$GPWPL,4807.038,N,01131.000,E,HOME*46")?;
/// assert_eq!(sentence.position(), None);
/// # Ok::<(), NmeaError>(())
/// ```
pub fn parse(info: &[u8]) -> Result<NmeaSentence<'_>, NmeaError> {
    // A relayed sentence may still carry its serial line terminator.
    let mut end = info.len();
    while end > 0 && matches!(info[end - 1], b'\r' | b'\n' | b' ' | b'\t') {
        end -= 1;
    }
    let info = &info[..end];

    let Some((&first, rest)) = info.split_first() else {
        return Err(NmeaError::Empty);
    };
    if first != b'$' {
        return Err(NmeaError::MissingStartDelimiter { got: first });
    }
    for (i, &byte) in rest.iter().enumerate() {
        if byte > 0x7E {
            return Err(NmeaError::NonPrintable {
                got: byte,
                position: i + 1,
            });
        }
    }

    let (body, checksum) = split_checksum(rest);

    let (_, tag) = field(body, 0);
    let [t0, t1, f0, f1, f2] =
        *<&[u8; 5]>::try_from(tag).map_err(|_| NmeaError::BadAddressLength { got: tag.len() })?;
    let talker = Talker([t0.to_ascii_uppercase(), t1.to_ascii_uppercase()]);
    let formatter_bytes = [
        f0.to_ascii_uppercase(),
        f1.to_ascii_uppercase(),
        f2.to_ascii_uppercase(),
    ];
    let formatter =
        Formatter::from_bytes(formatter_bytes).ok_or(NmeaError::UnsupportedFormatter {
            got: formatter_bytes,
        })?;

    let count = field_count(body);
    let data = match formatter {
        Formatter::Rmc => NmeaData::Rmc(parse_rmc(body, count)?),
        Formatter::Gga => NmeaData::Gga(parse_gga(body, count)?),
        Formatter::Gll => NmeaData::Gll(parse_gll(body, count)?),
        Formatter::Vtg => NmeaData::Vtg(parse_vtg(body, count)?),
        Formatter::Wpl => NmeaData::Wpl(parse_wpl(body, count)?),
    };

    Ok(NmeaSentence {
        talker,
        checksum,
        data,
    })
}

/// Splits the trailing `*hh` checksum off the sentence.
///
/// `rest` is everything after the `$`. The `*` is searched for from the
/// end (a stray `*` inside a field must not shadow the real one). A
/// trailer that is not exactly two hex digits is not a checksum at all:
/// the sentence is then treated as unchecksummed and the `*` stays in
/// the body, which is the recovering choice for a truncated frame.
fn split_checksum(rest: &[u8]) -> (&[u8], ChecksumStatus) {
    if let Some(star) = rest.iter().rposition(|&b| b == b'*')
        && let [high, low] = rest[star + 1..]
        && let (Some(high), Some(low)) = (hex_digit(high), hex_digit(low))
    {
        let body = &rest[..star];
        let received = (high << 4) | low;
        let computed = body.iter().fold(0u8, |acc, &b| acc ^ b);
        let status = if computed == received {
            ChecksumStatus::Valid
        } else {
            ChecksumStatus::Invalid { computed, received }
        };
        return (body, status);
    }
    (rest, ChecksumStatus::Absent)
}

/// Decodes one hexadecimal digit in either case.
const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// The `index`-th comma-separated field of `body` together with its
/// byte offset within the whole information field (hence the `1`: the
/// body starts after the `$`).
///
/// A field past the end of the sentence reads as empty, so an optional
/// trailing field needs no separate presence check.
fn field(body: &[u8], index: usize) -> (usize, &[u8]) {
    let mut offset = 1;
    for (i, part) in body.split(|&b| b == b',').enumerate() {
        if i == index {
            return (offset, part);
        }
        offset += part.len() + 1;
    }
    (offset, &[])
}

/// The number of comma-separated fields, the tag included.
fn field_count(body: &[u8]) -> usize {
    body.iter().filter(|&&b| b == b',').count() + 1
}

/// The `index`-th field, or `None` when it is absent or empty.
///
/// An empty NMEA field means "no data" and never zero, so every
/// optional value funnels through here.
fn opt(body: &[u8], index: usize) -> Option<(usize, &[u8])> {
    let (at, part) = field(body, index);
    if part.is_empty() {
        None
    } else {
        Some((at, part))
    }
}

/// The first byte of the `index`-th field, when it is not empty.
fn first_byte(body: &[u8], index: usize) -> Option<u8> {
    match opt(body, index) {
        Some((_, [byte, ..])) => Some(*byte),
        _ => None,
    }
}

/// The FAA mode indicator of the `index`-th field, when present.
fn parse_mode(body: &[u8], index: usize) -> Option<FaaMode> {
    first_byte(body, index).map(FaaMode::from_byte)
}

/// One ASCII digit, or a typed error carrying the offending byte.
fn to_digit(byte: u8, position: usize) -> Result<u8, NmeaError> {
    if byte.is_ascii_digit() {
        Ok(byte - b'0')
    } else {
        Err(NmeaError::BadDigit {
            got: byte,
            position,
        })
    }
}

/// Two ASCII digits as a value `0..=99`.
fn two_digits(digits: &[u8], offset: usize) -> Result<u8, NmeaError> {
    match digits {
        [tens, ones, ..] => Ok(to_digit(*tens, offset)? * 10 + to_digit(*ones, offset + 1)?),
        _ => Err(NmeaError::EmptyNumber { position: offset }),
    }
}

/// A run of ASCII digits as an unsigned value.
fn parse_uint(digits: &[u8], offset: usize) -> Result<u64, NmeaError> {
    if digits.is_empty() {
        return Err(NmeaError::EmptyNumber { position: offset });
    }
    let mut value = 0u64;
    for (i, &byte) in digits.iter().enumerate() {
        let digit = u64::from(to_digit(byte, offset + i)?);
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit))
            .ok_or(NmeaError::NumberOutOfRange { position: offset })?;
    }
    Ok(value)
}

/// Fractional digits scaled to `10^scale`, rounded half away from zero.
///
/// Every byte is validated even past the scale, so trailing garbage in
/// an over-precise field is still caught. The result is at most
/// `10^scale` (a carry out of the fraction), which the callers fold
/// into the whole part.
fn scale_fraction(digits: &[u8], offset: usize, scale: usize) -> Result<u64, NmeaError> {
    let mut value = 0u64;
    let mut taken = 0usize;
    for (i, &byte) in digits.iter().enumerate() {
        let digit = u64::from(to_digit(byte, offset + i)?);
        if taken < scale {
            value = value * 10 + digit;
            taken += 1;
        } else if taken == scale {
            // The first discarded digit decides the rounding; the rest
            // are validated only.
            if digit >= 5 {
                value += 1;
            }
            taken += 1;
        }
    }
    while taken < scale {
        value *= 10;
        taken += 1;
    }
    Ok(value)
}

/// Parses `[-+]?ddd[.ddd]` into a value scaled by `10^scale`, rounding
/// half away from zero.
fn parse_scaled(value: &[u8], offset: usize, scale: usize) -> Result<i64, NmeaError> {
    let (negative, skip) = match value.first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let rest = &value[skip..];
    let (whole_digits, frac_digits) = match rest.iter().position(|&b| b == b'.') {
        Some(dot) => (&rest[..dot], &rest[dot + 1..]),
        None => (rest, &[][..]),
    };
    if whole_digits.is_empty() && frac_digits.is_empty() {
        return Err(NmeaError::EmptyNumber { position: offset });
    }
    let whole = if whole_digits.is_empty() {
        0
    } else {
        parse_uint(whole_digits, offset + skip)?
    };
    let frac = scale_fraction(frac_digits, offset + skip + whole_digits.len() + 1, scale)?;
    let scaled = whole
        .checked_mul(POW10[scale])
        .and_then(|v| v.checked_add(frac))
        .and_then(|v| i64::try_from(v).ok())
        .ok_or(NmeaError::NumberOutOfRange { position: offset })?;
    Ok(if negative { -scaled } else { scaled })
}

/// An optional unsigned scaled field; a negative value is rejected.
fn parse_unsigned(body: &[u8], index: usize, scale: usize) -> Result<Option<u32>, NmeaError> {
    let Some((at, value)) = opt(body, index) else {
        return Ok(None);
    };
    let scaled = parse_scaled(value, at, scale)?;
    u32::try_from(scaled)
        .map(Some)
        .map_err(|_| NmeaError::NumberOutOfRange { position: at })
}

/// An optional signed scaled field.
fn parse_signed(body: &[u8], index: usize, scale: usize) -> Result<Option<i32>, NmeaError> {
    let Some((at, value)) = opt(body, index) else {
        return Ok(None);
    };
    let scaled = parse_scaled(value, at, scale)?;
    i32::try_from(scaled)
        .map(Some)
        .map_err(|_| NmeaError::NumberOutOfRange { position: at })
}

/// An optional small unsigned integer field (satellite count, quality).
fn parse_count(body: &[u8], index: usize) -> Result<Option<u8>, NmeaError> {
    let Some((at, value)) = opt(body, index) else {
        return Ok(None);
    };
    u8::try_from(parse_uint(value, at)?)
        .map(Some)
        .map_err(|_| NmeaError::NumberOutOfRange { position: at })
}

/// Which coordinate a `ddmm.mmmm` field carries.
#[derive(Clone, Copy)]
enum Axis {
    /// A latitude: `N`/`S`, at most 90 degrees.
    Latitude,
    /// A longitude: `E`/`W`, at most 180 degrees.
    Longitude,
}

/// Parses a `ddmm.mmmm` / `dddmm.mmmm` magnitude plus its hemisphere
/// letter into signed 1/100 arc-minutes.
///
/// The degree-digit count is **not** hardcoded: the `.` is located,
/// the two characters before it are the whole minutes, and everything
/// before those is the degrees. The fraction may be one digit or ten.
fn parse_coordinate(
    body: &[u8],
    value_index: usize,
    hemisphere_index: usize,
    axis: Axis,
) -> Result<Option<i64>, NmeaError> {
    let (Some((at, value)), Some((_, hemisphere))) =
        (opt(body, value_index), opt(body, hemisphere_index))
    else {
        // Either half missing means "no data": an unsigned magnitude is
        // not a position.
        return Ok(None);
    };
    let sign: i64 = match (axis, hemisphere) {
        (Axis::Latitude, [b'N' | b'n']) => 1,
        (Axis::Latitude, [b'S' | b's']) => -1,
        (Axis::Longitude, [b'E' | b'e']) => 1,
        (Axis::Longitude, [b'W' | b'w']) => -1,
        _ => {
            return Err(NmeaError::BadHemisphere {
                got: hemisphere.first().copied().unwrap_or(0),
            });
        }
    };
    let dot = value
        .iter()
        .position(|&b| b == b'.')
        .ok_or(NmeaError::MissingDecimalPoint { position: at })?;
    if dot < 3 {
        return Err(NmeaError::CoordinateTooShort {
            got: value.len(),
            position: at,
        });
    }
    let degrees = parse_uint(&value[..dot - 2], at)?;
    let minutes = two_digits(&value[dot - 2..dot], at + dot - 2)?;
    if minutes >= 60 {
        return Err(NmeaError::MinutesOutOfRange {
            got: minutes,
            position: at + dot - 2,
        });
    }
    // A fraction that rounds up to a full minute simply carries.
    //
    // NMEA does not fix the number of decimal places: the standard
    // calls it "a variable number of digits", four is the figure
    // usually quoted, and current receivers emit five by default and up
    // to seven. Scaling to a fixed two digits, as this did, threw away
    // everything past 1/100 arc-minute and put a 3.6 m error on a
    // five-place sentence. FRACTION_DIGITS places are now kept exactly,
    // because the storage unit was chosen so that a minute divides by
    // 10^7 without remainder; anything longer is rounded to nearest by
    // `scale_fraction` as before.
    let fraction = scale_fraction(&value[dot + 1..], at + dot + 1, FRACTION_DIGITS)?;
    let magnitude = degrees
        .checked_mul(UNITS_PER_DEGREE)
        .and_then(|v| v.checked_add(u64::from(minutes) * UNITS_PER_MINUTE))
        .and_then(|v| v.checked_add(fraction * UNITS_PER_FRACTION))
        .and_then(|v| i64::try_from(v).ok())
        .ok_or(match axis {
            Axis::Latitude => NmeaError::BadLatitude { got: i64::MAX },
            Axis::Longitude => NmeaError::BadLongitude { got: i64::MAX },
        })?;
    Ok(Some(sign * magnitude))
}

/// A `ddmm.mmmm` + `N`/`S` pair as a validated [`Latitude`].
fn parse_latitude(
    body: &[u8],
    value_index: usize,
    hemisphere_index: usize,
) -> Result<Option<Latitude>, NmeaError> {
    match parse_coordinate(body, value_index, hemisphere_index, Axis::Latitude)? {
        Some(hundredths) => Latitude::new(hundredths)
            .map(Some)
            .map_err(|_| NmeaError::BadLatitude { got: hundredths }),
        None => Ok(None),
    }
}

/// A `dddmm.mmmm` + `E`/`W` pair as a validated [`Longitude`].
fn parse_longitude(
    body: &[u8],
    value_index: usize,
    hemisphere_index: usize,
) -> Result<Option<Longitude>, NmeaError> {
    match parse_coordinate(body, value_index, hemisphere_index, Axis::Longitude)? {
        Some(hundredths) => Longitude::new(hundredths)
            .map(Some)
            .map_err(|_| NmeaError::BadLongitude { got: hundredths }),
        None => Ok(None),
    }
}

/// An `hhmmss[.sss]` UTC time field.
fn parse_time(body: &[u8], index: usize) -> Result<Option<NmeaTime>, NmeaError> {
    let Some((at, value)) = opt(body, index) else {
        return Ok(None);
    };
    if value.len() < 6 {
        return Err(NmeaError::BadTimeLength { got: value.len() });
    }
    let hour = two_digits(&value[0..2], at)?;
    if hour > 23 {
        return Err(NmeaError::BadTimestamp {
            field: b'h',
            got: hour,
        });
    }
    let minute = two_digits(&value[2..4], at + 2)?;
    if minute > 59 {
        return Err(NmeaError::BadTimestamp {
            field: b'm',
            got: minute,
        });
    }
    // 60 admits a leap second.
    let second = two_digits(&value[4..6], at + 4)?;
    if second > 60 {
        return Err(NmeaError::BadTimestamp {
            field: b's',
            got: second,
        });
    }
    let mut millisecond = 0u16;
    if value.len() > 6 {
        if value[6] != b'.' {
            return Err(NmeaError::ExpectedByte {
                expected: b'.',
                got: value[6],
                position: at + 6,
            });
        }
        // Truncated, never rounded: `.9999` must not invent a second.
        let mut taken = 0;
        for (i, &byte) in value[7..].iter().enumerate() {
            let digit = u16::from(to_digit(byte, at + 7 + i)?);
            if taken < 3 {
                millisecond = millisecond * 10 + digit;
                taken += 1;
            }
        }
        while taken < 3 {
            millisecond *= 10;
            taken += 1;
        }
    }
    Ok(Some(NmeaTime {
        hour,
        minute,
        second,
        millisecond,
    }))
}

/// A `ddmmyy` UTC date field.
fn parse_date(body: &[u8], index: usize) -> Result<Option<NmeaDate>, NmeaError> {
    let Some((at, value)) = opt(body, index) else {
        return Ok(None);
    };
    if value.len() != 6 {
        return Err(NmeaError::BadDateLength { got: value.len() });
    }
    let day = two_digits(&value[0..2], at)?;
    if day == 0 || day > 31 {
        return Err(NmeaError::BadTimestamp {
            field: b'D',
            got: day,
        });
    }
    let month = two_digits(&value[2..4], at + 2)?;
    if month == 0 || month > 12 {
        return Err(NmeaError::BadTimestamp {
            field: b'M',
            got: month,
        });
    }
    let year = two_digits(&value[4..6], at + 4)?;
    Ok(Some(NmeaDate { day, month, year }))
}

/// A magnetic variation magnitude plus its `E`/`W` byte, east positive.
fn parse_variation(
    body: &[u8],
    value_index: usize,
    hemisphere_index: usize,
) -> Result<Option<i32>, NmeaError> {
    let (Some((at, value)), Some((_, hemisphere))) =
        (opt(body, value_index), opt(body, hemisphere_index))
    else {
        return Ok(None);
    };
    let sign: i32 = match hemisphere {
        [b'E' | b'e'] => 1,
        [b'W' | b'w'] => -1,
        _ => {
            return Err(NmeaError::BadHemisphere {
                got: hemisphere.first().copied().unwrap_or(0),
            });
        }
    };
    let scaled = parse_scaled(value, at, 2)?;
    let scaled = i32::try_from(scaled).map_err(|_| NmeaError::NumberOutOfRange { position: at })?;
    Ok(Some(sign * scaled))
}

/// Whether a coordinate pair is usable: both halves present and not the
/// all-zero pair a receiver emits before it has a fix.
fn has_position(latitude: Option<Latitude>, longitude: Option<Longitude>) -> bool {
    match (latitude, longitude) {
        (Some(latitude), Some(longitude)) => latitude.units() != 0 || longitude.units() != 0,
        _ => false,
    }
}

/// Classifies an RMC/GLL `A`/`V` status byte.
///
/// A `V` is never a rejection: only a missing or all-zero coordinate
/// pair is [`FixQuality::Invalid`].
fn classify_status(
    status: Option<u8>,
    latitude: Option<Latitude>,
    longitude: Option<Longitude>,
) -> FixQuality {
    if !has_position(latitude, longitude) {
        FixQuality::Invalid
    } else if matches!(status, Some(b'A' | b'a')) {
        FixQuality::Valid
    } else {
        FixQuality::Degraded
    }
}

/// Classifies a GGA quality indicator.
///
/// The fix test is `!= 0`; the modes that are not a live GNSS
/// measurement (dead reckoning, manual input, simulator) are reported
/// as `Degraded` rather than dropped.
fn classify_quality(
    quality: Option<GgaQuality>,
    latitude: Option<Latitude>,
    longitude: Option<Longitude>,
) -> FixQuality {
    if !has_position(latitude, longitude) {
        return FixQuality::Invalid;
    }
    match quality {
        Some(quality) if !quality.has_fix() => FixQuality::Invalid,
        Some(GgaQuality::DeadReckoning | GgaQuality::ManualInput | GgaQuality::Simulator)
        | None => FixQuality::Degraded,
        Some(_) => FixQuality::Valid,
    }
}

/// Decodes an `RMC` body.
fn parse_rmc(body: &[u8], count: usize) -> Result<Rmc, NmeaError> {
    if count < RMC_MIN_FIELDS {
        return Err(NmeaError::TooFewFields {
            got: count,
            min: RMC_MIN_FIELDS,
        });
    }
    let status = first_byte(body, 2);
    let latitude = parse_latitude(body, 3, 4)?;
    let longitude = parse_longitude(body, 5, 6)?;
    Ok(Rmc {
        time: parse_time(body, 1)?,
        status,
        latitude,
        longitude,
        speed_milliknots: parse_unsigned(body, 7, 3)?,
        course_centidegrees: parse_unsigned(body, 8, 2)?,
        date: parse_date(body, 9)?,
        magnetic_variation_centidegrees: parse_variation(body, 10, 11)?,
        mode: parse_mode(body, 12),
        navigation_status: first_byte(body, 13),
        fix: classify_status(status, latitude, longitude),
    })
}

/// Decodes a `GGA` body.
fn parse_gga(body: &[u8], count: usize) -> Result<Gga, NmeaError> {
    if count < GGA_MIN_FIELDS {
        return Err(NmeaError::TooFewFields {
            got: count,
            min: GGA_MIN_FIELDS,
        });
    }
    let latitude = parse_latitude(body, 2, 3)?;
    let longitude = parse_longitude(body, 4, 5)?;
    let quality = parse_count(body, 6)?.map(GgaQuality::from_raw);
    Ok(Gga {
        time: parse_time(body, 1)?,
        latitude,
        longitude,
        quality,
        satellites: parse_count(body, 7)?,
        hdop_hundredths: parse_unsigned(body, 8, 2)?,
        altitude_centimeters: parse_signed(body, 9, 2)?,
        geoid_separation_centimeters: parse_signed(body, 11, 2)?,
        fix: classify_quality(quality, latitude, longitude),
    })
}

/// Decodes a `GLL` body.
fn parse_gll(body: &[u8], count: usize) -> Result<Gll, NmeaError> {
    if count < GLL_MIN_FIELDS {
        return Err(NmeaError::TooFewFields {
            got: count,
            min: GLL_MIN_FIELDS,
        });
    }
    let latitude = parse_latitude(body, 1, 2)?;
    let longitude = parse_longitude(body, 3, 4)?;
    let status = first_byte(body, 6);
    Ok(Gll {
        latitude,
        longitude,
        time: parse_time(body, 5)?,
        status,
        mode: parse_mode(body, 7),
        fix: classify_status(status, latitude, longitude),
    })
}

/// Decodes a `VTG` body, disambiguating the two historical layouts on
/// field 2 being the literal `T`.
fn parse_vtg(body: &[u8], count: usize) -> Result<Vtg, NmeaError> {
    let modern = matches!(field(body, 2).1, [b'T' | b't']);
    let min = if modern {
        VTG_MODERN_FIELDS
    } else {
        VTG_LEGACY_FIELDS
    };
    if count < min {
        return Err(NmeaError::TooFewFields { got: count, min });
    }
    if modern {
        Ok(Vtg {
            form: VtgForm::Modern,
            course_true_centidegrees: parse_unsigned(body, 1, 2)?,
            course_magnetic_centidegrees: parse_unsigned(body, 3, 2)?,
            speed_milliknots: parse_unsigned(body, 5, 3)?,
            speed_meters_per_hour: parse_unsigned(body, 7, 3)?,
            mode: parse_mode(body, 9),
        })
    } else {
        Ok(Vtg {
            form: VtgForm::Legacy,
            course_true_centidegrees: parse_unsigned(body, 1, 2)?,
            course_magnetic_centidegrees: parse_unsigned(body, 2, 2)?,
            speed_milliknots: parse_unsigned(body, 3, 3)?,
            speed_meters_per_hour: parse_unsigned(body, 4, 3)?,
            mode: None,
        })
    }
}

/// Decodes a `WPL` body.
fn parse_wpl(body: &[u8], count: usize) -> Result<Wpl<'_>, NmeaError> {
    if count < WPL_MIN_FIELDS {
        return Err(NmeaError::TooFewFields {
            got: count,
            min: WPL_MIN_FIELDS,
        });
    }
    Ok(Wpl {
        latitude: parse_latitude(body, 1, 2)?,
        longitude: parse_longitude(body, 3, 4)?,
        name: field(body, 5).1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses or reports the typed error verbatim.
    fn ok(input: &[u8]) -> NmeaSentence<'_> {
        match parse(input) {
            Ok(sentence) => sentence,
            Err(e) => panic!("{e}"),
        }
    }

    /// Known-answer vectors for the three unit-typed accessors.
    ///
    /// Written before the accessors, and **not** a round trip:
    /// `from_x(n).x() == n` holds for any consistent factor, which is
    /// exactly how a wrong constant ships (§14.2 of the data model
    /// plan). Every expectation below is arithmetic on a published
    /// definition — 1 kn = 1.852 km/h, 1 international mile = 1609.344 m,
    /// 1 ft = 0.3048 m — done by hand, and each is asserted in a unit
    /// the sentence does *not* state.
    #[test]
    fn unit_typed_accessors_have_known_answers() {
        // 34.0 knots, course 090.5 degrees true.
        let sentence =
            ok(b"$GPRMC,013641.06,A,3348.1607,N,11807.4631,W,34.0,090.5,231105,13.,E*73");
        let speed = sentence.speed().expect("RMC carries a speed");
        assert_eq!(speed.knots(), 34, "34.0 kn reads back as 34 kn");
        // 34 kn = 62.968 km/h = 39.1265 mph; both round to these.
        assert_eq!(speed.kmh(), 63);
        assert_eq!(speed.mph(), 39);
        // 090.5 rounds half away from zero to 91, not down to 90.
        assert_eq!(
            sentence.course().map(Bearing::degrees),
            Some(91),
            "090.5 degrees rounds to 91"
        );
        assert_eq!(sentence.altitude(), None, "RMC carries no altitude");

        // 114.2 m above mean sea level.
        let sentence = ok(b"$GPGGA,040332,3405.438,N,11801.836,W,1,06,1.1,114.2,M,-31.5,M,,*75");
        let altitude = sentence.altitude().expect("GGA carries an altitude");
        assert_eq!(altitude.meters(), 114);
        // 114.2 m / 0.3048 = 374.67 ft, which rounds to 375.
        assert_eq!(altitude.feet(), 375, "114.2 m is 375 ft, not 374");
        assert_eq!(sentence.speed(), None, "GGA carries no speed");
        assert_eq!(sentence.course(), None, "GGA carries no course");

        // 360.0 degrees is due north and must fold to 0, not overflow
        // the 0..=359 Bearing range.
        let sentence = ok(b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62");
        assert_eq!(sentence.course().map(Bearing::degrees), Some(0));
        assert_eq!(sentence.speed().map(Speed::knots), Some(0));

        // An empty field is unknown; a transmitted zero is a value.
        let sentence = ok(b"$GPRMC,081836,A,3751.65,S,14507.36,E,,,130998,011.3,E*62");
        assert_eq!(sentence.course(), None);
        assert_eq!(sentence.speed(), None);

        // VTG carries course and speed but no position, and falls back
        // to the metric K field when the knots field is empty.
        let sentence = ok(b"$GPVTG,054.7,T,034.4,M,,N,10.2,K*4A");
        assert_eq!(sentence.position(), None);
        assert_eq!(sentence.course().map(Bearing::degrees), Some(55));
        // 10.2 km/h = 5.5076 kn, rounding to 6, and 6.34 mph to 6.
        let speed = sentence.speed().expect("VTG K field");
        assert_eq!(speed.kmh(), 10);
        assert_eq!(speed.knots(), 6);
    }

    /// The RMC payload of a sentence known to be RMC.
    fn rmc(input: &[u8]) -> Rmc {
        match ok(input).data {
            NmeaData::Rmc(rmc) => rmc,
            other => panic!("expected RMC, got {other:?}"),
        }
    }

    /// Storage units to signed 1/100 arc-minutes, rounded to nearest.
    fn to_hundredths(units: i64) -> i64 {
        let step = crate::geo::UNITS_PER_HUNDREDTH_MINUTE;
        let half = if units < 0 { -step / 2 } else { step / 2 };
        (units + half) / step
    }

    /// Signed 1/100 arc-minutes of a latitude that must be present.
    ///
    /// The storage unit is finer than this, so the fixtures below,
    /// which are all written in hundredths, come through a rounding
    /// conversion. `fraction_length_varies_and_rounds_half_up` asserts
    /// the exact stored value instead, because that is the test whose
    /// subject is the precision.
    fn lat_of(sentence: &NmeaSentence<'_>) -> i64 {
        match sentence.position() {
            Some(coordinates) => to_hundredths(coordinates.latitude.units()),
            None => panic!("expected a position"),
        }
    }

    /// Signed 1/100 arc-minutes of a longitude that must be present.
    fn lon_of(sentence: &NmeaSentence<'_>) -> i64 {
        match sentence.position() {
            Some(coordinates) => to_hundredths(coordinates.longitude.units()),
            None => panic!("expected a position"),
        }
    }

    // -- Worked decodes of real sentences ----------------------------

    #[test]
    fn rmc_nmea_2_0_twelve_fields() {
        // 37 deg 51.65 min S, 145 deg 07.36 min E, stationary, due
        // north, 13 September 1998, 11.3 deg east variation.
        let input = b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62";
        let sentence = ok(input);
        assert_eq!(sentence.talker.as_bytes(), *b"GP");
        assert_eq!(sentence.talker.constellation(), Constellation::Gps);
        assert_eq!(sentence.formatter(), Formatter::Rmc);
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
        // -(37*6000 + 51*100 + 65) and +(145*6000 + 7*100 + 36).
        assert_eq!(lat_of(&sentence), -227_165);
        assert_eq!(lon_of(&sentence), 870_736);
        // -37.860833 deg / +145.122667 deg.
        assert!(
            (sentence.position().map_or(0.0, |c| c.latitude.to_degrees()) + 37.860_833).abs()
                < 1e-6
        );
        assert!(
            (sentence
                .position()
                .map_or(0.0, |c| c.longitude.to_degrees())
                - 145.122_667)
                .abs()
                < 1e-6
        );

        let report = rmc(input);
        assert_eq!(report.status, Some(b'A'));
        assert_eq!(report.fix, FixQuality::Valid);
        assert_eq!(report.speed_milliknots, Some(0));
        // 360 is due north, never folded to 0.
        assert_eq!(report.course_centidegrees, Some(36_000));
        assert_eq!(
            report.time,
            Some(NmeaTime {
                hour: 8,
                minute: 18,
                second: 36,
                millisecond: 0,
            })
        );
        assert_eq!(
            report.date,
            Some(NmeaDate {
                day: 13,
                month: 9,
                year: 98,
            })
        );
        assert_eq!(report.magnetic_variation_centidegrees, Some(1130));
        assert_eq!(report.mode, None);
        assert_eq!(report.navigation_status, None);
    }

    #[test]
    fn rmc_nmea_2_3_thirteen_fields_gn_talker() {
        // GN talker, five fractional-minute digits, empty course,
        // FAA mode present.
        let input = b"$GNRMC,001031.00,A,4404.13993,N,12118.86023,W,0.146,,100117,,,A*7B";
        let sentence = ok(input);
        assert_eq!(sentence.talker.as_bytes(), *b"GN");
        assert_eq!(sentence.talker.constellation(), Constellation::Combined);
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
        // 44*6000 + 4*100 + round(13.993) and -(121*6000 + 18*100 + round(86.023)).
        assert_eq!(lat_of(&sentence), 264_414);
        assert_eq!(lon_of(&sentence), -727_886);

        let report = rmc(input);
        assert_eq!(report.fix, FixQuality::Valid);
        assert_eq!(report.speed_milliknots, Some(146));
        // Empty is "no data", never zero.
        assert_eq!(report.course_centidegrees, None);
        assert_eq!(
            report.time,
            Some(NmeaTime {
                hour: 0,
                minute: 10,
                second: 31,
                millisecond: 0,
            })
        );
        assert_eq!(
            report.date,
            Some(NmeaDate {
                day: 10,
                month: 1,
                year: 17,
            })
        );
        assert_eq!(report.magnetic_variation_centidegrees, None);
        assert_eq!(report.mode, Some(FaaMode::Autonomous));
    }

    #[test]
    fn gga_fifteen_fields() {
        let input = b"$GPGGA,170834,4124.8963,N,08151.6838,W,1,05,1.5,280.2,M,-34.0,M,,*75";
        let sentence = ok(input);
        assert_eq!(sentence.formatter(), Formatter::Gga);
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
        // 41*6000 + 24*100 + round(89.63) and -(81*6000 + 51*100 + round(68.38)).
        assert_eq!(lat_of(&sentence), 248_490);
        assert_eq!(lon_of(&sentence), -491_168);

        let NmeaData::Gga(report) = sentence.data else {
            panic!("expected GGA");
        };
        assert_eq!(
            report.time,
            Some(NmeaTime {
                hour: 17,
                minute: 8,
                second: 34,
                millisecond: 0,
            })
        );
        assert_eq!(report.quality, Some(GgaQuality::Gps));
        assert_eq!(report.satellites, Some(5));
        assert_eq!(report.hdop_hundredths, Some(150));
        assert_eq!(report.altitude_centimeters, Some(28_020));
        assert_eq!(report.geoid_separation_centimeters, Some(-3400));
        assert_eq!(report.fix, FixQuality::Valid);
    }

    #[test]
    fn gll_seven_fields() {
        let input = b"$GPGLL,4748.811,N,12219.564,W,033850,A*3C";
        let sentence = ok(input);
        assert_eq!(sentence.formatter(), Formatter::Gll);
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
        // 47*6000 + 48*100 + round(81.1) and -(122*6000 + 19*100 + round(56.4)).
        assert_eq!(lat_of(&sentence), 286_881);
        assert_eq!(lon_of(&sentence), -733_956);

        let NmeaData::Gll(report) = sentence.data else {
            panic!("expected GLL");
        };
        assert_eq!(
            report.time,
            Some(NmeaTime {
                hour: 3,
                minute: 38,
                second: 50,
                millisecond: 0,
            })
        );
        assert_eq!(report.status, Some(b'A'));
        assert_eq!(report.mode, None);
        assert_eq!(report.fix, FixQuality::Valid);
    }

    #[test]
    fn vtg_modern_carries_no_position() {
        let input = b"$GPVTG,220.86,T,,M,2.550,N,4.724,K,A*34";
        let sentence = ok(input);
        assert_eq!(sentence.formatter(), Formatter::Vtg);
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
        // VTG has no position, and no fix classification either.
        assert_eq!(sentence.position(), None);
        assert_eq!(sentence.fix(), None);
        assert_eq!(sentence.time(), None);

        let NmeaData::Vtg(report) = sentence.data else {
            panic!("expected VTG");
        };
        assert_eq!(report.form, VtgForm::Modern);
        assert_eq!(report.course_true_centidegrees, Some(22_086));
        assert_eq!(report.course_magnetic_centidegrees, None);
        assert_eq!(report.speed_milliknots, Some(2550));
        assert_eq!(report.speed_meters_per_hour, Some(4724));
        assert_eq!(report.mode, Some(FaaMode::Autonomous));
    }

    #[test]
    fn vtg_legacy_five_fields() {
        // No T/M/N/K indicators: course true, course magnetic, knots,
        // km/h. Field 2 is a number, not `T`.
        let sentence = ok(b"$GPVTG,054.7,034.4,005.5,010.2");
        assert_eq!(sentence.checksum, ChecksumStatus::Absent);
        let NmeaData::Vtg(report) = sentence.data else {
            panic!("expected VTG");
        };
        assert_eq!(report.form, VtgForm::Legacy);
        assert_eq!(report.course_true_centidegrees, Some(5470));
        assert_eq!(report.course_magnetic_centidegrees, Some(3440));
        assert_eq!(report.speed_milliknots, Some(5500));
        assert_eq!(report.speed_meters_per_hour, Some(10_200));
        assert_eq!(report.mode, None);
    }

    #[test]
    fn wpl_is_a_waypoint_not_a_posit() {
        let sentence = ok(b"$GPWPL,4807.038,N,01131.000,E,HOME*46");
        assert_eq!(sentence.formatter(), Formatter::Wpl);
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
        // The station's position is *not* the waypoint.
        assert_eq!(sentence.position(), None);
        assert_eq!(sentence.fix(), None);

        let NmeaData::Wpl(waypoint) = sentence.data else {
            panic!("expected WPL");
        };
        assert_eq!(waypoint.name, b"HOME");
        assert_eq!(
            waypoint.latitude.map(|l| to_hundredths(l.units())),
            Some(48 * 6000 + 700 + 4)
        );
        assert_eq!(
            waypoint.longitude.map(|l| to_hundredths(l.units())),
            Some(11 * 6000 + 3100)
        );
    }

    // -- Talker independence -----------------------------------------

    #[test]
    fn every_talker_parses_identically() {
        let mut previous = None;
        for talker in [
            &b"GP"[..],
            b"GN",
            b"GL",
            b"GA",
            b"BD",
            b"GB",
            b"GQ",
            b"GI",
            b"II",
            b"ZZ",
        ] {
            let mut input = [0u8; 64];
            input[0] = b'$';
            input[1] = talker[0];
            input[2] = talker[1];
            let tail = b"GLL,4748.811,N,12219.564,W,033850,A";
            input[3..3 + tail.len()].copy_from_slice(tail);
            let len = 3 + tail.len();
            // `Gll` borrows nothing, so the payload outlives `input`.
            let report = {
                let sentence = ok(&input[..len]);
                assert_eq!(sentence.talker.as_bytes(), [talker[0], talker[1]]);
                assert_eq!(sentence.formatter(), Formatter::Gll);
                match sentence.data {
                    NmeaData::Gll(report) => report,
                    other => panic!("expected GLL, got {other:?}"),
                }
            };
            // Identical payload regardless of the talker.
            if let Some(previous) = previous {
                assert_eq!(report, previous);
            }
            previous = Some(report);
        }
    }

    #[test]
    fn constellations_are_classified() {
        let cases: [(&[u8; 2], Constellation); 8] = [
            (b"GP", Constellation::Gps),
            (b"GL", Constellation::Glonass),
            (b"GA", Constellation::Galileo),
            (b"BD", Constellation::BeiDou),
            (b"GB", Constellation::BeiDou),
            (b"GQ", Constellation::Qzss),
            (b"GI", Constellation::NavIc),
            (b"GN", Constellation::Combined),
        ];
        for (bytes, expected) in cases {
            assert_eq!(Talker(*bytes).constellation(), expected);
        }
        assert_eq!(Talker(*b"EC").constellation(), Constellation::Other);
    }

    #[test]
    fn lowercase_tag_and_hemisphere_accepted() {
        let sentence = ok(b"$gpgll,4748.811,n,12219.564,w,033850,A");
        assert_eq!(sentence.talker.as_bytes(), *b"GP");
        assert_eq!(sentence.formatter(), Formatter::Gll);
        assert_eq!(lat_of(&sentence), 286_881);
        assert_eq!(lon_of(&sentence), -733_956);
    }

    // -- Field-count tolerance ---------------------------------------

    #[test]
    fn trailing_extras_are_ignored_and_short_forms_accepted() {
        // NMEA 4.1 RMC: 14 fields, the last a navigational status.
        let long = rmc(b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E,A,S*70");
        assert_eq!(long.mode, Some(FaaMode::Autonomous));
        assert_eq!(long.navigation_status, Some(b'S'));

        // A hypothetical future field is simply not looked at.
        let longer =
            rmc(b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E,A,S,XYZ");
        assert_eq!(longer.latitude, long.latitude);
        assert_eq!(longer.navigation_status, Some(b'S'));

        // The NMEA 1.5 GLL form: five fields, no time and no status.
        let sentence = ok(b"$GPGLL,4748.811,N,12219.564,W");
        let NmeaData::Gll(report) = sentence.data else {
            panic!("expected GLL");
        };
        assert_eq!(report.time, None);
        assert_eq!(report.status, None);
        // A position with no status claim is degraded, not rejected.
        assert_eq!(report.fix, FixQuality::Degraded);
    }

    #[test]
    fn truncated_sentences_are_rejected() {
        assert_eq!(
            parse(b"$GPRMC,081836,A,3751.65,S"),
            Err(NmeaError::TooFewFields {
                got: 5,
                min: RMC_MIN_FIELDS
            })
        );
        assert_eq!(
            parse(b"$GPGGA,170834,4124.8963,N"),
            Err(NmeaError::TooFewFields {
                got: 4,
                min: GGA_MIN_FIELDS
            })
        );
        assert_eq!(
            parse(b"$GPGLL,4748.811,N"),
            Err(NmeaError::TooFewFields {
                got: 3,
                min: GLL_MIN_FIELDS
            })
        );
        assert_eq!(
            parse(b"$GPWPL,4807.038,N,01131.000,E"),
            Err(NmeaError::TooFewFields {
                got: 5,
                min: WPL_MIN_FIELDS
            })
        );
        // A modern VTG is held to the higher minimum than a legacy one.
        assert_eq!(
            parse(b"$GPVTG,220.86,T,,M"),
            Err(NmeaError::TooFewFields {
                got: 5,
                min: VTG_MODERN_FIELDS
            })
        );
        assert_eq!(
            parse(b"$GPVTG,220.86,034.4"),
            Err(NmeaError::TooFewFields {
                got: 3,
                min: VTG_LEGACY_FIELDS
            })
        );
    }

    // -- Empty fields everywhere -------------------------------------

    #[test]
    fn every_field_may_be_empty() {
        let report = rmc(b"$GPRMC,,,,,,,,,,,,,");
        assert_eq!(report.time, None);
        assert_eq!(report.status, None);
        assert_eq!(report.latitude, None);
        assert_eq!(report.longitude, None);
        assert_eq!(report.speed_milliknots, None);
        assert_eq!(report.course_centidegrees, None);
        assert_eq!(report.date, None);
        assert_eq!(report.magnetic_variation_centidegrees, None);
        assert_eq!(report.mode, None);
        assert_eq!(report.navigation_status, None);
        // No coordinates at all is the one thing that is Invalid.
        assert_eq!(report.fix, FixQuality::Invalid);

        let sentence = ok(b"$GPGGA,,,,,,,,,,,,,,");
        let NmeaData::Gga(gga) = sentence.data else {
            panic!("expected GGA");
        };
        // An empty quality field is "no data", not the value 0.
        assert_eq!(gga.quality, None);
        assert_eq!(gga.satellites, None);
        assert_eq!(gga.altitude_centimeters, None);
        assert_eq!(gga.fix, FixQuality::Invalid);
        assert_eq!(sentence.position(), None);

        let sentence = ok(b"$GPVTG,,T,,M,,N,,K,");
        let NmeaData::Vtg(vtg) = sentence.data else {
            panic!("expected VTG");
        };
        assert_eq!(vtg.form, VtgForm::Modern);
        assert_eq!(vtg.course_true_centidegrees, None);
        assert_eq!(vtg.speed_milliknots, None);
        assert_eq!(vtg.mode, None);

        // A magnitude with no hemisphere, or the reverse, is no data.
        let half = rmc(b"$GPRMC,081836,A,3751.65,,14507.36,E,000.0,360.0,130998,,");
        assert_eq!(half.latitude, None);
        assert_eq!(
            half.longitude.map(|l| to_hundredths(l.units())),
            Some(870_736)
        );
        assert_eq!(half.fix, FixQuality::Invalid);
    }

    #[test]
    fn empty_waypoint_name_is_an_empty_slice() {
        let sentence = ok(b"$GPWPL,4807.038,N,01131.000,E,");
        let NmeaData::Wpl(waypoint) = sentence.data else {
            panic!("expected WPL");
        };
        assert_eq!(waypoint.name, b"");
    }

    // -- Checksum tri-state ------------------------------------------

    #[test]
    fn checksum_absent_invalid_and_lowercase() {
        // No `*hh` at all: parsed, reported Absent.
        let sentence = ok(b"$GPGLL,4748.811,N,12219.564,W,033850,A");
        assert_eq!(sentence.checksum, ChecksumStatus::Absent);
        assert!(!sentence.checksum.is_valid());
        assert!(!sentence.checksum.is_present());

        // Wrong checksum: parsed anyway, both values reported.
        let sentence = ok(b"$GPGLL,4748.811,N,12219.564,W,033850,A*00");
        assert_eq!(
            sentence.checksum,
            ChecksumStatus::Invalid {
                computed: 0x3C,
                received: 0x00,
            }
        );
        assert!(sentence.checksum.is_present());
        assert!(!sentence.checksum.is_valid());
        // The payload is still fully decoded.
        assert_eq!(lat_of(&sentence), 286_881);

        // Lower-case hex digits are accepted.
        assert_eq!(
            ok(b"$GPGLL,4748.811,N,12219.564,W,033850,A*3c").checksum,
            ChecksumStatus::Valid
        );

        // A trailing CR/LF is trimmed before the checksum is taken.
        assert_eq!(
            ok(b"$GPGLL,4748.811,N,12219.564,W,033850,A*3C\r\n").checksum,
            ChecksumStatus::Valid
        );

        // A `*` that is not followed by two hex digits is not a
        // checksum: the sentence is treated as unchecksummed.
        assert_eq!(
            ok(b"$GPGLL,4748.811,N,12219.564,W,033850,A*3").checksum,
            ChecksumStatus::Absent
        );
    }

    #[test]
    fn checksum_search_starts_from_the_end() {
        // A `*` inside the waypoint name must not shadow the real one.
        let sentence = ok(b"$GPWPL,4807.038,N,01131.000,E,A*B*60");
        let NmeaData::Wpl(waypoint) = sentence.data else {
            panic!("expected WPL");
        };
        assert_eq!(waypoint.name, b"A*B");
        assert_eq!(sentence.checksum, ChecksumStatus::Valid);
    }

    // -- Fix quality --------------------------------------------------

    #[test]
    fn status_v_with_a_position_is_degraded_not_rejected() {
        let report = rmc(b"$GPRMC,081836,V,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E");
        assert_eq!(report.status, Some(b'V'));
        assert_eq!(report.fix, FixQuality::Degraded);
        // The position survives: that is the whole point.
        assert_eq!(
            report.latitude.map(|l| to_hundredths(l.units())),
            Some(-227_165)
        );
        assert!(report.fix.has_position());

        // A `V` with no coordinates really is Invalid.
        let empty = rmc(b"$GPRMC,081836,V,,,,,,,130998,,");
        assert_eq!(empty.fix, FixQuality::Invalid);
        assert!(!empty.fix.has_position());

        // So is the all-zero pair a receiver emits before it locks.
        let zeros = rmc(b"$GPRMC,081836,V,0000.0000,N,00000.0000,E,,,130998,,");
        assert_eq!(zeros.fix, FixQuality::Invalid);
    }

    #[test]
    fn gga_quality_covers_zero_through_eight() {
        let expected = [
            (b'0', GgaQuality::Invalid, FixQuality::Invalid),
            (b'1', GgaQuality::Gps, FixQuality::Valid),
            (b'2', GgaQuality::Differential, FixQuality::Valid),
            (b'3', GgaQuality::Pps, FixQuality::Valid),
            (b'4', GgaQuality::RtkFixed, FixQuality::Valid),
            (b'5', GgaQuality::RtkFloat, FixQuality::Valid),
            (b'6', GgaQuality::DeadReckoning, FixQuality::Degraded),
            (b'7', GgaQuality::ManualInput, FixQuality::Degraded),
            (b'8', GgaQuality::Simulator, FixQuality::Degraded),
        ];
        for (digit, quality, fix) in expected {
            let mut input = *b"$GPGGA,170834,4124.8963,N,08151.6838,W,X,05,1.5,280.2,M,-34.0,M,,";
            input[39] = digit;
            let sentence = ok(&input);
            let NmeaData::Gga(report) = sentence.data else {
                panic!("expected GGA");
            };
            assert_eq!(report.quality, Some(quality));
            assert_eq!(report.quality.map(GgaQuality::to_raw), Some(digit - b'0'));
            // Every non-zero indicator is a fix; 3/4/5 are better than 1.
            assert_eq!(
                report.quality.is_some_and(GgaQuality::has_fix),
                digit != b'0'
            );
            assert_eq!(report.fix, fix);
        }
        // 9 and above are vendor extensions, held verbatim and treated
        // as a fix.
        assert_eq!(GgaQuality::from_raw(9), GgaQuality::Other(9));
        assert!(GgaQuality::from_raw(9).has_fix());
        assert_eq!(GgaQuality::from_raw(200).to_raw(), 200);
    }

    #[test]
    fn faa_modes_are_classified() {
        assert_eq!(FaaMode::from_byte(b'A'), FaaMode::Autonomous);
        assert_eq!(FaaMode::from_byte(b'd'), FaaMode::Differential);
        assert_eq!(FaaMode::from_byte(b'E'), FaaMode::Estimated);
        assert_eq!(FaaMode::from_byte(b'F'), FaaMode::RtkFloat);
        assert_eq!(FaaMode::from_byte(b'M'), FaaMode::Manual);
        assert_eq!(FaaMode::from_byte(b'N'), FaaMode::NotValid);
        assert_eq!(FaaMode::from_byte(b'P'), FaaMode::Precise);
        assert_eq!(FaaMode::from_byte(b'R'), FaaMode::RtkInteger);
        assert_eq!(FaaMode::from_byte(b'S'), FaaMode::Simulator);
        assert_eq!(FaaMode::from_byte(b'?'), FaaMode::Other(b'?'));
        assert!(FaaMode::Autonomous.is_valid());
        assert!(!FaaMode::NotValid.is_valid());
    }

    // -- Coordinate edge cases ---------------------------------------

    #[test]
    fn minutes_of_sixty_or_more_are_rejected() {
        assert!(matches!(
            parse(b"$GPRMC,081836,A,3760.00,S,14507.36,E,000.0,360.0,130998,,"),
            Err(NmeaError::MinutesOutOfRange { got: 60, .. })
        ));
        assert!(matches!(
            parse(b"$GPRMC,081836,A,3751.65,S,14599.36,E,000.0,360.0,130998,,"),
            Err(NmeaError::MinutesOutOfRange { got: 99, .. })
        ));
        // 59.999 minutes is legal and carries into the degrees.
        let report = rmc(b"$GPRMC,081836,A,3759.999,N,00000.00,E,,,130998,,");
        assert_eq!(
            report.latitude.map(|l| to_hundredths(l.units())),
            Some(38 * 6000)
        );
    }

    #[test]
    fn degree_digit_count_is_not_hardcoded() {
        // One, two and three degree digits all decode.
        let one = rmc(b"$GPRMC,081836,A,451.00,N,00131.00,E,,,130998,,");
        assert_eq!(
            one.latitude.map(|l| to_hundredths(l.units())),
            Some(4 * 6000 + 5100)
        );
        let three = rmc(b"$GPRMC,081836,A,4404.00,N,00131.00,E,,,130998,,");
        assert_eq!(
            three.longitude.map(|l| to_hundredths(l.units())),
            Some(6000 + 3100)
        );
        // Fewer than three characters before the `.` leaves no degree
        // digit.
        assert!(matches!(
            parse(b"$GPRMC,081836,A,51.00,N,00131.00,E,,,130998,,"),
            Err(NmeaError::CoordinateTooShort { got: 5, .. })
        ));
        // No `.` at all.
        assert!(matches!(
            parse(b"$GPRMC,081836,A,375165,N,00131.00,E,,,130998,,"),
            Err(NmeaError::MissingDecimalPoint { .. })
        ));
    }

    #[test]
    fn fraction_length_varies_and_rounds_half_up() {
        // One through five fractional digits.
        let cases: [(&[u8], i64); 5] = [
            (b"3751.6", 37 * 6000 + 5160),
            (b"3751.65", 37 * 6000 + 5165),
            (b"3751.655", 37 * 6000 + 5166),
            (b"3751.6549", 37 * 6000 + 5165),
            (b"3751.65499", 37 * 6000 + 5165),
        ];
        for (magnitude, expected) in cases {
            let mut input = [0u8; 80];
            let head = b"$GPRMC,081836,A,";
            let tail = b",N,00131.00,E,,,130998,,";
            let mut len = 0;
            for chunk in [&head[..], magnitude, &tail[..]] {
                input[len..len + chunk.len()].copy_from_slice(chunk);
                len += chunk.len();
            }
            let report = rmc(&input[..len]);
            assert_eq!(
                report.latitude.map(|l| to_hundredths(l.units())),
                Some(expected)
            );
        }
    }

    #[test]
    fn hemispheres_and_ranges_are_checked() {
        assert_eq!(
            parse(b"$GPRMC,081836,A,3751.65,X,14507.36,E,,,130998,,"),
            Err(NmeaError::BadHemisphere { got: b'X' })
        );
        // An N/S letter in the longitude slot is wrong too.
        assert_eq!(
            parse(b"$GPRMC,081836,A,3751.65,N,14507.36,N,,,130998,,"),
            Err(NmeaError::BadHemisphere { got: b'N' })
        );
        // Beyond +/-90 and +/-180 degrees.
        assert_eq!(
            parse(b"$GPRMC,081836,A,9100.00,N,00131.00,E,,,130998,,"),
            Err(NmeaError::BadLatitude {
                got: 91 * 6000 * crate::geo::UNITS_PER_HUNDREDTH_MINUTE
            })
        );
        assert_eq!(
            parse(b"$GPRMC,081836,A,4404.00,N,18100.00,E,,,130998,,"),
            Err(NmeaError::BadLongitude {
                got: 181 * 6000 * crate::geo::UNITS_PER_HUNDREDTH_MINUTE
            })
        );
        // Exactly at the limits is fine.
        let edge = rmc(b"$GPRMC,081836,A,9000.00,S,18000.00,W,,,130998,,");
        assert_eq!(
            edge.latitude.map(|l| to_hundredths(l.units())),
            Some(-90 * 6000)
        );
        assert_eq!(
            edge.longitude.map(|l| to_hundredths(l.units())),
            Some(-180 * 6000)
        );
    }

    // -- Time and date ------------------------------------------------

    #[test]
    fn time_and_date_components_are_range_checked() {
        let report = rmc(b"$GPRMC,235960.75,A,3751.65,S,14507.36,E,,,311299,,");
        assert_eq!(
            report.time,
            Some(NmeaTime {
                hour: 23,
                minute: 59,
                // A leap second is legal.
                second: 60,
                millisecond: 750,
            })
        );
        assert_eq!(
            report.date,
            Some(NmeaDate {
                day: 31,
                month: 12,
                year: 99,
            })
        );

        // Over-precise fractional seconds truncate, never round up.
        let truncating = rmc(b"$GPRMC,235960.9999,A,3751.65,S,14507.36,E,,,311299,,");
        assert_eq!(truncating.time.map(|t| t.millisecond), Some(999));

        for (input, field, got) in [
            (&b"$GPRMC,241836,A,,,,,,,130998,,"[..], b'h', 24),
            (b"$GPRMC,086036,A,,,,,,,130998,,", b'm', 60),
            (b"$GPRMC,081861,A,,,,,,,130998,,", b's', 61),
        ] {
            assert_eq!(parse(input), Err(NmeaError::BadTimestamp { field, got }));
        }
        for (input, field, got) in [
            (&b"$GPRMC,081836,A,,,,,,,000998,,"[..], b'D', 0),
            (b"$GPRMC,081836,A,,,,,,,321298,,", b'D', 32),
            (b"$GPRMC,081836,A,,,,,,,130098,,", b'M', 0),
            (b"$GPRMC,081836,A,,,,,,,131398,,", b'M', 13),
        ] {
            assert_eq!(parse(input), Err(NmeaError::BadTimestamp { field, got }));
        }
        assert_eq!(
            parse(b"$GPRMC,0818,A,,,,,,,130998,,"),
            Err(NmeaError::BadTimeLength { got: 4 })
        );
        assert_eq!(
            parse(b"$GPRMC,081836,A,,,,,,,13099,,"),
            Err(NmeaError::BadDateLength { got: 5 })
        );
        assert_eq!(
            parse(b"$GPRMC,081836x00,A,,,,,,,130998,,"),
            Err(NmeaError::ExpectedByte {
                expected: b'.',
                got: b'x',
                position: 13,
            })
        );
    }

    // -- Structural rejections ----------------------------------------

    #[test]
    fn structural_rejections() {
        assert_eq!(parse(b""), Err(NmeaError::Empty));
        assert_eq!(parse(b"   \r\n"), Err(NmeaError::Empty));
        assert_eq!(
            parse(b"!4903.50N/07201.75W-"),
            Err(NmeaError::MissingStartDelimiter { got: b'!' })
        );
        assert_eq!(parse(b"$"), Err(NmeaError::BadAddressLength { got: 0 }));
        assert_eq!(
            parse(b"$GPRM,081836"),
            Err(NmeaError::BadAddressLength { got: 4 })
        );
        assert_eq!(
            parse(b"$GPRMCXX,081836"),
            Err(NmeaError::BadAddressLength { got: 7 })
        );
        // A proprietary tag happens to be five characters, so it is
        // rejected one step later, on the formatter.
        assert_eq!(
            parse(b"$PGRMZ,246,f,3*1A"),
            Err(NmeaError::UnsupportedFormatter { got: *b"RMZ" })
        );
        // Sentences that exist but this module does not decode.
        for tag in [&b"GSA"[..], b"GSV", b"ZDA", b"GST"] {
            let mut input = [0u8; 16];
            input[..3].copy_from_slice(b"$GP");
            input[3..6].copy_from_slice(tag);
            input[6] = b',';
            assert_eq!(
                parse(&input[..7]),
                Err(NmeaError::UnsupportedFormatter {
                    got: [tag[0], tag[1], tag[2]]
                })
            );
        }
        // Bytes above 0x7E are corruption.
        assert_eq!(
            parse(b"$GPRMC,08\xFF836,A,,,,,,,130998,,"),
            Err(NmeaError::NonPrintable {
                got: 0xFF,
                position: 9,
            })
        );
        // Non-digits in a numeric field.
        assert!(matches!(
            parse(b"$GPRMC,081836,A,37Z1.65,S,14507.36,E,,,130998,,"),
            Err(NmeaError::BadDigit { got: b'Z', .. })
        ));
        // A numeric field with no digits at all.
        assert!(matches!(
            parse(b"$GPRMC,081836,A,3751.65,S,14507.36,E,-,,130998,,"),
            Err(NmeaError::EmptyNumber { .. })
        ));
        // A speed that cannot fit its decoded type.
        assert!(matches!(
            parse(b"$GPRMC,081836,A,3751.65,S,14507.36,E,99999999999,,130998,,"),
            Err(NmeaError::NumberOutOfRange { .. })
        ));
    }

    #[test]
    fn errors_render() {
        let cases: [NmeaError; 19] = [
            NmeaError::Empty,
            NmeaError::MissingStartDelimiter { got: b'!' },
            NmeaError::NonPrintable {
                got: 0xFF,
                position: 3,
            },
            NmeaError::BadAddressLength { got: 4 },
            NmeaError::UnsupportedFormatter { got: *b"GSV" },
            NmeaError::TooFewFields { got: 3, min: 10 },
            NmeaError::BadDigit {
                got: b'Z',
                position: 7,
            },
            NmeaError::ExpectedByte {
                expected: b'.',
                got: b'x',
                position: 13,
            },
            NmeaError::EmptyNumber { position: 9 },
            NmeaError::NumberOutOfRange { position: 9 },
            NmeaError::MissingDecimalPoint { position: 16 },
            NmeaError::CoordinateTooShort {
                got: 5,
                position: 16,
            },
            NmeaError::MinutesOutOfRange {
                got: 60,
                position: 18,
            },
            NmeaError::BadHemisphere { got: b'X' },
            NmeaError::BadLatitude { got: 546_000 },
            NmeaError::BadLongitude { got: 1_086_000 },
            NmeaError::BadTimestamp {
                field: b'h',
                got: 24,
            },
            NmeaError::BadTimeLength { got: 4 },
            NmeaError::BadDateLength { got: 5 },
        ];
        for case in cases {
            let mut sink = CountingSink(0);
            match core::fmt::write(&mut sink, format_args!("{case}")) {
                Ok(()) => assert!(sink.0 > 20, "{case:?} rendered only {} bytes", sink.0),
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// A `core::fmt::Write` sink that only counts bytes, so the Display
    /// impls are exercised without allocating.
    struct CountingSink(usize);

    impl core::fmt::Write for CountingSink {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0 += s.len();
            Ok(())
        }
    }

    // -- Fuzz-ish robustness ------------------------------------------

    /// 64-bit LCG (MMIX constants), matching `tests/fuzz_decode.rs`.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn next_u8(&mut self) -> u8 {
            // The low bits of an LCG are weak; take the top byte.
            (self.next_u64() >> 56) as u8
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() >> 33) as usize % bound
        }
    }

    /// The alphabet a corrupted-but-plausible sentence draws from, so
    /// the soup reaches deep into the field decoders rather than
    /// bouncing off the `$` check.
    const ALPHABET: &[u8] = b"$*,.-+0123456789ABCDEFGKLMNPQRSTVWZgnprmcvtwl \r\n\x7f\x80\xff";

    /// Deterministic byte soup: the only assertion is that no input
    /// panics, indexes out of bounds, overflows or loops forever.
    #[test]
    fn fuzz_random_bytes_never_panic() {
        let mut rng = Lcg(0xA905_2024_4E4D_4541);
        let mut buf = [0u8; 96];

        // Fully random bytes.
        for _ in 0..4000 {
            let len = rng.below(buf.len() + 1);
            for slot in buf.iter_mut().take(len) {
                *slot = rng.next_u8();
            }
            let _ = parse(&buf[..len]);
        }

        // Random bytes drawn from the NMEA alphabet, always starting
        // with `$` so the formatter dispatch is reached.
        for _ in 0..4000 {
            let len = rng.below(buf.len() - 1) + 1;
            buf[0] = b'$';
            for slot in buf.iter_mut().take(len).skip(1) {
                *slot = ALPHABET[rng.below(ALPHABET.len())];
            }
            let _ = parse(&buf[..len]);
        }

        // A valid tag plus alphabet soup, one pass per formatter.
        for tag in [&b"$GPRMC"[..], b"$GNGGA", b"$GLGLL", b"$BDVTG", b"$GAWPL"] {
            for _ in 0..2000 {
                buf[..tag.len()].copy_from_slice(tag);
                let len = tag.len() + rng.below(buf.len() - tag.len());
                for slot in buf.iter_mut().take(len).skip(tag.len()) {
                    *slot = ALPHABET[rng.below(ALPHABET.len())];
                }
                let _ = parse(&buf[..len]);
            }
        }

        // Every truncation and every single-byte corruption of the
        // known-good vectors.
        let corpus: [&[u8]; 5] = [
            b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
            b"$GNRMC,001031.00,A,4404.13993,N,12118.86023,W,0.146,,100117,,,A*7B",
            b"$GPGGA,170834,4124.8963,N,08151.6838,W,1,05,1.5,280.2,M,-34.0,M,,*75",
            b"$GPGLL,4748.811,N,12219.564,W,033850,A*3C",
            b"$GPVTG,220.86,T,,M,2.550,N,4.724,K,A*34",
        ];
        for sentence in corpus {
            for cut in 0..=sentence.len() {
                let _ = parse(&sentence[..cut]);
            }
            for index in 0..sentence.len() {
                for _ in 0..8 {
                    buf[..sentence.len()].copy_from_slice(sentence);
                    buf[index] = rng.next_u8();
                    let _ = parse(&buf[..sentence.len()]);
                }
            }
        }
    }

    /// A sentence that survives the fuzz corpus must still round-trip
    /// its own checksum arithmetic: recomputing the XOR of the body of
    /// each known-good vector reproduces the transmitted digits.
    #[test]
    fn checksum_arithmetic_matches_the_published_vectors() {
        let corpus: [(&[u8], u8); 5] = [
            (
                b"$GPRMC,081836,A,3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62",
                0x62,
            ),
            (
                b"$GNRMC,001031.00,A,4404.13993,N,12118.86023,W,0.146,,100117,,,A*7B",
                0x7B,
            ),
            (
                b"$GPGGA,170834,4124.8963,N,08151.6838,W,1,05,1.5,280.2,M,-34.0,M,,*75",
                0x75,
            ),
            (b"$GPGLL,4748.811,N,12219.564,W,033850,A*3C", 0x3C),
            (b"$GPVTG,220.86,T,,M,2.550,N,4.724,K,A*34", 0x34),
        ];
        for (sentence, expected) in corpus {
            let star = match sentence.iter().rposition(|&b| b == b'*') {
                Some(star) => star,
                None => panic!("vector has no checksum"),
            };
            let computed = sentence[1..star].iter().fold(0u8, |acc, &b| acc ^ b);
            assert_eq!(computed, expected);
            assert_eq!(ok(sentence).checksum, ChecksumStatus::Valid);
        }
    }
}
