//! APRS position reports (`!` / `=` untimed, `/` / `@` timestamped).
//!
//! Both the human-readable uncompressed form
//! (`ddmm.mmN/dddmm.mmW$comment`, APRS 1.01 chapter 8) and the base-91
//! compressed form (`/YYYYXXXX$csT`, chapter 9) are supported for
//! building and parsing, either bare ([`Position`]) or preceded by a
//! 7-byte DHM/HMS timestamp ([`PositionTimestamped`]).
//!
//! Coordinates are stored fixed-point as signed **1/100 arc-minutes**
//! (the native resolution of the uncompressed format); no floating point
//! is required anywhere in the uncompressed build/parse path.
//! [`Latitude::to_degrees`] and friends are additive `f64` conveniences.
//!
//! Position ambiguity (space-padded digits) is **not** supported: a
//! space in a coordinate digit position is rejected as
//! [`AprsError::BadDigit`].
//!
//! # Compressed `csT` trailer
//!
//! The three bytes after the compressed symbol code carry one of four
//! typed payloads ([`CompressedCs`]) plus the compression-type byte
//! ([`CompressionType`], base-91 offset 33):
//!
//! * `c = ' '` — no data; the `s` and `T` bytes are ignored (built as
//!   the literal `" sT"`).
//! * `c` in `'!'..='z'` — course `(c - 33) * 4` degrees and speed
//!   `1.08^(s - 33) - 1` knots.
//! * `c = '{'` — pre-calculated radio range `2 * 1.08^(s - 33)` miles.
//! * `T` NMEA source = GGA — altitude `1.002^((c - 33) * 91 + (s - 33))`
//!   feet (checked before the `c`-byte forms above).
//!
//! Building emits the code that **decodes back to the value it was
//! given**, and the nearest code when the scale cannot express that
//! value at all. Courses round to the nearest 4-degree step (modulo
//! 360). Decoding rounds speed and range to the nearest integer and
//! truncates altitude to whole feet (matching the spec's worked
//! example).
//!
//! Inverting the *decoder* rather than the underlying power is what
//! makes a decode/re-encode value-stable, and it is not the same thing.
//! Altitude decoding truncates, so `1.002^e` sits above the foot count
//! it reports and the exponent nearest that foot count is routinely
//! `e - 1`: rebuilding through the power dropped a foot on 999 of the
//! 8281 altitude codes. See [`exponent_for`].

use super::AprsError;
use super::extension::{
    CommentTelemetry, Dao, DataExtension, altitude_feet, comment_telemetry, dao,
};
use super::object::Timestamp;
use super::symbol::Symbol;
use crate::geo::{
    Ambiguity, Coordinates, LAT_MAX, LON_MAX, Latitude, Longitude, UNITS_PER_DEGREE,
    UNITS_PER_HUNDREDTH_MINUTE, UNITS_PER_MINUTE,
};

/// Scale factor of the compressed latitude formula (per degree).
const COMP_LAT_SCALE: i64 = 380_926;
/// Storage units per step of the compressed latitude grid.
///
/// Exact, by construction: [`UNITS_PER_DEGREE`] is chosen so that
/// [`COMP_LAT_SCALE`] divides it. That is what lets the conversion be a
/// multiplication in one direction and a division in the other, with no
/// rounding on either, so a compressed position survives a round trip
/// byte for byte and no hemisphere can round differently from its
/// mirror.
const UNITS_PER_COMPRESSED_LAT: i64 = UNITS_PER_DEGREE / COMP_LAT_SCALE;
/// Scale factor of the compressed longitude formula (per degree).
const COMP_LON_SCALE: i64 = 190_463;
/// Storage units per step of the compressed longitude grid. Exact, for
/// the same reason as [`UNITS_PER_COMPRESSED_LAT`].
const UNITS_PER_COMPRESSED_LON: i64 = UNITS_PER_DEGREE / COMP_LON_SCALE;
/// Base of the compressed speed / radio-range exponent.
const CS_BASE: f64 = 1.08;
/// Base of the compressed altitude exponent.
const ALT_BASE: f64 = 1.002;
/// Largest speed / radio-range exponent (one base-91 digit).
const CS_MAX_EXP: u32 = 90;
/// Largest altitude exponent (two base-91 digits).
const ALT_MAX_EXP: u32 = 90 * 91 + 90;
/// Base-91 offset: index 0 is `'!'`.
const BASE91_OFFSET: u8 = 33;

/// A position report without timestamp.
///
/// The comment borrows from the parsed input or the caller's data.
///
/// Building the compressed form emits the no-data `" sT"` trailer; a
/// compressed report whose trailer carries course/speed, radio range
/// or altitude is [`PositionCs`].
///
/// # Common path: one line, valid by construction
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, Position, Symbol};
///
/// let pos = Position::new(
///     Latitude::from_degrees(49.0583)?,
///     Longitude::from_degrees(-72.0292)?,
///     Symbol::CAR,
/// )
/// .with_comment(b"warble");
/// let mut buf = [0u8; 64];
/// let len = pos.build(&mut buf)?;
/// assert!(buf[..len].starts_with(b"!"));
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Power user: fully typed symbol, exhaustively matched
///
/// ```
/// use warble::aprs::{
///     AprsError, Latitude, Longitude, OverlayId, Position, Symbol, SymbolCode, SymbolTable,
/// };
///
/// let pos = Position::new(
///     Latitude::new(0)?,
///     Longitude::new(0)?,
///     Symbol::overlay(OverlayId::new(b'W')?, SymbolCode::new(b'#')?),
/// );
/// match pos.symbol.table() {
///     Some(SymbolTable::Primary) => unreachable!(),
///     Some(SymbolTable::Alternate) => unreachable!(),
///     Some(SymbolTable::Overlay(id)) => assert_eq!(id.get(), b'W'),
///     None => unreachable!(),
/// }
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Raw hatch: out-of-spec wire bytes round-trip exactly
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, Position, Symbol};
///
/// // No spec blesses '~' as a table selector; hold it anyway.
/// let pos = Position::new(Latitude::new(0)?, Longitude::new(0)?, Symbol::from_wire(b'~', b'$'));
/// let mut buf = [0u8; 32];
/// let len = pos.build(&mut buf)?;
/// assert_eq!(&buf[..len], b"!0000.00N~00000.00E$");
/// assert_eq!(pos.symbol.to_wire(), (b'~', b'$'));
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position<'a> {
    /// The station latitude.
    pub latitude: Latitude,
    /// The station longitude.
    pub longitude: Longitude,
    /// The display symbol (table + code). Typed construction via
    /// [`Symbol::new`] or the named constants; [`Symbol::from_wire`]
    /// is the raw hatch that holds any byte pair (including the `a-j`
    /// digit-overlay forms of the compressed format) verbatim.
    pub symbol: Symbol,
    /// `true` builds/parsed the `=` identifier (station is
    /// message-capable), `false` the `!` identifier.
    pub messaging: bool,
    /// `true` builds the base-91 compressed form, `false` the
    /// uncompressed form. Set by the parser according to the input.
    pub compressed: bool,
    /// How many low-order coordinate digits the sender blanked.
    ///
    /// Chapter 6 position ambiguity: a station that does not wish to
    /// publish an exact fix replaces trailing digits with spaces, one
    /// digit being a tenth of an arc-minute of vagueness and four a
    /// whole degree.
    ///
    /// **Read the position through [`Self::coordinates`]**, which
    /// applies this to *both* axes. The latitude's blanked digits
    /// arrive as zeros, so that field already reads at the declared
    /// precision, but chapter 6 lets the longitude carry its digits in
    /// full and leaves discarding them to the receiver.
    pub ambiguity: Ambiguity,
    /// The 7-byte data extension between the symbol and the comment:
    /// course/speed, wind, `PHG`, `RNG` or `DFS`.
    ///
    /// **Only the uncompressed form can carry one.** A compressed
    /// position's 13 bytes substitute for the uncompressed position
    /// *and its extension slot*, and the specification says outright
    /// that the compressed "format does not support PHG" — course and
    /// speed live in its own `cs` bytes instead. Parsing an extension
    /// after a compressed position would eat the first 7 bytes of every
    /// such comment.
    pub extension: Option<DataExtension>,
    /// Free-text comment following the position and any data extension.
    ///
    /// The extension bytes are **not** here: they are in
    /// [`Self::extension`], and `build` re-emits them in place. Any
    /// `/A=nnnnnn` altitude *is* still here, because the specification
    /// places it anywhere within the comment text rather than at a
    /// fixed offset — see [`Self::altitude_feet`].
    pub comment: &'a [u8],
}

/// The payload of the compressed `cs` byte pair, per APRS 1.01
/// chapter 9.
///
/// Which variant applies is determined on the wire by the `T` byte
/// (NMEA source GGA selects [`CompressedCs::Altitude`]) and the `c`
/// byte (`' '` no data, `'{'` radio range, anything else
/// course/speed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressedCs {
    /// No course/speed/range/altitude data (`c = ' '`).
    #[default]
    NoData,
    /// Course and speed: `course = (c - 33) * 4` degrees,
    /// `speed = 1.08^(s - 33) - 1` knots.
    ///
    /// Building rounds the course to the nearest 4-degree step (modulo
    /// 360) and writes the `s` code that decodes back to this speed;
    /// parsing rounds the decoded speed to the nearest knot. 21 of the
    /// 91 codes decode to a speed some other code also decodes to, and
    /// building picks whichever of them has the nearest `1.08^s`, so a
    /// re-encode can change the byte without changing the knots.
    CourseSpeed {
        /// Course in degrees, `0..=359` (4-degree wire resolution).
        course: u16,
        /// Speed in knots, `0..=1018` (exponential wire resolution).
        speed: u16,
    },
    /// Pre-calculated radio range (`c = '{'`):
    /// `range = 2 * 1.08^(s - 33)` miles.
    ///
    /// Building writes the `s` code that decodes back to this range,
    /// the one with the nearest `1.08^s` where several decode alike;
    /// parsing rounds the decoded range to the nearest mile.
    RadioRange {
        /// Range in miles, `2..=2038` (exponential wire resolution).
        miles: u16,
    },
    /// Altitude (selected by `T` NMEA source = GGA):
    /// `altitude = 1.002^((c - 33) * 91 + (s - 33))` feet.
    ///
    /// Building writes the `cs` code that decodes back to this
    /// altitude, the lowest one where several decode alike, and forces
    /// the `T` byte's NMEA source to GGA; parsing truncates the decoded
    /// altitude to whole feet (matching the spec's worked example).
    ///
    /// Only 5669 of the 8281 codes name a distinct altitude, and above
    /// 5000 feet the code step exceeds 10 feet, so most whole-foot
    /// values are not on the scale at all and building rounds to one
    /// that is. A rebuild that moves an altitude by a few feet at
    /// height is the wire format, not a defect.
    Altitude {
        /// Altitude in feet, `1..=15_301_000` or so (exponential wire
        /// resolution).
        feet: u32,
    },
}

/// The NMEA sentence that sourced a compressed position, from bits 4-3
/// of the compression-type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NmeaSource {
    /// Other or unknown source (`0b00`).
    #[default]
    Other,
    /// GLL sentence (`0b01`).
    Gll,
    /// GGA sentence (`0b10`); selects the altitude `cs` form.
    Gga,
    /// RMC sentence (`0b11`).
    Rmc,
}

/// The origin of the compression, from bits 2-0 of the
/// compression-type byte (per the APRS 1.01 chapter 9 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionOrigin {
    /// Compressed (`0b000`).
    #[default]
    Compressed,
    /// TNC BText (`0b001`).
    TncBtext,
    /// Software (DOS/Mac versions) (`0b010`).
    Software,
    /// Reserved / to be defined (`0b011`).
    Tbd,
    /// KPC3 (`0b100`).
    Kpc3,
    /// Pico (`0b101`).
    Pico,
    /// Other tracker (`0b110`).
    OtherTracker,
    /// Digipeater conversion (`0b111`).
    Digipeater,
}

/// The typed compression-type `T` byte (base-91 offset 33): GPS fix
/// age (bit 5), NMEA source (bits 4-3) and compression origin
/// (bits 2-0). Bits 7-6 are unused and ignored on decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompressionType {
    /// `true` for a current GPS fix (bit 5 set), `false` for an old
    /// (last known) fix.
    pub current_fix: bool,
    /// The NMEA source bits; [`NmeaSource::Gga`] switches the `cs`
    /// bytes to the altitude form.
    pub nmea_source: NmeaSource,
    /// The compression origin bits.
    pub origin: CompressionOrigin,
}

impl CompressionType {
    /// Encodes the fields into the wire byte (`'!' + value`).
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        let fix = if self.current_fix { 1u8 << 5 } else { 0 };
        let source = match self.nmea_source {
            NmeaSource::Other => 0u8,
            NmeaSource::Gll => 1,
            NmeaSource::Gga => 2,
            NmeaSource::Rmc => 3,
        } << 3;
        let origin = match self.origin {
            CompressionOrigin::Compressed => 0u8,
            CompressionOrigin::TncBtext => 1,
            CompressionOrigin::Software => 2,
            CompressionOrigin::Tbd => 3,
            CompressionOrigin::Kpc3 => 4,
            CompressionOrigin::Pico => 5,
            CompressionOrigin::OtherTracker => 6,
            CompressionOrigin::Digipeater => 7,
        };
        BASE91_OFFSET + (fix | source | origin)
    }

    /// Decodes a wire byte at `position` (for error reporting).
    ///
    /// # Errors
    ///
    /// [`AprsError::BadBase91`] when the byte is outside `'!'..='{'`.
    pub const fn from_byte(byte: u8, position: usize) -> Result<Self, AprsError> {
        let value = match base91_digit(byte, position) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let nmea_source = match (value >> 3) & 0b11 {
            0 => NmeaSource::Other,
            1 => NmeaSource::Gll,
            2 => NmeaSource::Gga,
            _ => NmeaSource::Rmc,
        };
        let origin = match value & 0b111 {
            0 => CompressionOrigin::Compressed,
            1 => CompressionOrigin::TncBtext,
            2 => CompressionOrigin::Software,
            3 => CompressionOrigin::Tbd,
            4 => CompressionOrigin::Kpc3,
            5 => CompressionOrigin::Pico,
            6 => CompressionOrigin::OtherTracker,
            _ => CompressionOrigin::Digipeater,
        };
        Ok(Self {
            current_fix: value & (1 << 5) != 0,
            nmea_source,
            origin,
        })
    }
}

impl<'a> Position<'a> {
    /// Creates an uncompressed, non-messaging position report with an
    /// empty comment. Every part is validated by its own type, so the
    /// result is valid by construction; use the `with_*` methods to
    /// adjust the flags and comment.
    #[must_use]
    pub const fn new(latitude: Latitude, longitude: Longitude, symbol: Symbol) -> Self {
        Self {
            latitude,
            longitude,
            symbol,
            ambiguity: Ambiguity::EXACT,
            messaging: false,
            compressed: false,
            extension: None,
            comment: b"",
        }
    }

    /// Returns the report with the given free-text comment.
    #[must_use]
    pub const fn with_comment(self, comment: &'a [u8]) -> Self {
        Self { comment, ..self }
    }

    /// Returns the report carrying the given data extension.
    ///
    /// The extension is silently omitted when building the *compressed*
    /// form, which has no slot for one — course and speed go in its `cs`
    /// bytes instead (see [`PositionCs`]).
    #[must_use]
    pub const fn with_extension(self, extension: DataExtension) -> Self {
        Self {
            extension: Some(extension),
            ..self
        }
    }

    /// The `/A=nnnnnn` altitude in feet found in the comment, if any.
    ///
    /// Altitude is a **view of the comment**, not a field, because the
    /// specification places it "anywhere in the comment" rather than at
    /// a fixed offset. Keeping the bytes in `comment` is what makes
    /// `parse` → `build` byte-exact; see
    /// [`extension::altitude_feet`](super::extension::altitude_feet).
    ///
    /// ```
    /// # #[cfg(feature = "aprs")] {
    /// use warble::aprs::{AprsPacket, DataExtension};
    ///
    /// let AprsPacket::Position(p) =
    ///     AprsPacket::parse(b"!4903.50N/07201.75W>125/007/A=000984 rolling")?
    /// else { panic!() };
    ///
    /// // The extension is a field, stripped out of the comment...
    /// let Some(DataExtension::CourseSpeed { course, speed }) = p.extension else {
    ///     panic!()
    /// };
    /// assert_eq!(course.degrees(), Some(125));
    /// assert_eq!(speed.knots(), Some(7));
    /// // ...while the altitude stays in it, and is read as a view.
    /// assert_eq!(p.altitude_feet(), Some(984));
    /// assert_eq!(p.comment, b"/A=000984 rolling");
    /// # }
    /// # Ok::<(), warble::aprs::AprsError>(())
    /// ```
    #[must_use]
    pub fn altitude_feet(&self) -> Option<i32> {
        altitude_feet(self.comment)
    }

    /// Returns the report with the messaging flag set as given (`=`
    /// DTI when `true`, `!` when `false`).
    #[must_use]
    pub const fn with_messaging(self, messaging: bool) -> Self {
        Self { messaging, ..self }
    }

    /// Returns the report set to build the base-91 compressed form
    /// (`true`) or the uncompressed form (`false`).
    #[must_use]
    pub const fn with_compressed(self, compressed: bool) -> Self {
        Self { compressed, ..self }
    }

    /// The station position, pairing the `latitude` and `longitude`
    /// fields so call sites need not rely on tuple ordering, **masked
    /// to the precision the sender declared**.
    ///
    /// # Why this masks and the fields do not
    ///
    /// Chapter 6 blanks coordinate digits with spaces, and those arrive
    /// as zeros, so the latitude field already reads at the declared
    /// precision and masking it is the identity. The longitude need not
    /// carry the spaces at all:
    ///
    /// > The level of ambiguity specified in the latitude will
    /// > automatically apply to the longitude as well, it is
    /// > permissible but not necessary to include any space characters
    /// > in the longitude.
    ///
    /// So a sender may pair a blanked latitude with a full-precision
    /// longitude, and discarding the matching low-order digits is the
    /// receiver's job. Reading `longitude` directly publishes a
    /// position finer than the station claimed; this accessor does not.
    ///
    /// The same split, for the same reason, as
    /// [`MicE::coordinates`](super::MicE::coordinates), where the
    /// declaration lives in the destination address instead.
    ///
    /// # And why it also adds `!DAO!`
    ///
    /// A `!DAO!` field in the comment refines the position the other
    /// way, down to about a foot. It is applied here for the same
    /// reason ambiguity is: the declaration lives in a different wire
    /// slot from the value, so only an accessor can bring the two
    /// together. The fields keep the `DDMM.hh` the wire sent, which is
    /// what [`build`](Self::build) writes back, so adding the
    /// refinement here cannot disturb a rebuild.
    ///
    /// **There is one accessor rather than a refined and an unrefined
    /// one, on purpose.** Every renderer in this project has at some
    /// point read the raw fields instead of the accessor, twice for
    /// ambiguity, and a second accessor is the same trap with a
    /// friendlier name.
    ///
    /// Ambiguity wins when a packet somehow declares both: a station
    /// cannot coherently blank digits and refine them at once, and
    /// MEASURED over a 64 918-packet capture, none tries.
    #[must_use]
    pub fn coordinates(&self) -> Coordinates {
        let (mut lat_units, mut lon_units) = (
            self.ambiguity.mask(self.latitude.units()),
            self.ambiguity.mask(self.longitude.units()),
        );
        if self.ambiguity == Ambiguity::EXACT
            && let Some(refinement) = dao(self.comment)
        {
            // Signed toward the hemisphere already declared: a DAO on
            // 4903.50S refines it southward, which is further negative.
            lat_units += lat_units.signum() * refinement.latitude_units;
            lon_units += lon_units.signum() * refinement.longitude_units;
        }
        let latitude = match Latitude::new(lat_units) {
            Ok(value) => value,
            // Unreachable: masking only reduces a magnitude, and a DAO
            // addend is under a hundredth of a minute, so neither can
            // take an in-range field out of range.
            Err(_) => self.latitude,
        };
        let longitude = match Longitude::new(lon_units) {
            Ok(value) => value,
            // Unreachable, for the same reason.
            Err(_) => self.longitude,
        };
        Coordinates::new(latitude, longitude).with_ambiguity(self.ambiguity)
    }

    /// Base-91 comment telemetry, when the comment carries a block.
    ///
    /// A view of the comment, like [`altitude_feet`](Self::altitude_feet):
    /// the bytes stay where they are, so `build` reproduces them.
    #[must_use]
    pub fn comment_telemetry(&self) -> Option<CommentTelemetry> {
        comment_telemetry(self.comment)
    }

    /// The `!DAO!` datum and added precision, when the comment carries
    /// one.
    ///
    /// [`coordinates`](Self::coordinates) has already applied the
    /// precision. This exposes the datum byte, which it cannot.
    #[must_use]
    pub fn dao(&self) -> Option<Dao> {
        dao(self.comment)
    }

    /// Length of the uncompressed body (after DTI/timestamp):
    /// `ddmm.mmN` + table + `dddmm.mmW` + code.
    const UNCOMPRESSED_BODY: usize = LATLON_LEN;
    /// Length of the compressed body (after DTI/timestamp): table +
    /// 4 lat + 4 lon + code + `csT`.
    pub(crate) const COMPRESSED_BODY: usize = 1 + 4 + 4 + 1 + 3;
    /// Length of the uncompressed report: DTI + body.
    const UNCOMPRESSED_LEN: usize = 1 + Self::UNCOMPRESSED_BODY;
    /// Length of the compressed report: DTI + body.
    const COMPRESSED_LEN: usize = 1 + Self::COMPRESSED_BODY;

    /// Parses a `!` or `=` position report (dispatching on the second
    /// byte: a digit starts the uncompressed form, anything else the
    /// compressed form). The `csT` trailer of a compressed report is
    /// validated but its payload is dropped; use [`PositionCs::parse`]
    /// (or [`AprsPacket::parse`](super::AprsPacket::parse)) to obtain
    /// it.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] on a short field; [`AprsError::BadDigit`]
    /// / [`AprsError::ExpectedByte`] / [`AprsError::BadHemisphere`] /
    /// [`AprsError::BadLatitude`] / [`AprsError::BadLongitude`] /
    /// [`AprsError::BadSymbolTable`] / [`AprsError::BadBase91`] on
    /// malformed coordinates or `csT` trailer.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        PositionCs::parse(info).map(|p| p.position)
    }

    /// Parses a position body (uncompressed or compressed, dispatching
    /// on the first body byte) starting at `at`, returning the typed
    /// `csT` payload alongside (no-data defaults for the uncompressed
    /// form).
    /// `pub(crate)` so objects and items can share it verbatim.
    ///
    /// Chapter 9's compressed form is permitted in an object or item as
    /// well as a position report, and the discriminator is the same:
    /// the first byte of the position field is a digit only in the
    /// uncompressed form. Sharing this function rather than copying the
    /// base-91 arithmetic is deliberate here for a reason this crate
    /// has already paid for once: when the coordinate unit changed,
    /// nine duplicated divisors had to be found by hand and five of
    /// them the compiler could not catch.
    pub(crate) fn parse_body(
        info: &'a [u8],
        at: usize,
        messaging: bool,
    ) -> Result<(Self, CompressedCs, CompressionType), AprsError> {
        let first = byte_at(info, at)?;
        if first.is_ascii_digit() {
            Ok((
                Self::parse_uncompressed(info, at, messaging)?,
                CompressedCs::NoData,
                CompressionType::default(),
            ))
        } else {
            Self::parse_compressed(info, at, messaging)
        }
    }

    fn parse_uncompressed(info: &'a [u8], at: usize, messaging: bool) -> Result<Self, AprsError> {
        let end = at + Self::UNCOMPRESSED_BODY;
        if info.len() < end {
            return Err(AprsError::Truncated {
                expected: end,
                got: info.len(),
            });
        }
        let LatLonBlock {
            latitude,
            longitude,
            symbol,
            ambiguity,
        } = parse_latlon(info, at)?;
        let tail = info.get(end..).unwrap_or(&[]);
        // The extension is symbol-dependent: `ddd/sss` is course/speed
        // for every symbol except the weather `_`, where it is wind.
        let extension = DataExtension::parse(tail, symbol);
        let comment = match extension {
            Some(ext) => tail.get(ext.wire_len()..).unwrap_or(&[]),
            None => tail,
        };
        Ok(Self {
            latitude,
            longitude,
            symbol,
            ambiguity,
            messaging,
            compressed: false,
            extension,
            comment,
        })
    }

    fn parse_compressed(
        info: &'a [u8],
        at: usize,
        messaging: bool,
    ) -> Result<(Self, CompressedCs, CompressionType), AprsError> {
        let end = at + Self::COMPRESSED_BODY;
        if info.len() < end {
            return Err(AprsError::Truncated {
                expected: end,
                got: info.len(),
            });
        }
        let symbol_table = byte_at(info, at)?;
        check_symbol_table(symbol_table)?;
        let y = parse_base91(info, at + 1)?;
        let x = parse_base91(info, at + 5)?;
        let symbol_code = byte_at(info, at + 9)?;
        // lat_deg = 90 - y / 380926, in 1/100 arc-minutes.
        let lat_raw = LAT_MAX - y * UNITS_PER_COMPRESSED_LAT;
        // lon_deg = -180 + x / 190463, in 1/100 arc-minutes.
        let lon_raw = x * UNITS_PER_COMPRESSED_LON - LON_MAX;
        let latitude = Latitude::new(lat_raw)?;
        let longitude = Longitude::new(lon_raw)?;
        let (cs, compression_type) = parse_cs(info, at + 10)?;
        Ok((
            Self {
                latitude,
                longitude,
                symbol: Symbol::from_wire(symbol_table, symbol_code),
                // Chapter 6 ambiguity is spelled with spaces in a
                // decimal field, and this form has none: a space in the
                // compressed body is the `cs` no-data trailer, and
                // reading base-91 as decimal would invent a position.
                ambiguity: Ambiguity::EXACT,
                messaging,
                compressed: true,
                // Compressed positions have no extension slot; see the
                // field docs.
                extension: None,
                comment: info.get(end..).unwrap_or(&[]),
            },
            cs,
            compression_type,
        ))
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        let fixed = if self.compressed {
            Self::COMPRESSED_LEN
        } else {
            Self::UNCOMPRESSED_LEN
        };
        fixed + self.extension_len() + self.comment.len()
    }

    /// Wire length of the data extension, or 0 if there is none.
    ///
    /// Always 0 for the compressed form, which cannot carry one.
    pub(crate) const fn extension_len(&self) -> usize {
        match self.extension {
            Some(ext) if !self.compressed => ext.wire_len(),
            _ => 0,
        }
    }

    /// The serialized length of the body (without DTI or timestamp).
    pub(crate) const fn body_len(&self) -> usize {
        if self.compressed {
            Self::COMPRESSED_BODY
        } else {
            Self::UNCOMPRESSED_BODY
        }
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// The compressed form carries the no-data `" sT"` trailer; use
    /// [`PositionCs::build`] to encode course/speed, radio range or
    /// altitude.
    ///
    /// # Errors
    ///
    /// [`AprsError::BufferTooSmall`] when `buf` cannot hold the report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = if self.messaging { b'=' } else { b'!' };
        let fixed = 1 + self.write_body(
            &mut out[1..],
            CompressedCs::NoData,
            CompressionType::default(),
        )?;
        let after_ext = fixed + self.write_extension(&mut out[fixed..]);
        for (slot, byte) in out.iter_mut().skip(after_ext).zip(self.comment.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }

    /// Writes the data extension (if any) at the head of `out`.
    pub(crate) fn write_extension(&self, out: &mut [u8]) -> usize {
        match self.extension {
            Some(ext) if !self.compressed => ext.write(out),
            _ => 0,
        }
    }

    /// Writes the position body (no DTI/timestamp) into `out`, which
    /// must be at least [`Self::body_len`] long; returns the body
    /// length. The `cs`/`t` trailer applies to the compressed form
    /// only.
    /// `pub(crate)` for the same reason as [`Self::parse_body`].
    pub(crate) fn write_body(
        &self,
        out: &mut [u8],
        cs: CompressedCs,
        t: CompressionType,
    ) -> Result<usize, AprsError> {
        if self.compressed {
            self.write_compressed(out, cs, t)
        } else {
            Ok(self.write_uncompressed(out))
        }
    }

    /// Writes the uncompressed body; `out` is at least
    /// `UNCOMPRESSED_BODY` long.
    fn write_uncompressed(&self, out: &mut [u8]) -> usize {
        let mut fixed = [0u8; Self::UNCOMPRESSED_BODY];
        write_latlon(
            &mut fixed,
            &LatLonBlock {
                latitude: self.latitude,
                longitude: self.longitude,
                symbol: self.symbol,
                ambiguity: self.ambiguity,
            },
        );
        for (slot, byte) in out.iter_mut().zip(fixed.iter()) {
            *slot = *byte;
        }
        Self::UNCOMPRESSED_BODY
    }

    /// Writes the compressed body; `out` is at least `COMPRESSED_BODY`
    /// long. Per the spec example the scaled coordinate is truncated
    /// (not rounded); the parser's rounding recovers the exact 1/100
    /// arc-minute value. The `csT` bytes encode `cs` and `t` (nearest
    /// representable value; see the module docs for the rounding
    /// rules).
    fn write_compressed(
        &self,
        out: &mut [u8],
        cs: CompressedCs,
        t: CompressionType,
    ) -> Result<usize, AprsError> {
        let lat = self.latitude.units();
        let lon = self.longitude.units();
        // y = 380926 * (90 - lat_deg); x = 190463 * (180 + lon_deg).
        let y = (LAT_MAX - lat) / UNITS_PER_COMPRESSED_LAT;
        let x = (LON_MAX + lon) / UNITS_PER_COMPRESSED_LON;
        let mut fixed = [0u8; Self::COMPRESSED_BODY];
        let (symbol_table, symbol_code) = self.symbol.to_wire();
        fixed[0] = symbol_table;
        write_base91(&mut fixed[1..5], y);
        write_base91(&mut fixed[5..9], x);
        fixed[9] = symbol_code;
        let cst = build_cs(cs, t)?;
        fixed[10] = cst[0];
        fixed[11] = cst[1];
        fixed[12] = cst[2];
        for (slot, byte) in out.iter_mut().zip(fixed.iter()) {
            *slot = *byte;
        }
        Ok(Self::COMPRESSED_BODY)
    }
}

/// A position report without timestamp together with its typed `csT`
/// trailer payload (course/speed, radio range, altitude or no data).
///
/// [`Position`] alone always builds the no-data trailer; this wrapper
/// carries and encodes the full trailer. When `position.compressed` is
/// `false` the trailer is not representable on the wire and the
/// `cs`/`compression_type` fields are ignored on build (parsed
/// uncompressed reports carry the no-data defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionCs<'a> {
    /// The position report.
    pub position: Position<'a>,
    /// The `cs` trailer payload.
    pub cs: CompressedCs,
    /// The compression-type `T` byte fields. Ignored on build when
    /// `cs` is [`CompressedCs::NoData`] (the no-data trailer is the
    /// literal `" sT"`); forced to NMEA source GGA when `cs` is
    /// [`CompressedCs::Altitude`].
    pub compression_type: CompressionType,
}

impl<'a> PositionCs<'a> {
    /// Parses a `!` or `=` position report including the typed `csT`
    /// trailer of the compressed form.
    ///
    /// # Errors
    ///
    /// The errors of [`Position::parse`].
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = *info.first().ok_or(AprsError::Truncated {
            expected: 2,
            got: info.len(),
        })?;
        let messaging = dti == b'=';
        let (position, cs, compression_type) = Position::parse_body(info, 1, messaging)?;
        Ok(Self {
            position,
            cs,
            compression_type,
        })
    }

    /// The station position of the wrapped [`Position`], pairing its
    /// two coordinate fields so call sites need not rely on tuple
    /// ordering.
    #[must_use]
    pub fn coordinates(&self) -> Coordinates {
        self.position.coordinates()
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        self.position.encoded_len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::BufferTooSmall`] when `buf` cannot hold the report;
    /// [`AprsError::BadCourse`] / [`AprsError::BadSpeed`] /
    /// [`AprsError::BadRadioRange`] / [`AprsError::BadAltitude`] on an
    /// out-of-range `cs` value; [`AprsError::NmeaSourceConflict`] when
    /// course/speed or radio range are paired with the GGA NMEA source.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = if self.position.messaging { b'=' } else { b'!' };
        let mut fixed =
            1 + self
                .position
                .write_body(&mut out[1..], self.cs, self.compression_type)?;
        fixed += self.position.write_extension(&mut out[fixed..]);
        for (slot, byte) in out.iter_mut().skip(fixed).zip(self.position.comment.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }
}

/// A position report with timestamp (`/` without messaging, `@` with
/// messaging), per APRS 1.01 chapter 8: a 7-byte timestamp followed by
/// the same uncompressed or compressed body as [`Position`], including
/// the typed `csT` trailer of the compressed form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionTimestamped<'a> {
    /// The report timestamp (DHM zulu/local or HMS).
    pub timestamp: Timestamp,
    /// The position body. Its `messaging` flag selects the DTI (`@`
    /// when `true`, `/` when `false`).
    pub position: Position<'a>,
    /// The `cs` trailer payload of the compressed form (no-data for
    /// the uncompressed form; ignored on build when
    /// `position.compressed` is `false`).
    pub cs: CompressedCs,
    /// The compression-type `T` byte fields (see
    /// [`PositionCs::compression_type`]).
    pub compression_type: CompressionType,
}

impl<'a> PositionTimestamped<'a> {
    /// Parses a `/` or `@` timestamped position report.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] / [`AprsError::BadDigit`] on a
    /// malformed timestamp, plus the position errors of
    /// [`Position::parse`].
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = *info.first().ok_or(AprsError::Truncated {
            expected: 1 + Timestamp::LEN,
            got: info.len(),
        })?;
        let messaging = match dti {
            b'@' => true,
            b'/' => false,
            other => return Err(AprsError::InvalidDataType { got: other }),
        };
        let timestamp = Timestamp::parse(info, 1)?;
        let (position, cs, compression_type) =
            Position::parse_body(info, 1 + Timestamp::LEN, messaging)?;
        Ok(Self {
            timestamp,
            position,
            cs,
            compression_type,
        })
    }

    /// The station position of the wrapped [`Position`], pairing its
    /// two coordinate fields so call sites need not rely on tuple
    /// ordering.
    #[must_use]
    pub fn coordinates(&self) -> Coordinates {
        self.position.coordinates()
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        1 + Timestamp::LEN
            + self.position.body_len()
            + self.position.extension_len()
            + self.position.comment.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] on an out-of-range timestamp, plus
    /// the build errors of [`PositionCs::build`].
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = if self.position.messaging { b'@' } else { b'/' };
        self.timestamp.write(&mut out[1..1 + Timestamp::LEN])?;
        let mut at = 1 + Timestamp::LEN;
        at += self
            .position
            .write_body(&mut out[at..], self.cs, self.compression_type)?;
        at += self.position.write_extension(&mut out[at..]);
        for (slot, byte) in out.iter_mut().skip(at).zip(self.position.comment.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }
}

/// Parses the three `csT` trailer bytes at `at` into the typed `cs`
/// payload and compression-type fields.
fn parse_cs(info: &[u8], at: usize) -> Result<(CompressedCs, CompressionType), AprsError> {
    let c = byte_at(info, at)?;
    if c == b' ' {
        // No data: the s and T bytes carry no information.
        return Ok((CompressedCs::NoData, CompressionType::default()));
    }
    let s = base91_digit(byte_at(info, at + 1)?, at + 1)?;
    let t = CompressionType::from_byte(byte_at(info, at + 2)?, at + 2)?;
    if t.nmea_source == NmeaSource::Gga {
        let c = base91_digit(c, at)?;
        let exp = u32::from(c) * 91 + u32::from(s);
        return Ok((
            CompressedCs::Altitude {
                feet: decode_altitude(exp),
            },
            t,
        ));
    }
    if c == b'{' {
        let miles = decode_range(u32::from(s));
        #[allow(clippy::cast_possible_truncation)]
        return Ok((
            CompressedCs::RadioRange {
                miles: miles as u16,
            },
            t,
        ));
    }
    let c = base91_digit(c, at)?;
    let course = u16::from(c) * 4;
    if course >= 360 {
        return Err(AprsError::BadCourse { got: course });
    }
    let speed = decode_speed(u32::from(s));
    #[allow(clippy::cast_possible_truncation)]
    Ok((
        CompressedCs::CourseSpeed {
            course,
            speed: speed as u16,
        },
        t,
    ))
}

// The three `cs` scales, as the parser reads them. `build_cs` inverts
// these same functions rather than the powers behind them, so there is
// exactly one definition of what a code means and the two directions
// cannot drift apart. Each is non-decreasing in its exponent, which is
// what [`exponent_for`] searches on; `cs_scales_are_monotonic` in this
// module sweeps every code of all three to keep that true.

/// Speed in knots for the `s` code: `1.08^s - 1`, to the nearest knot.
fn decode_speed(exp: u32) -> u32 {
    round_u32(pow_f64(CS_BASE, exp) - 1.0)
}

/// Radio range in miles for the `s` code: `2 * 1.08^s`, to the nearest
/// mile.
fn decode_range(exp: u32) -> u32 {
    round_u32(2.0 * pow_f64(CS_BASE, exp))
}

/// Altitude in feet for the `cs` code: `1.002^cs`, truncated to whole
/// feet as in chapter 9's worked example.
fn decode_altitude(exp: u32) -> u32 {
    trunc_u32(pow_f64(ALT_BASE, exp))
}

/// Encodes the typed `cs` payload and compression-type fields into the
/// three wire bytes, choosing a code that reads back as the given value
/// (see [`exponent_for`]).
fn build_cs(cs: CompressedCs, t: CompressionType) -> Result<[u8; 3], AprsError> {
    match cs {
        CompressedCs::NoData => Ok([b' ', b's', b'T']),
        CompressedCs::CourseSpeed { course, speed } => {
            if t.nmea_source == NmeaSource::Gga {
                return Err(AprsError::NmeaSourceConflict);
            }
            if course >= 360 {
                return Err(AprsError::BadCourse { got: course });
            }
            // Nearest 4-degree step; 358..=359 round up to 360 == 0.
            let c = ((course + 2) / 4) % 90;
            let value = u32::from(speed);
            let s = exponent_for(
                CS_BASE,
                f64::from(speed) + 1.0,
                CS_MAX_EXP,
                value,
                decode_speed,
            )
            .ok_or(AprsError::BadSpeed { got: speed })?;
            #[allow(clippy::cast_possible_truncation)]
            Ok([
                BASE91_OFFSET + c as u8,
                BASE91_OFFSET + s as u8,
                t.to_byte(),
            ])
        }
        CompressedCs::RadioRange { miles } => {
            if t.nmea_source == NmeaSource::Gga {
                return Err(AprsError::NmeaSourceConflict);
            }
            let value = u32::from(miles);
            let s = exponent_for(
                CS_BASE,
                f64::from(miles) / 2.0,
                CS_MAX_EXP,
                value,
                decode_range,
            )
            .ok_or(AprsError::BadRadioRange { got: miles })?;
            #[allow(clippy::cast_possible_truncation)]
            Ok([b'{', BASE91_OFFSET + s as u8, t.to_byte()])
        }
        CompressedCs::Altitude { feet } => {
            let exp = exponent_for(
                ALT_BASE,
                f64::from(feet),
                ALT_MAX_EXP,
                feet,
                decode_altitude,
            )
            .ok_or(AprsError::BadAltitude { got: feet })?;
            let with_gga = CompressionType {
                nmea_source: NmeaSource::Gga,
                ..t
            };
            #[allow(clippy::cast_possible_truncation)]
            Ok([
                BASE91_OFFSET + (exp / 91) as u8,
                BASE91_OFFSET + (exp % 91) as u8,
                with_gga.to_byte(),
            ])
        }
    }
}

/// `base^exp` for non-negative integer `exp` via square-and-multiply
/// on `core` float arithmetic (no `std`/`libm`).
///
/// # Precision, and why `f64` is not optional here
///
/// Square-and-multiply is the accurate-enough method available without
/// `libm`, not the accurate one: each `factor *= factor` doubles the
/// relative error, so it accumulates roughly with `exp` rather than
/// with `log2(exp)`. Checked against exact rational arithmetic over all
/// 8281 altitude codes, the worst relative error is about 1.7e-13
/// (~780 ulp), against ~1.5e-14 for a library `pow`.
///
/// That is still correct everywhere on these scales: `floor` and the
/// roundings agree with exact arithmetic on **every** code of all three
/// scales, and the tightest approach to a decision boundary leaves a
/// factor of about 436 in hand (code 8212, whose exact value is
/// 13 357 623.9990). But the margin is three orders of magnitude, not
/// ten, so the type matters: computed in `f32` this disagrees with
/// exact arithmetic on 3903 of the 8281 altitude codes, first at code
/// 1927. Anything that narrows the base or the accumulator below `f64`
/// changes what packets mean.
///
/// `every_cs_code_is_value_stable_through_a_rebuild` pins the number of
/// codes that share a value (2612 on the altitude scale), which is a
/// fingerprint of this arithmetic: a change that moved any code across
/// a foot boundary would move that count.
fn pow_f64(base: f64, mut exp: u32) -> f64 {
    let mut acc = 1.0f64;
    let mut factor = base;
    while exp > 0 {
        if exp & 1 == 1 {
            acc *= factor;
        }
        factor *= factor;
        exp >>= 1;
    }
    acc
}

/// The code to write for `value` on a scale the parser reads back with
/// `decode`.
///
/// Returns a code that decodes to exactly `value` whenever the scale has
/// one. Where it has none, falls back to the code whose underlying power
/// is nearest `target`, and returns `None` on the same out-of-range
/// values [`nearest_exponent`] rejects.
///
/// # Why this is not just [`nearest_exponent`]
///
/// Build used to invert the power: pick the `e` minimising
/// `|base^e - target|`. That is the wrong inverse, because the parser
/// does not report `base^e`, it reports `decode(e)`, and the two round
/// in different directions. Altitude is the case that shows it.
/// `decode` truncates, so `1.002^e` lies anywhere in `[feet, feet + 1)`
/// and the exponent nearest `feet` is routinely `e - 1`. Code 2951
/// decodes to 363 feet; the exponent nearest 363.0 is 2950, which
/// decodes to 362. That hit 999 of the 8281 altitude codes and 302 of
/// the 57 731 buildable packets in a live APRS-IS capture.
///
/// The loss was **not** bounded at one foot, which is the part worth
/// knowing. APRS packets are parsed and re-emitted by igates and
/// digipeaters, so the cycle runs more than once, and in the band where
/// a code step is close to a whole foot it ratchets: MEASURED by
/// iterating the old rule to a fixed point over the whole domain, code
/// 3131 reads 520 feet and walks down to 480 over 41 passes. 417 codes
/// lost more than a foot. A quantity that decays every time it is
/// relayed is a different class of defect from one that is just
/// imprecise.
///
/// Speed and range round rather than truncate and were value-stable
/// already; they run through here too so that one rule covers all three
/// scales, and the clamp below is what keeps that from costing them
/// anything.
///
/// Searching the decoded values states the property being bought:
/// **`parse(build(parse(w))) = parse(w)`**, which is mandatory, as
/// against byte identity, which is optional and unreachable anyway
/// wherever several codes decode alike.
///
/// # The search
///
/// `decode` is non-decreasing in the exponent, so the codes decoding to
/// a given value form one contiguous run `lo..=hi`, found by two binary
/// searches. Monotonicity is the load-bearing assumption and is swept
/// over every code of all three scales by `cs_scales_are_monotonic`.
///
/// The answer is the nearest power's code **clamped into that run**
/// rather than either end of it. Clamping is what makes this change
/// free: where the old choice already decoded to `value` it is inside
/// the run and survives untouched, so no packet that used to rebuild
/// byte-exactly stops doing so, and only the codes that were losing
/// information move. Taking the low end instead would have re-spelled
/// 25 packets of the same capture for nothing.
///
/// Note where the clamp does and does not earn that. On a **truncating**
/// scale it is provably a no-op against "lowest in the run": if
/// `floor(b^lo) = value` then `b^lo` is in `[value, value + 1)`, so
/// every higher code in the run is strictly farther from `value` and
/// the clamp always lands on `lo`. All 25 of those packets are
/// therefore speed and range, whose decoders round, and where the
/// nearest code can sit anywhere in the run.
fn exponent_for(
    base: f64,
    target: f64,
    max: u32,
    value: u32,
    decode: fn(u32) -> u32,
) -> Option<u32> {
    // Range gate first, and it stays in the power domain: it decides
    // which values are buildable at all, which is a separate question
    // from which code spells them.
    let nearest = nearest_exponent(base, target, max)?;
    // First code decoding to at least `value`.
    let mut lo = 0;
    let mut hi = max;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if decode(mid) >= value {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    if decode(lo) != value {
        // `value` falls between two codes. Nothing decodes to it, so no
        // choice preserves it and the nearest power is as good an
        // answer as any; it is never more than one code away.
        return Some(nearest);
    }
    // Last code decoding to `value`, searched from the first.
    let (run_start, mut lo) = (lo, lo);
    let mut hi = max;
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if decode(mid) <= value {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(nearest.clamp(run_start, lo))
}

/// The exponent `e` in `0..=max` for which `base^e` is nearest to
/// `target` (in absolute distance), or `None` when `target` is above
/// the midpoint past `base^max`.
///
/// Used by [`exponent_for`] as the range gate and as the fallback for a
/// value the scale cannot express. Not the encoder on its own: see
/// there for why inverting the power is not inverting the parser.
fn nearest_exponent(base: f64, target: f64, max: u32) -> Option<u32> {
    if target <= 1.0 {
        return Some(0);
    }
    let mut below = 1.0f64;
    for e in 0..max {
        let above = below * base;
        if target <= above {
            return Some(if target - below <= above - target {
                e
            } else {
                e + 1
            });
        }
        below = above;
    }
    // target > base^max: accept up to the rounding midpoint.
    if target - below <= below * (base - 1.0) / 2.0 {
        Some(max)
    } else {
        None
    }
}

/// Rounds a non-negative `f64` to the nearest `u32` (saturating).
fn round_u32(value: f64) -> u32 {
    trunc_u32(value + 0.5)
}

/// Truncates a non-negative `f64` to a `u32` (saturating).
fn trunc_u32(value: f64) -> u32 {
    if value <= 0.0 {
        return 0;
    }
    if value >= 4_294_967_295.0 {
        u32::MAX
    } else {
        // Non-negative and in range by the checks above.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            value as u32
        }
    }
}

/// Decodes one base-91 byte at `position` into its digit value.
const fn base91_digit(byte: u8, position: usize) -> Result<u8, AprsError> {
    if byte >= b'!' && byte <= b'{' {
        Ok(byte - BASE91_OFFSET)
    } else {
        Err(AprsError::BadBase91 {
            got: byte,
            position,
        })
    }
}

/// The 19-byte uncompressed lat/table/lon/code block shared by position,
/// object and item reports (`ddmm.mmN` + table + `dddmm.mmW` + code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LatLonBlock {
    pub(crate) latitude: Latitude,
    pub(crate) longitude: Longitude,
    pub(crate) symbol: Symbol,
    /// How many low-order coordinate digits the sender blanked, per
    /// chapter 6. Zero for a report that gave its position in full.
    pub(crate) ambiguity: Ambiguity,
}

/// Byte length of a [`LatLonBlock`].
pub(crate) const LATLON_LEN: usize = 8 + 1 + 9 + 1;

/// The four `DDMM.hh` digit offsets that chapter 6 may blank, in the
/// order it blanks them: rightmost first.
///
/// Offsets are relative to the first digit of the field, so they suit
/// both `ddmm.hh` and `dddmm.hh` once the caller adds its own base.
const MASKABLE: [usize; 4] = [6, 5, 3, 2];

/// Reads the blanked-digit count of a `DDMM.hh` field.
///
/// Chapter 6 blanks from the right, one digit at a time, and shows all
/// four levels: `4903.5_` to the nearest tenth of a minute, `4903.__`
/// to the minute, `490_.__` to ten minutes, `49__.__` to the degree.
///
/// # Errors
///
/// [`AprsError::BadDigit`] when a space appears anywhere other than a
/// right-aligned run of the four maskable positions. Scattered spaces
/// are corruption, not ambiguity, and accepting them would turn a
/// damaged coordinate into a confident one: chapter 6 gives no meaning
/// to a hole in the middle of a number, so the digits to the right of
/// it cannot be read as the low-order digits of anything.
fn ambiguity_of(info: &[u8], at: usize) -> Result<u8, AprsError> {
    let mut blanked = 0u8;
    for (rank, offset) in MASKABLE.iter().enumerate() {
        let byte = byte_at(info, at + offset)?;
        if byte == b' ' {
            // A space is only legal if every position to its right in
            // the blanking order is also a space, which is exactly
            // "this is the next one to blank".
            if usize::from(blanked) != rank {
                return Err(AprsError::BadDigit {
                    got: byte,
                    position: at + offset,
                });
            }
            blanked += 1;
        }
    }
    // The degree digits are never maskable, so a space there is
    // corruption whatever the rest of the field says. `parse_digits`
    // rejects it when it reads them.
    Ok(blanked)
}

/// Reads `count` digits at `position`, taking a space as a zero.
///
/// Only for the maskable positions of a coordinate: [`ambiguity_of`]
/// has already checked that any spaces form a right-aligned run, and a
/// blanked digit reads as zero because the masking below removes it
/// again. Everywhere else, use [`parse_digits`].
fn parse_digits_blankable(info: &[u8], position: usize, count: usize) -> Result<i32, AprsError> {
    let mut value: i32 = 0;
    for offset in 0..count {
        let byte = byte_at(info, position + offset)?;
        let digit = match byte {
            b' ' => 0,
            b if b.is_ascii_digit() => i32::from(b - b'0'),
            b => {
                return Err(AprsError::BadDigit {
                    got: b,
                    position: position + offset,
                });
            }
        };
        value = value * 10 + digit;
    }
    Ok(value)
}

/// The ambiguity level in force, given what each axis spells.
///
/// Chapter 6 makes the latitude authoritative: "the level of ambiguity
/// specified in the latitude will automatically apply to the longitude
/// as well, it is permissible but not necessary to include any space
/// characters in the longitude."
///
/// A longitude blanked *further* than the latitude is taken at face
/// value rather than sharpened, because those digits are not on the
/// wire to be read and claiming the latitude's finer level would mean
/// inventing them. MEASURED over a 64 918-packet capture: that never
/// occurs, so this is a safety rule rather than a compatibility one.
const fn declared_level(lat_blanks: u8, lon_blanks: u8) -> u8 {
    if lon_blanks > lat_blanks {
        lon_blanks
    } else {
        lat_blanks
    }
}

/// Parses a [`LatLonBlock`] starting at `position` (the caller has
/// already length-checked `info`).
pub(crate) fn parse_latlon(info: &[u8], position: usize) -> Result<LatLonBlock, AprsError> {
    // ddmm.hhN, with chapter 6 blanking. The degree digits are never
    // maskable, so they stay strict; only `mm` and `hh` may be spaces,
    // and `ambiguity_of` has already checked that any of them form a
    // right-aligned run.
    let lat_blanks = ambiguity_of(info, position)?;
    let deg = parse_digits(info, position, 2)?;
    let min = parse_digits_blankable(info, position + 2, 2)?;
    expect_byte(info, position + 4, b'.')?;
    let hundredths = parse_digits_blankable(info, position + 5, 2)?;
    let lat_hem = byte_at(info, position + 7)?;
    // Accepted case-insensitively. The spec specifies upper case and
    // `write_latlon` always emits it, but lower case is seen on the air
    // from sloppy encoders and says nothing about whether the position
    // decoded -- the same asymmetry (validate on transmit, preserve on
    // receive) applied to the Mic-E symbol table byte.
    let lat_sign = match lat_hem.to_ascii_uppercase() {
        b'N' => 1,
        b'S' => -1,
        _ => return Err(AprsError::BadHemisphere { got: lat_hem }),
    };
    let lat_raw = i64::from(lat_sign)
        * (i64::from(deg) * UNITS_PER_DEGREE
            + i64::from(min) * UNITS_PER_MINUTE
            + i64::from(hundredths) * UNITS_PER_HUNDREDTH_MINUTE);
    if min >= 60 {
        return Err(AprsError::BadLatitude { got: lat_raw });
    }
    let latitude = Latitude::new(lat_raw)?;

    let symbol_table = byte_at(info, position + 8)?;
    check_symbol_table(symbol_table)?;

    // dddmm.hhW. The `+ 1` past the block offset is the third degree
    // digit: `MASKABLE` is indexed from the first digit of a `DDMM.hh`
    // field and the longitude is `DDDMM.hh`, so shifting the base by
    // one lands the same four offsets on `mm` and `hh` again.
    let lon_blanks = ambiguity_of(info, position + 9 + 1)?;
    let deg = parse_digits(info, position + 9, 3)?;
    let min = parse_digits_blankable(info, position + 12, 2)?;
    expect_byte(info, position + 14, b'.')?;
    let hundredths = parse_digits_blankable(info, position + 15, 2)?;
    let lon_hem = byte_at(info, position + 17)?;
    // Case-insensitive, as for latitude above.
    let lon_sign = match lon_hem.to_ascii_uppercase() {
        b'E' => 1,
        b'W' => -1,
        _ => return Err(AprsError::BadHemisphere { got: lon_hem }),
    };
    let lon_raw = i64::from(lon_sign)
        * (i64::from(deg) * UNITS_PER_DEGREE
            + i64::from(min) * UNITS_PER_MINUTE
            + i64::from(hundredths) * UNITS_PER_HUNDREDTH_MINUTE);
    if min >= 60 {
        return Err(AprsError::BadLongitude { got: lon_raw });
    }
    let longitude = Longitude::new(lon_raw)?;

    let symbol_code = byte_at(info, position + 18)?;
    // Both counts are 0..=4 by construction, so `new` cannot fail; the
    // fallback keeps this total rather than introducing a panic path.
    let ambiguity =
        Ambiguity::new(declared_level(lat_blanks, lon_blanks)).unwrap_or(Ambiguity::EXACT);
    Ok(LatLonBlock {
        latitude,
        longitude,
        symbol: Symbol::from_wire(symbol_table, symbol_code),
        ambiguity,
    })
}

/// Writes a [`LatLonBlock`] into `out` (at least [`LATLON_LEN`] bytes).
pub(crate) fn write_latlon(out: &mut [u8], block: &LatLonBlock) {
    let lat = block.latitude.units();
    let lon = block.longitude.units();
    let lat_hem = if lat < 0 { b'S' } else { b'N' };
    let lon_hem = if lon < 0 { b'W' } else { b'E' };
    // The storage unit is far finer than the DDMM.hh written here, so
    // the magnitude is rounded to whole hundredths of a minute ONCE,
    // up front, and the familiar divisors then apply to that. Two
    // reasons for rounding rather than truncating, and for doing it
    // before the split rather than per field:
    //
    //   * truncation throws away up to a whole hundredth (18.55 m) on
    //     any coordinate that arrived in a finer format, where
    //     rounding throws away at most half of one;
    //   * rounding the total first means a value that carries into a
    //     full 60 minutes carries into the degrees by itself, instead
    //     of spelling `59.100`.
    //
    // A coordinate that arrived as DDMM.hh is an exact multiple of a
    // hundredth, so for that format this is the identity and the round
    // trip stays byte-exact.
    let lat = to_hundredths(lat);
    let lon = to_hundredths(lon);
    let (symbol_table, symbol_code) = block.symbol.to_wire();
    let mut fixed = [0u8; LATLON_LEN];
    write_digits(&mut fixed[0..2], lat / 6000);
    write_digits(&mut fixed[2..4], lat / 100 % 60);
    fixed[4] = b'.';
    write_digits(&mut fixed[5..7], lat % 100);
    fixed[7] = lat_hem;
    fixed[8] = symbol_table;
    write_digits(&mut fixed[9..12], lon / 6000);
    write_digits(&mut fixed[12..14], lon / 100 % 60);
    fixed[14] = b'.';
    write_digits(&mut fixed[15..17], lon % 100);
    fixed[17] = lon_hem;
    fixed[18] = symbol_code;
    // Blank the declared digits in BOTH axes. Chapter 6 says the
    // longitude inherits the latitude's level and that spaces there are
    // "permissible but not necessary", so writing them is legal, and it
    // reproduces what 207 of the 211 captured senders wrote. The other
    // four spelled a full-precision longitude beside a blanked
    // latitude; those come back re-spelled, which changes no value.
    //
    // Same `9 + 1` shift as the parser, for the same reason.
    for (rank, offset) in MASKABLE.iter().enumerate() {
        if rank < block.ambiguity.digits() as usize {
            fixed[*offset] = b' ';
            fixed[9 + 1 + *offset] = b' ';
        }
    }
    for (slot, byte) in out.iter_mut().zip(fixed.iter()) {
        *slot = *byte;
    }
}

/// A coordinate magnitude in whole 1/100 arc-minutes, rounded to
/// nearest. The sign is dropped; callers take the hemisphere first.
fn to_hundredths(units: i64) -> u64 {
    let magnitude = units.unsigned_abs();
    let step = UNITS_PER_HUNDREDTH_MINUTE.unsigned_abs();
    (magnitude + step / 2) / step
}

/// Fetches one byte, mapping out-of-bounds to [`AprsError::Truncated`].
pub(crate) fn byte_at(info: &[u8], position: usize) -> Result<u8, AprsError> {
    info.get(position).copied().ok_or(AprsError::Truncated {
        expected: position + 1,
        got: info.len(),
    })
}

/// Requires the literal `expected` at `position`.
pub(crate) fn expect_byte(info: &[u8], position: usize, expected: u8) -> Result<(), AprsError> {
    let got = byte_at(info, position)?;
    if got == expected {
        Ok(())
    } else {
        Err(AprsError::ExpectedByte {
            expected,
            got,
            position,
        })
    }
}

/// Parses `count` ASCII digits at `position` into an integer.
pub(crate) fn parse_digits(info: &[u8], position: usize, count: usize) -> Result<i32, AprsError> {
    let mut value: i32 = 0;
    for offset in 0..count {
        let byte = byte_at(info, position + offset)?;
        if !byte.is_ascii_digit() {
            return Err(AprsError::BadDigit {
                got: byte,
                position: position + offset,
            });
        }
        value = value * 10 + i32::from(byte - b'0');
    }
    Ok(value)
}

/// Writes `value` as zero-padded ASCII digits filling `out`.
pub(crate) fn write_digits(out: &mut [u8], mut value: u64) {
    for slot in out.iter_mut().rev() {
        *slot = b'0' + u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
    }
}

/// Parses four base-91 bytes at `position` into an integer.
fn parse_base91(info: &[u8], position: usize) -> Result<i64, AprsError> {
    let mut value: i64 = 0;
    for offset in 0..4 {
        let byte = byte_at(info, position + offset)?;
        if !(b'!'..=b'{').contains(&byte) {
            return Err(AprsError::BadBase91 {
                got: byte,
                position: position + offset,
            });
        }
        value = value * 91 + i64::from(byte - b'!');
    }
    Ok(value)
}

/// Writes `value` as four base-91 bytes filling `out`.
fn write_base91(out: &mut [u8], mut value: i64) {
    for slot in out.iter_mut().rev() {
        *slot = b'!' + u8::try_from(value.rem_euclid(91)).unwrap_or(0);
        value = value.div_euclid(91);
    }
}

/// Validates a symbol table identifier: `/`, `\`, digit or letter
/// overlay (`a-j` occurs in the compressed form for digit overlays).
fn check_symbol_table(byte: u8) -> Result<(), AprsError> {
    match byte {
        b'/' | b'\\' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'j' => Ok(()),
        other => Err(AprsError::BadSymbolTable { got: other }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A latitude from 1/100 arc-minutes, the unit every fixture in
    /// this module is written in.
    fn lat(hundredths: i64) -> Latitude {
        match Latitude::new(hundredths * UNITS_PER_HUNDREDTH_MINUTE) {
            Ok(l) => l,
            Err(e) => panic!("{e}"),
        }
    }

    /// A longitude from 1/100 arc-minutes.
    fn lon(hundredths: i64) -> Longitude {
        match Longitude::new(hundredths * UNITS_PER_HUNDREDTH_MINUTE) {
            Ok(l) => l,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn coordinates_pair_the_fields() {
        let pos = match Position::parse(b"!4903.50N/07201.75W-") {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(
            pos.coordinates(),
            Coordinates::new(lat(49 * 6000 + 350), lon(-(72 * 6000 + 175)))
        );
        assert_eq!(pos.coordinates().latitude, pos.latitude);
        assert_eq!(pos.coordinates().longitude, pos.longitude);

        // Both wrappers delegate to the position they hold.
        let cs = match PositionCs::parse(b"!4903.50N/07201.75W-") {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(cs.coordinates(), pos.coordinates());
        let stamped = match PositionTimestamped::parse(b"/092345z4903.50N/07201.75W-") {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(stamped.coordinates(), pos.coordinates());
    }

    #[test]
    fn spec_uncompressed_vector() {
        // APRS 1.01 example: 49 deg 03.50 min N, 072 deg 01.75 min W.
        let parsed = match Position::parse(b"!4903.50N/07201.75W-Test 001234") {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(parsed.latitude, lat(49 * 6000 + 350));
        assert_eq!(parsed.longitude, lon(-(72 * 6000 + 175)));
        assert_eq!(parsed.symbol.to_wire(), (b'/', b'-'));
        assert!(!parsed.messaging);
        assert!(!parsed.compressed);
        assert_eq!(parsed.comment, b"Test 001234");

        let mut buf = [0u8; 64];
        let len = match parsed.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b"!4903.50N/07201.75W-Test 001234");
    }

    #[test]
    fn southern_eastern_hemispheres_round_trip() {
        let pos = Position {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(-(35 * 6000 + 1234)),
            longitude: lon(138 * 6000 + 5999),
            symbol: Symbol::from_wire(b'\\', b'>'),
            messaging: true,
            compressed: false,
            extension: None,
            comment: b"",
        };
        let mut buf = [0u8; 32];
        let len = match pos.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b"=3512.34S\\13859.99E>");
        assert_eq!(Position::parse(&buf[..len]), Ok(pos));
    }

    #[test]
    fn spec_compressed_vector() {
        // APRS 1.01 chapter 9 example: /5L!!<*e7> sT decodes to
        // Chapter 9's own example. The spec's prose rounds it to
        // "49.5 deg N, 72.75 deg W"; the wire is finer than that, and
        // this now stores what the wire says rather than the rounding.
        // `<*e7` is base-91 20 427 156, so the longitude is exactly
        // -180 + 20427156/190463 = -72.75000393777269 degrees. Storing
        // it to the nearest 1/100 arc-minute, as this crate did, moved
        // the station 0.44 m.
        let parsed = match Position::parse(b"!/5L!!<*e7> sT") {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert!(parsed.compressed);
        assert_eq!(parsed.symbol.to_wire(), (b'/', b'>'));
        assert_eq!(
            parsed.latitude,
            Latitude::new(LAT_MAX - 15_427_503 * UNITS_PER_COMPRESSED_LAT).expect("in range")
        );
        assert_eq!(
            parsed.longitude,
            Longitude::new(20_427_156 * UNITS_PER_COMPRESSED_LON - LON_MAX).expect("in range")
        );
        // Which is the spec's quoted reading, to the precision the spec
        // quotes it to.
        assert!((parsed.latitude.to_degrees() - 49.5).abs() < 1e-5);
        assert!((parsed.longitude.to_degrees() + 72.75).abs() < 1e-5);
        assert_eq!(parsed.comment, b"");

        // Rebuilding reproduces the coordinate bytes exactly (the csT
        // suffix is our fixed " sT").
        let mut buf = [0u8; 32];
        let len = match parsed.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b"!/5L!!<*e7> sT");
    }

    #[test]
    fn compressed_round_trip_all_quadrants() {
        for (la, lo) in [
            (49 * 6000 + 350, -(72 * 6000 + 175)),
            (-(89 * 6000 + 5999), 179 * 6000 + 5999),
            (0, 0),
            (540_000, -1_080_000),
        ] {
            let pos = Position {
                ambiguity: Ambiguity::EXACT,
                latitude: lat(la),
                longitude: lon(lo),
                symbol: Symbol::BALLOON,
                messaging: false,
                compressed: true,
                extension: None,
                comment: b"balloon",
            };
            let mut buf = [0u8; 64];
            let len = match pos.build(&mut buf) {
                Ok(n) => n,
                Err(e) => panic!("{e}"),
            };
            // A value on the 1/100 arc-minute grid is not generally on
            // the base-91 grid, so building quantises it to the nearest
            // compressed step and parsing returns that. The old
            // behaviour looked exact only because both grids were
            // rounded onto the coarser one. What must hold is that the
            // error is bounded by one compressed step, and that the
            // WIRE round trip (parse then build) is exact, which
            // `spec_compressed_vector` and the tier-2 vectors assert.
            let back = match Position::parse(&buf[..len]) {
                Ok(p) => p,
                Err(e) => panic!("{e}"),
            };
            let dlat = (back.latitude.units() - pos.latitude.units()).abs();
            let dlon = (back.longitude.units() - pos.longitude.units()).abs();
            assert!(
                dlat <= UNITS_PER_COMPRESSED_LAT,
                "({la}, {lo}) latitude moved {dlat} units"
            );
            assert!(
                dlon <= UNITS_PER_COMPRESSED_LON,
                "({la}, {lo}) longitude moved {dlon} units"
            );
            assert_eq!(back.symbol, pos.symbol, "({la}, {lo})");
            assert_eq!(back.comment, pos.comment, "({la}, {lo})");
        }
    }

    /// The assumption [`exponent_for`]'s binary search rests on.
    ///
    /// A binary search for the lower bound of a value is only correct
    /// on a non-decreasing function. These three are powers evaluated
    /// by square-and-multiply and then rounded or truncated, so
    /// monotonicity is a property of the floating-point arithmetic
    /// rather than of the mathematics, and it is cheap enough to check
    /// exhaustively: 91 + 91 + 8281 codes.
    #[test]
    fn cs_scales_are_monotonic() {
        /// One `cs` scale: its name, the parser's reading of a code, and
        /// the highest code it has.
        type Scale = (&'static str, fn(u32) -> u32, u32);
        let scales: [Scale; 3] = [
            ("speed", decode_speed, CS_MAX_EXP),
            ("range", decode_range, CS_MAX_EXP),
            ("altitude", decode_altitude, ALT_MAX_EXP),
        ];
        for (name, decode, max) in scales {
            let mut previous = decode(0);
            for exp in 1..=max {
                let value = decode(exp);
                assert!(
                    value >= previous,
                    "{name} decodes code {exp} as {value}, below code \
                     {} at {previous}; exponent_for's binary search \
                     needs this to be non-decreasing",
                    exp - 1
                );
                previous = value;
            }
        }
    }

    /// **F3 over every `cs` code of all three scales**: the value a
    /// code decodes to must survive being written back out.
    ///
    /// Driven through `build_cs` and `parse_cs` rather than through
    /// `exponent_for`, because the defect this pins was a build path
    /// calling the wrong inverse, and a test of the inverse alone stays
    /// green while the caller ignores it.
    ///
    /// Byte identity is not asserted, because it is unreachable: 21
    /// speed codes, 13 range codes and 2612 altitude codes decode to a
    /// value some lower code also decodes to, and the builder cannot
    /// know which of them arrived. The value is what must survive.
    #[test]
    fn every_cs_code_is_value_stable_through_a_rebuild() {
        // Parse one wire trailer, build the value back, parse that
        // again: did the value hold, and did the bytes?
        let cycle = |wire: [u8; 3]| -> (bool, bool) {
            let (cs, t) = parse_cs(&wire, 0).expect("a well-formed csT trailer");
            let built = build_cs(cs, t).expect("a parsed csT must rebuild");
            let (again, _) = parse_cs(&built, 0).expect("a built csT must re-parse");
            (again == cs, built == wire)
        };

        let plain = CompressionType::default().to_byte();
        let gga = CompressionType {
            nmea_source: NmeaSource::Gga,
            ..CompressionType::default()
        }
        .to_byte();

        // Codes that come back as a different code spelling the same
        // value, per scale. Pinned rather than just counted so that a
        // build which stopped looking for the exact code, and fell back
        // to the nearest power everywhere, cannot pass the value
        // assertions by luck.
        let mut respelled = [0usize; 3];
        for code in 0..=CS_MAX_EXP {
            #[allow(clippy::cast_possible_truncation)]
            let s = BASE91_OFFSET + code as u8;
            // Course/speed, with the course held at its lowest code so
            // that only `s` varies.
            let (held, same) = cycle([BASE91_OFFSET, s, plain]);
            assert!(held, "speed code {code} changed value on rebuild");
            respelled[0] += usize::from(!same);
            // Radio range.
            let (held, same) = cycle([b'{', s, plain]);
            assert!(held, "range code {code} changed value on rebuild");
            respelled[1] += usize::from(!same);
        }
        // The scale that was broken. Code 2951 is the worked example in
        // `exponent_for`'s documentation: 363 feet, which used to
        // rebuild as code 2950 and read back as 362.
        for code in 0..=ALT_MAX_EXP {
            #[allow(clippy::cast_possible_truncation)]
            let wire = [
                BASE91_OFFSET + (code / 91) as u8,
                BASE91_OFFSET + (code % 91) as u8,
                gga,
            ];
            let (held, same) = cycle(wire);
            assert!(held, "altitude code {code} changed value on rebuild");
            respelled[2] += usize::from(!same);
        }
        assert_eq!(
            respelled,
            [21, 13, 2612],
            "codes rebuilt as a different code of the same value, by \
             scale: speed of 91, range of 91, altitude of 8281. Before \
             the repair the altitude figure was 2978, of which 999 came \
             back as a different VALUE rather than a different spelling"
        );
    }

    #[test]
    fn parse_rejections() {
        assert_eq!(
            Position::parse(b"!49x3.50N/07201.75W-"),
            Err(AprsError::BadDigit {
                got: b'x',
                position: 3
            })
        );
        assert_eq!(
            Position::parse(b"!4903,50N/07201.75W-"),
            Err(AprsError::ExpectedByte {
                expected: b'.',
                got: b',',
                position: 5
            })
        );
        assert_eq!(
            Position::parse(b"!4903.50Q/07201.75W-"),
            Err(AprsError::BadHemisphere { got: b'Q' })
        );
        assert_eq!(
            Position::parse(b"!9903.50N/07201.75W-"),
            Err(AprsError::BadLatitude {
                got: (99 * 6000 + 350) * UNITS_PER_HUNDREDTH_MINUTE
            })
        );
        assert_eq!(
            Position::parse(b"!4963.50N/07201.75W-"),
            Err(AprsError::BadLatitude {
                got: (49 * 6000 + 63 * 100 + 50) * UNITS_PER_HUNDREDTH_MINUTE
            })
        );
        assert_eq!(
            Position::parse(b"!4903.50N/18201.75E-"),
            Err(AprsError::BadLongitude {
                got: (182 * 6000 + 175) * UNITS_PER_HUNDREDTH_MINUTE
            })
        );
        assert_eq!(
            Position::parse(b"!4903.50N~07201.75W-"),
            Err(AprsError::BadSymbolTable { got: b'~' })
        );
        assert_eq!(
            Position::parse(b"!4903.50N"),
            Err(AprsError::Truncated {
                expected: Position::UNCOMPRESSED_LEN,
                got: 9
            })
        );
        assert_eq!(
            Position::parse(b"!/5L!!<*e7>"),
            Err(AprsError::Truncated {
                expected: Position::COMPRESSED_LEN,
                got: 11
            })
        );
        assert_eq!(
            Position::parse(b"!/5L !<*e7>abc"),
            Err(AprsError::BadBase91 {
                got: b' ',
                position: 4
            })
        );
        assert_eq!(
            Position::parse(b"!"),
            Err(AprsError::Truncated {
                expected: 2,
                got: 1
            })
        );
    }

    #[test]
    fn build_overflow() {
        let pos = Position {
            ambiguity: Ambiguity::EXACT,
            latitude: lat(0),
            longitude: lon(0),
            symbol: Symbol::HOUSE,
            messaging: false,
            compressed: false,
            extension: None,
            comment: b"c",
        };
        let mut buf = [0u8; 8];
        assert_eq!(
            pos.build(&mut buf),
            Err(AprsError::BufferTooSmall { needed: 21, max: 8 })
        );
    }
}
