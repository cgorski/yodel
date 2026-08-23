//! Peet Bros. Ultimeter weather stations (`$ULTW`, `!!`, `*`, `#`).
//!
//! Ultimeter consoles emit fixed-width, hex-encoded records that APRS
//! carries verbatim under four different data-type identifiers. Three
//! unrelated wire forms live here:
//!
//! * **Packet mode** ([`PacketMode`]), identifier `$` plus the literal
//!   `ULTW`: 13 fields of four hex digits (44, 48 or 52 characters).
//! * **Data-logger mode** ([`DataLoggerMode`]), identifier `!` plus a
//!   second `!`: 12 fields of four hex digits (40, 44 or 48
//!   characters). The field *order differs from packet mode* from the
//!   sixth field onward — it carries indoor sensors where packet mode
//!   carries the barometer correction factor.
//! * **Ultimeter II** ([`UltimeterTwo`]), identifier `*` or `#`: a much
//!   older encoding, 13 hex characters in five irregular-width fields
//!   with their own scaling rules. `*` reports wind speed in **mph**,
//!   `#` in **km/h**; the APRS 1.01 identifier table describes the two
//!   identically and does not mention the difference, so conflating
//!   them is a silent 1.6x error.
//!
//! Entry point: [`parse`], which takes the whole information field
//! *including* the leading identifier byte and dispatches internally.
//! [`detect`] answers the same question without decoding, which is what
//! a packet dispatcher wants: the `$` identifier is shared with raw
//! NMEA sentences and `!` with position reports.
//!
//! # Common rules for the two 4-digit formats
//!
//! * Fields are four ASCII hex digits, most-significant digit first,
//!   with no delimiters; positions are fixed, not tag-driven. Lowercase
//!   hex is accepted.
//! * Signed fields are 16-bit two's complement (`0xFF9C` is -100).
//! * An absent sensor is written as ASCII hyphens filling the *whole*
//!   field (`----`). This convention is not in the vendor document but
//!   is universally implemented; a field mixing hyphens with hex digits
//!   is corrupt and rejected with [`UltimeterError::MixedDashField`].
//!   Every field is therefore an `Option`: a missing sensor is `None`,
//!   never a substituted zero.
//! * There is **no checksum** on any of these formats.
//! * Units unless stated otherwise: wind speed 0.1 kph, wind direction
//!   0-255 over the full circle, temperature 0.1 degrees Fahrenheit,
//!   rain 0.01 inch, humidity 0.1 %, barometer 0.1 mbar. The structs
//!   here hold those raw units; [`UltimeterRecord::to_weather_report`]
//!   converts to the APRS units of [`WeatherReport`].
//! * Shorter records are legal: instruments may omit the last one or
//!   two fields. Fields past the end of the block decode to `None`.
//! * Bytes after the field block (the `<CR><LF>` a console appends, or
//!   any comment) are not interpreted and are handed back as `rest`.
//!
//! # Documented decisions
//!
//! * **Wind direction divides by 255, not 256.** The vendor says the
//!   field is "0-255 corresponding to 0-360 degrees", so 255 maps to a
//!   full circle. Major implementations disagree here (some divide by
//!   256); the two differ by at most 1.4 degrees, far inside any
//!   anemometer's accuracy. The raw 0-255 value is kept in the struct
//!   so a caller who prefers the other convention can redo the scaling.
//!   The top byte is masked off first: it can read `FF` after a
//!   calibration offset is applied.
//! * **Rain is assumed to be in hundredths of an inch.** With a 0.1 mm
//!   gauge selected the console reports 0.1 mm increments instead, and
//!   *nothing in the record says which*. The inch reading matches every
//!   major implementation and the APRS rain fields, so it is what
//!   [`WeatherReport`] receives.
//! * **The Ultimeter II rain gauges are not mapped into
//!   [`WeatherReport`].** The format carries an "upper" and a "lower"
//!   display total whose correspondence to the APRS 1-hour / 24-hour /
//!   since-midnight fields is not specified anywhere; both are exposed
//!   raw on [`UltimeterTwo`] instead of being guessed at.
//! * **The absent-sensor marker is accepted at any field width**, not
//!   just four. The vendor documents `----` for the 4-digit formats
//!   only, but nothing else can be meant by a hyphen inside an
//!   Ultimeter II field.
//! * `$ULTI` **does not exist** and is not accepted; it is a
//!   mis-recollection of `$ULTW`.
//!
//! # Example
//!
//! ```
//! use warble::aprs::ultimeter::{UltimeterFormat, UltimeterRecord, parse};
//!
//! let record = parse(b"$ULTW0000000001FF000427C70002CCD30001026E003A050F00040000")?;
//! assert_eq!(record.format(), UltimeterFormat::Packet);
//!
//! let UltimeterRecord::Packet(packet) = record else {
//!     panic!("packet mode expected");
//! };
//! assert_eq!(packet.temperature, Some(511)); // 51.1 degrees F
//! assert_eq!(packet.barometer, Some(10_183)); // 1018.3 mbar
//! assert_eq!(packet.day_of_year(), Some(59));
//! assert_eq!(packet.time_of_day(), Some((21, 35)));
//!
//! // ... and the same record as physical quantities, which the APRS
//! // builder will render into whichever unit its wire field wants.
//! let weather = packet.to_weather_report();
//! let temperature = weather.temperature.expect("field 3 was present");
//! assert_eq!(temperature.tenths_fahrenheit(), 511); // as measured
//! assert_eq!(temperature.fahrenheit(), 51); // as the `t` field spells it
//! assert_eq!(temperature.celsius(), 11); // and for the rest of the world
//! let pressure = weather.barometric_pressure.expect("field 5");
//! assert_eq!(pressure.tenths_hpa(), 10_183); // as the `b` field spells it
//! assert_eq!(pressure.hundredths_inhg(), 3007); // 30.07 inHg
//! assert_eq!(weather.humidity.map(|h| h.percent()), Some(62));
//! # Ok::<(), warble::aprs::ultimeter::UltimeterError>(())
//! ```
//!
//! The format is published by the vendor as the "Ultimeter Weather
//! Station Serial Data Format" (`peetbros.com`, packet/data-logger
//! modes and the Ultimeter II encoding).

use core::fmt;

use super::weather::WeatherReport;
use crate::units::{Humidity, Pressure, Rainfall, Speed, Temperature};

/// The four body bytes that must follow `$` in packet mode.
const PACKET_PREFIX: [u8; 4] = *b"ULTW";

/// The absent-sensor marker byte (ASCII hyphen).
const ABSENT: u8 = b'-';

/// Widest field any format uses, in hex characters. Four hex digits is
/// the widest value that still fits the `u16` accumulator.
const MAX_FIELD_WIDTH: usize = 4;

/// Field-block lengths packet mode allows, in hex characters:
/// fields 1-11, 1-12 or all 13.
const PACKET_LENGTHS: [usize; 3] = [44, 48, 52];

/// Field-block lengths data-logger mode allows, in hex characters:
/// fields 1-10, 1-11 or all 12.
const DATA_LOGGER_LENGTHS: [usize; 3] = [40, 44, 48];

/// The single field-block length Ultimeter II allows: `1 + 2 + 2 + 4 +
/// 4` hex characters.
const ULTIMETER_TWO_LENGTHS: [usize; 1] = [13];

/// The unit of an Ultimeter II wind-speed field, selected by the
/// data-type identifier.
///
/// The two identifiers are otherwise interchangeable, and the APRS
/// specification's identifier table lists both with the same
/// description. The unit difference comes only from the vendor
/// document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindUnit {
    /// Identifier `*`: the wind-speed field is already in mph.
    Mph,
    /// Identifier `#`: the wind-speed field is in km/h.
    Kph,
}

/// Which Ultimeter wire form an information field carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UltimeterFormat {
    /// `$ULTW` packet mode: 13 fields of four hex digits.
    Packet,
    /// `!!` data-logger mode: 12 fields of four hex digits.
    DataLogger,
    /// `*` / `#` Ultimeter II: 13 hex characters, irregular widths. The
    /// unit records which of the two identifiers was seen, because it
    /// selects the wind-speed unit.
    UltimeterTwo(WindUnit),
}

impl UltimeterFormat {
    /// The data-type identifier byte this format arrives under.
    #[must_use]
    pub const fn identifier(self) -> u8 {
        match self {
            UltimeterFormat::Packet => b'$',
            UltimeterFormat::DataLogger => b'!',
            UltimeterFormat::UltimeterTwo(WindUnit::Mph) => b'*',
            UltimeterFormat::UltimeterTwo(WindUnit::Kph) => b'#',
        }
    }

    /// The field-block lengths this format allows, in hex characters.
    const fn lengths(self) -> &'static [usize] {
        match self {
            UltimeterFormat::Packet => &PACKET_LENGTHS,
            UltimeterFormat::DataLogger => &DATA_LOGGER_LENGTHS,
            UltimeterFormat::UltimeterTwo(_) => &ULTIMETER_TWO_LENGTHS,
        }
    }
}

/// An Ultimeter record that violated the vendor's serial-data format.
///
/// Every variant carries the offending byte or value together with the
/// rule it violated, so the rendered message is self-explanatory. Byte
/// offsets are counted from the first character of the field block
/// (after `$ULTW`, `!!`, `*` or `#`), which is how the vendor's tables
/// number them.
///
/// The enum is `#[non_exhaustive]`: the vendor has published further
/// record types over the years, and adding one must not be a breaking
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UltimeterError {
    /// The information field was empty, or ended inside the fixed
    /// prefix that identifies the format.
    Truncated {
        /// Number of bytes the format requires.
        expected: usize,
        /// Number of bytes present.
        got: usize,
    },
    /// The data-type identifier is not one of the four Ultimeter forms.
    InvalidDataType {
        /// The rejected identifier byte.
        got: u8,
    },
    /// A `$` information field whose next four bytes were not `ULTW`.
    /// The identifier is shared with raw NMEA sentences, which this is
    /// most likely to be.
    NotPacketMode {
        /// The four rejected bytes.
        got: [u8; 4],
    },
    /// A `!` information field whose second byte was not `!`. The
    /// identifier is shared with position-without-timestamp reports,
    /// whose second byte is a digit (uncompressed) or a symbol-table
    /// character (compressed) — never `!`.
    NotDataLogger {
        /// The rejected second byte.
        got: u8,
    },
    /// The run of field characters was not one of the lengths the
    /// format allows.
    BadBodyLength {
        /// The format the identifier selected.
        format: UltimeterFormat,
        /// The rejected length in hex characters.
        got: usize,
    },
    /// A field character was neither an ASCII hex digit nor the `-`
    /// absent marker.
    BadHexDigit {
        /// The rejected byte.
        got: u8,
        /// Offset of the rejected byte within the field block.
        position: usize,
    },
    /// A field mixed hex digits with the `-` absent marker. A sensor is
    /// absent only when the *whole* field is hyphens; anything else is
    /// corruption, not a missing reading.
    MixedDashField {
        /// Offset of the field's first character within the block.
        position: usize,
    },
}

impl fmt::Display for UltimeterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            UltimeterError::Truncated { expected, got } => write!(
                f,
                "information field of {got} bytes is truncated: at least {expected} bytes are required"
            ),
            UltimeterError::InvalidDataType { got } => write!(
                f,
                "data-type identifier 0x{got:02X} is not Ultimeter: '$', '!', '*' or '#' is required"
            ),
            UltimeterError::NotPacketMode { got } => write!(
                f,
                "'$' body starts 0x{:02X}{:02X}{:02X}{:02X}, not 'ULTW': this is not an Ultimeter packet-mode record",
                got[0], got[1], got[2], got[3]
            ),
            UltimeterError::NotDataLogger { got } => write!(
                f,
                "'!' followed by 0x{got:02X} is a position report, not an Ultimeter data-logger record: a second '!' is required"
            ),
            UltimeterError::BadBodyLength { format, got } => {
                let allowed = match format {
                    UltimeterFormat::Packet => "44, 48 or 52",
                    UltimeterFormat::DataLogger => "40, 44 or 48",
                    UltimeterFormat::UltimeterTwo(_) => "exactly 13",
                };
                write!(
                    f,
                    "field block of {got} characters is invalid: {allowed} hex characters are required"
                )
            }
            UltimeterError::BadHexDigit { got, position } => write!(
                f,
                "field byte 0x{got:02X} at offset {position} is not a hex digit or the '-' absent marker"
            ),
            UltimeterError::MixedDashField { position } => write!(
                f,
                "field at offset {position} mixes hex digits with '-': an absent sensor fills the whole field"
            ),
        }
    }
}

impl core::error::Error for UltimeterError {}

/// A `$ULTW` packet-mode record: 13 fields of four hex digits.
///
/// Values are held in the vendor's units, exactly as decoded; see each
/// field. Instruments may omit fields 12 and 13, which then decode to
/// `None` like any absent sensor.
///
/// The wind fields map cleanly onto APRS: field 1 is the peak over the
/// last five minutes, which is the APRS definition of a gust, and field
/// 13 is a five-minute average, the closest thing on the wire to the
/// sustained one-minute wind speed APRS asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketMode<'a> {
    /// Field 1: wind-speed peak over the last five minutes, 0.1 kph.
    pub wind_peak: Option<u16>,
    /// Field 2: wind direction at that peak, `0..=255` over the full
    /// circle (already masked to the low byte). See
    /// [`PacketMode::wind_direction_degrees`].
    pub wind_direction: Option<u8>,
    /// Field 3: current outdoor temperature, 0.1 degrees Fahrenheit,
    /// signed.
    pub temperature: Option<i16>,
    /// Field 4: rain long-term total, 0.01 inch.
    pub rain_total: Option<u16>,
    /// Field 5: current barometer, 0.1 mbar.
    pub barometer: Option<u16>,
    /// Field 6: barometer delta, 0.1 mbar, signed.
    pub barometer_delta: Option<i16>,
    /// Field 7: barometer correction factor, least significant word
    /// (a raw console calibration value, no unit).
    pub barometer_correction_lsw: Option<u16>,
    /// Field 8: barometer correction factor, most significant word.
    pub barometer_correction_msw: Option<u16>,
    /// Field 9: current outdoor humidity, 0.1 %.
    pub humidity: Option<u16>,
    /// Field 10: date as a day of the year, `0x0000` being January 1.
    /// See [`PacketMode::day_of_year`] for the ordinary 1-based count.
    pub date: Option<u16>,
    /// Field 11: time as a minute of the day, `0x0000` being midnight.
    /// See [`PacketMode::time_of_day`].
    pub time: Option<u16>,
    /// Field 12: today's rain total, 0.01 inch. Absent on some
    /// instruments.
    pub rain_today: Option<u16>,
    /// Field 13: five-minute wind-speed average, 0.1 kph. Absent on
    /// some instruments.
    pub wind_average: Option<u16>,
    /// Bytes after the field block (`<CR><LF>` and anything else),
    /// uninterpreted.
    pub rest: &'a [u8],
}

/// A `!!` data-logger-mode record: 12 fields of four hex digits.
///
/// Fields 1-5 match [`PacketMode`]; from field 6 on the layout
/// diverges, carrying the indoor sensors where packet mode carries the
/// barometer correction factor and its own humidity slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataLoggerMode<'a> {
    /// Field 1: instantaneous wind speed, 0.1 kph.
    pub wind_speed: Option<u16>,
    /// Field 2: wind direction, `0..=255` over the full circle (already
    /// masked to the low byte). See
    /// [`DataLoggerMode::wind_direction_degrees`].
    pub wind_direction: Option<u8>,
    /// Field 3: outdoor temperature, 0.1 degrees Fahrenheit, signed.
    pub temperature: Option<i16>,
    /// Field 4: rain long-term total, 0.01 inch.
    pub rain_total: Option<u16>,
    /// Field 5: barometer, 0.1 mbar.
    pub barometer: Option<u16>,
    /// Field 6: **indoor** temperature, 0.1 degrees Fahrenheit, signed.
    pub indoor_temperature: Option<i16>,
    /// Field 7: outdoor humidity, 0.1 %.
    pub humidity: Option<u16>,
    /// Field 8: **indoor** humidity, 0.1 %.
    pub indoor_humidity: Option<u16>,
    /// Field 9: date as a day of the year, `0x0000` being January 1.
    /// See [`DataLoggerMode::day_of_year`].
    pub date: Option<u16>,
    /// Field 10: time as a minute of the day, `0x0000` being midnight.
    /// See [`DataLoggerMode::time_of_day`].
    pub time: Option<u16>,
    /// Field 11: today's rain total, 0.01 inch. Absent on some
    /// instruments.
    pub rain_today: Option<u16>,
    /// Field 12: one-minute wind-speed average, 0.1 kph. Absent on some
    /// instruments. This, not field 1, is what the APRS wind-speed
    /// field wants.
    pub wind_average: Option<u16>,
    /// Bytes after the field block (`<CR><LF>` and anything else),
    /// uninterpreted.
    pub rest: &'a [u8],
}

/// A `*` / `#` Ultimeter II record: 13 hex characters in five fields of
/// irregular width.
///
/// This is a completely different, older encoding: none of the 4-digit
/// format's rules (two's complement, tenths scaling) apply. Wind
/// direction is a 16-point compass digit, wind speed is already in
/// whole mph or km/h, and temperature is a *biased* byte rather than a
/// signed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UltimeterTwo<'a> {
    /// Which identifier was seen, and therefore the unit of
    /// [`UltimeterTwo::wind_speed`].
    pub unit: WindUnit,
    /// Field 1 (one hex digit): wind direction as a 16-point compass
    /// index, `0..=15` in 22.5-degree steps (0 = N, 4 = E, 8 = S,
    /// 12 = W). See [`UltimeterTwo::wind_direction_degrees`].
    pub wind_direction: Option<u8>,
    /// Field 2 (two hex digits): wind speed in whole mph (`*`) or whole
    /// km/h (`#`) — already scaled, unlike every other format here.
    pub wind_speed: Option<u8>,
    /// Field 3 (two hex digits): the biased temperature byte. Degrees
    /// Fahrenheit are `raw - 56`; see [`UltimeterTwo::temperature`].
    pub temperature_bias: Option<u8>,
    /// Field 4 (four hex digits): upper-display rain gauge total,
    /// 0.01 inch.
    pub rain_upper: Option<u16>,
    /// Field 5 (four hex digits): lower-display rain gauge total,
    /// 0.01 inch.
    pub rain_lower: Option<u16>,
    /// Bytes after the field block (`<CR><LF>` and anything else),
    /// uninterpreted.
    pub rest: &'a [u8],
}

/// A decoded Ultimeter record, borrowing its trailing bytes from the
/// information field.
///
/// Use [`UltimeterRecord::to_weather_report`] for the APRS view of any
/// of them, or match to reach the fields a single format has (indoor
/// sensors, the barometer correction factor, the two Ultimeter II rain
/// gauges).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UltimeterRecord<'a> {
    /// A `$ULTW` packet-mode record.
    Packet(PacketMode<'a>),
    /// A `!!` data-logger-mode record.
    DataLogger(DataLoggerMode<'a>),
    /// A `*` / `#` Ultimeter II record.
    UltimeterTwo(UltimeterTwo<'a>),
}

impl<'a> UltimeterRecord<'a> {
    /// Which wire form this record was decoded from.
    #[must_use]
    pub const fn format(self) -> UltimeterFormat {
        match self {
            UltimeterRecord::Packet(_) => UltimeterFormat::Packet,
            UltimeterRecord::DataLogger(_) => UltimeterFormat::DataLogger,
            UltimeterRecord::UltimeterTwo(two) => UltimeterFormat::UltimeterTwo(two.unit),
        }
    }

    /// The uninterpreted bytes following the field block.
    #[must_use]
    pub const fn rest(self) -> &'a [u8] {
        match self {
            UltimeterRecord::Packet(packet) => packet.rest,
            UltimeterRecord::DataLogger(logger) => logger.rest,
            UltimeterRecord::UltimeterTwo(two) => two.rest,
        }
    }

    /// The record as an APRS [`WeatherReport`].
    ///
    /// See [`PacketMode::to_weather_report`],
    /// [`DataLoggerMode::to_weather_report`] and
    /// [`UltimeterTwo::to_weather_report`] for what each format can and
    /// cannot fill in.
    #[must_use]
    pub fn to_weather_report(self) -> WeatherReport {
        match self {
            UltimeterRecord::Packet(packet) => packet.to_weather_report(),
            UltimeterRecord::DataLogger(logger) => logger.to_weather_report(),
            UltimeterRecord::UltimeterTwo(two) => two.to_weather_report(),
        }
    }
}

impl PacketMode<'_> {
    /// Wind direction in degrees, `0..=360`, from the raw 0-255 field.
    ///
    /// Scaled by `360 / 255` and rounded to the nearest degree; see the
    /// module documentation for why 255 rather than 256.
    #[must_use]
    pub fn wind_direction_degrees(self) -> Option<u16> {
        self.wind_direction.map(direction_degrees)
    }

    /// The date field as an ordinary 1-based day of the year (the wire
    /// value counts from zero, so this is one more).
    #[must_use]
    pub fn day_of_year(self) -> Option<u16> {
        self.date.map(|day| day.saturating_add(1))
    }

    /// The time field as `(hour, minute)`, or `None` when the field is
    /// absent or holds a minute count no day can contain.
    #[must_use]
    pub fn time_of_day(self) -> Option<(u8, u8)> {
        self.time.and_then(time_of_day)
    }

    /// The record in APRS units.
    ///
    /// Field 1 (the five-minute peak) becomes the gust and field 13
    /// (the five-minute average) the wind speed; when an instrument
    /// omits field 13 the wind speed stays `None` rather than being
    /// filled in from the peak, which would overstate it. The rain
    /// long-term total and the barometer correction factor have no APRS
    /// field and are dropped; the 1-hour and 24-hour rain fields are
    /// not carried by this format.
    #[must_use]
    pub fn to_weather_report(self) -> WeatherReport {
        WeatherReport {
            wind_direction: self.wind_direction_degrees(),
            wind_speed: self.wind_average.map(tenth_kph_to_speed),
            gust: self.wind_peak.map(tenth_kph_to_speed),
            temperature: self.temperature.map(tenths_f_to_temperature),
            rain_1h: None,
            rain_24h: None,
            rain_midnight: self
                .rain_today
                .map(|v| Rainfall::from_hundredths_inch(i32::from(v))),
            humidity: self.humidity.and_then(tenths_to_humidity),
            barometric_pressure: self
                .barometer
                .map(|v| Pressure::from_tenths_hpa(i32::from(v))),
            // No Ultimeter format has a snow gauge or a light sensor.
            luminosity: None,
            snowfall: None,
        }
    }
}

impl DataLoggerMode<'_> {
    /// Wind direction in degrees, `0..=360`, from the raw 0-255 field.
    ///
    /// Scaled by `360 / 255` and rounded to the nearest degree; see the
    /// module documentation for why 255 rather than 256.
    #[must_use]
    pub fn wind_direction_degrees(self) -> Option<u16> {
        self.wind_direction.map(direction_degrees)
    }

    /// The date field as an ordinary 1-based day of the year (the wire
    /// value counts from zero, so this is one more).
    #[must_use]
    pub fn day_of_year(self) -> Option<u16> {
        self.date.map(|day| day.saturating_add(1))
    }

    /// The time field as `(hour, minute)`, or `None` when the field is
    /// absent or holds a minute count no day can contain.
    #[must_use]
    pub fn time_of_day(self) -> Option<(u8, u8)> {
        self.time.and_then(time_of_day)
    }

    /// The record in APRS units.
    ///
    /// The wind speed comes from field 12, the one-minute average,
    /// which is what the APRS field is defined as; when an instrument
    /// omits it the instantaneous field 1 is used instead, the closest
    /// available reading. This format has no peak field, so the gust
    /// stays `None`. The indoor sensors and the rain long-term total
    /// have no APRS field and are dropped.
    #[must_use]
    pub fn to_weather_report(self) -> WeatherReport {
        WeatherReport {
            wind_direction: self.wind_direction_degrees(),
            wind_speed: self
                .wind_average
                .or(self.wind_speed)
                .map(tenth_kph_to_speed),
            gust: None,
            temperature: self.temperature.map(tenths_f_to_temperature),
            rain_1h: None,
            rain_24h: None,
            rain_midnight: self
                .rain_today
                .map(|v| Rainfall::from_hundredths_inch(i32::from(v))),
            humidity: self.humidity.and_then(tenths_to_humidity),
            barometric_pressure: self
                .barometer
                .map(|v| Pressure::from_tenths_hpa(i32::from(v))),
            luminosity: None,
            snowfall: None,
        }
    }
}

impl UltimeterTwo<'_> {
    /// Wind direction in degrees, `0..=338`, from the 16-point compass
    /// index: 22.5-degree steps rounded to the nearest degree.
    #[must_use]
    pub fn wind_direction_degrees(self) -> Option<u16> {
        self.wind_direction.map(point_degrees)
    }

    /// Temperature in whole degrees Fahrenheit: the raw byte less the
    /// vendor's bias of 56, giving `-56..=199`. This is a bias, not
    /// two's complement.
    #[must_use]
    pub fn temperature(self) -> Option<i16> {
        self.temperature_bias
            .map(|raw| i16::from(raw) - TEMPERATURE_BIAS)
    }

    /// Wind speed in whole mph, converting from km/h for a `#` record.
    #[must_use]
    pub fn wind_speed_mph(self) -> Option<u16> {
        self.wind_speed_typed()
            .map(|speed| u16::try_from(speed.mph()).unwrap_or(u16::MAX))
    }

    /// Wind speed as a [`Speed`], whichever unit the record used.
    #[must_use]
    pub const fn wind_speed_typed(self) -> Option<Speed> {
        match self.wind_speed {
            None => None,
            Some(speed) => Some(match self.unit {
                WindUnit::Mph => Speed::from_mph(speed as i32),
                WindUnit::Kph => tenth_kph_to_speed(speed as u16 * 10),
            }),
        }
    }

    /// The record in APRS units.
    ///
    /// This format carries no humidity, barometer or gust. Its two rain
    /// gauge totals are *not* mapped: which of the APRS rain fields
    /// (1-hour, 24-hour, since-midnight) an upper or lower display
    /// total corresponds to is unspecified, so they are left on
    /// [`UltimeterTwo::rain_upper`] and [`UltimeterTwo::rain_lower`]
    /// for a caller who knows their station.
    #[must_use]
    pub fn to_weather_report(self) -> WeatherReport {
        WeatherReport {
            wind_direction: self.wind_direction_degrees(),
            wind_speed: self.wind_speed_typed(),
            gust: None,
            temperature: self
                .temperature()
                .map(|f| Temperature::from_fahrenheit(i32::from(f))),
            rain_1h: None,
            rain_24h: None,
            rain_midnight: None,
            humidity: None,
            barometric_pressure: None,
            luminosity: None,
            snowfall: None,
        }
    }
}

/// The bias subtracted from an Ultimeter II temperature byte.
const TEMPERATURE_BIAS: i16 = 56;

/// Reports which Ultimeter format an information field carries, without
/// decoding it.
///
/// This is the sniff a packet dispatcher needs: two of the four
/// identifiers are shared with other APRS data types, and both
/// collisions are resolved by the byte after the identifier — `$` is
/// Ultimeter only when followed by `ULTW` (otherwise it is a raw NMEA
/// sentence), and `!` only when followed by a second `!` (after a `!`
/// identifier a position report's next byte is a digit for the
/// uncompressed form, or one of `/`, `\`, `A`-`Z`, `a`-`j` for the
/// compressed form — never `!`).
///
/// A `Some` answer does not promise the body decodes; it promises the
/// field belongs to this module rather than another one.
///
/// ```
/// use warble::aprs::ultimeter::{UltimeterFormat, WindUnit, detect};
///
/// assert_eq!(detect(b"$ULTW0000"), Some(UltimeterFormat::Packet));
/// assert_eq!(detect(b"$GPRMC,1"), None); // raw NMEA, not Ultimeter
/// assert_eq!(detect(b"!!000000"), Some(UltimeterFormat::DataLogger));
/// assert_eq!(detect(b"!4903.50N"), None); // a position report
/// assert_eq!(
///     detect(b"#4C8"),
///     Some(UltimeterFormat::UltimeterTwo(WindUnit::Kph))
/// );
/// ```
#[must_use]
pub fn detect(info: &[u8]) -> Option<UltimeterFormat> {
    match *info.first()? {
        b'$' if info.get(1..5) == Some(&PACKET_PREFIX[..]) => Some(UltimeterFormat::Packet),
        b'!' if info.get(1) == Some(&b'!') => Some(UltimeterFormat::DataLogger),
        b'*' => Some(UltimeterFormat::UltimeterTwo(WindUnit::Mph)),
        b'#' => Some(UltimeterFormat::UltimeterTwo(WindUnit::Kph)),
        _ => None,
    }
}

/// Parses a complete APRS information field, *including* the leading
/// data-type identifier, as an Ultimeter record.
///
/// Dispatches on `$ULTW`, `!!`, `*` and `#`; see [`detect`] for how the
/// two shared identifiers are told apart from raw NMEA and from
/// position reports.
///
/// # Errors
///
/// [`UltimeterError::InvalidDataType`] for an identifier no Ultimeter
/// format uses, [`UltimeterError::NotPacketMode`] /
/// [`UltimeterError::NotDataLogger`] when a shared identifier turns out
/// to belong to the other format, [`UltimeterError::BadBodyLength`]
/// when the field block is not one of the lengths the format allows,
/// and [`UltimeterError::BadHexDigit`] /
/// [`UltimeterError::MixedDashField`] for a corrupt field.
///
/// ```
/// use warble::aprs::ultimeter::{UltimeterError, UltimeterRecord, parse};
///
/// // The APRS specification's own data-logger example (three absent
/// // sensors), with a <CR><LF> trailer the console appended.
/// let info = b"!!006B005803500000----03E9--------002105140000005D\r\n";
/// let UltimeterRecord::DataLogger(logger) = parse(info)? else {
///     panic!("data-logger mode expected");
/// };
/// assert_eq!(logger.temperature, Some(848)); // 84.8 degrees F
/// assert_eq!(logger.barometer, None); // "----": absent, not zero
/// assert_eq!(logger.indoor_humidity, None);
/// assert_eq!(logger.wind_average, Some(93)); // 9.3 kph
/// assert_eq!(logger.rest, b"\r\n");
/// # Ok::<(), UltimeterError>(())
/// ```
pub fn parse(info: &[u8]) -> Result<UltimeterRecord<'_>, UltimeterError> {
    match info.first().copied() {
        Some(b'$') => parse_packet(info),
        Some(b'!') => parse_data_logger(info),
        Some(b'*') => parse_ultimeter_two(info, WindUnit::Mph),
        Some(b'#') => parse_ultimeter_two(info, WindUnit::Kph),
        Some(got) => Err(UltimeterError::InvalidDataType { got }),
        None => Err(UltimeterError::Truncated {
            expected: 1,
            got: 0,
        }),
    }
}

/// Parses a `$ULTW` packet-mode record.
fn parse_packet(info: &[u8]) -> Result<UltimeterRecord<'_>, UltimeterError> {
    let header = 1 + PACKET_PREFIX.len();
    if info.len() < header {
        return Err(UltimeterError::Truncated {
            expected: header,
            got: info.len(),
        });
    }
    let prefix = &info[1..header];
    if prefix != &PACKET_PREFIX[..] {
        let mut got = [0u8; 4];
        got.copy_from_slice(prefix);
        return Err(UltimeterError::NotPacketMode { got });
    }
    let (block, rest) = split_block(&info[header..], UltimeterFormat::Packet)?;
    Ok(UltimeterRecord::Packet(PacketMode {
        wind_peak: word(block, 0)?,
        wind_direction: direction(block, 4)?,
        temperature: signed_word(block, 8)?,
        rain_total: word(block, 12)?,
        barometer: word(block, 16)?,
        barometer_delta: signed_word(block, 20)?,
        barometer_correction_lsw: word(block, 24)?,
        barometer_correction_msw: word(block, 28)?,
        humidity: word(block, 32)?,
        date: word(block, 36)?,
        time: word(block, 40)?,
        rain_today: word(block, 44)?,
        wind_average: word(block, 48)?,
        rest,
    }))
}

/// Parses a `!!` data-logger-mode record.
fn parse_data_logger(info: &[u8]) -> Result<UltimeterRecord<'_>, UltimeterError> {
    let second = *info.get(1).ok_or(UltimeterError::Truncated {
        expected: 2,
        got: info.len(),
    })?;
    if second != b'!' {
        return Err(UltimeterError::NotDataLogger { got: second });
    }
    let (block, rest) = split_block(&info[2..], UltimeterFormat::DataLogger)?;
    Ok(UltimeterRecord::DataLogger(DataLoggerMode {
        wind_speed: word(block, 0)?,
        wind_direction: direction(block, 4)?,
        temperature: signed_word(block, 8)?,
        rain_total: word(block, 12)?,
        barometer: word(block, 16)?,
        indoor_temperature: signed_word(block, 20)?,
        humidity: word(block, 24)?,
        indoor_humidity: word(block, 28)?,
        date: word(block, 32)?,
        time: word(block, 36)?,
        rain_today: word(block, 40)?,
        wind_average: word(block, 44)?,
        rest,
    }))
}

/// Parses a `*` / `#` Ultimeter II record.
fn parse_ultimeter_two(info: &[u8], unit: WindUnit) -> Result<UltimeterRecord<'_>, UltimeterError> {
    let (block, rest) = split_block(&info[1..], UltimeterFormat::UltimeterTwo(unit))?;
    Ok(UltimeterRecord::UltimeterTwo(UltimeterTwo {
        unit,
        wind_direction: byte_field(block, 0, 1)?,
        wind_speed: byte_field(block, 1, 2)?,
        temperature_bias: byte_field(block, 3, 2)?,
        rain_upper: hex_field(block, 5, 4)?,
        rain_lower: hex_field(block, 9, 4)?,
        rest,
    }))
}

/// Splits `body` into its field block and the uninterpreted trailer,
/// rejecting a block length `format` does not allow.
///
/// The block is the leading run of field characters (hex digits and the
/// `-` marker), so a `<CR><LF>` or any comment ends it.
fn split_block(body: &[u8], format: UltimeterFormat) -> Result<(&[u8], &[u8]), UltimeterError> {
    let run = body
        .iter()
        .take_while(|&&byte| byte == ABSENT || hex_digit(byte).is_some())
        .count();
    if !format.lengths().contains(&run) {
        return Err(UltimeterError::BadBodyLength { format, got: run });
    }
    Ok(body.split_at(run))
}

/// The value of one ASCII hex digit, or `None` if it is not one.
const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes one fixed-width field of at most [`MAX_FIELD_WIDTH`] hex
/// characters, `None` being an absent sensor (the whole field hyphens).
///
/// A field that starts past the end of the block is absent too: that is
/// how the optional trailing fields of a short record decode, and it is
/// what keeps every offset in this module from indexing out of range.
fn hex_field(block: &[u8], offset: usize, width: usize) -> Result<Option<u16>, UltimeterError> {
    debug_assert!(width <= MAX_FIELD_WIDTH, "field accumulator is a u16");
    let Some(field) = block.get(offset..offset + width) else {
        return Ok(None);
    };
    let mut value = 0u16;
    let mut dashes = 0usize;
    for (index, &byte) in field.iter().enumerate() {
        if byte == ABSENT {
            dashes += 1;
            continue;
        }
        let Some(digit) = hex_digit(byte) else {
            return Err(UltimeterError::BadHexDigit {
                got: byte,
                position: offset + index,
            });
        };
        value = value * 16 + u16::from(digit);
    }
    if dashes == width {
        return Ok(None);
    }
    if dashes != 0 {
        return Err(UltimeterError::MixedDashField { position: offset });
    }
    Ok(Some(value))
}

/// Decodes a four-hex-digit unsigned field.
fn word(block: &[u8], offset: usize) -> Result<Option<u16>, UltimeterError> {
    hex_field(block, offset, MAX_FIELD_WIDTH)
}

/// Decodes a four-hex-digit 16-bit two's-complement field.
fn signed_word(block: &[u8], offset: usize) -> Result<Option<i16>, UltimeterError> {
    Ok(word(block, offset)?.map(u16::cast_signed))
}

/// Decodes a wind-direction field, masking off the top byte: it can
/// read `FF` once a calibration offset has been applied.
fn direction(block: &[u8], offset: usize) -> Result<Option<u8>, UltimeterError> {
    Ok(word(block, offset)?.map(|raw| narrow(raw & 0xFF)))
}

/// Decodes a field of one or two hex characters (Ultimeter II).
fn byte_field(block: &[u8], offset: usize, width: usize) -> Result<Option<u8>, UltimeterError> {
    Ok(hex_field(block, offset, width)?.map(narrow))
}

/// Narrows a value already known to fit a byte.
fn narrow(value: u16) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

/// Wind direction in degrees from the raw 0-255 field, rounded.
///
/// `360 / 255`, so the full-scale reading is a full circle; see the
/// module documentation.
fn direction_degrees(raw: u8) -> u16 {
    let degrees = (u32::from(raw) * 360 + 127) / 255;
    u16::try_from(degrees).unwrap_or(360)
}

/// Wind direction in degrees from a 16-point compass index, rounded
/// (22.5-degree steps).
fn point_degrees(point: u8) -> u16 {
    (u16::from(point & 0x0F) * 45).div_ceil(2)
}

/// Converts 0.1 kph to a [`Speed`], exactly.
///
/// A tenth of a kilometre per hour is 100 000 µm/h and the canonical
/// unit is millimetres per hour, so this loses nothing. The previous
/// version of this function multiplied by 0.062 14 to reach whole
/// miles per hour on the spot, which was both approximate and
/// premature: the display unit is not the sensor's business.
const fn tenth_kph_to_speed(tenth_kph: u16) -> Speed {
    Speed::from_millimeters_per_hour(tenth_kph as i64 * 100_000)
}

/// Converts tenths of a degree Fahrenheit to a [`Temperature`],
/// exactly.
///
/// The canonical unit is 1/45 000 °C, chosen so integer Fahrenheit is
/// exact; a tenth of a degree Fahrenheit is 2500 of them.
const fn tenths_f_to_temperature(tenths: i16) -> Temperature {
    Temperature::from_tenths_fahrenheit(tenths as i32)
}

/// Converts tenths of a percent to a [`Humidity`], rounded.
///
/// Out-of-range readings become `None`: a relative humidity of zero or
/// above 100 is a corrupt field rather than a measurement, and the
/// wire has a spelling for "no value" but none for "nonsense".
fn tenths_to_humidity(tenths: u16) -> Option<Humidity> {
    let percent = (u32::from(tenths) + 5) / 10;
    Humidity::new(u8::try_from(percent).unwrap_or(u8::MAX)).ok()
}

/// Splits a minute-of-day count into `(hour, minute)`, rejecting a
/// count no day can contain.
fn time_of_day(minute_of_day: u16) -> Option<(u8, u8)> {
    if minute_of_day >= 24 * 60 {
        return None;
    }
    let hour = u8::try_from(minute_of_day / 60).ok()?;
    let minute = u8::try_from(minute_of_day % 60).ok()?;
    Some((hour, minute))
}

#[cfg(test)]
mod tests {
    use core::fmt::Write as _;

    use super::*;

    /// The verified off-air vector: all 13 packet-mode fields.
    const VERIFIED: &[u8] = b"$ULTW0000000001FF000427C70002CCD30001026E003A050F00040000";

    /// The data-logger vector from the APRS specification (chapter 12,
    /// "Examples"), with three absent sensors.
    const LOGGER: &[u8] = b"!!006B005803500000----03E9--------002105140000005D";

    fn packet(info: &[u8]) -> PacketMode<'_> {
        match parse(info) {
            Ok(UltimeterRecord::Packet(p)) => p,
            other => panic!("expected packet mode, got {other:?}"),
        }
    }

    fn logger(info: &[u8]) -> DataLoggerMode<'_> {
        match parse(info) {
            Ok(UltimeterRecord::DataLogger(l)) => l,
            other => panic!("expected data-logger mode, got {other:?}"),
        }
    }

    fn two(info: &[u8]) -> UltimeterTwo<'_> {
        match parse(info) {
            Ok(UltimeterRecord::UltimeterTwo(t)) => t,
            other => panic!("expected Ultimeter II, got {other:?}"),
        }
    }

    /// Builds a 52-character packet-mode record whose third field (the
    /// outdoor temperature) is `hex`.
    fn packet_with_temperature(hex: &[u8; 4], out: &mut [u8; 57]) {
        out.copy_from_slice(VERIFIED);
        out[13..17].copy_from_slice(hex);
    }

    #[test]
    fn packet_mode_verified_vector() {
        let p = packet(VERIFIED);
        assert_eq!(p.wind_peak, Some(0)); // 0.0 kph
        assert_eq!(p.wind_direction, Some(0)); // north
        assert_eq!(p.temperature, Some(511)); // +51.1 F
        assert_eq!(p.rain_total, Some(4)); // 0.04 in
        assert_eq!(p.barometer, Some(10_183)); // 1018.3 mbar
        assert_eq!(p.barometer_delta, Some(2)); // +0.2 mbar
        assert_eq!(p.barometer_correction_lsw, Some(0xCCD3));
        assert_eq!(p.barometer_correction_msw, Some(1));
        assert_eq!(p.humidity, Some(622)); // 62.2 %
        assert_eq!(p.date, Some(58));
        assert_eq!(p.day_of_year(), Some(59));
        assert_eq!(p.time, Some(1295));
        assert_eq!(p.time_of_day(), Some((21, 35)));
        assert_eq!(p.rain_today, Some(4)); // 0.04 in
        assert_eq!(p.wind_average, Some(0)); // 0.0 kph
        assert_eq!(p.rest, b"");

        let w = p.to_weather_report();
        assert_eq!(w.wind_direction, Some(0));
        assert_eq!(w.wind_speed.map(Speed::mph), Some(0));
        assert_eq!(w.gust.map(Speed::mph), Some(0));
        assert_eq!(w.temperature.map(Temperature::fahrenheit), Some(51));
        assert_eq!(w.rain_1h, None);
        assert_eq!(w.rain_24h, None);
        assert_eq!(w.rain_midnight.map(Rainfall::hundredths_inch), Some(4));
        assert_eq!(w.humidity.map(Humidity::percent), Some(62));
        assert_eq!(
            w.barometric_pressure.map(Pressure::tenths_hpa),
            Some(10_183)
        );
    }

    #[test]
    fn packet_mode_truncation_lengths() {
        // 52 characters: every field present.
        assert_eq!(VERIFIED.len(), 5 + 52);
        let full = packet(VERIFIED);
        assert_eq!(full.rain_today, Some(4));
        assert_eq!(full.wind_average, Some(0));

        // 48 characters: fields 1-12, no five-minute average.
        let short = packet(&VERIFIED[..5 + 48]);
        assert_eq!(short.time, Some(1295));
        assert_eq!(short.rain_today, Some(4));
        assert_eq!(short.wind_average, None);

        // 44 characters: fields 1-11, no rain total for today either.
        let shorter = packet(&VERIFIED[..5 + 44]);
        assert_eq!(shorter.time, Some(1295));
        assert_eq!(shorter.rain_today, None);
        assert_eq!(shorter.wind_average, None);
        // A missing field is missing, never zero.
        assert_eq!(shorter.to_weather_report().wind_speed, None);
        assert_eq!(shorter.to_weather_report().rain_midnight, None);

        // Anything else is corrupt.
        assert_eq!(
            parse(&VERIFIED[..5 + 46]),
            Err(UltimeterError::BadBodyLength {
                format: UltimeterFormat::Packet,
                got: 46
            })
        );
    }

    #[test]
    fn data_logger_spec_vector() {
        let l = logger(LOGGER);
        assert_eq!(l.wind_speed, Some(0x6B));
        assert_eq!(l.wind_direction, Some(0x58));
        assert_eq!(l.temperature, Some(848)); // 84.8 F
        assert_eq!(l.rain_total, Some(0));
        // Absent sensors: field 5, then fields 7 and 8 together.
        assert_eq!(l.barometer, None);
        assert_eq!(l.indoor_temperature, Some(1001));
        assert_eq!(l.humidity, None);
        assert_eq!(l.indoor_humidity, None);
        assert_eq!(l.date, Some(33));
        assert_eq!(l.day_of_year(), Some(34));
        assert_eq!(l.time, Some(1300));
        assert_eq!(l.time_of_day(), Some((21, 40)));
        assert_eq!(l.rain_today, Some(0));
        assert_eq!(l.wind_average, Some(93)); // 9.3 kph

        let w = l.to_weather_report();
        // The one-minute average, not the instantaneous field 1.
        assert_eq!(w.wind_speed.map(Speed::mph), Some(6)); // 9.3 kph == 5.8 mph
        assert_eq!(w.gust, None); // no peak field in this format
        assert_eq!(w.temperature.map(Temperature::fahrenheit), Some(85)); // 84.8 F
        assert_eq!(w.barometric_pressure, None);
        assert_eq!(w.humidity, None);
        assert_eq!(w.rain_midnight.map(Rainfall::hundredths_inch), Some(0));
    }

    #[test]
    fn data_logger_field_order_differs_from_packet_mode() {
        // Same 20 leading characters in both formats, then a field the
        // formats disagree about: packet mode reads a signed barometer
        // delta, the data logger an indoor temperature.
        let body = b"000000A600B5000027C7FF9C";
        let mut packet_info = [0u8; 5 + 52];
        packet_info[..5].copy_from_slice(b"$ULTW");
        packet_info[5..5 + body.len()].copy_from_slice(body);
        for slot in &mut packet_info[5 + body.len()..] {
            *slot = b'0';
        }
        let mut logger_info = [0u8; 2 + 48];
        logger_info[..2].copy_from_slice(b"!!");
        logger_info[2..2 + body.len()].copy_from_slice(body);
        for slot in &mut logger_info[2 + body.len()..] {
            *slot = b'0';
        }

        let p = packet(&packet_info);
        let l = logger(&logger_info);
        assert_eq!(p.barometer, l.barometer);
        assert_eq!(p.barometer_delta, Some(-100));
        assert_eq!(l.indoor_temperature, Some(-100));
        assert_eq!(p.humidity, Some(0)); // field 9 in packet mode
        assert_eq!(l.humidity, Some(0)); // field 7 in the data logger
    }

    #[test]
    fn data_logger_truncation_lengths() {
        assert_eq!(LOGGER.len(), 2 + 48);
        let full = logger(LOGGER);
        assert_eq!(full.rain_today, Some(0));
        assert_eq!(full.wind_average, Some(93));

        let short = logger(&LOGGER[..2 + 44]);
        assert_eq!(short.rain_today, Some(0));
        assert_eq!(short.wind_average, None);
        // Field 12 gone: the instantaneous reading (field 1, 10.7 kph)
        // is the fallback.
        assert_eq!(
            short.to_weather_report().wind_speed.map(Speed::mph),
            Some(7)
        );

        let shorter = logger(&LOGGER[..2 + 40]);
        assert_eq!(shorter.time, Some(1300));
        assert_eq!(shorter.rain_today, None);
        assert_eq!(shorter.wind_average, None);

        assert_eq!(
            parse(&LOGGER[..2 + 42]),
            Err(UltimeterError::BadBodyLength {
                format: UltimeterFormat::DataLogger,
                got: 42
            })
        );
    }

    #[test]
    fn negative_temperatures_are_twos_complement() {
        let mut info = [0u8; 57];

        packet_with_temperature(b"FF9C", &mut info);
        let p = packet(&info);
        assert_eq!(p.temperature, Some(-100)); // -10.0 F
        assert_eq!(
            p.to_weather_report()
                .temperature
                .map(Temperature::fahrenheit),
            Some(-10)
        );

        packet_with_temperature(b"FFFF", &mut info);
        let p = packet(&info);
        assert_eq!(p.temperature, Some(-1)); // -0.1 F
        assert_eq!(
            p.to_weather_report()
                .temperature
                .map(Temperature::fahrenheit),
            Some(0)
        );

        // The extremes of the two's-complement range.
        packet_with_temperature(b"8000", &mut info);
        assert_eq!(packet(&info).temperature, Some(-32_768));
        packet_with_temperature(b"7FFF", &mut info);
        assert_eq!(packet(&info).temperature, Some(32_767));
        // ... and the boundary itself.
        packet_with_temperature(b"0000", &mut info);
        assert_eq!(packet(&info).temperature, Some(0));

        // Lowercase hex decodes identically.
        packet_with_temperature(b"ff9c", &mut info);
        assert_eq!(packet(&info).temperature, Some(-100));
    }

    #[test]
    fn absent_sensors_are_none_and_mixed_fields_are_corrupt() {
        let mut info = [0u8; 57];

        packet_with_temperature(b"----", &mut info);
        let p = packet(&info);
        assert_eq!(p.temperature, None);
        assert_eq!(p.to_weather_report().temperature, None);
        // The neighbouring fields still decode.
        assert_eq!(p.wind_direction, Some(0));
        assert_eq!(p.rain_total, Some(4));

        // A field that is part hex, part marker is corruption, not an
        // absent sensor: it must not decode to 0x12 or to None.
        packet_with_temperature(b"12--", &mut info);
        assert_eq!(
            parse(&info),
            Err(UltimeterError::MixedDashField { position: 8 })
        );
        packet_with_temperature(b"--12", &mut info);
        assert_eq!(
            parse(&info),
            Err(UltimeterError::MixedDashField { position: 8 })
        );
        packet_with_temperature(b"-1-2", &mut info);
        assert_eq!(
            parse(&info),
            Err(UltimeterError::MixedDashField { position: 8 })
        );
    }

    #[test]
    fn wind_direction_masks_the_top_byte_and_divides_by_255() {
        let mut info = [0u8; 57];
        info.copy_from_slice(VERIFIED);

        // A calibration offset can leave the top byte set; only the low
        // byte is the direction.
        info[9..13].copy_from_slice(b"FFFF");
        let p = packet(&info);
        assert_eq!(p.wind_direction, Some(0xFF));
        // Full scale is a full circle, which is what picks 255 over 256
        // as the divisor: 256 would give 359 degrees here.
        assert_eq!(p.wind_direction_degrees(), Some(360));

        info[9..13].copy_from_slice(b"FF40");
        let p = packet(&info);
        assert_eq!(p.wind_direction, Some(0x40));
        assert_eq!(p.wind_direction_degrees(), Some(90)); // east
        assert_eq!(p.to_weather_report().wind_direction, Some(90));

        // The cost of mapping full scale onto a full circle: the
        // cardinals land a degree high (256 would put them exactly on
        // 180 and 270). Well inside any anemometer's accuracy.
        info[9..13].copy_from_slice(b"0080");
        assert_eq!(packet(&info).wind_direction_degrees(), Some(181)); // south
        info[9..13].copy_from_slice(b"00C0");
        assert_eq!(packet(&info).wind_direction_degrees(), Some(271)); // west
        info[9..13].copy_from_slice(b"----");
        assert_eq!(packet(&info).wind_direction_degrees(), None);
    }

    #[test]
    fn ultimeter_two_speed_unit_follows_the_identifier() {
        // Identical bodies, different identifiers.
        let mph = two(b"*41EA0006400C8");
        let kph = two(b"#41EA0006400C8");
        assert_eq!(mph.unit, WindUnit::Mph);
        assert_eq!(kph.unit, WindUnit::Kph);
        assert_eq!(mph.wind_speed, Some(0x1E)); // 30, raw
        assert_eq!(kph.wind_speed, Some(0x1E));

        // ... and the same raw reading is a 1.6x different speed.
        assert_eq!(mph.wind_speed_mph(), Some(30));
        assert_eq!(kph.wind_speed_mph(), Some(19)); // 30 km/h
        assert_eq!(mph.to_weather_report().wind_speed.map(Speed::mph), Some(30));
        assert_eq!(kph.to_weather_report().wind_speed.map(Speed::mph), Some(19));

        assert_eq!(
            UltimeterRecord::UltimeterTwo(mph).format(),
            UltimeterFormat::UltimeterTwo(WindUnit::Mph)
        );
        assert_eq!(
            UltimeterRecord::UltimeterTwo(kph).format(),
            UltimeterFormat::UltimeterTwo(WindUnit::Kph)
        );
    }

    #[test]
    fn ultimeter_two_fields_use_their_own_scaling() {
        let t = two(b"*41EA0006400C8\r\n");
        // One hex digit, 16-point compass: 4 is east.
        assert_eq!(t.wind_direction, Some(4));
        assert_eq!(t.wind_direction_degrees(), Some(90));
        // Temperature is biased by 56, not two's complement.
        assert_eq!(t.temperature_bias, Some(0xA0));
        assert_eq!(t.temperature(), Some(104)); // 160 - 56
        // Rain gauges are already hundredths of an inch.
        assert_eq!(t.rain_upper, Some(100)); // 1.00 in
        assert_eq!(t.rain_lower, Some(200)); // 2.00 in
        assert_eq!(t.rest, b"\r\n");

        // The two gauges are not mapped into APRS.
        let w = t.to_weather_report();
        assert_eq!(w.rain_1h, None);
        assert_eq!(w.rain_24h, None);
        assert_eq!(w.rain_midnight, None);
        assert_eq!(w.humidity, None);
        assert_eq!(w.barometric_pressure, None);
        assert_eq!(w.gust, None);

        // The whole compass, and the bias at both ends.
        assert_eq!(two(b"*0000000000000").wind_direction_degrees(), Some(0));
        assert_eq!(two(b"*8000000000000").wind_direction_degrees(), Some(180));
        assert_eq!(two(b"*C000000000000").wind_direction_degrees(), Some(270));
        assert_eq!(two(b"*1000000000000").wind_direction_degrees(), Some(23));
        assert_eq!(two(b"*0000000000000").temperature(), Some(-56));
        assert_eq!(two(b"*000FF00000000").temperature(), Some(199));
        // An absent gauge is still absent at this width.
        assert_eq!(two(b"*41EA0----00C8").rain_upper, None);
    }

    #[test]
    fn identifiers_are_disambiguated_from_their_neighbours() {
        // '!' is shared with position-without-timestamp.
        assert_eq!(detect(b"!4903.50N/07201.75W#"), None);
        assert_eq!(
            parse(b"!4903.50N/07201.75W#"),
            Err(UltimeterError::NotDataLogger { got: b'4' })
        );
        // ... including the compressed form, whose second byte is a
        // symbol table identifier.
        assert_eq!(
            parse(b"!/5L!!<*e7>7P["),
            Err(UltimeterError::NotDataLogger { got: b'/' })
        );

        // '$' is shared with raw NMEA.
        assert_eq!(detect(b"$GPRMC,063740,A,3349.2153,N"), None);
        assert_eq!(
            parse(b"$GPRMC,063740,A,3349.2153,N"),
            Err(UltimeterError::NotPacketMode { got: *b"GPRM" })
        );
        // '$ULTI' is a mis-recollection and is not accepted.
        assert_eq!(
            parse(b"$ULTI0000000001FF"),
            Err(UltimeterError::NotPacketMode { got: *b"ULTI" })
        );

        assert_eq!(detect(VERIFIED), Some(UltimeterFormat::Packet));
        assert_eq!(detect(LOGGER), Some(UltimeterFormat::DataLogger));
        assert_eq!(
            detect(b"*41EA0006400C8"),
            Some(UltimeterFormat::UltimeterTwo(WindUnit::Mph))
        );
        assert_eq!(
            detect(b"#41EA0006400C8"),
            Some(UltimeterFormat::UltimeterTwo(WindUnit::Kph))
        );
        assert_eq!(detect(b""), None);
        assert_eq!(detect(b"_10090556c220"), None);
    }

    #[test]
    fn identifier_round_trip() {
        assert_eq!(UltimeterFormat::Packet.identifier(), b'$');
        assert_eq!(UltimeterFormat::DataLogger.identifier(), b'!');
        assert_eq!(
            UltimeterFormat::UltimeterTwo(WindUnit::Mph).identifier(),
            b'*'
        );
        assert_eq!(
            UltimeterFormat::UltimeterTwo(WindUnit::Kph).identifier(),
            b'#'
        );
    }

    #[test]
    fn trailers_are_borrowed_not_interpreted() {
        let mut info = [0u8; 59];
        info[..57].copy_from_slice(VERIFIED);
        info[57] = b'\r';
        info[58] = b'\n';
        let record = match parse(&info) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(record.rest(), b"\r\n");
        assert_eq!(record.format(), UltimeterFormat::Packet);
        assert_eq!(
            record
                .to_weather_report()
                .temperature
                .map(Temperature::fahrenheit),
            Some(51)
        );
    }

    #[test]
    fn malformed_records_are_typed_errors() {
        assert_eq!(
            parse(b""),
            Err(UltimeterError::Truncated {
                expected: 1,
                got: 0
            })
        );
        assert_eq!(
            parse(b"!"),
            Err(UltimeterError::Truncated {
                expected: 2,
                got: 1
            })
        );
        assert_eq!(
            parse(b"$ULT"),
            Err(UltimeterError::Truncated {
                expected: 5,
                got: 4
            })
        );
        assert_eq!(
            parse(b"T#005,199,000,255,073,123,01101001"),
            Err(UltimeterError::InvalidDataType { got: b'T' })
        );
        // A non-hex byte inside the block ends the run, so the block is
        // the wrong length rather than holding a bad digit ...
        assert_eq!(
            parse(b"$ULTW0000000001FF000427C70002CCD3000!026E003A050F00040000"),
            Err(UltimeterError::BadBodyLength {
                format: UltimeterFormat::Packet,
                got: 31
            })
        );
        // ... which leaves BadHexDigit for a block whose length is
        // right but that the trailing scan never saw, e.g. a NUL.
        assert_eq!(
            hex_field(b"00\x000", 0, 4),
            Err(UltimeterError::BadHexDigit {
                got: 0,
                position: 2
            })
        );
        // An Ultimeter II record is exactly 13 characters.
        assert_eq!(
            parse(b"*41EA0006400C"),
            Err(UltimeterError::BadBodyLength {
                format: UltimeterFormat::UltimeterTwo(WindUnit::Mph),
                got: 12
            })
        );
    }

    #[test]
    fn unit_conversions_round_to_nearest() {
        // 0.1 kph -> mph.
        // 0.1 kph into a `Speed` is exact; the mph reading rounds at
        // the point somebody asks for it, not at the sensor.
        assert_eq!(tenth_kph_to_speed(0).mph(), 0);
        assert_eq!(tenth_kph_to_speed(161).mph(), 10); // 16.1 kph == 10.0 mph
        assert_eq!(tenth_kph_to_speed(100).mph(), 6); // 10.0 kph == 6.2 mph
        assert_eq!(tenth_kph_to_speed(80).mph(), 5); // 8.0 kph == 5.0 mph
        assert_eq!(tenth_kph_to_speed(1000).mph(), 62); // 100 kph == 62.1 mph
        assert_eq!(tenth_kph_to_speed(1000).kmh(), 100); // and exactly 100 km/h
        assert_eq!(tenth_kph_to_speed(u16::MAX).mph(), 4072); // no overflow

        // 0.1 F -> whole F, away from zero at the half.
        assert_eq!(tenths_f_to_temperature(0).fahrenheit(), 0);
        assert_eq!(tenths_f_to_temperature(5).fahrenheit(), 1);
        assert_eq!(tenths_f_to_temperature(4).fahrenheit(), 0);
        assert_eq!(tenths_f_to_temperature(-5).fahrenheit(), -1);
        assert_eq!(tenths_f_to_temperature(-4).fahrenheit(), 0);
        assert_eq!(tenths_f_to_temperature(i16::MAX).fahrenheit(), 3277);
        assert_eq!(tenths_f_to_temperature(i16::MIN).fahrenheit(), -3277);
        // The two anchors everyone knows, on the other scale.
        assert_eq!(tenths_f_to_temperature(320).celsius(), 0);
        assert_eq!(tenths_f_to_temperature(2120).celsius(), 100);

        // 0.1 % -> whole %.
        // Zero and above-100 readings are corruption, not
        // measurements, and the wire has a spelling for "no value" but
        // none for "nonsense".
        assert_eq!(tenths_to_humidity(0), None);
        assert_eq!(tenths_to_humidity(622).map(Humidity::percent), Some(62));
        assert_eq!(tenths_to_humidity(1000).map(Humidity::percent), Some(100));
        assert_eq!(tenths_to_humidity(u16::MAX), None);

        // Minute of day -> wall clock.
        assert_eq!(time_of_day(0), Some((0, 0)));
        assert_eq!(time_of_day(1295), Some((21, 35)));
        assert_eq!(time_of_day(1439), Some((23, 59)));
        assert_eq!(time_of_day(1440), None);
        assert_eq!(time_of_day(u16::MAX), None);
    }

    #[test]
    fn error_messages_render() {
        let errors = [
            UltimeterError::Truncated {
                expected: 5,
                got: 2,
            },
            UltimeterError::InvalidDataType { got: b'T' },
            UltimeterError::NotPacketMode { got: *b"GPRM" },
            UltimeterError::NotDataLogger { got: b'4' },
            UltimeterError::BadBodyLength {
                format: UltimeterFormat::Packet,
                got: 46,
            },
            UltimeterError::BadBodyLength {
                format: UltimeterFormat::DataLogger,
                got: 42,
            },
            UltimeterError::BadBodyLength {
                format: UltimeterFormat::UltimeterTwo(WindUnit::Kph),
                got: 12,
            },
            UltimeterError::BadHexDigit {
                got: b'x',
                position: 3,
            },
            UltimeterError::MixedDashField { position: 8 },
        ];
        for error in errors {
            let mut sink = Sink::new();
            match write!(&mut sink, "{error}") {
                Ok(()) => {}
                Err(e) => panic!("{e}"),
            }
            assert!(sink.len > 16, "{error:?} rendered {} bytes", sink.len);
        }
    }

    #[test]
    fn byte_soup_never_panics() {
        let mut rng = Xorshift(0x2545_F491_4F6C_DD1D);
        // A field-shaped alphabet finds far more parser states than
        // uniform noise would: hex digits, the absent marker, the four
        // identifiers, and the bytes that end a block.
        const ALPHABET: &[u8] = b"0123456789abcdefABCDEF------$!*#ULTW \r\n,.";
        const MAX_LEN: u64 = 64;
        let mut buf = [0u8; MAX_LEN as usize];
        for _ in 0..20_000 {
            let draw = rng.next_u64();
            let len = usize::try_from(draw % (MAX_LEN + 1)).unwrap_or(0);
            let biased = (draw & (1u64 << 63)) == 0;
            for slot in &mut buf[..len] {
                let byte = u8::try_from(rng.next_u64() & 0xFF).unwrap_or(0);
                *slot = if biased {
                    ALPHABET[usize::from(byte) % ALPHABET.len()]
                } else {
                    byte
                };
            }
            let info = &buf[..len];
            let detected = detect(info);
            match parse(info) {
                Ok(record) => {
                    assert_eq!(detected, Some(record.format()));
                    assert!(record.rest().len() < info.len());
                    // Every accessor must survive whatever decoded, and
                    // hold its documented range while it does.
                    let weather = record.to_weather_report();
                    assert!(weather.wind_direction.is_none_or(|d| d <= 360));
                    let clock = match record {
                        UltimeterRecord::Packet(p) => {
                            assert!(p.day_of_year().is_none_or(|d| d >= 1));
                            p.time_of_day()
                        }
                        UltimeterRecord::DataLogger(l) => {
                            assert!(l.day_of_year().is_none_or(|d| d >= 1));
                            l.time_of_day()
                        }
                        UltimeterRecord::UltimeterTwo(t) => {
                            assert!(t.temperature().is_none_or(|c| c >= -56));
                            assert!(t.wind_direction_degrees().is_none_or(|d| d <= 338));
                            None
                        }
                    };
                    assert!(clock.is_none_or(|(h, m)| h < 24 && m < 60));
                }
                Err(e) => {
                    // A rejected field must not have been detected as a
                    // format unless the body itself was at fault.
                    assert!(
                        detected.is_some()
                            || matches!(
                                e,
                                UltimeterError::Truncated { .. }
                                    | UltimeterError::InvalidDataType { .. }
                                    | UltimeterError::NotPacketMode { .. }
                                    | UltimeterError::NotDataLogger { .. }
                            ),
                        "{e:?} for {info:?}"
                    );
                }
            }
        }
    }

    /// A deterministic xorshift64 generator: the byte-soup test must
    /// exercise the same inputs on every run.
    struct Xorshift(u64);

    impl Xorshift {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    /// A fixed-size `core::fmt::Write` sink: the crate has no allocator
    /// to render into.
    struct Sink {
        buf: [u8; 160],
        len: usize,
    }

    impl Sink {
        fn new() -> Self {
            Sink {
                buf: [0; 160],
                len: 0,
            }
        }
    }

    impl fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = (self.len + s.len()).min(self.buf.len());
            let taken = end - self.len;
            self.buf[self.len..end].copy_from_slice(&s.as_bytes()[..taken]);
            self.len = end;
            Ok(())
        }
    }
}
