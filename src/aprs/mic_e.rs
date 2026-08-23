//! Mic-E compressed position reports (APRS 1.01 chapter 10).
//!
//! Mic-E packs a full position report into the AX.25 **destination
//! address** (latitude digits, N/S, longitude offset, W/E and the three
//! message bits) plus a nine-byte-minimum **information field**
//! (longitude, speed/course, symbol, then an optional base-91 altitude
//! and free status text). Because the destination carries half the
//! data, decoding needs both fields: use [`decode`] with the six
//! destination characters and the information field, or
//! [`decode_address`] when you already hold a parsed
//! [`crate::ax25::Address`]. Plain [`super::AprsPacket::parse`] rejects
//! the Mic-E data-type identifiers `` ` `` and `'` with
//! [`super::AprsError::InvalidDataType`], because an information field
//! alone cannot carry a Mic-E report; the frame-level entry point that
//! *can* is [`super::Decoded::decode_frame`].
//!
//! Everything here is `no_std`, allocation-free and integer-only:
//! [`MicE::encode`] serializes into caller-provided buffers, [`decode`]
//! borrows the status text from the input slice.

use core::fmt;

use super::extension::{CommentTelemetry, Dao, comment_telemetry, dao};
use super::symbol::Symbol;
use crate::ax25::Address;
use crate::geo::{
    Ambiguity, Coordinates, Latitude, Longitude, UNITS_PER_DEGREE, UNITS_PER_HUNDREDTH_MINUTE,
    UNITS_PER_MINUTE,
};
/// Altitude offset in meters (chapter 10: altitude is offset by 10 km).
const ALTITUDE_OFFSET: i32 = 10_000;

/// Which Mic-E data-type identifier a packet used.
///
/// Chapter 10 defines `` ` `` (0x60) for current GPS data and `'`
/// (0x27) for old (previous-beacon) GPS data. Both encode identically;
/// the identifier is preserved so a round trip is byte-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicEFix {
    /// Data-type identifier `` ` ``: current GPS data.
    Current,
    /// Data-type identifier `'`: old (previous-beacon) GPS data.
    Old,
}

impl MicEFix {
    /// The data-type identifier byte for this fix kind.
    #[must_use]
    pub const fn type_byte(self) -> u8 {
        match self {
            MicEFix::Current => b'`',
            MicEFix::Old => b'\'',
        }
    }
}

/// The Mic-E message type carried by the destination's A/B/C bits.
///
/// Chapter 10 defines two parallel sets selected by the *character set*
/// of the destination's first three columns: the **standard** set
/// (columns `P`-`Z`) and the **custom** set (columns `A`-`K`). The
/// all-zero bit pattern is `Emergency` in both sets (a zero bit is a
/// plain digit, so the sets are indistinguishable there); consequently
/// there are seven custom types, `Custom0`..=`Custom6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicEMessage {
    /// Standard M0 (bits 111): off duty.
    OffDuty,
    /// Standard M1 (bits 110): en route.
    EnRoute,
    /// Standard M2 (bits 101): in service.
    InService,
    /// Standard M3 (bits 100): returning.
    Returning,
    /// Standard M4 (bits 011): committed.
    Committed,
    /// Standard M5 (bits 010): special.
    Special,
    /// Standard M6 (bits 001): priority.
    Priority,
    /// Bits 000 in either set: emergency.
    Emergency,
    /// Custom C0 (bits 111).
    Custom0,
    /// Custom C1 (bits 110).
    Custom1,
    /// Custom C2 (bits 101).
    Custom2,
    /// Custom C3 (bits 100).
    Custom3,
    /// Custom C4 (bits 011).
    Custom4,
    /// Custom C5 (bits 010).
    Custom5,
    /// Custom C6 (bits 001).
    Custom6,
}

impl MicEMessage {
    /// The three message bits `A B C` (bit 2 = A) and whether the
    /// custom character set carries them.
    ///
    /// `Emergency` is bits `000`; a zero bit is encoded as a plain
    /// digit, so its `custom` flag is reported as `false` but never
    /// reaches the wire.
    #[must_use]
    pub const fn bits(self) -> (u8, bool) {
        match self {
            MicEMessage::OffDuty => (0b111, false),
            MicEMessage::EnRoute => (0b110, false),
            MicEMessage::InService => (0b101, false),
            MicEMessage::Returning => (0b100, false),
            MicEMessage::Committed => (0b011, false),
            MicEMessage::Special => (0b010, false),
            MicEMessage::Priority => (0b001, false),
            MicEMessage::Emergency => (0b000, false),
            MicEMessage::Custom0 => (0b111, true),
            MicEMessage::Custom1 => (0b110, true),
            MicEMessage::Custom2 => (0b101, true),
            MicEMessage::Custom3 => (0b100, true),
            MicEMessage::Custom4 => (0b011, true),
            MicEMessage::Custom5 => (0b010, true),
            MicEMessage::Custom6 => (0b001, true),
        }
    }

    /// Reconstructs a message type from bits and character set.
    const fn from_bits(bits: u8, custom: bool) -> Self {
        match (bits & 0b111, custom) {
            (0b111, false) => MicEMessage::OffDuty,
            (0b110, false) => MicEMessage::EnRoute,
            (0b101, false) => MicEMessage::InService,
            (0b100, false) => MicEMessage::Returning,
            (0b011, false) => MicEMessage::Committed,
            (0b010, false) => MicEMessage::Special,
            (0b001, false) => MicEMessage::Priority,
            (0b111, true) => MicEMessage::Custom0,
            (0b110, true) => MicEMessage::Custom1,
            (0b101, true) => MicEMessage::Custom2,
            (0b100, true) => MicEMessage::Custom3,
            (0b011, true) => MicEMessage::Custom4,
            (0b010, true) => MicEMessage::Custom5,
            (0b001, true) => MicEMessage::Custom6,
            _ => MicEMessage::Emergency,
        }
    }
}

/// A Mic-E protocol violation: an invalid field value on encode, or a
/// malformed destination/information field on decode.
///
/// Every variant carries the offending byte or value together with the
/// rule it violated.
///
/// Marked `#[non_exhaustive]` for the same reason as
/// [`super::AprsError`]: Mic-E has spellings this crate does not read
/// yet, and adding a rejection reason should not be a breaking release.
/// Match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MicEError {
    /// The destination field was not exactly six characters.
    BadDestLength {
        /// The rejected length in bytes.
        got: usize,
    },
    /// A destination character is outside the set chapter 10 allows
    /// for its column (`0`-`9`, `A`-`K`, `L`, `P`-`Z` in columns 1-3;
    /// `0`-`9`, `L`, `P`-`Z` in columns 4-6).
    BadDestChar {
        /// The rejected byte.
        got: u8,
        /// Zero-based column of the rejected byte within the
        /// destination field.
        column: usize,
    },
    /// The three message-bit columns mixed the standard (`P`-`Z`) and
    /// custom (`A`-`K`) character sets.
    MixedMessageBits {
        /// The three destination bytes carrying the message bits.
        got: [u8; 3],
    },
    /// An ambiguity space was followed by a non-space latitude digit;
    /// ambiguity must be a contiguous suffix.
    NonTrailingAmbiguity {
        /// Zero-based column of the offending non-space digit.
        column: usize,
    },
    /// The information field ended before a required field was
    /// complete.
    Truncated {
        /// The minimum length in bytes the format requires.
        expected: usize,
        /// The length in bytes available.
        got: usize,
    },
    /// The data-type identifier (first info byte) was neither `` ` ``
    /// nor `'`.
    InvalidDataType {
        /// The rejected identifier byte.
        got: u8,
    },
    /// A longitude byte decoded outside its legal range.
    BadLongitudeByte {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information
        /// field.
        position: usize,
    },
    /// A speed/course byte was below the +28 encoding floor.
    BadSpeedCourseByte {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information
        /// field.
        position: usize,
    },
    /// An altitude character (before `}`) was outside the base-91
    /// alphabet `!`..=`{`.
    BadAltitudeChar {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information
        /// field.
        position: usize,
    },
    /// An encode altitude was outside the three-character base-91
    /// range `-10_000..=743_570` meters.
    BadAltitude {
        /// The rejected altitude in meters.
        got: i32,
    },
    /// A device-identifier prefix was not one of the four bytes
    /// chapter 10 lists (`>`, `]`, `` ` ``, `'`).
    BadDevicePrefix {
        /// The rejected byte.
        got: u8,
    },
    /// A latitude was out of range (minutes 60 or more, or magnitude
    /// beyond 90 degrees).
    BadLatitude {
        /// The rejected value in signed 1/100 arc-minutes.
        got: i64,
    },
    /// A longitude was out of range: Mic-E encodes at most
    /// 179 degrees 59.99 minutes.
    BadLongitude {
        /// The rejected value in signed 1/100 arc-minutes.
        got: i64,
    },
    /// An encode speed exceeded the 799-knot Mic-E maximum.
    BadSpeed {
        /// The rejected speed in knots.
        got: u16,
    },
    /// An encode course exceeded 360 degrees.
    BadCourse {
        /// The rejected course in degrees.
        got: u16,
    },
    /// A position ambiguity level exceeded 4.
    BadAmbiguity {
        /// The rejected ambiguity level.
        got: u8,
    },
    /// The symbol table identifier was not `/`, `\` or an overlay
    /// character.
    BadSymbolTable {
        /// The rejected byte.
        got: u8,
    },
    /// A caller-provided output buffer cannot hold the serialized
    /// field.
    BufferTooSmall {
        /// The required length in bytes.
        needed: usize,
        /// The buffer capacity in bytes.
        max: usize,
    },
}

impl fmt::Display for MicEError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            MicEError::BadDestLength { got } => write!(
                f,
                "destination field of {got} bytes is invalid: exactly 6 characters are required"
            ),
            MicEError::BadDestChar { got, column } => write!(
                f,
                "destination byte 0x{got:02X} in column {column} is outside the Mic-E alphabet"
            ),
            MicEError::MixedMessageBits { got } => write!(
                f,
                "message-bit columns {:02X} {:02X} {:02X} mix the standard and custom sets",
                got[0], got[1], got[2]
            ),
            MicEError::NonTrailingAmbiguity { column } => write!(
                f,
                "latitude digit in column {column} follows an ambiguity space: ambiguity must be a contiguous suffix"
            ),
            MicEError::Truncated { expected, got } => write!(
                f,
                "information field of {got} bytes is truncated: at least {expected} bytes are required"
            ),
            MicEError::InvalidDataType { got } => write!(
                f,
                "data-type identifier 0x{got:02X} is not Mic-E: 0x60 '`' or 0x27 '\\'' is required"
            ),
            MicEError::BadLongitudeByte { got, position } => write!(
                f,
                "longitude byte 0x{got:02X} at offset {position} decodes outside its legal range"
            ),
            MicEError::BadSpeedCourseByte { got, position } => write!(
                f,
                "speed/course byte 0x{got:02X} at offset {position} is below the +28 encoding floor"
            ),
            MicEError::BadAltitudeChar { got, position } => write!(
                f,
                "altitude byte 0x{got:02X} at offset {position} is outside the base-91 alphabet '!'..='{{'"
            ),
            MicEError::BadAltitude { got } => write!(
                f,
                "altitude of {got} meters is out of range: -10000..=743570 fits three base-91 characters"
            ),
            MicEError::BadDevicePrefix { got } => write!(
                f,
                "device-identifier prefix 0x{got:02X} is invalid: must be '>', ']', '`' or '\\''"
            ),
            MicEError::BadLatitude { got } => write!(
                f,
                "latitude of {got} 1/100 arc-minutes is out of range: must be within \u{b1}90\u{b0} with minutes below 60"
            ),
            MicEError::BadLongitude { got } => write!(
                f,
                "longitude of {got} 1/100 arc-minutes is out of Mic-E range: at most 179\u{b0} 59.99' with minutes below 60"
            ),
            MicEError::BadSpeed { got } => write!(
                f,
                "speed of {got} knots is out of range: Mic-E encodes at most 799 knots"
            ),
            MicEError::BadCourse { got } => write!(
                f,
                "course of {got} degrees is out of range: at most 360 degrees"
            ),
            MicEError::BadAmbiguity { got } => write!(
                f,
                "position ambiguity of {got} digits is out of range: at most 4"
            ),
            MicEError::BadSymbolTable { got } => write!(
                f,
                "symbol table byte 0x{got:02X} is invalid: must be '/', '\\' or an overlay character"
            ),
            MicEError::BufferTooSmall { needed, max } => write!(
                f,
                "field of {needed} bytes does not fit: the buffer holds at most {max} bytes"
            ),
        }
    }
}

impl core::error::Error for MicEError {}

/// A parsed or to-be-built Mic-E report.
///
/// Parsed reports borrow their status text from the information field.
///
/// # Common path: one line, valid by construction
///
/// ```
/// use warble::aprs::{
///     Latitude, LatitudeHemisphere, Longitude, LongitudeHemisphere, MicE, MicEError,
///     MicEMessage, Symbol,
/// };
///
/// // Built on the 1/100 arc-minute grid, which is what Mic-E carries.
/// // `from_degrees` would place it a unit or two off that grid and the
/// // round trip below would come back a few centimetres away.
/// let report = MicE::new(
///     Latitude::from_degrees_minutes(33, 2564, LatitudeHemisphere::North)
///         .map_err(|_| MicEError::BadLatitude { got: 0 })?,
///     Longitude::from_degrees_minutes(112, 700, LongitudeHemisphere::West)
///         .map_err(|_| MicEError::BadLongitude { got: 0 })?,
///     20,  // knots
///     251, // degrees
///     Symbol::CAR,
///     MicEMessage::InService,
/// )?
/// .with_status(b"hello");
/// // `dest` is exactly 6: Mic-E encodes the latitude into the six
/// // characters of the AX.25 destination callsign, so this length is
/// // structural, not a guess. `info` is just ample — the encoder
/// // returns `MicEError::BufferTooSmall { needed, .. }` rather than
/// // truncating, so an under-sized buffer is a caught error.
/// let mut dest = [0u8; 6];
/// let mut info = [0u8; 32];
/// let len = report.encode(&mut dest, &mut info)?;
/// let decoded = warble::aprs::mic_e::decode(&dest, &info[..len])?;
/// assert_eq!(decoded, report);
/// assert_eq!(len, 9 + b"hello".len());
/// # Ok::<(), MicEError>(())
/// ```
///
/// # Power user: fully typed, exhaustively matched
///
/// ```
/// use warble::aprs::{
///     Latitude, Longitude, MicE, MicEError, MicEFix, MicEMessage, Symbol, SymbolCode,
///     SymbolTable,
/// };
///
/// let report = MicE::new(
///     Latitude::new(0).map_err(|_| MicEError::BadLatitude { got: 0 })?,
///     Longitude::new(0).map_err(|_| MicEError::BadLongitude { got: 0 })?,
///     0,
///     0,
///     Symbol::primary(SymbolCode::new(b'j').map_err(|_| MicEError::BadSymbolTable { got: 0 })?),
///     MicEMessage::Custom2,
/// )?
/// .with_fix(MicEFix::Old)
/// .with_altitude(Some(61));
/// match report.symbol.table() {
///     Some(SymbolTable::Primary) => {}
///     Some(SymbolTable::Alternate | SymbolTable::Overlay(_)) | None => unreachable!(),
/// }
/// # Ok::<(), MicEError>(())
/// ```
///
/// # Raw hatch: out-of-spec symbols are held, rejected only on encode
///
/// ```
/// use warble::aprs::{Latitude, Longitude, MicE, MicEError, MicEMessage, Symbol};
///
/// // '~' is no Mic-E table selector; new() rejects it up front, but
/// // the struct-literal escape hatch can still hold it (decode does
/// // the same for weird traffic) and encode rejects it late.
/// let lat = Latitude::new(0).map_err(|_| MicEError::BadLatitude { got: 0 })?;
/// let lon = Longitude::new(0).map_err(|_| MicEError::BadLongitude { got: 0 })?;
/// assert_eq!(
///     MicE::new(lat, lon, 0, 0, Symbol::from_wire(b'~', b'$'), MicEMessage::OffDuty),
///     Err(MicEError::BadSymbolTable { got: b'~' })
/// );
/// # Ok::<(), MicEError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicE<'a> {
    /// The position latitude. Decoding reads ambiguous (space) digits
    /// as zero and reports the blanked count in `ambiguity`.
    pub latitude: Latitude,
    /// The position longitude (at most 179 degrees 59.99 minutes in
    /// magnitude; Mic-E cannot express 180 degrees).
    pub longitude: Longitude,
    /// Speed over ground in knots, `0..=799`.
    ///
    /// The whole range is on-air encodable in 7-bit ASCII, which is not
    /// obvious from chapter 10's SP+28 table: the two-column region
    /// (`tens + 108` or `tens + 28`) stops at 199 knots, and only the
    /// single `tens + 28` column continues to 799. It is the `+ 108`
    /// spelling that runs out of ASCII at 200 knots, not the speed;
    /// [`MicE::encode_info`] switches columns there.
    pub speed: u16,
    /// Course over ground in degrees, `0..=360`.
    ///
    /// 0 degrees means unknown or indefinite and 360 means due north,
    /// per chapter 10; the two are distinct values and both encode.
    pub course: u16,
    /// The APRS display symbol (table + code). Mic-E only encodes
    /// tables `/`, `\` or an uppercase/digit overlay; anything else
    /// (held via the raw hatch [`Symbol::from_wire`]) is rejected on
    /// encode with [`MicEError::BadSymbolTable`].
    pub symbol: Symbol,
    /// The message type carried by the destination's A/B/C bits.
    pub message: MicEMessage,
    /// Which data-type identifier the report uses.
    pub fix: MicEFix,
    /// Altitude in meters above the -10 km datum, when present.
    pub altitude: Option<i32>,
    /// The device-identifier prefix byte standing in front of the
    /// altitude field, when there is one.
    ///
    /// Chapter 10 places the optional altitude "first after the Mic-E
    /// type byte", then adds: *"Note that this comes after any device
    /// identifier prefix character."* The early Kenwood radios insert
    /// `>` (TH-D7) or `]` (TM-D700) at the front of the status text,
    /// and other manufacturers use `` ` `` (messaging capable) or `'`
    /// (not); the spec's own worked example lists `"4T}`, `>"4T}` and
    /// `]"4T}` as the three spellings of the same altitude.
    ///
    /// Holding the byte here is what lets [`MicE::altitude`] see past
    /// it while [`MicE::encode`] stays byte-exact. MEASURED: 68% of
    /// the Mic-E frames in the corpus put their altitude behind such a
    /// prefix, so a decoder that does not skip it loses two altitudes
    /// in three — which is exactly what this crate did until the
    /// field-level differential compared altitudes with an independent
    /// decoder.
    ///
    /// The prefix is read *independently* of the altitude, because
    /// chapter 10 makes the two optional separately and shows a prefix
    /// with no altitude behind it in its own Maidenhead examples,
    /// `>IO91SX/G Helloworld` and `]IO91SX/G Helloworld`. MEASURED: 35
    /// corpus frames are exactly that shape (`]Stopped`, `]`,
    /// `]Palomar REACT Digi`), and `AE6GR-7` transmits both spellings
    /// from the same radio in the same session — `]"6[}` with an
    /// altitude and an empty status, `]Stopped` with neither.
    ///
    /// For `` ` `` and `'` the reading is safe: chapter 10 forbids
    /// status text from starting with either, or with 0x1d, since it
    /// would be confused with obsolete telemetry. For `>` and `]`
    /// there is no such prohibition, so this is a **conjecture** — but
    /// it is the conjecture the specification instructs applications
    /// to make (*"APRS display applications should remove any extra
    /// prefix and suffix before displaying the text"*), and the
    /// reference decoder strips a leading `>` or `]` unconditionally
    /// — for `]Stopped` it names the TM-D700 and displays `Stopped`,
    /// with no altitude anywhere in the frame. Nothing is lost
    /// when it guesses wrong: the byte is held here rather than
    /// dropped, [`MicE::encode`] puts it straight back, and a caller
    /// that disagrees can re-prepend it. The cost of a wrong guess is
    /// bounded at exactly one byte of status text.
    pub device_prefix: Option<u8>,
    /// Position ambiguity: how many trailing latitude digits are
    /// blanked, `0..=4`.
    pub ambiguity: u8,
    /// Free-form status text following the fixed fields (and the
    /// device prefix and altitude, when present).
    pub status: &'a [u8],
}

/// Splits an absolute coordinate in storage units into
/// (degrees, minutes, hundredths of a minute).
///
/// Mic-E carries hundredths and the storage unit is finer, so the
/// magnitude is rounded to whole hundredths once, up front, and split
/// from there. Rounding the total rather than each field is what stops
/// a value that carries into a full 60 minutes from spelling itself as
/// 59 minutes and 100 hundredths.
const fn split_dmh(abs: i64) -> (i64, i64, i64) {
    let step = UNITS_PER_HUNDREDTH_MINUTE;
    let hundredths = (abs + step / 2) / step;
    (hundredths / 6000, hundredths / 100 % 60, hundredths % 100)
}

impl<'a> MicE<'a> {
    /// Creates a current-fix Mic-E report with no altitude, no
    /// ambiguity and empty status, validating every field up front so
    /// [`MicE::encode`] cannot fail except on a short buffer. Use the
    /// `with_*` methods to adjust the optional parts.
    ///
    /// # Errors
    ///
    /// [`MicEError::BadLongitude`] at 180 degrees magnitude,
    /// [`MicEError::BadSpeed`] above 799 knots,
    /// [`MicEError::BadCourse`] above 360 degrees and
    /// [`MicEError::BadSymbolTable`] on a table identifier Mic-E
    /// cannot carry.
    pub fn new(
        latitude: Latitude,
        longitude: Longitude,
        speed: u16,
        course: u16,
        symbol: Symbol,
        message: MicEMessage,
    ) -> Result<Self, MicEError> {
        let lon = longitude.units();
        if lon.unsigned_abs() as i64 / UNITS_PER_DEGREE > 179 {
            return Err(MicEError::BadLongitude { got: lon });
        }
        if speed > 799 {
            return Err(MicEError::BadSpeed { got: speed });
        }
        if course > 360 {
            return Err(MicEError::BadCourse { got: course });
        }
        check_symbol_table(symbol.to_wire().0)?;
        Ok(Self {
            latitude,
            longitude,
            speed,
            course,
            symbol,
            message,
            fix: MicEFix::Current,
            altitude: None,
            device_prefix: None,
            ambiguity: 0,
            status: b"",
        })
    }

    /// Returns the report with the given free-form status text.
    #[must_use]
    pub const fn with_status(self, status: &'a [u8]) -> Self {
        Self { status, ..self }
    }

    /// Returns the report with the given fix kind (data-type
    /// identifier).
    #[must_use]
    pub const fn with_fix(self, fix: MicEFix) -> Self {
        Self { fix, ..self }
    }

    /// Returns the report with the given altitude in meters (or
    /// `None` for no altitude field).
    #[must_use]
    pub const fn with_altitude(self, altitude: Option<i32>) -> Self {
        Self { altitude, ..self }
    }

    /// Returns the report with the given device-identifier prefix in
    /// front of the altitude field.
    ///
    /// See [`MicE::device_prefix`]. Transmitters that are not
    /// impersonating a particular radio should leave this `None`; it
    /// exists so a received report re-encodes to the bytes it arrived
    /// as.
    ///
    /// # Errors
    ///
    /// [`MicEError::BadDevicePrefix`] for any byte other than the four
    /// the specification lists.
    pub const fn with_device_prefix(self, device_prefix: Option<u8>) -> Result<Self, MicEError> {
        if let Some(byte) = device_prefix
            && !is_device_prefix(byte)
        {
            return Err(MicEError::BadDevicePrefix { got: byte });
        }
        Ok(Self {
            device_prefix,
            ..self
        })
    }

    /// Returns the report with the given position ambiguity,
    /// validated to at most four blanked digits.
    ///
    /// # Errors
    ///
    /// [`MicEError::BadAmbiguity`] above 4.
    pub const fn with_ambiguity(self, ambiguity: u8) -> Result<Self, MicEError> {
        if ambiguity > 4 {
            return Err(MicEError::BadAmbiguity { got: ambiguity });
        }
        Ok(Self { ambiguity, ..self })
    }

    /// The position, pairing the `latitude` and `longitude` fields so
    /// call sites need not rely on tuple ordering, **masked to the
    /// precision the sender declared**.
    ///
    /// The declared [`MicE::ambiguity`] is carried onto
    /// [`Coordinates::ambiguity`], so a caller holding only the
    /// returned pair still learns that digits were masked rather than
    /// being told a blurred position is exact.
    ///
    /// # Why this masks and the fields do not
    ///
    /// Mic-E spells ambiguity in the destination address, and the
    /// latitude digits it covers arrive as zeros, so `latitude` already
    /// reads at the declared precision. **The longitude does not.**
    /// Mic-E has no way to blank a longitude digit, so the longitude is
    /// always transmitted at full precision and chapter 10 makes
    /// discarding the matching digits the receiver's job:
    ///
    /// > The position ambiguity is specified for the latitude (in the
    /// > destination address). The same degree of ambiguity will then
    /// > also apply to the longitude. For example, if the destination
    /// > address is `T4SQZZ`, the last two digits of the latitude are
    /// > ambiguous (represented by `ZZ`). Then, if the longitude data in
    /// > the Information field is `(_f`, as in the above example, the
    /// > last two digits of the computed longitude will be ignored —
    /// > that is, the longitude will be 112 degrees 7 minutes.
    ///
    /// A decoder that reports the transmitted longitude answers 112
    /// degrees 7.74 minutes to that example, which is 1.4 km more
    /// precise than the sender claimed, and nothing in the position
    /// reveals it. That is the whole reason this accessor exists rather
    /// than callers reading the two fields.
    ///
    /// The fields themselves keep the wire, so that [`MicE::encode`]
    /// remains the exact inverse of [`decode`]: masking at parse would
    /// zero longitude bytes the sender did transmit and break the
    /// byte-for-byte round trip on every ambiguous frame.
    ///
    /// `ambiguity` is a public `u8`, so a struct literal can hold a count
    /// above the four digits the wire format has room for (both
    /// [`MicE::with_ambiguity`] and [`decode`] reject those, and
    /// [`MicE::encode`] fails with [`MicEError::BadAmbiguity`]). This
    /// accessor never panics: an out-of-range count **saturates at four**
    /// masked digits. Saturating errs toward less precision, whereas
    /// falling back to [`Ambiguity::EXACT`] would answer "exact" for a
    /// report that explicitly declared it is not — the one answer already
    /// known to be wrong.
    #[must_use]
    pub fn coordinates(&self) -> Coordinates {
        let digits = if self.ambiguity > 4 {
            4
        } else {
            self.ambiguity
        };
        let ambiguity = match Ambiguity::new(digits) {
            Ok(value) => value,
            // Unreachable: `digits` is clamped to 4 just above, which
            // `Ambiguity::new` accepts. A `match` rather than an unwrap
            // because `Result::expect` is not callable in a `const fn`,
            // and because this accessor must not panic.
            Err(_) => Ambiguity::EXACT,
        };
        // Both axes, not just the longitude. The latitude's masked
        // digits already read as zero, so masking it is the identity
        // and costs nothing; applying one rule to the pair is what
        // keeps the two axes from drifting apart if either side of the
        // decode changes.
        let mut lat_units = ambiguity.mask(self.latitude.units());
        let mut lon_units = ambiguity.mask(self.longitude.units());
        // A `!DAO!` in the status text refines the position the other
        // way, for the same reason ambiguity coarsens it: the
        // declaration lives in a different wire slot from the value.
        // Ambiguity wins when both appear, because a station cannot
        // coherently blank digits and refine them at once.
        if ambiguity == Ambiguity::EXACT
            && let Some(refinement) = dao(self.status)
        {
            lat_units += lat_units.signum() * refinement.latitude_units;
            lon_units += lon_units.signum() * refinement.longitude_units;
        }
        let latitude = match Latitude::new(lat_units) {
            Ok(value) => value,
            // Unreachable: masking only reduces a magnitude, a DAO
            // addend is under a hundredth of a minute, and
            // `self.latitude` is already in range.
            Err(_) => self.latitude,
        };
        let longitude = match Longitude::new(lon_units) {
            Ok(value) => value,
            // Unreachable, for the same reason.
            Err(_) => self.longitude,
        };
        Coordinates::new(latitude, longitude).with_ambiguity(ambiguity)
    }

    /// Base-91 comment telemetry carried in the status text.
    ///
    /// Chapter 13 allows the block in Mic-E as well as the two
    /// uncompressed forms. A view, so `build` reproduces the bytes.
    #[must_use]
    pub fn comment_telemetry(&self) -> Option<CommentTelemetry> {
        comment_telemetry(self.status)
    }

    /// The `!DAO!` datum and added precision from the status text.
    ///
    /// [`coordinates`](Self::coordinates) has already applied the
    /// precision. This exposes the datum byte, which it cannot.
    #[must_use]
    pub fn dao(&self) -> Option<Dao> {
        dao(self.status)
    }

    /// Serializes the six-character destination field into `dest` and
    /// the information field into `info`, returning the information
    /// field length (`dest` is always exactly six bytes).
    ///
    /// # Errors
    ///
    /// [`MicEError::BadLongitude`] at 180 degrees magnitude,
    /// [`MicEError::BadSpeed`] above 799 knots, [`MicEError::BadCourse`]
    /// above 360 degrees, [`MicEError::BadAmbiguity`] above 4,
    /// [`MicEError::BadSymbolTable`] on a bad table identifier, and
    /// [`MicEError::BufferTooSmall`] when either buffer is short.
    pub fn encode(&self, dest: &mut [u8], info: &mut [u8]) -> Result<usize, MicEError> {
        self.encode_destination(dest)?;
        self.encode_info(info)
    }

    /// Serializes only the six-character destination field.
    ///
    /// # Errors
    ///
    /// As [`MicE::encode`], destination-side variants only.
    pub fn encode_destination(&self, dest: &mut [u8]) -> Result<(), MicEError> {
        if dest.len() < 6 {
            return Err(MicEError::BufferTooSmall {
                needed: 6,
                max: dest.len(),
            });
        }
        if self.ambiguity > 4 {
            return Err(MicEError::BadAmbiguity {
                got: self.ambiguity,
            });
        }
        let lat = self.latitude.units();
        let north = lat >= 0;
        let (deg, min, hund) = split_dmh(lat.unsigned_abs() as i64);
        let digits = [deg / 10, deg % 10, min / 10, min % 10, hund / 10, hund % 10];
        let lon_west = self.longitude.units() < 0;
        let lon_deg = self.longitude.units().unsigned_abs() as i64 / UNITS_PER_DEGREE;
        let lon_offset = !(10..=99).contains(&lon_deg);
        let (bits, custom) = self.message.bits();
        for (col, out) in dest.iter_mut().enumerate().take(6) {
            let blank = col >= 6 - self.ambiguity as usize;
            // Truncation is impossible: every digit is 0..=9.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let digit = digits[col] as u8;
            let one = match col {
                0 => bits & 0b100 != 0,
                1 => bits & 0b010 != 0,
                2 => bits & 0b001 != 0,
                3 => north,
                4 => lon_offset,
                _ => lon_west,
            };
            *out = dest_char(digit, blank, one, col < 3 && custom);
        }
        Ok(())
    }

    /// Serializes only the information field, returning its length.
    ///
    /// # Errors
    ///
    /// As [`MicE::encode`], information-side variants only.
    pub fn encode_info(&self, info: &mut [u8]) -> Result<usize, MicEError> {
        let lon = self.longitude.units();
        let (deg, min, hund) = split_dmh(lon.unsigned_abs() as i64);
        if deg > 179 {
            return Err(MicEError::BadLongitude { got: lon });
        }
        if self.speed > 799 {
            return Err(MicEError::BadSpeed { got: self.speed });
        }
        if self.course > 360 {
            return Err(MicEError::BadCourse { got: self.course });
        }
        check_symbol_table(self.symbol.to_wire().0)?;
        if let Some(byte) = self.device_prefix
            && !is_device_prefix(byte)
        {
            return Err(MicEError::BadDevicePrefix { got: byte });
        }
        let needed = 9
            + usize::from(self.device_prefix.is_some())
            + if self.altitude.is_some() { 4 } else { 0 }
            + self.status.len();
        if info.len() < needed {
            return Err(MicEError::BufferTooSmall {
                needed,
                max: info.len(),
            });
        }
        info[0] = self.fix.type_byte();
        // Longitude degrees, chapter 10 remap (the +100 offset flag
        // lives in destination column 5).
        let d28 = match deg {
            0..=9 => deg + 118,
            10..=99 => deg + 28,
            100..=109 => deg + 8,
            _ => deg - 72,
        };
        // Longitude minutes: +28, with 0-9 shifted up by 60 per spec.
        let m28 = if min <= 9 { min + 88 } else { min + 28 };
        // All arithmetic below stays well within u8 by the range
        // checks above.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            info[1] = d28 as u8;
            info[2] = m28 as u8;
            info[3] = (hund + 28) as u8;
        }
        // Speed, in tens and units of knots. Chapter 10's SP+28 table
        // offers *two* equally valid columns for 0-199 knots, `tens +
        // 108` and `tens + 28`, and a *single* column, `tens + 28`, for
        // 200-799. The "+800 knots" offset that the decoder wraps back
        // off is exactly the `tens + 108` column, so it may only be
        // applied below 200 knots: at 200 knots it emits `20 + 108` =
        // 128, which is neither in the table nor inside 7-bit ASCII, and
        // putting an eight-bit byte in an information field is not ours
        // to do. The unoffset column reaches 799 knots as `k` (107), so
        // every speed in chapter 10's stated 0-799 range is encodable
        // and none of it needs rejecting.
        //
        // Below 200 knots we keep emitting the offset column, which is
        // the byte every release so far has put on the air. Both columns
        // decode to the same speed, here and in the reference (VERIFIED:
        // SP+28 0x7F and 0x2F both adjudicate as 199 knots), so the
        // choice is free -- but it does mean a *received* frame written
        // in the unoffset column below 200 knots re-encodes to the
        // offset one. That is inherent to a format with two spellings
        // and one encoder, not a regression: MEASURED over the corpus,
        // 799 of 894 Mic-E frames use the offset column and 95 the
        // unoffset one, all of them under 200 knots.
        let tens = u32::from(self.speed) / 10;
        let units = u32::from(self.speed) % 10;
        // Course, in hundreds then tens/units of degrees. The "+400
        // degrees" offset is unconditional and safe: for a course of
        // 0-360 it puts the hundreds digit in `4..=7`, which is DC+28's
        // own offset column, never reaches 10 and so never carries into
        // the units-of-knots digit beside it. The widest DC+28 byte it
        // can produce is `9 * 10 + 7 + 28` = 125, and the widest SE+28
        // byte is `99 + 28` = 127 (DEL, which the SE+28 table lists) --
        // so course, unlike speed, had no eight-bit case to fix.
        let cs = u32::from(self.course) + 400;
        #[allow(clippy::cast_possible_truncation)]
        {
            info[4] = (tens + if tens < 20 { 108 } else { 28 }) as u8;
            info[5] = (units * 10 + cs / 100 + 28) as u8;
            info[6] = (cs % 100 + 28) as u8;
        }
        info[7] = self.symbol.to_wire().1;
        info[8] = self.symbol.to_wire().0;
        let mut at = 9;
        // The prefix precedes the altitude field, per chapter 10.
        if let Some(byte) = self.device_prefix {
            info[at] = byte;
            at += 1;
        }
        if let Some(alt) = self.altitude {
            let v = alt
                .checked_add(ALTITUDE_OFFSET)
                .filter(|v| (0..91 * 91 * 91).contains(v))
                .ok_or(MicEError::BadAltitude { got: alt })?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                info[at] = (v / (91 * 91)) as u8 + 33;
                info[at + 1] = (v / 91 % 91) as u8 + 33;
                info[at + 2] = (v % 91) as u8 + 33;
            }
            info[at + 3] = b'}';
            at += 4;
        }
        info[at..at + self.status.len()].copy_from_slice(self.status);
        Ok(needed)
    }
}

/// Validates a symbol table identifier: `/`, `\`, digit or uppercase
/// letter overlay.
const fn check_symbol_table(byte: u8) -> Result<(), MicEError> {
    match byte {
        b'/' | b'\\' | b'0'..=b'9' | b'A'..=b'Z' => Ok(()),
        got => Err(MicEError::BadSymbolTable { got }),
    }
}

/// One decoded destination column: its latitude digit, whether it is
/// an ambiguity space, its overlaid bit, and the character set
/// (`Some(true)` custom, `Some(false)` standard, `None` for a digit
/// or `L` which belongs to neither).
struct DestCol {
    digit: u8,
    blank: bool,
    one: bool,
    custom: Option<bool>,
}

/// Decodes one destination character per the chapter 10 table.
fn dest_col(byte: u8, column: usize) -> Result<DestCol, MicEError> {
    let (digit, blank, one, custom) = match byte {
        b'0'..=b'9' => (byte - b'0', false, false, None),
        b'L' => (0, true, false, None),
        b'P'..=b'Y' => (byte - b'P', false, true, Some(false)),
        b'Z' => (0, true, true, Some(false)),
        b'A'..=b'J' if column < 3 => (byte - b'A', false, true, Some(true)),
        b'K' if column < 3 => (0, true, true, Some(true)),
        got => return Err(MicEError::BadDestChar { got, column }),
    };
    Ok(DestCol {
        digit,
        blank,
        one,
        custom,
    })
}

/// Decodes a Mic-E report from the six destination characters and the
/// information field.
///
/// `dest` is the textual destination callsign (six characters, no
/// SSID). Ambiguous (space) latitude digits decode as zero with the
/// blanked count reported in [`MicE::ambiguity`]. The returned report
/// borrows its status text from `info`.
///
/// # Errors
///
/// [`MicEError::BadDestLength`], [`MicEError::BadDestChar`],
/// [`MicEError::MixedMessageBits`], [`MicEError::NonTrailingAmbiguity`],
/// [`MicEError::BadAmbiguity`] (more than four blanked digits),
/// [`MicEError::Truncated`], [`MicEError::InvalidDataType`],
/// [`MicEError::BadLongitudeByte`], [`MicEError::BadSpeedCourseByte`],
/// [`MicEError::BadSymbolTable`], [`MicEError::BadAltitudeChar`],
/// [`MicEError::BadLatitude`] and [`MicEError::BadLongitude`], each
/// carrying the offending byte or value.
pub fn decode<'a>(dest: &[u8], info: &'a [u8]) -> Result<MicE<'a>, MicEError> {
    if dest.len() != 6 {
        return Err(MicEError::BadDestLength { got: dest.len() });
    }
    let mut digits = [0u8; 6];
    let mut bits = 0u8;
    let mut custom: Option<bool> = None;
    let mut blanks = 0u8;
    let mut cols = [false; 6];
    for (column, &byte) in dest.iter().enumerate() {
        let col = dest_col(byte, column)?;
        digits[column] = col.digit;
        cols[column] = col.blank;
        if col.blank {
            blanks += 1;
        }
        if column < 3 {
            if col.one {
                bits |= 4 >> column;
            }
            if let Some(set) = col.custom {
                match custom {
                    Some(prev) if prev != set => {
                        return Err(MicEError::MixedMessageBits {
                            got: [dest[0], dest[1], dest[2]],
                        });
                    }
                    _ => custom = Some(set),
                }
            }
        }
    }
    // Ambiguity must be a contiguous suffix of the six digits.
    for column in 1..6 {
        if cols[column - 1] && !cols[column] {
            return Err(MicEError::NonTrailingAmbiguity { column });
        }
    }
    if blanks > 4 {
        return Err(MicEError::BadAmbiguity { got: blanks });
    }
    let north = dest[3] >= b'P';
    let lon_offset = dest[4] >= b'P';
    let west = dest[5] >= b'P';
    let message = MicEMessage::from_bits(bits, custom.unwrap_or(false));
    decode_info(info, digits, north, lon_offset, west, message, blanks)
}

/// Decodes a Mic-E report from a parsed AX.25 destination address and
/// the information field.
///
/// The typed peer of [`decode`], which takes the six destination
/// characters as a byte slice: a received frame already carries an
/// [`Address`], so this spares every caller the space-padding step (the
/// callsign is one to six characters, the Mic-E alphabet is exactly
/// six). Equivalent to `decode(&dest.callsign.as_padded(), info)`.
///
/// **The destination SSID is ignored.** Chapter 10 encodes the whole
/// report in the six callsign characters and nothing in the SSID octet,
/// so `APZ123-5` and `APZ123` decode identically. Said here rather than
/// left to inference, because a silently discarded field is exactly the
/// kind of omission that makes a decoder confidently wrong.
///
/// # Errors
///
/// Exactly those of [`decode`], except [`MicEError::BadDestLength`]:
/// an [`Address`] is padded to six characters by construction, so that
/// variant is unreachable through this entry point.
pub fn decode_address<'a>(dest: Address, info: &'a [u8]) -> Result<MicE<'a>, MicEError> {
    decode(&dest.callsign.as_padded(), info)
}

/// Decodes the information field, combining it with the
/// destination-derived values.
fn decode_info<'a>(
    info: &'a [u8],
    digits: [u8; 6],
    north: bool,
    lon_offset: bool,
    west: bool,
    message: MicEMessage,
    ambiguity: u8,
) -> Result<MicE<'a>, MicEError> {
    if info.len() < 9 {
        return Err(MicEError::Truncated {
            expected: 9,
            got: info.len(),
        });
    }
    let fix = match info[0] {
        b'`' => MicEFix::Current,
        b'\'' => MicEFix::Old,
        got => return Err(MicEError::InvalidDataType { got }),
    };
    // Latitude from the destination digits.
    let lat_deg = i64::from(digits[0]) * 10 + i64::from(digits[1]);
    let lat_min = i64::from(digits[2]) * 10 + i64::from(digits[3]);
    let lat_hund = i64::from(digits[4]) * 10 + i64::from(digits[5]);
    let lat_abs = lat_deg * UNITS_PER_DEGREE
        + lat_min * UNITS_PER_MINUTE
        + lat_hund * UNITS_PER_HUNDREDTH_MINUTE;
    if lat_deg > 90 || lat_min >= 60 || (lat_deg == 90 && (lat_min != 0 || lat_hund != 0)) {
        return Err(MicEError::BadLatitude {
            got: if north { lat_abs } else { -lat_abs },
        });
    }
    let latitude = Latitude::new(if north { lat_abs } else { -lat_abs })
        .map_err(|_| MicEError::BadLatitude { got: lat_abs })?;
    // Longitude degrees: undo the +28 remap, then the +100 offset.
    let d = i32::from(info[1]);
    #[allow(clippy::cast_lossless)]
    let mut lon_deg = i64::from(d) - 28;
    if !(0..=99).contains(&lon_deg) {
        return Err(MicEError::BadLongitudeByte {
            got: info[1],
            position: 1,
        });
    }
    if lon_offset {
        lon_deg += 100;
    }
    if (180..=189).contains(&lon_deg) {
        lon_deg -= 80;
    } else if lon_deg >= 190 {
        lon_deg -= 190;
    }
    // Longitude minutes: undo +28, values 60-69 mean 0-9.
    let mut lon_min = i64::from(info[2]) - 28;
    if !(0..=69).contains(&lon_min) {
        return Err(MicEError::BadLongitudeByte {
            got: info[2],
            position: 2,
        });
    }
    if lon_min >= 60 {
        lon_min -= 60;
    }
    let lon_hund = i64::from(info[3]) - 28;
    if !(0..=99).contains(&lon_hund) {
        return Err(MicEError::BadLongitudeByte {
            got: info[3],
            position: 3,
        });
    }
    let lon_abs = lon_deg * UNITS_PER_DEGREE
        + lon_min * UNITS_PER_MINUTE
        + lon_hund * UNITS_PER_HUNDREDTH_MINUTE;
    let longitude = Longitude::new(if west { -lon_abs } else { lon_abs })
        .map_err(|_| MicEError::BadLongitude { got: lon_abs })?;
    // Speed/course: undo +28, then the +800/+400 wrap.
    let mut sp_dc_se = [0i32; 3];
    for (i, v) in sp_dc_se.iter_mut().enumerate() {
        let raw = i32::from(info[4 + i]) - 28;
        if raw < 0 {
            return Err(MicEError::BadSpeedCourseByte {
                got: info[4 + i],
                position: 4 + i,
            });
        }
        *v = raw;
    }
    let mut speed = sp_dc_se[0] * 10 + sp_dc_se[1] / 10;
    let mut course = (sp_dc_se[1] % 10) * 100 + sp_dc_se[2];
    if speed >= 800 {
        speed -= 800;
    }
    if course >= 400 {
        course -= 400;
    }
    // One subtraction does not bound this to a legal course. `course`
    // is `(DC % 10) * 100 + SE`, so it reaches 999 before the wrap and
    // 599 after it, while chapter 10 gives the field `0..=360`. A
    // comment here used to assert "course < 400 after the wrap", which
    // is false, and the struct field documents `0..=360`, which the
    // decoder was therefore breaking. MEASURED over 205 635 live
    // packets: 5 reports arrive with 466 or 366 degrees.
    //
    // Reported as unknown rather than refused, for the same reason the
    // symbol table byte below is not validated: an impossible course
    // says nothing about whether the position decoded, and chapter 10
    // already spells "unknown or indefinite" as 0. Refusing would throw
    // away a good fix over a field the sender got wrong.
    if course > 360 {
        course = 0;
    }
    // The symbol table identifier is NOT validated here.
    //
    // Mic-E packs position, course, speed and symbol into one field, and
    // an out-of-spec symbol byte says nothing about whether the position
    // decoded correctly. Rejecting the packet over it would discard a
    // perfectly good position report because of a cosmetic field — real
    // traffic carries table bytes outside `/ \ 0-9 A-Z`, and every one
    // of them still yields a valid fix.
    //
    // `Symbol::from_wire` below preserves the raw pair losslessly, so
    // nothing is invented and nothing is lost: `Symbol::table()` simply
    // returns `None` for a byte it cannot name. Encoding still validates
    // (see `MicE::new` and `MicE::encode`), which is where being strict
    // protects someone — we should never transmit a table identifier we
    // would not accept.
    // Cannot truncate: speed < 800 after the wrap, course <= 360 after
    // the range check above.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (speed, course) = (speed as u16, course as u16);
    let tail = split_altitude(&info[9..])?;
    Ok(MicE {
        latitude,
        longitude,
        speed,
        course,
        symbol: Symbol::from_wire(info[8], info[7]),
        message,
        fix,
        altitude: tail.altitude,
        device_prefix: tail.device_prefix,
        ambiguity,
        status: tail.status,
    })
}

/// Whether a byte is one of the four device-identifier prefixes
/// chapter 10 lists: `>` (TH-D7 family), `]` (TM-D700 family),
/// `` ` `` (messaging capable) and `'` (not messaging capable).
///
/// The matching **suffixes** are not implemented. Their shape would be
/// a single `=`, `^` or `&` after `>` or `]`, and a two-character `_x`
/// or `|x` after `` ` `` or `'`. They recover **0** frames from this
/// corpus — it is 2005 traffic and every suffix-bearing radio model
/// postdates it — so there is nothing here to validate them against,
/// and an unvalidated guess at the *end* of status text is far more
/// expensive than one at the start. Chapter 10 also has applications
/// read the device table from a runtime data file, which a `no_std`
/// core cannot do: naming the radio is the caller's job, and this
/// crate's job is to hand back the identifying byte losslessly.
const fn is_device_prefix(byte: u8) -> bool {
    matches!(byte, b'>' | b']' | b'`' | b'\'')
}

/// Whether a slice has room for an `xxx}` altitude field, with the `}`
/// terminator in its fourth byte.
const fn has_altitude_shape(bytes: &[u8]) -> bool {
    matches!(bytes, [_, _, _, b'}', ..])
}

/// Splits an optional device-identifier prefix and an optional base-91
/// altitude (`xxx}`) off the status text.
///
/// Chapter 10 makes the two **independent**. The altitude comes "first
/// after the Mic-E type byte", *after* any device prefix — the spec's
/// worked example gives `"4T}`, `>"4T}` and `]"4T}` as three spellings
/// of one altitude — but the same chapter shows a prefix with no
/// altitude at all, in `>IO91SX/G Helloworld`. Gating the prefix on
/// the altitude left the byte stranded in `status` for 35 corpus
/// frames, all of the form `]Stopped`.
///
/// Three readings are tried in order:
///
/// 1. prefix, then altitude (`]"4T}`);
/// 2. altitude alone (`"4T}`), *including* when its leading base-91
///    digit happens to be one of the four prefix bytes;
/// 3. prefix alone (`]Stopped`), or neither.
///
/// Reading 2 has to outrank reading 3 or this crate's own encoder
/// stops being invertible: `MicE { device_prefix: None, altitude:
/// Some(39_686), .. }` encodes to `'!!}`, and taking that `'` for a
/// device would hand back a report with the altitude thrown away.
/// The bands where the question arises are `'` 39.7-48.0 km,
/// `` ` `` 511.7-520.0 km, `>` 230.1-238.4 km and `]` 486.9-495.1 km:
/// the first is weather-balloon traffic and the rest are not traffic
/// at all. The reference decoder resolves the same tie by consulting
/// its runtime device table first, which gives it the altitude for
/// `'` and `` ` `` (no table entry matches a bare one) and the radio
/// for `>` and `]` (both are prefix-only entries) — so we agree with
/// it exactly where real traffic lives and differ only above 230 km,
/// where we prefer keeping our own encoder invertible.
///
/// Guessing wrong must never reject the frame, so a prefixed form
/// whose three following characters are not base-91 falls through to
/// reading 3, "no altitude, all status". The unprefixed form keeps its
/// stricter reading, where a `}` in the fourth byte is unambiguous
/// enough to make a bad character an error worth reporting.
fn split_altitude(rest: &[u8]) -> Result<AltitudeSplit<'_>, MicEError> {
    let (device_prefix, body) = match rest {
        [first, tail @ ..] if is_device_prefix(*first) => (Some(*first), tail),
        all => (None, all),
    };
    if device_prefix.is_none() {
        // Reading 2, strict: with no prefix byte in play a `}` in the
        // fourth position commits us to an altitude, so a character
        // outside the alphabet is a typed error rather than a shrug.
        if !has_altitude_shape(rest) {
            return Ok(AltitudeSplit::all_status(None, rest));
        }
        let mut value = 0i32;
        for (i, &byte) in rest.iter().enumerate().take(3) {
            if !(b'!'..=b'{').contains(&byte) {
                return Err(MicEError::BadAltitudeChar {
                    got: byte,
                    position: 9 + i,
                });
            }
            value = value * 91 + i32::from(byte - b'!');
        }
        return Ok(AltitudeSplit {
            device_prefix: None,
            altitude: Some(value - ALTITUDE_OFFSET),
            status: &rest[4..],
        });
    }
    // Reading 1: the prefix byte is a prefix and an altitude follows.
    if has_altitude_shape(body)
        && let Some(meters) = base91_altitude(body)
    {
        return Ok(AltitudeSplit {
            device_prefix,
            altitude: Some(meters),
            status: &body[4..],
        });
    }
    // Reading 2, lenient: the prefix-shaped byte is really the first
    // base-91 digit of an unprefixed altitude. Lenient because we are
    // now guessing between two readings and neither may reject the
    // frame over it.
    if has_altitude_shape(rest)
        && let Some(meters) = base91_altitude(rest)
    {
        return Ok(AltitudeSplit {
            device_prefix: None,
            altitude: Some(meters),
            status: &rest[4..],
        });
    }
    // Reading 3: a prefix with no altitude behind it.
    Ok(AltitudeSplit::all_status(device_prefix, body))
}

/// What [`split_altitude`] found at the head of the status text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AltitudeSplit<'a> {
    device_prefix: Option<u8>,
    altitude: Option<i32>,
    status: &'a [u8],
}

impl<'a> AltitudeSplit<'a> {
    /// No altitude field: every remaining byte is status text. The
    /// prefix is passed in because it is found independently of the
    /// altitude, and survives its absence.
    const fn all_status(device_prefix: Option<u8>, status: &'a [u8]) -> Self {
        Self {
            device_prefix,
            altitude: None,
            status,
        }
    }
}

/// Decodes three base-91 altitude characters into meters above mean
/// sea level, or `None` if any of them is outside the alphabet.
fn base91_altitude(body: &[u8]) -> Option<i32> {
    let mut value = 0i32;
    for &byte in body.iter().take(3) {
        if !(b'!'..=b'{').contains(&byte) {
            return None;
        }
        value = value * 91 + i32::from(byte - b'!');
    }
    Some(value - ALTITUDE_OFFSET)
}

/// Encodes one destination character from its digit, ambiguity flag,
/// bit value and (columns 1-3 only) character set.
const fn dest_char(digit: u8, blank: bool, one: bool, custom: bool) -> u8 {
    match (one, blank, custom) {
        (false, false, _) => b'0' + digit,
        (false, true, _) => b'L',
        (true, false, false) => b'P' + digit,
        (true, true, false) => b'Z',
        (true, false, true) => b'A' + digit,
        (true, true, true) => b'K',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_dmh_known_answers() {
        // 33 deg 25.64 min == 33*6000 + 25*100 + 64 hundredths.
        assert_eq!(
            split_dmh((33 * 6000 + 2564) * UNITS_PER_HUNDREDTH_MINUTE),
            (33, 25, 64)
        );
        assert_eq!(split_dmh(0), (0, 0, 0));
        assert_eq!(
            split_dmh(90 * 6000 * UNITS_PER_HUNDREDTH_MINUTE),
            (90, 0, 0)
        );
        assert_eq!(
            split_dmh((179 * 6000 + 5999) * UNITS_PER_HUNDREDTH_MINUTE),
            (179, 59, 99)
        );
    }

    #[test]
    fn dest_char_table() {
        // Chapter 10 destination character table, all six cases.
        assert_eq!(dest_char(3, false, false, false), b'3');
        assert_eq!(dest_char(0, true, false, false), b'L');
        assert_eq!(dest_char(3, false, true, false), b'S');
        assert_eq!(dest_char(0, true, true, false), b'Z');
        assert_eq!(dest_char(3, false, true, true), b'D');
        assert_eq!(dest_char(0, true, true, true), b'K');
    }

    #[test]
    fn dest_col_inverts_dest_char() {
        for byte in [b'0', b'9', b'L', b'P', b'Y', b'Z', b'A', b'J', b'K'] {
            let col = match dest_col(byte, 0) {
                Ok(c) => c,
                Err(e) => panic!("{e}"),
            };
            assert_eq!(
                dest_char(col.digit, col.blank, col.one, col.custom == Some(true)),
                byte
            );
        }
        // Custom-set characters only appear in the first three columns.
        assert!(matches!(
            dest_col(b'A', 3),
            Err(MicEError::BadDestChar {
                got: b'A',
                column: 3
            })
        ));
        assert!(matches!(
            dest_col(b'O', 0),
            Err(MicEError::BadDestChar {
                got: b'O',
                column: 0
            })
        ));
    }

    #[test]
    fn coordinates_pair_the_fields() {
        let latitude = match Latitude::new((33 * 6000 + 2564) * UNITS_PER_HUNDREDTH_MINUTE) {
            Ok(l) => l,
            Err(e) => panic!("{e}"),
        };
        let longitude = match Longitude::new(-(112 * 6000 + 1229) * UNITS_PER_HUNDREDTH_MINUTE) {
            Ok(l) => l,
            Err(e) => panic!("{e}"),
        };
        let report = match MicE::new(
            latitude,
            longitude,
            20,
            251,
            Symbol::from_wire(b'/', b'>'),
            MicEMessage::EnRoute,
        ) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(report.coordinates(), Coordinates::new(latitude, longitude));
        assert_eq!(report.coordinates().latitude, report.latitude);
        assert_eq!(report.coordinates().longitude, report.longitude);
    }

    #[test]
    fn message_bits_round_trip() {
        let all = [
            MicEMessage::OffDuty,
            MicEMessage::EnRoute,
            MicEMessage::InService,
            MicEMessage::Returning,
            MicEMessage::Committed,
            MicEMessage::Special,
            MicEMessage::Priority,
            MicEMessage::Emergency,
            MicEMessage::Custom0,
            MicEMessage::Custom1,
            MicEMessage::Custom2,
            MicEMessage::Custom3,
            MicEMessage::Custom4,
            MicEMessage::Custom5,
            MicEMessage::Custom6,
        ];
        for msg in all {
            let (bits, custom) = msg.bits();
            assert_eq!(MicEMessage::from_bits(bits, custom), msg);
        }
        // Bits 000 are Emergency in both character sets.
        assert_eq!(MicEMessage::from_bits(0, true), MicEMessage::Emergency);
        assert_eq!(MicEMessage::from_bits(0, false), MicEMessage::Emergency);
    }

    #[test]
    fn altitude_splitter() {
        // "4T} " prefix: (('4'-33)*91 + ('T'-33))*91 ... 3 chars + '}'.
        // 0 m == 10000 relative: 10000 = 1*91*91 + 18*91 + 61 ->
        // chars 33+1='"', 33+18='3', 33+61='~'... compute directly:
        let rest = b"\"4T}Hello";
        let split = match split_altitude(rest) {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        };
        // (1*91 + 19)*91 + 51 - 10000 = 61 m.
        assert_eq!(split.device_prefix, None);
        assert_eq!(split.altitude, Some(61));
        assert_eq!(split.status, b"Hello");
        // The same altitude behind each of the four device prefixes
        // chapter 10 lists. Its own worked example gives `"4T}`, `>"4T}`
        // and `]"4T}` as three spellings of one altitude, so decoding
        // them to three different things is a defect, not a nuance.
        for prefix in [b'>', b']', b'`', b'\''] {
            let mut rest = [0u8; 10];
            rest[0] = prefix;
            rest[1..10].copy_from_slice(b"\"4T}Hello");
            assert_eq!(
                split_altitude(&rest),
                Ok(AltitudeSplit {
                    device_prefix: Some(prefix),
                    altitude: Some(61),
                    status: b"Hello",
                }),
                "altitude behind prefix {:?}",
                prefix as char
            );
        }
        // No prefix and no altitude: everything is status text.
        for text in [
            // No '}' at index 3: everything is status, no altitude.
            &b"just text"[..],
            // Short rest never indexes out of bounds.
            &b"ab"[..],
            &b""[..],
        ] {
            assert_eq!(
                split_altitude(text),
                Ok(AltitudeSplit::all_status(None, text)),
                "{:?} carries neither prefix nor altitude",
                core::str::from_utf8(text).unwrap_or("<binary>")
            );
        }
        // A prefix with no altitude behind it is still a prefix: the
        // altitude is optional and the two are found independently.
        // Guessing wrong must not consume four bytes of somebody's
        // comment, only the one byte we hand back on encode.
        for (text, prefix, status) in [
            (&b">Hello there"[..], b'>', &b"Hello there"[..]),
            // Three non-base-91 characters before a '}': not an
            // altitude, but the prefix survives and nothing errors.
            (&b">\x7fzz}tail"[..], b'>', &b"\x7fzz}tail"[..]),
            // A bare prefix, as five AE6NM-1 frames carry.
            (&b">"[..], b'>', &b""[..]),
            (&b"]"[..], b']', &b""[..]),
            // The corpus frame this reading exists for.
            (&b"]Stopped\r"[..], b']', &b"Stopped\r"[..]),
        ] {
            assert_eq!(
                split_altitude(text),
                Ok(AltitudeSplit::all_status(Some(prefix), status)),
                "{:?} is a prefix with no altitude",
                core::str::from_utf8(text).unwrap_or("<binary>")
            );
        }
        // An unprefixed altitude whose leading base-91 digit is itself
        // a prefix byte still reads as an altitude: `]xy}` is 487 km,
        // and reading the `]` as a Kenwood would throw the number
        // away. 60*8281 + 61*91 + 60 - 10000 == 492471.
        assert_eq!(
            split_altitude(b"]^]}up"),
            Ok(AltitudeSplit {
                device_prefix: None,
                altitude: Some(492_471),
                status: b"up",
            })
        );
        // A byte outside the base-91 alphabet is a typed error.
        assert!(matches!(
            split_altitude(b"\x7fzz}"),
            Err(MicEError::BadAltitudeChar { got: 0x7f, .. })
        ));
    }

    #[test]
    fn device_prefixed_altitude_round_trips_byte_exactly() {
        // The whole point of holding the prefix: an igate that hears
        // `]"4T}TM-D700` must put those bytes back on the air, not a
        // helpfully normalised `"4T}TM-D700`.
        let dest = *b"S32U6T";
        for prefix in [b'>', b']', b'`', b'\''] {
            let mut info = [0u8; 32];
            info[..9].copy_from_slice(b"`(_fn\"Oj/");
            info[9] = prefix;
            info[10..19].copy_from_slice(b"\"4T}Hello");
            let report = decode(&dest, &info[..19]).expect("decode");
            assert_eq!(report.device_prefix, Some(prefix));
            assert_eq!(report.altitude, Some(61));
            assert_eq!(report.status, b"Hello");

            let mut out = [0u8; 32];
            let len = report.encode_info(&mut out).expect("encode");
            assert_eq!(&out[..len], &info[..19], "prefix {:?}", prefix as char);
        }
    }

    #[test]
    fn encode_rejects_an_invented_device_prefix() {
        let dest = *b"S32U6T";
        let mut info = [0u8; 32];
        info[..9].copy_from_slice(b"`(_fn\"Oj/");
        let mut report = decode(&dest, &info[..9]).expect("decode");
        report.device_prefix = Some(b'X');
        assert_eq!(
            report.encode_info(&mut [0u8; 32]),
            Err(MicEError::BadDevicePrefix { got: b'X' })
        );
        assert_eq!(
            report.with_device_prefix(Some(b'X')),
            Err(MicEError::BadDevicePrefix { got: b'X' })
        );
        assert!(report.with_device_prefix(Some(b']')).is_ok());
        assert!(report.with_device_prefix(None).is_ok());
    }

    #[test]
    fn symbol_table_check() {
        for ok in [b'/', b'\\', b'0', b'9', b'A', b'Z'] {
            assert_eq!(check_symbol_table(ok), Ok(()));
        }
        for bad in [b' ', b'a', b'~'] {
            assert_eq!(
                check_symbol_table(bad),
                Err(MicEError::BadSymbolTable { got: bad })
            );
        }
    }
}
