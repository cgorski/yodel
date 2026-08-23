//! APRS (Automatic Packet Reporting System) payloads.
//!
//! This module builds and parses the *information field* of APRS packets
//! per the APRS 1.01 specification: position reports (uncompressed and
//! base-91 compressed, [`position`]), weather reports ([`weather`]),
//! telemetry ([`telemetry`]), object and item reports ([`object`]),
//! status reports ([`status`]) and text messages including ack/rej
//! ([`message`]). The glue helpers
//! [`build_ui_frame`] / [`packet_from_ui`] connect an [`AprsPacket`] to
//! the [`crate::ax25`] UI-frame layer.
//!
//! Everything is `no_std` and allocation-free: builders serialize into
//! caller-provided byte buffers and return the written length; parsers
//! borrow the input (comment and message text are sub-slices).
//!
//! # Supported data types
//!
//! * `!` / `=` — position without timestamp (without / with messaging),
//!   both the uncompressed `ddmm.mmN/dddmm.mmW$` form and the compressed
//!   base-91 form including the typed `csT` trailer (course/speed,
//!   radio range, altitude or no data). An uncompressed report whose
//!   symbol code is `_` parses as a position-with-weather report
//!   ([`weather`]).
//! * `/` / `@` — position with timestamp (without / with messaging):
//!   a 7-byte DHM/HMS timestamp followed by the same uncompressed or
//!   compressed position body ([`position::PositionTimestamped`]).
//! * `_` — positionless weather report ([`weather`]).
//! * `T` — telemetry report ([`telemetry`]).
//! * `;` — object report ([`object`]).
//! * `)` — item report ([`object`]).
//! * `>` — status report (free text; the optional leading timestamp is
//!   treated as part of the text, not decoded).
//! * `:` — message with 9-character space-padded addressee, optional
//!   `{`-prefixed message id, and the `ack`/`rej` replies.
//!
//! Any other data-type identifier parses to the *typed error*
//! [`AprsError::InvalidDataType`] carrying the identifier byte (there is
//! no `Unsupported` variant; rejecting keeps the enum total). This
//! includes the Mic-E identifiers `` ` `` and `'`: a Mic-E report cannot
//! be decoded from the information field alone (the destination
//! callsign carries the latitude), so with the `micE` feature use
//! [`mic_e::decode`] with both fields instead.
//!
//! # Destination callsign convention
//!
//! APRS abuses the AX.25 destination address as a protocol marker rather
//! than a real recipient. Generic packets conventionally use `APRS`;
//! software distributes tocalls of the form `APxxxx` (e.g. `APZ` plus
//! three characters for experimental/beta software, per the APRS
//! to-call registry). Choose the destination with
//! [`crate::ax25::Address::new`] when building a frame.

use core::fmt;

pub mod capabilities;
pub mod extension;
pub mod message;
#[cfg(feature = "micE")]
pub mod mic_e;
pub mod monitor;
pub mod nmea;
pub mod object;
pub mod position;
pub mod status;
pub mod symbol;
pub mod telemetry;
pub mod thirdparty;
pub mod ultimeter;
pub mod weather;

pub use capabilities::Capabilities;
pub use extension::{Bearing, DataExtension, Dfs, Phg, PhgRate, Speed};
// `Decoded` and `DecodedKind` are defined in this module.
pub use message::{Addressee, Message, MessageContent};
#[cfg(feature = "micE")]
pub use mic_e::{MicE, MicEError, MicEFix, MicEMessage};
pub use nmea::{NmeaData, NmeaError, NmeaSentence};
pub use object::{Item, Object, Timestamp};
pub use position::{
    CompressedCs, CompressionOrigin, CompressionType, NmeaSource, Position, PositionCs,
    PositionTimestamped,
};
// The position primitives now live at the crate root, because grid
// squares and coordinates are not APRS concepts (WSPR and FT8 use them
// too). Re-exported here so `warble::aprs::Latitude` keeps resolving:
// dozens of call sites and every README doctest spell it that way.
pub use crate::geo::{
    Ambiguity, Coordinates, DegreesMinutes, GeoError, GridPrecision, Latitude, LatitudeHemisphere,
    Longitude, LongitudeHemisphere, MaidenheadGrid,
};
pub use extension::{CommentTelemetry, Dao, comment_telemetry, dao};
pub use status::{BeamHeading, Status, StatusGrid};
pub use symbol::{OverlayId, Symbol, SymbolCode, SymbolDescription, SymbolTable};
pub use telemetry::{
    Telemetry, TelemetryBitSense, TelemetryDefinition, TelemetryEquations, TelemetryLabels,
    TelemetryValue,
};
pub use thirdparty::ThirdParty;
pub use ultimeter::{UltimeterError, UltimeterFormat, UltimeterRecord};
pub use weather::{PositionWeather, PositionlessWeather, WeatherReport};

use crate::ax25::{Address, Ax25Error, UiFrame};

/// An APRS protocol violation: an invalid field value on build, or a
/// malformed information field on parse.
///
/// Every variant carries the offending byte or value together with the
/// rule it violated, so the rendered message is self-explanatory.
///
/// Marked `#[non_exhaustive]`: APRS is a living protocol and this crate
/// still has data types left to implement, so new variants will appear
/// without a breaking release. Match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AprsError {
    /// The data-type identifier (first info byte) is not supported.
    InvalidDataType {
        /// The rejected identifier byte.
        got: u8,
    },
    /// A byte that must be an ASCII digit was something else.
    BadDigit {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information field.
        position: usize,
    },
    /// A fixed literal byte (such as the `.` in `ddmm.mm`) was wrong.
    ExpectedByte {
        /// The byte the format requires at this position.
        expected: u8,
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information field.
        position: usize,
    },
    /// A hemisphere letter was not `N`/`S` (latitude) or `E`/`W`
    /// (longitude).
    BadHemisphere {
        /// The rejected byte.
        got: u8,
    },
    /// A latitude was outside `-90..=90` degrees (or its minutes field
    /// was 60 or more).
    BadLatitude {
        /// The rejected value in signed 1/100 arc-minutes.
        got: i64,
    },
    /// A longitude was outside `-180..=180` degrees (or its minutes
    /// field was 60 or more).
    BadLongitude {
        /// The rejected value in signed 1/100 arc-minutes.
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
    /// The symbol table identifier was not `/`, `\` or an overlay
    /// character.
    BadSymbolTable {
        /// The rejected byte.
        got: u8,
    },
    /// An overlay character was not `0-9` or `A-Z`.
    BadOverlay {
        /// The rejected byte.
        got: u8,
    },
    /// A symbol code was outside printable ASCII (`!`..=`~`).
    BadSymbolCode {
        /// The rejected byte.
        got: u8,
    },
    /// A compressed-position byte was outside the base-91 alphabet
    /// (`!`..=`{`).
    BadBase91 {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information field.
        position: usize,
    },
    /// A course was 360 degrees or more on build.
    BadCourse {
        /// The rejected course in degrees.
        got: u16,
    },
    /// A speed exceeded the largest compressed-encodable value
    /// (about 1018 knots) on build.
    BadSpeed {
        /// The rejected speed in knots.
        got: u16,
    },
    /// A radio range exceeded the largest compressed-encodable value
    /// (about 2037 miles) on build.
    BadRadioRange {
        /// The rejected range in miles.
        got: u16,
    },
    /// An altitude exceeded the largest compressed-encodable value
    /// (about 15,301,000 feet) on build.
    BadAltitude {
        /// The rejected altitude in feet.
        got: u32,
    },
    /// A course/speed or radio-range `csT` trailer named the GGA NMEA
    /// source, which the decoder reserves for the altitude form.
    NmeaSourceConflict,
    /// A message addressee was longer than nine characters.
    AddresseeTooLong {
        /// The rejected length in bytes.
        len: usize,
    },
    /// A message addressee was empty (all spaces).
    AddresseeEmpty,
    /// A message addressee contained a byte outside printable ASCII
    /// (space and `:` are also excluded).
    InvalidAddresseeChar {
        /// The rejected byte.
        got: u8,
    },
    /// A message id (after `{`, or following `ack`/`rej`) was empty or
    /// longer than five characters.
    MessageIdLengthInvalid {
        /// The rejected length in bytes.
        len: usize,
    },
    /// The information field ended before a required field was complete.
    Truncated {
        /// The minimum length in bytes the format requires.
        expected: usize,
        /// The length in bytes available.
        got: usize,
    },
    /// A timestamp component was out of range (or an unknown timestamp
    /// format letter was seen).
    BadTimestamp {
        /// The component: `M` month, `D` day, `H` hour, `m` minute,
        /// `S` second, or `?` for an unknown format letter.
        field: u8,
        /// The rejected value (for `?`, the format letter byte).
        got: i32,
    },
    /// A weather field tag byte was not recognized.
    UnknownWeatherField {
        /// The rejected tag byte.
        got: u8,
    },
    /// A weather measurement was outside its field's range on build.
    BadWeatherValue {
        /// The field tag (`c`, `s`, `g`, `t`, `r`, `p`, `P`, `h`, `b`).
        field: u8,
        /// The rejected value.
        got: i32,
    },
    /// A telemetry sequence byte was not an ASCII digit (the `MIC`
    /// sequence form is not supported).
    BadTelemetrySequence {
        /// The rejected byte.
        got: u8,
    },
    /// A telemetry sequence value exceeded 999 on build.
    TelemetrySequenceOutOfRange {
        /// The rejected sequence value.
        got: u32,
    },
    /// A telemetry analog field held a number the value type cannot
    /// represent: a mantissa past `i64`, or a fraction wider than
    /// [`TelemetryValue`] carries.
    ///
    /// Chapter 13's `0..=255` range is **not** enforced. MEASURED over
    /// a 64 918-packet capture, 1 724 reports carry an ordinary value
    /// outside it.
    BadAnalogValue {
        /// Byte offset of the rejected field within the information
        /// field.
        position: usize,
    },
    /// A telemetry channel carried a fraction too wide to build.
    TelemetryDecimalsOutOfRange {
        /// The rejected digit count.
        got: u8,
    },
    /// A telemetry digital byte was not `0` or `1`.
    BadDigitalBit {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte within the information field.
        position: usize,
    },
    /// An object/item live-killed byte was not `*`/`_` (object) or
    /// `!`/`_` (item).
    BadLiveKilled {
        /// The rejected byte.
        got: u8,
    },
    /// A callsign in a third-party header had an invalid length.
    ///
    /// Empty, or longer than
    /// [`thirdparty::CALLSIGN_MAX`]. Note this bound is looser than
    /// AX.25 allows, because encapsulated traffic legitimately carries
    /// APRS-IS addresses that AX.25 would reject.
    BadCallsignLength {
        /// The rejected length in bytes.
        len: usize,
    },
    /// An object or item name had an invalid length.
    NameLengthInvalid {
        /// The rejected length in bytes.
        len: usize,
        /// The minimum allowed length.
        min: usize,
        /// The maximum allowed length.
        max: usize,
    },
    /// An object or item name contained a non-printable byte (or, for
    /// items, a `!`/`_` terminator byte).
    BadNameChar {
        /// The rejected byte.
        got: u8,
        /// Byte offset of the rejected byte.
        position: usize,
    },
    /// The caller-provided output buffer cannot hold the serialized
    /// information field.
    BufferTooSmall {
        /// The required length in bytes.
        needed: usize,
        /// The buffer capacity in bytes.
        max: usize,
    },
    /// A Mic-E report (data type `` ` `` or `'`) was recognized but did
    /// not decode.
    ///
    /// Mic-E keeps its own error type because it validates a field
    /// [`AprsError`] never sees — the AX.25 destination address. This
    /// variant is the bridge, so a malformed Mic-E report can travel
    /// the *existing* [`DecodedKind::Malformed`] path from
    /// [`Decoded::decode_frame`] rather than needing a parallel
    /// variant that every downstream `match` would have to learn.
    #[cfg(feature = "micE")]
    MicE(MicEError),
}

impl fmt::Display for AprsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            AprsError::InvalidDataType { got } => write!(
                f,
                "data-type identifier 0x{got:02X} is not a supported APRS packet type"
            ),
            AprsError::BadDigit { got, position } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is invalid: an ASCII digit is required"
            ),
            AprsError::ExpectedByte {
                expected,
                got,
                position,
            } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is invalid: 0x{expected:02X} is required"
            ),
            AprsError::BadHemisphere { got } => write!(
                f,
                "hemisphere byte 0x{got:02X} is invalid: must be N/S (latitude) or E/W (longitude)"
            ),
            AprsError::BadLatitude { got } => write!(
                f,
                "latitude of {got} 1/100 arc-minutes is out of range: must be within \u{b1}90\u{b0} with minutes below 60"
            ),
            AprsError::BadLongitude { got } => write!(
                f,
                "longitude of {got} 1/100 arc-minutes is out of range: must be within \u{b1}180\u{b0} with minutes below 60"
            ),
            AprsError::BadAmbiguity { got } => write!(
                f,
                "position ambiguity {got} is out of range: 0..=4 digits may be masked"
            ),
            AprsError::BadGridLength { got } => write!(
                f,
                "Maidenhead locator length {got} is invalid: must be 4, 6 or 8 characters"
            ),
            AprsError::BadGridChar { got, position } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is invalid in a Maidenhead locator"
            ),
            AprsError::BadSymbolTable { got } => write!(
                f,
                "symbol table byte 0x{got:02X} is invalid: must be '/', '\\' or an overlay character"
            ),
            AprsError::BadOverlay { got } => write!(
                f,
                "overlay byte 0x{got:02X} is invalid: must be '0'-'9' or 'A'-'Z'"
            ),
            AprsError::BadSymbolCode { got } => write!(
                f,
                "symbol code byte 0x{got:02X} is invalid: printable ASCII '!'..='~' is required"
            ),
            AprsError::BadBase91 { got, position } => write!(
                f,
                "byte 0x{got:02X} at offset {position} is outside the base-91 alphabet '!'..='{{'"
            ),
            AprsError::BadCourse { got } => write!(
                f,
                "course of {got} degrees is out of range: 0..=359 is required"
            ),
            AprsError::BadSpeed { got } => write!(
                f,
                "speed of {got} knots exceeds the largest compressed-encodable value"
            ),
            AprsError::BadRadioRange { got } => write!(
                f,
                "radio range of {got} miles exceeds the largest compressed-encodable value"
            ),
            AprsError::BadAltitude { got } => write!(
                f,
                "altitude of {got} feet exceeds the largest compressed-encodable value"
            ),
            AprsError::NmeaSourceConflict => write!(
                f,
                "NMEA source GGA selects the altitude form of the csT trailer: course/speed and radio range require another source"
            ),
            AprsError::AddresseeTooLong { len } => write!(
                f,
                "addressee of {len} bytes is too long: at most 9 characters fit the field"
            ),
            AprsError::AddresseeEmpty => {
                write!(f, "addressee is empty: at least one character is required")
            }
            AprsError::InvalidAddresseeChar { got } => write!(
                f,
                "addressee byte 0x{got:02X} is invalid: printable ASCII excluding space and ':' is required"
            ),
            AprsError::MessageIdLengthInvalid { len } => write!(
                f,
                "message id of {len} bytes is invalid: must be 1..=5 characters"
            ),
            AprsError::Truncated { expected, got } => write!(
                f,
                "information field of {got} bytes is truncated: at least {expected} bytes are required"
            ),
            AprsError::BadTimestamp { field, got } => write!(
                f,
                "timestamp component '{}' value {got} is out of range",
                field as char
            ),
            AprsError::UnknownWeatherField { got } => write!(
                f,
                "weather field tag 0x{got:02X} is not a recognized measurement"
            ),
            AprsError::BadWeatherValue { field, got } => write!(
                f,
                "weather field '{}' value {got} is out of range",
                field as char
            ),
            AprsError::BadTelemetrySequence { got } => write!(
                f,
                "telemetry sequence byte 0x{got:02X} is invalid: one to five ASCII digits are required (the MIC form is unsupported)"
            ),
            AprsError::TelemetrySequenceOutOfRange { got } => write!(
                f,
                "telemetry sequence {got} is out of range: at most five digits fit the wire"
            ),
            AprsError::BadAnalogValue { position } => write!(
                f,
                "telemetry analog field at offset {position} is not a number this crate can hold"
            ),
            AprsError::TelemetryDecimalsOutOfRange { got } => write!(
                f,
                "telemetry value with {got} decimal places is out of range: at most 18 fit an i64 mantissa"
            ),
            AprsError::BadDigitalBit { got, position } => write!(
                f,
                "telemetry digital byte 0x{got:02X} at offset {position} is invalid: '0' or '1' is required"
            ),
            AprsError::BadLiveKilled { got } => write!(
                f,
                "live/killed byte 0x{got:02X} is invalid: '*'/'_' (object) or '!'/'_' (item) is required"
            ),
            AprsError::BadCallsignLength { len } => write!(
                f,
                "third-party callsign of {len} bytes is invalid: 1..={} characters are required",
                thirdparty::CALLSIGN_MAX
            ),
            AprsError::NameLengthInvalid { len, min, max } => write!(
                f,
                "name of {len} bytes is invalid: {min}..={max} characters are required"
            ),
            AprsError::BadNameChar { got, position } => write!(
                f,
                "name byte 0x{got:02X} at offset {position} is invalid: printable ASCII is required"
            ),
            AprsError::BufferTooSmall { needed, max } => write!(
                f,
                "information field of {needed} bytes does not fit: the buffer holds at most {max} bytes"
            ),
            #[cfg(feature = "micE")]
            AprsError::MicE(e) => write!(f, "Mic-E report: {e}"),
        }
    }
}

impl core::error::Error for AprsError {}

/// Lifts a geographic validation failure into the APRS error surface.
///
/// The position primitives moved to [`crate::geo`], which cannot depend
/// on this module, so they carry their own error type. This conversion
/// keeps `?` working unchanged at the ~100 call sites that build a
/// coordinate inside an APRS parser or builder, and maps each variant
/// onto the equivalent `AprsError` rather than burying it in a wrapper,
/// so rendered messages are unchanged too.
impl From<GeoError> for AprsError {
    fn from(error: GeoError) -> Self {
        match error {
            GeoError::BadLatitude { got } => Self::BadLatitude { got },
            GeoError::BadLongitude { got } => Self::BadLongitude { got },
            GeoError::BadAmbiguity { got } => Self::BadAmbiguity { got },
            GeoError::BadGridLength { got } => Self::BadGridLength { got },
            GeoError::BadGridChar { got, position } => Self::BadGridChar { got, position },
        }
    }
}

/// Lifts a Mic-E decode failure into the APRS error surface.
///
/// Unlike the [`GeoError`] conversion above this *wraps* rather than
/// flattens: [`MicEError`]'s variants name Mic-E structures
/// ([`MicEError::BadDestChar`], [`MicEError::MixedMessageBits`]) that
/// have no `AprsError` counterpart, and inventing lossy mappings for
/// them would throw away the column and byte a caller needs to see.
#[cfg(feature = "micE")]
impl From<MicEError> for AprsError {
    fn from(error: MicEError) -> Self {
        Self::MicE(error)
    }
}

/// A parsed or to-be-built APRS information field.
///
/// Parsed packets borrow their free-text portions (comment, status text,
/// message text and id) from the input slice.
///
/// Weather in a position report is a **separate variant**
/// ([`AprsPacket::PositionWeather`], symbol code `_`) rather than a
/// field on [`Position`]: the comment of such a report is structured
/// weather data, not free text, so mixing the two in one struct would
/// make either form ambiguous to build.
///
/// Marked `#[non_exhaustive]` for the same reason as [`AprsError`]:
/// APRS still has data types this crate does not implement, and adding
/// one should not be a breaking release. Match with a `_` arm. Note
/// that receive-only formats (raw NMEA, Ultimeter weather, third-party
/// encapsulation) live on [`Decoded`] instead, so every variant here
/// really is something this crate can also build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AprsPacket<'a> {
    /// A position report without timestamp (`!` or `=`) whose
    /// compressed `csT` trailer (if any) carries no data.
    Position(Position<'a>),
    /// A compressed position report without timestamp (`!` or `=`)
    /// whose `csT` trailer carries course/speed, radio range or
    /// altitude data.
    PositionCs(PositionCs<'a>),
    /// A position report with timestamp (`/` or `@`).
    PositionTimestamped(PositionTimestamped<'a>),
    /// A position report whose symbol is `_` carrying weather data.
    PositionWeather(PositionWeather<'a>),
    /// A positionless weather report (`_`).
    Weather(PositionlessWeather<'a>),
    /// A telemetry report (`T`).
    Telemetry(Telemetry<'a>),
    /// An object report (`;`).
    Object(Object<'a>),
    /// An item report (`)`).
    Item(Item<'a>),
    /// A status report (`>`).
    Status(Status<'a>),
    /// A text message, ack or rej (`:`).
    Message(Message<'a>),
    /// A station capability report (`<`).
    Capabilities(Capabilities<'a>),
}

/// The result of looking at an information field — **this decode
/// cannot fail**.
///
/// [`AprsPacket::parse`] is strict: it returns an error for anything it
/// does not fully understand, which is the right contract when you are
/// about to act on a packet. But a receiver also wants the opposite
/// contract, because roughly a quarter of real off-air traffic is
/// something this crate cannot type — and losing the bytes is worse
/// than not typing them.
///
/// [`Decoded::decode`] is **total**: it has no `Result`, it always
/// produces a value, and the original bytes are always reachable. The
/// two contracts sit side by side; pick per call site.
///
/// # Two constructors, one per thing you might be holding
///
/// * [`Decoded::decode`] takes the information field alone — what you
///   have when a payload arrives without its frame: inside a
///   third-party wrapper, over KISS, out of a log.
/// * [`Decoded::decode_frame`] additionally takes the AX.25
///   destination address, and is the one to reach for on a *received
///   frame*. Mic-E encodes its latitude digits, N/S, longitude offset,
///   W/E and message bits in the destination callsign, so it is a
///   **frame-level** format that no information-field-only entry point
///   can decode. Handed only the information field, `decode` says so
///   with [`DecodedKind::NeedsDestination`] rather than the untrue
///   [`DecodedKind::Unsupported`].
///
/// The two agree on everything else, and that is a checked property
/// rather than a convention: for any information field whose first byte
/// is not `` ` `` or `'`, `decode_frame(d, info).kind` equals
/// `decode(info).kind` for **every** `d`. A destination address cannot
/// change the meaning of a non-Mic-E packet. `tests/decoded_laws.rs`
/// asserts it.
///
/// # Why this is not just more `AprsPacket` variants
///
/// The variants below split four ways, and the split is the point:
///
/// * [`DecodedKind::Packet`] wraps an [`AprsPacket`] — a payload this crate
///   can both parse *and* build. Every such value is validated.
/// * [`DecodedKind::MicE`] is **frame-level**. It is validated and this
///   crate can build it too ([`mic_e::MicE::encode`]), but it does not
///   belong in [`AprsPacket`], because [`AprsPacket::build`] writes the
///   information field *only* while [`build_ui_frame`] takes the
///   destination from its caller. An `AprsPacket::MicE` variant would
///   make `build_ui_frame(&packet, Address::new(b"APRS", 0)?, …)`
///   compile and return `Ok` while putting a Mic-E information field on
///   the air under a tocall that contradicts it — the receiver decodes
///   00°00.00' with garbage message bits. A wrong value transmitted by
///   a call that succeeded is the worst failure this crate has;
///   `tests/mic_e.rs::aprs_parse_rejects_mic_e_ids` guards the layering.
/// * [`DecodedKind::Nmea`], [`DecodedKind::Ultimeter`] and
///   [`DecodedKind::ThirdParty`] are **receive-only**. APRS never
///   originates them from this library, so putting them in
///   [`AprsPacket`] would force [`AprsPacket::build`] to grow arms that
///   can only ever fail. Keeping them here means `AprsPacket` stays
///   round-trippable.
/// * [`DecodedKind::NeedsDestination`], [`DecodedKind::Unsupported`] and
///   [`DecodedKind::Malformed`] carry bytes we could not type, labelled
///   with *why*.
///
/// So an `AprsPacket` value still means "a validated packet I
/// understand and could re-transmit *from this information field
/// alone*", and nothing weaker is smuggled into it.
///
/// ```
/// use warble::aprs::{AprsPacket, Decoded, DecodedKind};
///
/// // A type we implement: fully parsed.
/// let d = Decoded::decode(b">on the air");
/// assert!(matches!(d.kind, DecodedKind::Packet(AprsPacket::Status(_))));
/// assert_eq!(d.info, b">on the air");
///
/// // A type we do not implement: labelled, never lost.
/// let d = Decoded::decode(b"?APRSD");
/// assert!(matches!(d.kind, DecodedKind::Unsupported { dti: b'?' }));
/// assert_eq!(d.info, b"?APRSD");
/// ```
///
/// A Mic-E field needs the frame, and says which one it is missing:
///
/// ```
/// # #[cfg(feature = "micE")] {
/// use warble::aprs::{Decoded, DecodedKind};
/// use warble::ax25::Address;
///
/// const INFO: &[u8] = b"`(_fn\"Oj/";
///
/// let d = Decoded::decode(INFO);
/// assert!(matches!(d.kind, DecodedKind::NeedsDestination { dti: b'`' }));
///
/// let dest = Address::new(b"S32UVT", 0)?;
/// let d = Decoded::decode_frame(dest, INFO);
/// let report = d.mic_e().expect("Mic-E needs only the destination");
/// assert!((report.latitude.to_degrees() - 33.427_333).abs() < 1e-5);
/// # }
/// # Ok::<(), warble::ax25::Ax25Error>(())
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded<'a> {
    /// The information field this was decoded from, verbatim.
    ///
    /// Present on **every** outcome. This is what makes "the bytes are
    /// never lost" a property of the type rather than a promise in
    /// prose: there is no variant from which the original is
    /// unreachable.
    pub info: &'a [u8],
    /// What the field turned out to be.
    pub kind: DecodedKind<'a>,
}

/// What an information field turned out to be. See [`Decoded`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DecodedKind<'a> {
    /// A payload this crate understands and can also build.
    Packet(AprsPacket<'a>),
    /// A Mic-E compressed position report (`` ` `` or `'`), read from
    /// the frame's destination address *and* its information field.
    ///
    /// Only [`Decoded::decode_frame`] ever produces this;
    /// [`Decoded::decode`] cannot, because half the report is in the
    /// destination callsign it was not given. Reach it with
    /// [`Decoded::mic_e`].
    #[cfg(feature = "micE")]
    MicE(MicE<'a>),
    /// A raw NMEA 0183 sentence (`$`). Receive-only.
    Nmea(nmea::NmeaSentence<'a>),
    /// A Peet Bros Ultimeter weather record (`$ULTW`, `!!`, `*`, `#`).
    /// Receive-only.
    Ultimeter(ultimeter::UltimeterRecord<'a>),
    /// Third-party (gateway-encapsulated) traffic (`}`). Receive-only.
    ///
    /// The inner payload is *not* decoded; call [`Decoded::decode`] on
    /// [`ThirdParty::payload`](thirdparty::ThirdParty::payload) if you
    /// want to descend. Doing so explicitly is what bounds recursion.
    ThirdParty(thirdparty::ThirdParty<'a>),
    /// An AX.25 frame carrying plain text rather than an APRS payload.
    ///
    /// Not every frame on 144.39 MHz is APRS. Stations beacon readable
    /// text: a TNC's station identification (conventionally addressed
    /// to `ID`), its beacon text (`BEACON`), a digipeater's firmware
    /// banner (`UIDIGI`), and human-written weather bulletins.
    ///
    /// These frames carry **no data type identifier at all**. Chapter
    /// 5's table marks `A`-`S`, `U`-`Z`, `a`-`z`, `0`-`9`, `|` and `~`
    /// as "[Do not use]", and does not list the control characters or
    /// the space, so a field opening with one of those is not an APRS
    /// packet by the specification's own account. Reporting the first
    /// byte of `WA6TK/R RELAY/D` as a data type identifier of `W` is
    /// simply wrong, and that is what this variant replaces.
    ///
    /// # This is not counted as a typed APRS value
    ///
    /// [`Decoded::is_typed`] answers `false` here, on purpose. The
    /// frame is classified rather than decoded: there are no fields to
    /// extract, and letting it raise the structured-coverage figure
    /// would inflate that number with traffic that is not APRS. See
    /// `tests/corpus_aprs.rs`, which counts these separately and uses
    /// them to state coverage over APRS frames rather than over every
    /// frame heard.
    ///
    /// MEASURED over 2182 off-air frames: 75, of which 42 are station
    /// identifications, 23 beacon text, 4 firmware banners and 6
    /// plain-text weather bulletins.
    Text {
        /// The information field, verbatim.
        text: &'a [u8],
    },
    /// A data type identifier whose payload is split between the
    /// information field and the AX.25 destination address, decoded
    /// from the information field alone.
    ///
    /// Today that means exactly Mic-E, `` ` `` (0x60) and `'` (0x27).
    /// The bytes are on [`Decoded::info`]; re-decode with
    /// [`Decoded::decode_frame`] once you have the frame's destination.
    ///
    /// This is *not* [`DecodedKind::Unsupported`]: this crate does
    /// implement Mic-E, and labelling the single most common packet on
    /// 144.39 MHz "unimplemented" sent readers looking for a decoder
    /// that was already there.
    ///
    /// Produced only with the `micE` feature enabled. Without it there
    /// is no Mic-E decoder to point at, so `` ` `` and `'` keep coming
    /// back as [`DecodedKind::Unsupported`] — which is then accurate.
    NeedsDestination {
        /// The leading data type identifier byte.
        dti: u8,
    },
    /// A data type identifier this crate does not implement, or a
    /// non-APRS beacon.
    ///
    /// The specification requires that programs "be able to process
    /// [non-conforming packets] without ill effects", which is exactly
    /// this variant. The bytes are on [`Decoded::info`].
    Unsupported {
        /// The leading data type identifier byte.
        dti: u8,
    },
    /// A recognized data type whose body did not parse.
    ///
    /// The bytes are on [`Decoded::info`].
    Malformed {
        /// The leading data type identifier byte.
        dti: u8,
        /// Why the parse failed.
        error: AprsError,
    },
}

impl<'a> Decoded<'a> {
    /// Decodes an information field. **Never fails.**
    ///
    /// Dispatch follows the data type identifier, with three
    /// disambiguations the identifier alone cannot express:
    ///
    /// * `$` introduces either an Ultimeter record (`$ULTW`) or an
    ///   NMEA sentence.
    /// * `!` introduces either an Ultimeter data-logger record (when
    ///   the next byte is also `!`) or a position report. A position
    ///   report's next byte is a digit or a symbol-table character,
    ///   never `!`, so this is unambiguous.
    /// * An uncompressed position whose symbol code is `_` is *usually*
    ///   a weather report, but the symbol is a hint rather than a
    ///   guarantee; the position is kept when the weather body does not
    ///   parse.
    ///
    /// The Mic-E identifiers `` ` `` and `'` come back as
    /// [`DecodedKind::NeedsDestination`]: their latitude lives in the
    /// AX.25 destination address, which this entry point was not given.
    /// Use [`Decoded::decode_frame`] when you have the frame.
    #[must_use]
    pub fn decode(info: &'a [u8]) -> Self {
        Self {
            info,
            kind: DecodedKind::classify(None, info),
        }
    }

    /// Decodes a received frame: its AX.25 destination address plus its
    /// information field. **Never fails.**
    ///
    /// The receive-side total decode. Identical to [`Decoded::decode`]
    /// for every input whose first byte is not `` ` `` or `'` — a
    /// destination address must not change the meaning of a non-Mic-E
    /// packet, and `tests/decoded_laws.rs` asserts that over every
    /// destination it can generate. For those two identifiers it does
    /// what `decode` structurally cannot: combines the destination's
    /// latitude digits, N/S, longitude offset, W/E and message bits
    /// with the information field to yield [`DecodedKind::MicE`], or
    /// [`DecodedKind::Malformed`] carrying [`AprsError::MicE`] when the
    /// report does not decode.
    ///
    /// `dest` is taken by value: [`Address`] is [`Copy`] and eight
    /// bytes. Only `info` is borrowed by the result, so passing a
    /// temporary destination — as
    /// [`RxFrame::decoded`](crate::tnc::RxFrame::decoded) does — is
    /// fine.
    ///
    /// The destination **SSID is ignored**, as it is throughout Mic-E:
    /// chapter 10 puts the whole encoding in the six callsign
    /// characters.
    #[must_use]
    pub fn decode_frame(dest: Address, info: &'a [u8]) -> Self {
        Self {
            info,
            kind: DecodedKind::classify(Some(dest), info),
        }
    }

    /// The typed packet, if this decoded to one this crate can build.
    ///
    /// Returns `None` for a Mic-E report, which is not an
    /// [`AprsPacket`] — see [`Decoded::mic_e`].
    #[must_use]
    pub fn packet(&self) -> Option<&AprsPacket<'a>> {
        match &self.kind {
            DecodedKind::Packet(p) => Some(p),
            _ => None,
        }
    }

    /// The Mic-E report, if this decoded to one.
    ///
    /// The peer of [`Decoded::packet`], and it exists because that one
    /// cannot cover Mic-E: Mic-E is around 41% of real 144.39 MHz
    /// traffic and lives outside [`AprsPacket`], so a caller who
    /// reaches only for `packet()` gets a silent `None` for two frames
    /// in five. Always `None` from [`Decoded::decode`], which never
    /// sees a destination address.
    #[cfg(feature = "micE")]
    #[must_use]
    pub fn mic_e(&self) -> Option<&MicE<'a>> {
        match &self.kind {
            DecodedKind::MicE(m) => Some(m),
            _ => None,
        }
    }

    /// Whether the decode produced a typed value of any kind, as
    /// opposed to [`DecodedKind::NeedsDestination`],
    /// [`DecodedKind::Unsupported`] or [`DecodedKind::Malformed`].
    ///
    /// Adding `NeedsDestination` changed no answer: those inputs were
    /// `Unsupported` before, and so already `false`.
    ///
    /// # Why this lists the `true` variants, exhaustively
    ///
    /// It was once `!matches!(…the three negatives…)`, which is the
    /// safe default only for a new **typed** variant. For a new
    /// *untyped* one it fails the wrong way: a fourth "could not type
    /// this" label — the obvious next addition, since three of the
    /// seven variants today are already refusals — would answer `true`
    /// and compile silently. That answer is not cosmetic; it feeds
    /// `MIN_STRUCTURED_PERCENT` in `tests/corpus_aprs.rs`, the crate's
    /// headline "how much of real 144.39 MHz traffic decodes to a type"
    /// number, so the failure mode is a coverage claim inflated by a
    /// variant that decodes to nothing.
    ///
    /// Written positively with **no `_` arm**, a new variant is a
    /// compile error here until someone classifies it. The enum is
    /// `#[non_exhaustive]` for downstream crates only, so matching it
    /// exhaustively inside this one is exactly the intended privilege.
    /// The match is also `const`-callable, so `const fn` stands.
    #[must_use]
    pub const fn is_typed(&self) -> bool {
        match self.kind {
            DecodedKind::Packet(_)
            | DecodedKind::Nmea(_)
            | DecodedKind::Ultimeter(_)
            | DecodedKind::ThirdParty(_) => true,
            // Same answer, separate arm: an attribute cannot sit on one
            // alternative of an or-pattern.
            #[cfg(feature = "micE")]
            DecodedKind::MicE(_) => true,
            // `Text` is a classification, not a decode: the frame is
            // positively identified as non-APRS, but no APRS field
            // comes out of it, so it must not raise the coverage
            // figure this function feeds.
            DecodedKind::Text { .. }
            | DecodedKind::NeedsDestination { .. }
            | DecodedKind::Unsupported { .. }
            | DecodedKind::Malformed { .. } => false,
        }
    }

    /// Whether the information field is an APRS payload at all.
    ///
    /// `false` only for [`DecodedKind::Text`], which is the case the
    /// specification's own data-type-identifier table rules out. This
    /// is the denominator a coverage measurement wants: a frame that is
    /// not APRS is not a frame the APRS parser failed on.
    #[must_use]
    pub const fn is_aprs(&self) -> bool {
        !matches!(self.kind, DecodedKind::Text { .. })
    }
}

impl<'a> DecodedKind<'a> {
    /// Classifies an information field; the body of both
    /// [`Decoded::decode`] (`dest: None`) and [`Decoded::decode_frame`]
    /// (`dest: Some`).
    ///
    /// One classifier rather than two, so "a destination address
    /// changes nothing outside Mic-E" holds by construction: there is
    /// only one dispatch table, and `dest` is read in exactly one arm
    /// of it. Two `match`es kept in sync by review would make that a
    /// convention instead of a structural fact.
    fn classify(dest: Option<Address>, info: &'a [u8]) -> Self {
        // Without the `micE` feature there is no Mic-E decoder to route
        // to, so the destination really is unused and `` ` `` / `'`
        // stay `Unsupported` -- which in that build is the truth.
        #[cfg(not(feature = "micE"))]
        let _ = dest;

        let Some(&dti) = info.first() else {
            return DecodedKind::Unsupported { dti: 0 };
        };

        // Mic-E is frame-level, not information-field-level: the
        // destination callsign carries the latitude digits, the
        // hemispheres and the message bits. Handled before the shared
        // identifiers below because nothing else claims 0x60 / 0x27.
        #[cfg(feature = "micE")]
        if matches!(dti, b'`' | b'\'') {
            let Some(dest) = dest else {
                return DecodedKind::NeedsDestination { dti };
            };
            return match mic_e::decode_address(dest, info) {
                Ok(report) => DecodedKind::MicE(report),
                Err(error) => DecodedKind::Malformed {
                    dti,
                    error: error.into(),
                },
            };
        }

        // Formats sharing an identifier with something else.
        match dti {
            b'$' | b'!' | b'*' | b'#' if ultimeter::detect(info).is_some() => {
                return match ultimeter::parse(info) {
                    Ok(record) => DecodedKind::Ultimeter(record),
                    Err(_) => DecodedKind::Unsupported { dti },
                };
            }
            b'$' => {
                return match nmea::parse(info) {
                    Ok(sentence) => DecodedKind::Nmea(sentence),
                    Err(_) => DecodedKind::Unsupported { dti },
                };
            }
            b'}' => {
                return match thirdparty::ThirdParty::parse(info) {
                    Ok(tp) => DecodedKind::ThirdParty(tp),
                    Err(error) => DecodedKind::Malformed { dti, error },
                };
            }
            _ => {}
        }

        match AprsPacket::parse(info) {
            Ok(packet) => DecodedKind::Packet(packet),
            Err(AprsError::InvalidDataType { got }) => {
                // A byte chapter 5 rules out as an identifier means the
                // frame is not APRS, rather than APRS this crate has
                // not implemented. Say which, because the two want
                // different things from a caller: one is text to show,
                // the other is a gap to fill.
                if !is_data_type_identifier(got) && info.iter().any(u8::is_ascii_graphic) {
                    DecodedKind::Text { text: info }
                } else {
                    DecodedKind::Unsupported { dti: got }
                }
            }
            Err(error) => DecodedKind::Malformed { dti, error },
        }
    }
}

/// Whether `byte` is a data type identifier chapter 5 assigns or
/// reserves, as opposed to one it rules out.
///
/// The table in chapter 5 ("APRS Data Type Identifiers") lists every
/// identifier. Four of its rows are ranges marked **"[Do not use]"**,
/// `A`-`S`, `U`-`Z`, `a`-`z` and `0`-`9`, plus `|` and `~`; `T`
/// (telemetry) is the one letter carved out of them. Bytes absent from
/// the table altogether, which is every control character except the
/// two Mic-E betas and the space, are equally not identifiers.
///
/// Rows marked "[Unused]" or "[Reserved]" are a different matter and
/// answer `true`: the specification keeps them for itself, so a frame
/// opening with one is APRS that this crate does not implement, and
/// [`DecodedKind::Unsupported`] is the accurate label.
const fn is_data_type_identifier(byte: u8) -> bool {
    match byte {
        // "[Do not use]", so not an identifier. `T` is telemetry and is
        // matched below, before this arm can claim it.
        b'A'..=b'S' | b'U'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'|' | b'~' => false,
        // Current and old Mic-E Data (Rev 0 beta).
        0x1c | 0x1d => true,
        b'T' => true,
        // Every printable identifier the table names, assigned,
        // unused or reserved.
        b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{' | b'}' => true,
        _ => false,
    }
}

impl<'a> AprsPacket<'a> {
    /// Parses an APRS information field.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] on an empty field,
    /// [`AprsError::InvalidDataType`] on an unsupported data-type
    /// identifier, and the per-type parse errors documented on
    /// [`Position::parse`], [`Status::parse`] and [`Message::parse`].
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = *info.first().ok_or(AprsError::Truncated {
            expected: 1,
            got: 0,
        })?;
        match dti {
            b'!' | b'=' => {
                let with_cs = PositionCs::parse(info)?;
                let position = with_cs.position;
                // The weather symbol is a *hint*, not a guarantee: plenty
                // of stations use it on an ordinary position report whose
                // comment is free text or a PHG extension rather than the
                // `DDD/SSS` wind block a Complete Weather Report needs.
                // Try the weather reading, but fall back to the position
                // rather than losing the whole packet.
                if !position.compressed
                    && position.symbol.to_wire().1 == b'_'
                    && let Ok(weather) = PositionWeather::parse(info)
                {
                    return Ok(AprsPacket::PositionWeather(weather));
                }
                if with_cs.cs == CompressedCs::NoData {
                    Ok(AprsPacket::Position(position))
                } else {
                    Ok(AprsPacket::PositionCs(with_cs))
                }
            }
            b'/' | b'@' => {
                let timestamped = PositionTimestamped::parse(info)?;
                // Chapter 12's Complete Weather Report has four
                // uncompressed spellings, not one: `!` and `=` without
                // a timestamp, `/` and `@` with. Only the first two
                // were ever tried here, so 92 corpus frames -- 54
                // directly and 38 inside third-party wrappers -- came
                // back as an ordinary timestamped position whose
                // weather block stayed uninterpreted text. As above,
                // the `_` symbol is a hint and the weather reading is
                // allowed to fail back to the position.
                if !timestamped.position.compressed
                    && timestamped.position.symbol.to_wire().1 == b'_'
                    && let Ok(weather) = PositionWeather::parse(info)
                {
                    return Ok(AprsPacket::PositionWeather(weather));
                }
                Ok(AprsPacket::PositionTimestamped(timestamped))
            }
            b'_' => PositionlessWeather::parse(info).map(AprsPacket::Weather),
            b'T' => Telemetry::parse(info).map(AprsPacket::Telemetry),
            b';' => Object::parse(info).map(AprsPacket::Object),
            b')' => Item::parse(info).map(AprsPacket::Item),
            b'>' => Status::parse(info).map(AprsPacket::Status),
            b':' => Message::parse(info).map(AprsPacket::Message),
            b'<' => Capabilities::parse(info).map(AprsPacket::Capabilities),
            other => Err(AprsError::InvalidDataType { got: other }),
        }
    }

    /// Serializes the information field into `buf`, returning the
    /// written length.
    ///
    /// # How big must `buf` be?
    ///
    /// There is no `INFO_MAX` constant, because no defensible one
    /// exists: several variants embed a caller-supplied slice (a
    /// position comment, a status text, an object's payload) whose
    /// length this type does not bound. Any constant would be a guess
    /// dressed as a guarantee.
    ///
    /// Sizing is checkable instead of guessable:
    ///
    /// * An under-sized buffer is **never** silently truncated. `build`
    ///   returns [`AprsError::BufferTooSmall`], and that error carries
    ///   `needed`, so a caller that cannot pick a size up front can size
    ///   from the failure and retry.
    /// * For a bound you can allocate against, work from the frame
    ///   layer: `tnc::MAX_FRAME_BYTES` (330) covers the longest AX.25
    ///   address field, control, PID, a 256-byte information field and
    ///   the FCS — so an information field that fits a standard AX.25
    ///   frame fits in 256 bytes.
    ///
    /// Buffer lengths in this crate's examples are chosen to be
    /// comfortably larger than the example's own output and carry no
    /// other meaning.
    ///
    /// # Errors
    ///
    /// [`AprsError::BufferTooSmall`] when `buf` cannot hold the field.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        self.build_inner(buf)
    }

    /// Serializes the information field into a fresh vector.
    ///
    /// The ergonomic counterpart to [`AprsPacket::build`] for callers
    /// who are not counting bytes. It sizes itself, so there is no
    /// length it refuses: the buffer starts at the 256 bytes a standard
    /// AX.25 frame leaves for an information field and grows to fit
    /// anything longer.
    ///
    /// # Why it grows rather than capping
    ///
    /// [`AprsPacket::build`]'s sizing note prescribes this directly: a
    /// caller that cannot pick a size up front sizes from the failure
    /// and retries. This is that caller, so 256 is a first guess rather
    /// than a limit.
    ///
    /// An earlier revision treated it as a limit, reasoning that an
    /// information field too long for an AX.25 frame cannot arise. It
    /// arises. APRS-IS imposes no frame size, and gateways that sign
    /// their beacons emit 250 to 400 bytes routinely. MEASURED over a
    /// 244-second capture of the full feed: 115 of 30 051 packets
    /// exceed 256 bytes, and every one of them that had a builder
    /// failed to re-serialize. Nothing was truncated or misread, but a
    /// caller asking "did I understand this packet?" was told no about
    /// 80 packets it had understood perfectly.
    ///
    /// One retry suffices by construction, because every builder
    /// reports `needed` as its whole `encoded_len()`, so the second
    /// attempt is sized exactly. A second failure is a real error and
    /// propagates.
    ///
    /// ```
    /// # #[cfg(all(feature = "aprs", feature = "alloc"))] {
    /// use warble::aprs::{AprsPacket, Latitude, Longitude, Position, Symbol};
    ///
    /// let packet = AprsPacket::Position(Position::new(
    ///     Latitude::from_degrees(49.0583)?,
    ///     Longitude::from_degrees(-72.0292)?,
    ///     Symbol::CAR,
    /// ));
    /// assert_eq!(packet.to_vec()?, b"!4903.50N/07201.75W>");
    /// # }
    /// # Ok::<(), warble::aprs::AprsError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// As [`AprsPacket::build`].
    #[cfg(feature = "alloc")]
    pub fn to_vec(&self) -> Result<alloc::vec::Vec<u8>, AprsError> {
        // 256 is what a 330-byte AX.25 frame leaves for an information
        // field once addresses, control, PID and the FCS are paid for,
        // which makes it the right first guess and the wrong ceiling.
        let mut buf = alloc::vec![0u8; 256];
        let written = match self.build_inner(&mut buf) {
            Ok(n) => n,
            Err(AprsError::BufferTooSmall { needed, .. }) => {
                buf.resize(needed, 0);
                self.build_inner(&mut buf)?
            }
            Err(e) => return Err(e),
        };
        buf.truncate(written);
        Ok(buf)
    }

    fn build_inner(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        match *self {
            AprsPacket::Position(ref p) => p.build(buf),
            AprsPacket::PositionCs(ref p) => p.build(buf),
            AprsPacket::PositionTimestamped(ref p) => p.build(buf),
            AprsPacket::PositionWeather(ref w) => w.build(buf),
            AprsPacket::Weather(ref w) => w.build(buf),
            AprsPacket::Telemetry(ref t) => t.build(buf),
            AprsPacket::Object(ref o) => o.build(buf),
            AprsPacket::Item(ref i) => i.build(buf),
            AprsPacket::Status(ref s) => s.build(buf),
            AprsPacket::Message(ref m) => m.build(buf),
            AprsPacket::Capabilities(ref c) => c.build(buf),
        }
    }
}

/// An error from the [`build_ui_frame`] glue: either layer can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AprsUiError {
    /// The APRS information field could not be serialized.
    Aprs(AprsError),
    /// The AX.25 frame could not be serialized.
    Ax25(Ax25Error),
}

impl fmt::Display for AprsUiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            AprsUiError::Aprs(ref e) => write!(f, "APRS layer: {e}"),
            AprsUiError::Ax25(ref e) => write!(f, "AX.25 layer: {e}"),
        }
    }
}

impl core::error::Error for AprsUiError {}

impl From<AprsError> for AprsUiError {
    fn from(e: AprsError) -> Self {
        AprsUiError::Aprs(e)
    }
}

impl From<Ax25Error> for AprsUiError {
    fn from(e: Ax25Error) -> Self {
        AprsUiError::Ax25(e)
    }
}

/// Builds a complete AX.25 UI frame body carrying an APRS packet.
///
/// The packet is serialized into `info_buf`, then wrapped with the given
/// destination (see the module docs for the `APRS`/`APxxxx` tocall
/// convention), source and digipeater path into `frame_buf` via
/// [`UiFrame::build`]. Returns the frame body length in `frame_buf`; the
/// result is ready for [`crate::ax25::hdlc::frame_bits`] (or, with the
/// `mod` feature, [`crate::ax25::tx_i16`] / [`crate::ax25::tx_f32`]).
///
/// # Errors
///
/// [`AprsUiError::Aprs`] when `info_buf` is too small for the packet;
/// [`AprsUiError::Ax25`] when the path is too long or `frame_buf` is too
/// small for the frame.
pub fn build_ui_frame(
    packet: &AprsPacket<'_>,
    dest: Address,
    src: Address,
    path: &[Address],
    info_buf: &mut [u8],
    frame_buf: &mut [u8],
) -> Result<usize, AprsUiError> {
    let info_len = packet.build(info_buf)?;
    let info = info_buf.get(..info_len).ok_or(AprsError::BufferTooSmall {
        needed: info_len,
        max: info_buf.len(),
    })?;
    let frame = UiFrame::with_path(dest, src, path, info)?;
    Ok(frame.build(frame_buf)?)
}

/// Builds a complete AX.25 UI frame for `packet` into a fresh vector.
///
/// The ergonomic counterpart to [`build_ui_frame`], which needs two
/// caller-provided buffers: one for the information field and one for
/// the frame around it. This sizes both.
///
/// ```
/// # #[cfg(all(feature = "aprs", feature = "alloc"))] {
/// use warble::aprs::{AprsPacket, Status, build_ui_frame_to_vec};
/// use warble::ax25::Address;
///
/// let packet = AprsPacket::Status(Status { text: b"on the air" });
/// let frame = build_ui_frame_to_vec(
///     &packet,
///     Address::new(b"APRS", 0)?,
///     Address::new(b"N0CALL", 7)?,
///     &[Address::new(b"WIDE1", 1)?],
/// )?;
/// assert!(frame.len() > 21); // three addresses, control, PID, then the text
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// As [`build_ui_frame`], except that neither buffer can be too small.
#[cfg(feature = "alloc")]
pub fn build_ui_frame_to_vec(
    packet: &AprsPacket<'_>,
    dest: Address,
    src: Address,
    path: &[Address],
) -> Result<alloc::vec::Vec<u8>, AprsUiError> {
    let info = packet.to_vec()?;
    Ok(UiFrame::with_path(dest, src, path, &info)?.to_vec())
}

/// Extracts the APRS packet from a parsed AX.25 UI frame.
///
/// A thin convenience over [`AprsPacket::parse`] on the frame's
/// information field; the returned packet borrows from the frame.
///
/// Strict, and information-field-only: a Mic-E frame fails here with
/// [`AprsError::InvalidDataType`]. Use [`decoded_from_ui`] for the
/// total, frame-level decode.
///
/// # Errors
///
/// The parse errors documented on [`AprsPacket::parse`].
pub fn packet_from_ui<'a>(frame: &UiFrame<'a>) -> Result<AprsPacket<'a>, AprsError> {
    AprsPacket::parse(frame.info)
}

/// Totally decodes a parsed AX.25 UI frame. **Never fails.**
///
/// The peer of [`packet_from_ui`] and the one to prefer on receive: it
/// passes the frame's destination as well as its information field, so
/// Mic-E decodes here and nothing is ever lost — unrecognised bytes are
/// labelled rather than rejected. Equivalent to
/// `Decoded::decode_frame(frame.dest, frame.info)`; the result borrows
/// from the frame's information field only.
#[must_use]
pub fn decoded_from_ui<'a>(frame: &UiFrame<'a>) -> Decoded<'a> {
    Decoded::decode_frame(frame.dest, frame.info)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::format;
    use std::string::ToString;

    use super::*;

    #[test]
    fn empty_info_is_truncated() {
        assert_eq!(
            AprsPacket::parse(b""),
            Err(AprsError::Truncated {
                expected: 1,
                got: 0
            })
        );
    }

    #[test]
    fn unknown_data_type_is_typed_error() {
        assert_eq!(
            AprsPacket::parse(b"?query"),
            Err(AprsError::InvalidDataType { got: b'?' })
        );
        assert_eq!(
            AprsPacket::parse(b"$GPGGA,..."),
            Err(AprsError::InvalidDataType { got: b'$' })
        );
    }

    #[test]
    fn errors_render() {
        // Every variant has a self-explanatory Display.
        let samples = [
            AprsError::InvalidDataType { got: b'?' }.to_string(),
            AprsError::BadDigit {
                got: b'x',
                position: 3,
            }
            .to_string(),
            AprsError::ExpectedByte {
                expected: b'.',
                got: b',',
                position: 5,
            }
            .to_string(),
            AprsError::BadHemisphere { got: b'Q' }.to_string(),
            AprsError::BadLatitude { got: 540_100 }.to_string(),
            AprsError::BadLongitude { got: -1_080_100 }.to_string(),
            AprsError::BadSymbolTable { got: b'~' }.to_string(),
            AprsError::BadOverlay { got: b'a' }.to_string(),
            AprsError::BadSymbolCode { got: 0x1F }.to_string(),
            AprsError::BadBase91 {
                got: b' ',
                position: 2,
            }
            .to_string(),
            AprsError::BadCourse { got: 360 }.to_string(),
            AprsError::BadSpeed { got: 2000 }.to_string(),
            AprsError::BadRadioRange { got: 3000 }.to_string(),
            AprsError::BadAltitude { got: 20_000_000 }.to_string(),
            AprsError::NmeaSourceConflict.to_string(),
            AprsError::AddresseeTooLong { len: 10 }.to_string(),
            AprsError::AddresseeEmpty.to_string(),
            AprsError::InvalidAddresseeChar { got: b':' }.to_string(),
            AprsError::MessageIdLengthInvalid { len: 6 }.to_string(),
            AprsError::Truncated {
                expected: 9,
                got: 4,
            }
            .to_string(),
            AprsError::BadTimestamp {
                field: b'M',
                got: 13,
            }
            .to_string(),
            AprsError::UnknownWeatherField { got: b'q' }.to_string(),
            AprsError::BadWeatherValue {
                field: b'c',
                got: 361,
            }
            .to_string(),
            AprsError::BadTelemetrySequence { got: b'M' }.to_string(),
            AprsError::TelemetrySequenceOutOfRange { got: 1000 }.to_string(),
            AprsError::BadAnalogValue { position: 6 }.to_string(),
            AprsError::TelemetryDecimalsOutOfRange { got: 19 }.to_string(),
            AprsError::BadDigitalBit {
                got: b'2',
                position: 26,
            }
            .to_string(),
            AprsError::BadLiveKilled { got: b'x' }.to_string(),
            AprsError::NameLengthInvalid {
                len: 10,
                min: 3,
                max: 9,
            }
            .to_string(),
            AprsError::BadNameChar {
                got: 0x07,
                position: 2,
            }
            .to_string(),
            AprsError::BufferTooSmall { needed: 20, max: 8 }.to_string(),
        ];
        for s in samples {
            assert!(!s.is_empty());
        }
        // The Mic-E bridge variant labels its layer and then defers to
        // the inner error, so nothing the Mic-E decoder reported is lost
        // on the way through `DecodedKind::Malformed`.
        #[cfg(feature = "micE")]
        {
            let inner = MicEError::BadDestChar {
                got: b'a',
                column: 0,
            };
            assert_eq!(AprsError::from(inner), AprsError::MicE(inner));
            let rendered = AprsError::MicE(inner).to_string();
            assert!(rendered.starts_with("Mic-E report: "), "{rendered}");
            assert!(rendered.ends_with(&inner.to_string()), "{rendered}");
        }
        let ui = AprsUiError::from(AprsError::AddresseeEmpty);
        assert!(format!("{ui}").starts_with("APRS layer"));
        let ui = AprsUiError::from(Ax25Error::SsidOutOfRange { got: 99 });
        assert!(format!("{ui}").starts_with("AX.25 layer"));
    }
}
